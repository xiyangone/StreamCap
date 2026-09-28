//! Bounded notifications. Transport errors never print destination credentials.
use crate::{config::ConfigStore, model::Recording, store::Store};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::RwLock;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
#[derive(Clone, PartialEq)]
pub struct Delivery {
    pub channel: &'static str,
    pub url: String,
    pub payload: Value,
    pub form: bool,
}
pub struct DeliveryPlan {
    pub deliveries: Vec<Delivery>,
    pub errors: Vec<String>,
}
pub fn deliveries(config: &ConfigStore, title: &str, content: &str) -> DeliveryPlan {
    let mut plan = DeliveryPlan {
        deliveries: Vec::new(),
        errors: Vec::new(),
    };
    let mut add = |channel: &'static str, result: Result<Delivery, String>| match result {
        Ok(delivery) => plan.deliveries.push(delivery),
        Err(error) => plan.errors.push(format!("{channel} 配置无效：{error}")),
    };
    for (enabled, key, channel, payload) in [
        (
            "dingtalk_enabled",
            "dingtalk_webhook_url",
            "dingtalk",
            json!({"msgtype":"text","text":{"content":format!("{title}\n{content}")},"at":{"atMobiles":config.get_str("dingtalk_at_objects","").split([',','，',';']).map(str::trim).filter(|s|!s.is_empty()).collect::<Vec<_>>(),"isAtAll":config.get_bool("dingtalk_at_all",false)}}),
        ),
        (
            "wechat_enabled",
            "wechat_webhook_url",
            "wechat",
            json!({"msgtype":"text","text":{"content":format!("{title}\n{content}")}}),
        ),
        (
            "feishu_enabled",
            "feishu_webhook_url",
            "feishu",
            json!({"msg_type":"text","content":{"text":format!("{title}\n{content}")}}),
        ),
        (
            "bark_enabled",
            "bark_webhook_url",
            "bark",
            json!({"title":title,"body":content,"level":config.get_str("bark_interrupt_level","active"),"sound":config.get_str("bark_sound","")}),
        ),
        (
            "ntfy_enabled",
            "ntfy_server_url",
            "ntfy",
            json!({"title":title,"message":content,"tags":config.get_str("ntfy_tags","").split(',').map(str::trim).filter(|s|!s.is_empty()).collect::<Vec<_>>() }),
        ),
    ] {
        if !config.get_bool(enabled, false) {
            continue;
        }
        let result = (|| {
            let mut url = checked_url(&config.get_str(key, ""))?;
            let mut payload = payload;
            if channel == "bark" {
                let device = url.path().trim_matches('/');
                if device.is_empty() {
                    return Err("地址缺少设备标识".into());
                }
                payload["device_key"] = json!(device);
                url.set_path("/push");
            }
            if channel == "ntfy" {
                let topic = url.path().trim_matches('/');
                if topic.is_empty() {
                    return Err("地址缺少 topic".into());
                }
                payload["topic"] = json!(topic);
                url.set_path("/");
                let email = config.get_str("ntfy_email", "");
                if !email.trim().is_empty() {
                    payload["email"] = json!(email);
                }
                let action = config.get_str("ntfy_action_url", "");
                if !action.trim().is_empty() {
                    checked_url(&action)?;
                    payload["actions"] =
                        json!([{"action":"view","label":"打开直播间","url":action}]);
                }
            }
            Ok(Delivery {
                channel,
                url: url.to_string(),
                payload,
                form: false,
            })
        })();
        add(channel, result);
    }
    if config.get_bool("telegram_enabled", false) {
        let result = (|| {
            let token = config.get_str("telegram_api_token", "");
            let chat = config.get_str("telegram_chat_id", "");
            if token.is_empty()
                || token.contains(['/', '?', '#', '\r', '\n'])
                || chat.trim().is_empty()
            {
                return Err("需要有效 Token 和 Chat ID".into());
            }
            Ok(Delivery {
                channel: "telegram",
                url: format!("https://api.telegram.org/bot{token}/sendMessage"),
                payload: json!({"chat_id":chat,"text":format!("{title}\n{content}")}),
                form: false,
            })
        })();
        add("telegram", result);
    }
    if config.get_bool("serverchan_enabled", false) {
        let result = (|| {
            let key = config.get_str("serverchan_sendkey", "");
            if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return Err("SendKey 无效".into());
            }
            Ok(Delivery {
                channel: "serverchan",
                url: format!("https://sctapi.ftqq.com/{key}.send"),
                payload: json!({"title":title,"desp":content,"channel":config.get_str("serverchan_channel",""),"tags":config.get_str("serverchan_tags","")}),
                form: true,
            })
        })();
        add("serverchan", result);
    }
    plan
}
fn checked_url(value: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(value).map_err(|_| "通知地址无效")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("通知地址无效".into());
    }
    Ok(url)
}
pub fn accepted(channel: &str, value: &Value) -> bool {
    match channel {
        "telegram" => value["ok"] == true,
        "dingtalk" | "wechat" => value["errcode"] == 0,
        "feishu" => value["code"] == 0 || value["StatusCode"] == 0,
        "serverchan" => value["code"] == 0,
        "bark" => value["code"] == 200,
        "ntfy" => value["event"] == "message",
        _ => false,
    }
}
const NOTIFICATION_CAPACITY: usize = 128;
const NOTIFICATION_WORKERS: usize = 4;
#[derive(PartialEq)]
struct Notification {
    record_id: String,
    started: bool,
    title: String,
    content: String,
    deliveries: Vec<Delivery>,
    email: Option<EmailConfig>,
}
#[derive(Default)]
struct NotificationQueue {
    pending: VecDeque<Arc<Notification>>,
    active: HashMap<String, Arc<Notification>>,
    workers: usize,
}
impl NotificationQueue {
    fn enqueue(&mut self, message: Notification) -> Result<(), ()> {
        // Coalesce only the latest same-state event; preserve start/end/start transitions.
        if let Some(index) = self
            .pending
            .iter()
            .rposition(|p| p.record_id == message.record_id)
        {
            if self.pending[index].started == message.started {
                self.pending[index] = Arc::new(message);
                return Ok(());
            }
        } else if self
            .active
            .get(&message.record_id)
            .is_some_and(|active| active.as_ref() == &message)
        {
            return Ok(());
        }
        if self.pending.len() + self.active.len() >= NOTIFICATION_CAPACITY {
            return Err(());
        }
        self.pending.push_back(Arc::new(message));
        Ok(())
    }
    fn take(&mut self) -> Option<Arc<Notification>> {
        let index = self
            .pending
            .iter()
            .position(|p| !self.active.contains_key(&p.record_id))?;
        let message = self.pending.remove(index)?;
        self.active
            .insert(message.record_id.clone(), message.clone());
        Some(message)
    }
}

