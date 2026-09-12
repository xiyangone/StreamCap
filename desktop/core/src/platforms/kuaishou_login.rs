//! In-process QR session manager. Tokens stay in memory and only explicit UI save persists cookies.
use super::http::{body, transport_error, PlatformHttp};
use crate::resolver::QrSnapshot;
use reqwest::{cookie::CookieStore, Url};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

const SID: &str = "kuaishou.live.web";
#[derive(Clone)]
pub struct LoginEndpoints {
    pub identity: Url,
    pub live: Url,
}
impl Default for LoginEndpoints {
    fn default() -> Self {
        Self {
            identity: Url::parse("https://id.kuaishou.com/").expect("static URL"),
            live: Url::parse("https://live.kuaishou.com/").expect("static URL"),
        }
    }
}
impl LoginEndpoints {
    pub fn loopback(base: &str) -> Result<Self, String> {
        let url = Url::parse(base).map_err(|_| "无效测试地址")?;
        if !matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
            || url.scheme() != "http"
        {
            return Err("测试端点必须是本机回环地址".into());
        }
        Ok(Self {
            identity: url.clone(),
            live: url,
        })
    }
    fn is_loopback(&self) -> bool {
        self.identity.scheme() == "http"
    }
}
struct Session {
    snapshot: QrSnapshot,
    expires: u64,
    cancel: CancellationToken,
}
#[derive(Clone)]
pub struct LoginManager {
    sessions: Arc<Mutex<HashMap<String, Arc<Mutex<Session>>>>>,
    stop: CancellationToken,
    tasks: TaskTracker,
    endpoints: LoginEndpoints,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
impl LoginManager {
    pub fn new(stop: CancellationToken) -> Self {
        Self::with_endpoints(stop, LoginEndpoints::default())
    }
    pub fn with_endpoints(stop: CancellationToken, endpoints: LoginEndpoints) -> Self {
        Self {
            sessions: Arc::default(),
            stop,
            tasks: TaskTracker::new(),
            endpoints,
        }
    }
    pub async fn start(&self, proxy: Option<&str>) -> Result<QrSnapshot, String> {
        if self.stop.is_cancelled() {
            return Err("应用正在退出".into());
        }
        let mut sessions = self.sessions.lock().await;
        if self.stop.is_cancelled() {
            return Err("应用正在退出".into());
        }
        let mut expired = Vec::new();
        for (id, entry) in sessions.iter() {
            let s = entry.lock().await;
            if s.expires + 180 < now() {
                s.cancel.cancel();
                expired.push(id.clone());
            }
        }
        for id in expired {
            sessions.remove(&id);
        }
        if self.stop.is_cancelled() {
            return Err("应用正在退出".into());
        }
        if sessions.len() >= 4 {
            return Err("登录会话过多，请先关闭现有二维码".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let entry = Arc::new(Mutex::new(Session {
            snapshot: QrSnapshot {
                session_id: id.clone(),
                state: "loading".into(),
                message: "正在获取二维码".into(),
                ..Default::default()
            },
            expires: now() + 60,
            cancel: self.stop.child_token(),
        }));
        sessions.insert(id.clone(), entry.clone());
        let endpoints = self.endpoints.clone();
        let proxy = proxy.map(str::to_owned);
        let session = entry.clone();
        self.tasks.spawn(async move{
            let cancel=session.lock().await.cancel.clone();
            let outcome=tokio::select!{biased; _=cancel.cancelled()=>Err("已取消".into()), result=tokio::time::timeout(Duration::from_secs(300),login_flow(&session,&endpoints,proxy.as_deref()))=>result.unwrap_or_else(|_|Err("登录会话已超时".into()))};
            if let Err(message)=outcome{
                let mut s=session.lock().await;
                s.snapshot.state=if cancel.is_cancelled(){"cancelled"}else if message.contains("过期")||message.contains("超时"){ "expired" }else{"error"}.into();
                s.snapshot.message=message;s.snapshot.cookies=None;
            }
        });
        drop(sessions);
        for _ in 0..95 {
            let snapshot = entry.lock().await.snapshot.clone();
            if !snapshot.image_base64.is_empty() || snapshot.is_terminal() {
                return Ok(snapshot);
            }
            tokio::select! {_=self.stop.cancelled()=>return Err("应用正在退出".into()),_=tokio::time::sleep(Duration::from_millis(100))=>{}}
        }
        self.status(&id).await
    }
    pub async fn status(&self, id: &str) -> Result<QrSnapshot, String> {
        let entry = self
            .sessions
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or("登录会话不存在或已过期")?;
        let mut session = entry.lock().await;
        let seconds = session.expires.saturating_sub(now()) as i64;
        session.snapshot.seconds_left = seconds;
        Ok(session.snapshot.clone())
    }
    pub async fn cancel(&self, id: &str) -> Result<(), String> {
        if let Some(entry) = self.sessions.lock().await.remove(id) {
            entry.lock().await.cancel.cancel();
        }
        Ok(())
    }
    pub async fn shutdown(&self) {
        self.stop.cancel();
        // Serialize task registration with closing: no QR worker may appear after wait has returned.
        let mut sessions = self.sessions.lock().await;
        self.tasks.close();
        self.tasks.wait().await;
        sessions.clear();
    }
    pub fn active_tasks(&self) -> usize {
        self.tasks.len()
    }
}
async fn post(http: &PlatformHttp, url: Url, form: &[(&str, &str)]) -> Result<Value, String> {
    let response = http
        .client
        .post(url)
        .header("Referer", "https://www.kuaishou.com/")
        .form(form)
        .send()
        .await
        .map_err(transport_error)?;
    let text = body(response).await?;
    let value: Value = serde_json::from_str(&text).map_err(|_| "登录接口返回内容不是 JSON")?;
    if value["result"].as_i64() == Some(707) {
        return Err("登录二维码已过期".into());
    }
    Ok(value)
}
fn require_ok(data: &Value, step: &str) -> Result<(), String> {
    if data["result"].as_i64() == Some(1) {
        Ok(())
    } else {
        Err(format!(
            "{step}未获平台确认（返回码 {}）",
            data["result"]
                .as_i64()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "未知".into())
        ))
    }
}
async fn login_flow(
    session: &Arc<Mutex<Session>>,
    endpoints: &LoginEndpoints,
    proxy: Option<&str>,
) -> Result<(), String> {
    let http = if endpoints.is_loopback() {
        PlatformHttp::for_loopback()?
    } else {
        PlatformHttp::new("kuaishou.com", proxy, None, Duration::from_secs(75))?
    };
    let endpoint = |p: &str| {
        endpoints
            .identity
            .join(p)
            .map_err(|_| "登录端点无效".to_string())
    };
    let data = post(
        &http,
        endpoint("rest/c/infra/ks/qr/start")?,
        &[("sid", SID)],
    )
    .await?;
    require_ok(&data, "获取二维码")?;
    let token = data["qrLoginToken"]
        .as_str()
        .ok_or("二维码令牌缺失")?
        .to_owned();
    let signature = data["qrLoginSignature"]
        .as_str()
        .ok_or("二维码签名缺失")?
        .to_owned();
    let image = data["imageData"]
        .as_str()
        .filter(|s| s.len() <= 1024 * 1024)
        .ok_or("二维码图片缺失或过大")?
        .to_owned();
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&image)
        .map_err(|_| "二维码不是有效图片编码")?;
    if !decoded.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("二维码图片不是 PNG".into());
    }
    let expires = data["expireTime"]
        .as_u64()
        .map(|t| t / 1000)
        .unwrap_or(now() + 60)
        .min(now() + 300);
    {
        let mut s = session.lock().await;
        s.expires = expires;
        s.snapshot.state = "waiting".into();
        s.snapshot.message = "请使用快手扫码".into();
        s.snapshot.image_base64 = image;
        s.snapshot.seconds_left = expires.saturating_sub(now()) as i64;
    }
    let scan = tokio::time::timeout(
        Duration::from_secs(expires.saturating_sub(now()).max(1)),
        post(
            &http,
            endpoint("rest/c/infra/ks/qr/scanResult")?,
            &[("qrLoginToken", &token), ("qrLoginSignature", &signature)],
        ),
    )
    .await
    .map_err(|_| "等待扫码超时")??;
    require_ok(&scan, "扫码")?;
    {
        let mut s = session.lock().await;
        s.expires = now() + 120;
        s.snapshot.state = "scanned".into();
        s.snapshot.message = "已扫码，请在手机确认".into();
    }
    let accepted = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let result = post(
                &http,
                endpoint("rest/c/infra/ks/qr/acceptResult")?,
                &[
                    ("qrLoginToken", &token),
                    ("qrLoginSignature", &signature),
                    ("sid", SID),
                ],
            )
            .await?;
            if result["result"].as_i64() == Some(1) {
                if let Some(token) = result["qrToken"].as_str() {
                    return Ok::<String, String>(token.to_owned());
                }
            }
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
    })
    .await
    .map_err(|_| "等待手机确认超时")??;
    let callback = post(
        &http,
        endpoint("pass/kuaishou/login/qr/callback")?,
        &[("qrToken", &accepted), ("sid", SID)],
    )
    .await?;
    require_ok(&callback, "换取登录凭证")?;
    http.get(endpoints.live.clone(), "https://live.kuaishou.com/")
        .await?;
    let result = post(
        &http,
        endpoint("pass/kuaishou/login/passToken")?,
        &[("sid", SID)],
    )
    .await?;
    require_ok(&result, "换取直播站令牌")?;
    let session_token = result[format!("{SID}_st")]
        .as_str()
        .filter(|v| !v.is_empty() && !v.contains(['\r', '\n', ';']))
        .ok_or("直播站会话令牌缺失")?;
    let user_id = super::text(&result["userId"]);
    if user_id.is_empty() {
        return Err("登录结果缺少账号标识".into());
    }
    http.jar.add_cookie_str(
        &format!(
            "{SID}_st={session_token}; Path=/{}",
            if endpoints.is_loopback() {
                ""
            } else {
                "; Secure"
            }
        ),
        &endpoints.live,
    );
    let cookies = http
        .jar
        .cookies(&endpoints.live)
        .and_then(|h| h.to_str().ok().map(str::to_owned))
        .ok_or("登录成功但缺少直播站 Cookie")?;
    let page = http
        .get(endpoints.live.clone(), "https://live.kuaishou.com/")
        .await?;
    let state = super::json_after(&page, "window.__INITIAL_STATE__=")
        .ok_or("直播站未提供可验证的登录状态")?;
    let account = super::find_object(
        &state,
        &|v| super::text(&v["userId"]) == user_id || super::text(&v["principalId"]) == user_id,
        0,
    )
    .ok_or("直播站未确认当前登录账号")?;
    let mut s = session.lock().await;
    s.snapshot.state = "success".into();
    s.snapshot.message = "登录成功，请保存登录信息".into();
    s.snapshot.cookies = Some(cookies);
    s.snapshot.username = account["name"].as_str().map(str::to_owned);
    Ok(())
}
