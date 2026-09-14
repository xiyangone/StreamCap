//! Bounded notifications. Transport errors never print destination credentials.
use crate::{config::ConfigStore, model::Recording, store::Store};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::sync::{RwLock, Semaphore};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
#[derive(Clone)]
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
#[derive(Clone)]
pub struct Notifications {
    config: Arc<RwLock<ConfigStore>>,
    store: Store,
    stop: CancellationToken,
    tasks: TaskTracker,
    slots: Arc<Semaphore>,
    admission: Arc<std::sync::Mutex<()>>,
}
impl Notifications {
    pub fn new(config: Arc<RwLock<ConfigStore>>, store: Store) -> Self {
        Self {
            config,
            store,
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
            slots: Arc::new(Semaphore::new(32)),
            admission: Arc::new(std::sync::Mutex::new(())),
        }
    }
    pub async fn changed(&self, record: &Recording, started: bool) {
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
            self.store.snack(error);
        }
        let plan = plan.deliveries;
        let email = config
            .get_bool("email_enabled", false)
            .then(|| email_config(&config));
        drop(config);
        let _admission = self.admission.lock().expect("notification admission");
        if self.stop.is_cancelled() {
            return;
        }
        let Ok(permit) = self.slots.clone().try_acquire_owned() else {
            self.store.snack("通知队列已满，本次消息未发送");
            return;
        };
        let manager = self.clone();
        self.tasks.spawn(async move {
            let _permit = permit;
            let work = async {
                let client = match reqwest::Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(12))
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                {
                    Ok(c) => c,
                    Err(_) => return,
                };
                for delivery in plan {
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
                        let value: Value = response_json(response).await?;
                        if accepted(delivery.channel, &value) {
                            Ok(())
                        } else {
                            Err(())
                        }
                    }
                    .await;
                    if result.is_err() {
                        manager.store.snack(format!(
                            "{} 通知发送失败，请检查配置和网络",
                            delivery.channel
                        ));
                    }
                }
                if let Some(email) = email {
                    if send_email(email, &title, &content).await.is_err() {
                        manager.store.snack("邮件通知发送失败，请检查 SMTP 配置");
                    }
                }
            };
            tokio::select! {biased;_=manager.stop.cancelled()=>{},_=work=>{}};
        });
    }
    pub async fn shutdown(&self) {
        {
            let _admission = self.admission.lock().expect("notification admission");
            self.stop.cancel();
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
