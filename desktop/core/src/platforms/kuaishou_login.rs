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
    finished: Option<u64>,
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
        // Terminal results do not occupy live login slots; keep a bounded retry window.
        let mut expired = Vec::new();
        let mut terminal = Vec::new();
        let mut active = 0;
        for (id, entry) in sessions.iter() {
            let s = entry.lock().await;
            if s.snapshot.is_terminal() {
                let finished = s.finished.unwrap_or_else(now);
                if finished + 60 <= now() {
                    s.cancel.cancel();
                    expired.push(id.clone());
                } else {
                    terminal.push((finished, id.clone()));
                }
            } else if s.expires + 180 < now() {
                s.cancel.cancel();
                expired.push(id.clone());
            } else {
                active += 1;
            }
        }
        terminal.sort();
        for (_, id) in terminal.iter().take(terminal.len().saturating_sub(12)) {
            if let Some(entry) = sessions.get(id) {
                entry.lock().await.cancel.cancel();
            }
            expired.push(id.clone());
        }
        for id in expired {
            sessions.remove(&id);
        }
        if self.stop.is_cancelled() {
            return Err("应用正在退出".into());
        }
        if active >= 4 {
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
            finished: None,
            cancel: self.stop.child_token(),
        }));
        sessions.insert(id.clone(), entry.clone());
        let endpoints = self.endpoints.clone();
        let proxy = proxy.map(str::to_owned);
        let session = entry.clone();
        let session_map = self.sessions.clone();
        let task_id = id.clone();
        self.tasks.spawn(async move{
            let cancel=session.lock().await.cancel.clone();
            let outcome=tokio::select!{biased; _=cancel.cancelled()=>Err("已取消".into()), result=tokio::time::timeout(Duration::from_secs(300),login_flow(&session,&endpoints,proxy.as_deref()))=>result.unwrap_or_else(|_|Err("登录会话已超时".into()))};
            if let Err(message)=outcome{
                let mut s=session.lock().await;
                let verified_stage=s.snapshot.state=="verifying";
                let message=if verified_stage&&!message.starts_with("手机已确认"){format!("手机已确认，但登录验证失败：{message}。登录信息未保存。")}else{message};
                s.snapshot.state=if cancel.is_cancelled(){"cancelled"}else if !verified_stage&&(message.contains("过期")||message.contains("超时")){ "expired" }else{"error"}.into();
                s.snapshot.message=message;s.snapshot.cookies=None;s.snapshot.username=None;s.snapshot.image_base64.clear();s.snapshot.seconds_left=0;
            }
            session.lock().await.finished=Some(now());
            tokio::select!{_=cancel.cancelled()=>{},_=tokio::time::sleep(Duration::from_secs(60))=>{}}
            session_map.lock().await.remove(&task_id);
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
        session.snapshot.seconds_left =
            if matches!(session.snapshot.state.as_str(), "waiting" | "scanned") {
                seconds
            } else {
                0
            };
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
        {
            let mut sessions = self.sessions.lock().await;
            self.tasks.close();
            sessions.clear();
        }
        self.tasks.wait().await;
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
        s.snapshot.message = "已扫码，请在手机上确认登录".into();
        s.snapshot.image_base64.clear();
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
    {
        let mut s = session.lock().await;
        s.snapshot.state = "verifying".into();
        s.snapshot.message = "手机已确认，正在验证直播站登录状态".into();
        s.snapshot.image_base64.clear();
        s.snapshot.seconds_left = 0;
    }
    let callback = post(
        &http,
        endpoint("pass/kuaishou/login/qr/callback")?,
        &[("qrToken", &accepted), ("sid", SID)],
    )
    .await?;
    require_ok(&callback, "换取登录凭证")?;
    // The warm-up only seeds live-domain cookies. A challenge/temporary HTTP
    // failure here must not discard a valid pass-token exchange.
    let _ = http
        .get(endpoints.live.clone(), "https://live.kuaishou.com/")
        .await;
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
    let cookie_attributes = if endpoints.is_loopback() {
        "Path=/"
    } else {
        "Path=/; Secure"
    };
    http.jar.add_cookie_str(
        &format!("{SID}_st={session_token}; {cookie_attributes}"),
        &endpoints.live,
    );
    if let Some(value) = result
        .get(format!("{SID}_ph"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && !value.contains(['\r', '\n', ';']))
    {
        http.jar.add_cookie_str(
            &format!("{SID}_ph={value}; {cookie_attributes}"),
            &endpoints.live,
        );
    }
    let page = http
        .get(endpoints.live.clone(), "https://live.kuaishou.com/")
        .await?;
    let username = verify_login_page(&page, &user_id)?;
    // The verification request can refresh a live-domain cookie. Collect only
    // after it succeeds so the saved value is the final jar state.
    let cookies = http
        .jar
        .cookies(&endpoints.live)
        .and_then(|h| h.to_str().ok().map(str::to_owned))
        .ok_or("登录成功但缺少直播站 Cookie")?;
    let mut s = session.lock().await;
    s.snapshot.state = "success".into();
    s.snapshot.message = match &username {
        Some(name) => format!("已验证账号：{name}"),
        None => "快手账号已验证".into(),
    };
    s.snapshot.cookies = Some(cookies);
    s.snapshot.username = username;
    s.snapshot.seconds_left = 0;
    s.snapshot.image_base64.clear();
    Ok(())
}

fn first_json_value(text: &str) -> Option<Value> {
    serde_json::Deserializer::from_str(text.trim_start())
        .into_iter::<Value>()
        .next()?
        .ok()
}
fn js_string(text: &str) -> Option<String> {
    let text = text.trim_start();
    if text.starts_with('"') {
        return first_json_value(text)?.as_str().map(str::to_owned);
    }
    let bytes = text.as_bytes();
    if bytes.first().copied()? != b'\'' {
        return None;
    }
    let mut result = String::new();
    let mut index = 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' => return Some(result),
            b'\\' => {
                index += 1;
                let escaped = *bytes.get(index)?;
                match escaped {
                    b'n' => result.push('\n'),
                    b'r' => result.push('\r'),
                    b't' => result.push('\t'),
                    b'b' => result.push('\u{0008}'),
                    b'f' => result.push('\u{000C}'),
                    b'\\' | b'\'' | b'"' | b'/' => result.push(escaped as char),
                    b'u' => {
                        let hex = std::str::from_utf8(bytes.get(index + 1..index + 5)?).ok()?;
                        let code = u16::from_str_radix(hex, 16).ok()?;
                        result.push(char::from_u32(code as u32)?);
                        index += 4;
                    }
                    _ => result.push(escaped as char),
                }
            }
            _ => {
                let character = text.get(index..)?.chars().next()?;
                result.push(character);
                index += character.len_utf8() - 1;
            }
        }
        index += 1;
    }
    None
}
fn assigned_json(text: &str) -> Option<Value> {
    let text = text.trim_start();
    let parse_tail = text
        .strip_prefix("JSON.parse")
        .map(str::trim_start)
        .and_then(|tail| tail.strip_prefix('('));
    if let Some(tail) = parse_tail {
        let encoded = js_string(tail)?;
        return first_json_value(&encoded);
    }
    first_json_value(text)
}
fn first_text_value(text: &str) -> Option<Value> {
    first_json_value(text).or_else(|| js_string(text).map(Value::String))
}
pub(crate) fn initial_state(page: &str) -> Option<Value> {
    for marker in [
        "window.__INITIAL_STATE__",
        "self.__INITIAL_STATE__",
        "__INITIAL_STATE__",
    ] {
        let mut offset = 0;
        while let Some(relative) = page.get(offset..)?.find(marker) {
            let start = offset + relative + marker.len();
            let tail = page.get(start..)?.trim_start();
            let tail = tail
                .strip_prefix('=')
                .or_else(|| tail.strip_prefix(':'))
                .map(str::trim_start);
            if let Some(value) = tail.and_then(assigned_json) {
                return Some(value);
            }
            offset = start;
        }
    }
    None
}