#[derive(Clone)]
pub struct Notifications {
    config: Arc<RwLock<ConfigStore>>,
    store: Store,
    stop: CancellationToken,
    tasks: TaskTracker,
    queue: Arc<Mutex<NotificationQueue>>,
}
impl Notifications {
    pub fn new(config: Arc<RwLock<ConfigStore>>, store: Store) -> Self {
        Self {
            config,
            store,
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
            queue: Arc::default(),
        }
    }
    pub async fn changed(&self, record: &Recording, started: bool) {
        if self.stop.is_cancelled() {
            return;
        }
        let config = self.config.read().await;
        let enabled = config.get_bool(
            if started {
                "stream_start_notification_enabled"
            } else {
                "stream_end_notification_enabled"
            },
            false,
        );
        if record.enabled_message_push == Some(false)
            || (!enabled && !config.get_bool("system_notification_enabled", false))
        {
            return;
        }
        let title = config.get_str("custom_notification_title", "StreamCap");
        let title = if title.trim().is_empty() {
            "StreamCap".to_owned()
        } else {
            title
        };
        let template = config.get_str(
            if started {
                "custom_stream_start_content"
            } else {
                "custom_stream_end_content"
            },
            if started {
                "[room_name] 已开播：[title]"
            } else {
                "[room_name] 已下播"
            },
        );
        let content = template
            .replace("[room_name]", &record.streamer_name)
            .replace("[title]", record.live_title.as_deref().unwrap_or(""))
            .replace(
                "[time]",
                &chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            );
        if config.get_bool("system_notification_enabled", false) {
            self.store
                .emit("nativeNotification", json!({"title":title,"body":content}));
        }
        if !enabled {
            return;
        }
        let plan = deliveries(&config, &title, &content);
        for error in plan.errors {
            self.store.snack_error(error);
        }
        let plan = plan.deliveries;
        let email = config
            .get_bool("email_enabled", false)
            .then(|| email_config(&config));
        drop(config);
        if plan.is_empty() && email.is_none() {
            return;
        }
        self.enqueue(Notification {
            record_id: record.rec_id.clone(),
            started,
            title,
            content,
            deliveries: plan,
            email,
        });
    }
    fn enqueue(&self, message: Notification) {
        let mut queue = self.queue.lock().expect("notification queue");
        if self.stop.is_cancelled() {
            return;
        }
        if queue.enqueue(message).is_err() {
            drop(queue);
            self.store.snack_error("通知队列已满，本次消息未发送");
            return;
        }
        // Only a fixed number of workers may wait on transports. No task per queued message.
        while queue.workers < NOTIFICATION_WORKERS
            && queue.workers < queue.pending.len() + queue.active.len()
        {
            queue.workers += 1;
            let manager = self.clone();
            self.tasks.spawn(async move {
                manager.run_worker().await;
            });
        }
    }
    async fn run_worker(&self) {
        loop {
            let message = {
                let mut queue = self.queue.lock().expect("notification queue");
                let message = if self.stop.is_cancelled() {
                    None
                } else {
                    queue.take()
                };
                match message {
                    Some(message) => message,
                    None => {
                        queue.workers -= 1;
                        return;
                    }
                }
            };
            tokio::select! { biased;
                _ = self.stop.cancelled() => {},
                _ = self.deliver(&message) => {},
            }
            self.queue
                .lock()
                .expect("notification queue")
                .active
                .remove(&message.record_id);
        }
    }
    async fn deliver(&self, message: &Notification) {
        let client = match reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(12))
            .redirect(reqwest::redirect::Policy::none())
            .build()
        {
            Ok(client) => client,
            Err(_) => {
                self.store.snack_error("无法初始化通知客户端");
                return;
            }
        };
        for delivery in &message.deliveries {
            let result = async {
                let request = client.post(&delivery.url);
                let response = if delivery.form {
                    request.form(&delivery.payload)
                } else {
                    request.json(&delivery.payload)
                }
                .send()
                .await
                .map_err(|_| ())?;
                if !response.status().is_success() {
                    return Err(());
                }
                let value = response_json(response).await?;
                if accepted(delivery.channel, &value) {
                    Ok(())
                } else {
                    Err(())
                }
            }
            .await;
            if result.is_err() {
                self.store.snack_error(format!(
                    "{} 通知发送失败，请检查配置和网络",
                    delivery.channel
                ));
            }
        }
        if let Some(email) = &message.email {
            if send_email(email.clone(), &message.title, &message.content)
                .await
                .is_err()
            {
                self.store.snack_error("邮件通知发送失败，请检查 SMTP 配置");
            }
        }
    }
    pub async fn shutdown(&self) {
        {
            let mut queue = self.queue.lock().expect("notification queue");
            self.stop.cancel();
            queue.pending.clear();
            self.tasks.close();
        }
        self.tasks.wait().await;
    }
}
async fn response_json(mut response: reqwest::Response) -> Result<Value, ()> {
    if response.content_length().is_some_and(|n| n > 64 * 1024) {
        return Err(());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        if body.len() + chunk.len() > 64 * 1024 {
            return Err(());
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| ())
}

#[derive(Clone, PartialEq)]
struct EmailConfig {
    host: String,
    port: u16,
    user: String,
    password: String,
    from: String,
    sender_name: String,
    to: String,
}
fn email_config(c: &ConfigStore) -> EmailConfig {
    EmailConfig {
        host: c.get_str("smtp_server", ""),
        port: u16::try_from(c.get_i64("smtp_port", 587)).unwrap_or(0),
        user: c.get_str("email_username", ""),
        password: c.get_str("email_password", ""),
        from: c.get_str("sender_email", ""),
        sender_name: c.get_str("sender_name", ""),
        to: c.get_str("recipient_email", ""),
    }
}
async fn send_email(c: EmailConfig, title: &str, body: &str) -> Result<(), String> {
    use lettre::{
        transport::smtp::authentication::Credentials, AsyncSmtpTransport, AsyncTransport, Message,
        Tokio1Executor,
    };
    if c.port == 0 {
        return Err("SMTP 端口无效".into());
    }
    let sender = lettre::message::Mailbox::new(
        (!c.sender_name.trim().is_empty()).then_some(c.sender_name),
        c.from.parse().map_err(|_| "发件人无效")?,
    );
    let mut builder = Message::builder().from(sender).subject(title);
    for recipient in
        c.to.split([',', ';'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        builder = builder.to(recipient.parse().map_err(|_| "收件人无效")?);
    }
    let message = builder.body(body.to_string()).map_err(|_| "邮件格式无效")?;
    let builder = if c.port == 465 {
        AsyncSmtpTransport::<Tokio1Executor>::relay(&c.host)
    } else {
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&c.host)
    }
    .map_err(|_| "SMTP 配置无效")?;
    let transport = builder
        .port(c.port)
        .timeout(Some(Duration::from_secs(15)))
        .credentials(Credentials::new(c.user, c.password))
        .build();
    transport.send(message).await.map_err(|_| "SMTP 发送失败")?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn message(id: &str, started: bool) -> Notification {
        Notification {
            record_id: id.into(),
            started,
            title: "fixture".into(),
            content: "body".into(),
            deliveries: vec![],
            email: None,
        }
    }
    #[test]
    fn notification_queue_coalesces_duplicates_without_losing_transitions() {
        let mut queue = NotificationQueue::default();
        queue.enqueue(message("a", true)).unwrap();
        queue.enqueue(message("a", true)).unwrap();
        assert_eq!(queue.pending.len(), 1);
        let active = queue.take().unwrap();
        assert!(active.started);
        queue.enqueue(message("a", true)).unwrap();
        assert!(queue.pending.is_empty());
        queue.enqueue(message("a", false)).unwrap();
        queue.enqueue(message("a", true)).unwrap();
        queue.enqueue(message("b", true)).unwrap();
        assert_eq!(queue.take().unwrap().record_id, "b");
        assert!(queue.take().is_none());
        queue.active.remove("a");
        assert!(!queue.take().unwrap().started);
        queue.active.remove("a");
        assert!(queue.take().unwrap().started);
    }
    #[test]
    fn notification_capacity_counts_active_and_pending() {
        let mut queue = NotificationQueue::default();
        for i in 0..NOTIFICATION_CAPACITY {
            queue.enqueue(message(&i.to_string(), true)).unwrap();
        }
        queue.take().unwrap();
        assert!(queue.enqueue(message("overflow", true)).is_err());
        assert!(queue.enqueue(message("1", true)).is_ok());
        queue.active.remove("0");
        assert!(queue.enqueue(message("next", true)).is_ok());
    }
    #[tokio::test]
    async fn notifications_drain_burst_with_bounded_workers_and_cancel_on_shutdown() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let received = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (r, a, p) = (received.clone(), active.clone(), peak.clone());
        let app = axum::Router::new().route(
            "/",
            axum::routing::post(move || {
                let (r, a, p) = (r.clone(), a.clone(), p.clone());
                async move {
                    let count = a.fetch_add(1, Ordering::SeqCst) + 1;
                    p.fetch_max(count, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(12)).await;
                    a.fetch_sub(1, Ordering::SeqCst);
                    r.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({"ok":true}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = tempfile::tempdir().unwrap();
        let config = ConfigStore::load(crate::Workspace::from_repo_root(dir.path())).unwrap();
        let manager = Notifications::new(
            Arc::new(RwLock::new(config)),
            Store::new(crate::Workspace::from_repo_root(dir.path())),
        );
        for i in 0..40 {
            let mut m = message(&i.to_string(), true);
            m.deliveries.push(Delivery {
                channel: "telegram",
                url: url.clone(),
                payload: json!({}),
                form: false,
            });
            manager.enqueue(m);
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while received.load(Ordering::SeqCst) < 40 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(peak.load(Ordering::SeqCst) <= NOTIFICATION_WORKERS);
        manager.shutdown().await;
        manager.enqueue(message("after-shutdown", true));
        let queue = manager.queue.lock().unwrap();
        assert!(queue.pending.is_empty());
        assert!(queue.active.is_empty());
        assert_eq!(queue.workers, 0);
        server.abort();
    }
    #[tokio::test]
    async fn shutdown_cancels_inflight_delivery_and_discards_pending_messages() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let (accepted, ready) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let _ = accepted.send(());
            std::future::pending::<()>().await;
            drop(socket);
        });
        let dir = tempfile::tempdir().unwrap();
        let workspace = crate::Workspace::from_repo_root(dir.path());
        let manager = Notifications::new(
            Arc::new(RwLock::new(ConfigStore::load(workspace.clone()).unwrap())),
            Store::new(workspace),
        );
        let mut first = message("a", true);
        first.deliveries.push(Delivery {
            channel: "telegram",
            url,
            payload: json!({}),
            form: false,
        });
        manager.enqueue(first);
        tokio::time::timeout(Duration::from_secs(2), ready)
            .await
            .unwrap()
            .unwrap();
        for n in 0..30 {
            manager.enqueue(message("a", n % 2 == 0));
        }
        assert!(!manager.queue.lock().unwrap().pending.is_empty());
        tokio::time::timeout(Duration::from_secs(1), manager.shutdown())
            .await
            .unwrap();
        let queue = manager.queue.lock().unwrap();
        assert!(queue.pending.is_empty());
        assert!(queue.active.is_empty());
        assert_eq!(queue.workers, 0);
        server.abort();
    }
    #[test]
    fn provider_failure_is_not_reported_as_success() {
        assert!(accepted("telegram", &json!({"ok":true})));
        assert!(!accepted("wechat", &json!({"errcode":40013})));
        assert!(!accepted("ntfy", &json!({"error":"bad topic"})));
    }
    #[test]
    fn all_enabled_webhook_channels_have_provider_payloads() {
        let temp = tempfile::tempdir().unwrap();
        let mut c = ConfigStore::load(crate::Workspace::from_repo_root(temp.path())).unwrap();
        c.update_user_config(json!({"dingtalk_enabled":true,"dingtalk_webhook_url":"https://example.test/ding","wechat_enabled":true,"wechat_webhook_url":"https://example.test/wechat","feishu_enabled":true,"feishu_webhook_url":"https://example.test/feishu","bark_enabled":true,"bark_webhook_url":"https://example.test/device","ntfy_enabled":true,"ntfy_server_url":"https://example.test/topic","telegram_enabled":true,"telegram_api_token":"fixture:token","telegram_chat_id":"fixture","serverchan_enabled":true,"serverchan_sendkey":"fixture"}).as_object().unwrap().clone()).unwrap();
        let plan = deliveries(&c, "title", "body");
        assert!(plan.errors.is_empty());
        let values = plan.deliveries;
        assert_eq!(values.len(), 7);
        assert!(values
            .iter()
            .any(|v| v.channel == "ntfy" && v.payload["topic"] == "topic"));
    }
}