/// Verify exact account identity in login-owned state only, never arbitrary room recommendations.
/// Phone confirmation, token exchange and website login verification are independent stages.
pub fn verify_login_page(page: &str, user_id: &str) -> Result<Option<String>, String> {
    const UNVERIFIED: &str =
        "手机已确认，但直播站未返回可验证的登录状态。登录信息未保存，请稍后重试。";
    if user_id.is_empty() {
        return Err(UNVERIFIED.into());
    }
    fn logged_out(value: &Value) -> bool {
        ["isLogin", "isLoggedIn", "loggedIn"].iter().any(|key| {
            value.get(*key).is_some_and(|flag| {
                flag == &Value::Bool(false)
                    || flag == &Value::from(0)
                    || matches!(flag.as_str(), Some("false" | "0"))
            })
        })
    }
    fn logged_in(value: &Value) -> bool {
        ["isLogin", "isLoggedIn", "loggedIn"].iter().any(|key| {
            value.get(*key).is_some_and(|flag| {
                flag == &Value::Bool(true)
                    || flag == &Value::from(1)
                    || matches!(flag.as_str(), Some("true" | "1"))
            })
        })
    }
    fn identity(value: &Value, user_id: &str) -> bool {
        if logged_out(value) {
            return false;
        }
        [
            "userId",
            "user_id",
            "principalId",
            "uid",
            "originUserId",
            "id",
        ]
        .iter()
        .any(|key| {
            super::text(&value[*key]) == user_id
                || value[*key]
                    .as_u64()
                    .is_some_and(|id| id.to_string() == user_id)
        })
    }
    fn current_account<'a>(value: &'a Value, user_id: &str, depth: usize) -> Option<&'a Value> {
        if depth > 20 || logged_out(value) {
            return None;
        }
        let map = value.as_object()?;
        for key in [
            "currentUser",
            "loginUser",
            "loggedInUser",
            "selfUser",
            "currentAccount",
            "currentUserInfo",
            "loginUserInfo",
        ] {
            if let Some(account) = map.get(key) {
                if identity(account, user_id) {
                    return Some(account);
                }
            }
        }
        map.values()
            .find_map(|child| current_account(child, user_id, depth + 1))
    }
    fn logged_in_identity<'a>(
        value: &'a Value,
        user_id: &str,
        inherited_login: bool,
        depth: usize,
    ) -> Option<&'a Value> {
        if depth > 30 || logged_out(value) {
            return None;
        }
        let in_login_context = inherited_login || logged_in(value);
        if in_login_context && identity(value, user_id) {
            return Some(value);
        }
        match value {
            Value::Object(map) => map
                .values()
                .find_map(|child| logged_in_identity(child, user_id, in_login_context, depth + 1)),
            Value::Array(values) => values
                .iter()
                .find_map(|child| logged_in_identity(child, user_id, in_login_context, depth + 1)),
            _ => None,
        }
    }
    fn account_name(account: &Value, user_id: &str) -> String {
        ["name", "userName", "user_name", "nickname", "nickName"]
            .iter()
            .find_map(|key| {
                account[*key]
                    .as_str()
                    .filter(|value| !value.trim().is_empty())
            })
            .map(|name| {
                name.chars()
                    .filter(|character| !character.is_control())
                    .take(80)
                    .collect()
            })
            .filter(|name: &String| !name.trim().is_empty())
            .unwrap_or_else(|| user_id.to_owned())
    }
    fn same_id(value: &Value, user_id: &str) -> bool {
        super::text(value) == user_id || value.as_u64().is_some_and(|id| id.to_string() == user_id)
    }
    fn text_identity_field(page: &str, key: &str, user_id: &str) -> bool {
        for token in [format!("\"{key}\""), format!("'{key}'"), key.to_owned()] {
            let mut offset = 0;
            while let Some(relative) = page.get(offset..).and_then(|tail| tail.find(&token)) {
                let start = offset + relative;
                let end = start + token.len();
                if token == key {
                    let left = page
                        .get(..start)
                        .and_then(|prefix| prefix.chars().next_back());
                    let right = page.get(end..).and_then(|suffix| suffix.chars().next());
                    if left.is_some_and(|character| {
                        character.is_ascii_alphanumeric() || character == '_'
                    }) || right.is_some_and(|character| {
                        character.is_ascii_alphanumeric() || character == '_'
                    }) {
                        offset = end;
                        continue;
                    }
                }
                let Some(tail) = page.get(end..).map(str::trim_start) else {
                    break;
                };
                let Some(tail) = tail.strip_prefix(':').map(str::trim_start) else {
                    offset = end;
                    continue;
                };
                if let Some(value) = first_text_value(tail) {
                    if same_id(&value, user_id) {
                        return true;
                    }
                }
                offset = end;
            }
        }
        false
    }
    fn text_flag(page: &str, key: &str, expected: bool) -> bool {
        for token in [format!("\"{key}\""), format!("'{key}'")] {
            let mut offset = 0;
            while let Some(relative) = page.get(offset..).and_then(|tail| tail.find(&token)) {
                let start = offset + relative + token.len();
                let Some(tail) = page.get(start..).map(str::trim_start) else {
                    break;
                };
                let Some(tail) = tail.strip_prefix(':').map(str::trim_start) else {
                    offset = start;
                    continue;
                };
                if let Some(value) = first_text_value(tail) {
                    let actual = value.as_bool().unwrap_or(false)
                        || value.as_i64().is_some_and(|number| number == 1)
                        || value.as_u64().is_some_and(|number| number == 1)
                        || matches!(value.as_str(), Some("true" | "1"));
                    if actual == expected {
                        return true;
                    }
                }
                offset = start;
            }
        }
        false
    }
    fn text_login_context(page: &str) -> bool {
        [
            "currentUser",
            "loginUser",
            "loggedInUser",
            "selfUser",
            "currentAccount",
            "currentUserInfo",
            "loginUserInfo",
        ]
        .iter()
        .any(|key| page.contains(&format!("\"{key}\"")) || page.contains(&format!("'{key}'")))
            || text_flag(page, "isLogin", true)
            || text_flag(page, "isLoggedIn", true)
            || text_flag(page, "loggedIn", true)
    }
    fn exact_id_in_text(page: &str, user_id: &str) -> bool {
        if user_id.is_empty() {
            return false;
        }
        let is_identifier = |character: Option<char>| {
            character.is_some_and(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            })
        };
        let mut offset = 0;
        while let Some(relative) = page.get(offset..).and_then(|tail| tail.find(user_id)) {
            let start = offset + relative;
            let end = start + user_id.len();
            let left = page
                .get(..start)
                .and_then(|prefix| prefix.chars().next_back());
            let right = page.get(end..).and_then(|suffix| suffix.chars().next());
            if !is_identifier(left) && !is_identifier(right) {
                return true;
            }
            offset = end;
        }
        false
    }
    fn has_state_key(value: &Value, keys: &[&str], depth: usize) -> bool {
        if depth > 30 {
            return false;
        }
        match value {
            Value::Object(map) => {
                map.keys()
                    .any(|key| keys.iter().any(|candidate| key == candidate))
                    || map
                        .values()
                        .any(|child| has_state_key(child, keys, depth + 1))
            }
            Value::Array(values) => values
                .iter()
                .any(|child| has_state_key(child, keys, depth + 1)),
            _ => false,
        }
    }

    fn identity_outside_collection(
        value: &Value,
        user_id: &str,
        in_collection: bool,
        depth: usize,
    ) -> bool {
        if depth > 30 || logged_out(value) {
            return false;
        }
        if !in_collection && identity(value, user_id) {
            return true;
        }
        let is_collection = |key: &str| {
            matches!(
                key,
                "recommendations"
                    | "roomList"
                    | "rooms"
                    | "room"
                    | "roomInfo"
                    | "liveStream"
                    | "cards"
                    | "author"
            )
        };
        match value {
            Value::Object(map) => map.iter().any(|(key, child)| {
                identity_outside_collection(
                    child,
                    user_id,
                    in_collection || is_collection(key),
                    depth + 1,
                )
            }),
            Value::Array(values) => values
                .iter()
                .any(|child| identity_outside_collection(child, user_id, in_collection, depth + 1)),
            _ => false,
        }
    }
    let state = initial_state(page);
    if let Some(state) = state.as_ref() {
        if logged_out(state) {
            return Err(UNVERIFIED.into());
        }
        let account = current_account(state, user_id, 0)
            .or_else(|| {
                [
                    "/userStore/userInfo",
                    "/userStore/user",
                    "/userInfoStore/userInfo",
                    "/loginStore/userInfo",
                    "/accountStore/userInfo",
                    "/user/userInfoQuery/ownerInfo",
                    "/user/userInfo",
                    "/userInfo",
                ]
                .iter()
                .filter(|pointer| {
                    pointer
                        .rsplit_once('/')
                        .and_then(|(parent, _)| state.pointer(parent))
                        .is_none_or(|parent| !logged_out(parent))
                })
                .filter_map(|pointer| state.pointer(pointer))
                .find(|account| identity(account, user_id))
            })
            .or_else(|| logged_in_identity(state, user_id, false, 0));
        if let Some(account) = account {
            return Ok(Some(account_name(account, user_id)));
        }
    }

    // Some live-site deployments render the account state through a different
    // bootstrap variable or a challenge wrapper. The pass-token exchange has
    // already supplied the expected ID, so an exact ID token in the 200-page
    // response is a useful fallback even when its field name is obfuscated or
    // serialized by JavaScript. Room/recommendation-only payloads must not be
    // accepted unless a login context is also present.
    let recommendation_only = state.as_ref().is_some_and(|value| {
        has_state_key(
            value,
            &[
                "recommendations",
                "roomList",
                "rooms",
                "room",
                "roomInfo",
                "liveStream",
                "cards",
                "author",
            ],
            0,
        )
    }) || [
        "recommendations",
        "roomList",
        "rooms",
        "room",
        "roomInfo",
        "liveStream",
        "cards",
        "author",
    ]
    .iter()
    .any(|key| page.contains(&format!("\"{key}\"")) || page.contains(&format!("'{key}'")));
    let login_context = state
        .as_ref()
        .is_some_and(|value| logged_in_identity(value, user_id, false, 0).is_some())
        || text_login_context(page);
    let state_identity = state
        .as_ref()
        .is_some_and(|value| identity_outside_collection(value, user_id, false, 0));
    let text_identity = ["userId", "user_id", "principalId", "uid", "originUserId"]
        .iter()
        .any(|key| text_identity_field(page, key, user_id));
    if !text_flag(page, "isLogin", false)
        && !text_flag(page, "isLoggedIn", false)
        && !text_flag(page, "loggedIn", false)
        && (!recommendation_only || login_context || state_identity)
        && (state_identity || text_identity || exact_id_in_text(page, user_id))
    {
        return Ok(Some(user_id.to_owned()));
    }
    Err(UNVERIFIED.into())
}
