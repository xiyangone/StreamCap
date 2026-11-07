//! Browser-owned Kuaishou access, strict target parsing and preserved platform cooldowns.
use super::{http::validate_url, quality_index, quality_name, text};
use crate::resolver::{ResolveRequest, StreamInfo};
use serde_json::Value;
use std::{
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot, Mutex},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

pub const ACCESS_UNAVAILABLE: &str = "快手接口暂不可读（400002），直播状态未确认，本次检测已停止";
pub const LIVE_ACCESS_UNAVAILABLE: &str =
    "快手主播正在直播，但播放信息受平台验证限制（400002），本次未取得录制地址";
pub const VERIFICATION_REQUIRED: &str =
    "快手需要完成官方验证（400002），请在设置中打开快手网页，完成后恢复检测";
pub const BROWSER_UNAVAILABLE: &str = "快手浏览器会话不可用，请使用桌面应用";
pub const BROWSER_TIMEOUT: &str = "快手网页请求超时，请稍后重新检测";
pub const BROWSER_CANCELLED: &str = "快手网页请求已取消";
pub const RATE_LIMITED: &str = "快手请求过快，已暂停请求并进入冷却；请勿反复刷新或重新登录";
pub const LOGIN_REQUIRED: &str = "快手目标页面明确要求登录，请在设置中检查登录状态";
pub const LOGIN_PROMPT: &str = "快手页面出现登录提示，但未确认登录失效；已停止本次检查";
pub const PAGE_UNAVAILABLE: &str = "快手页面暂不可读，已停止本次检查；未确认下播或需要验证码";
pub fn verification_required(error: &str) -> bool {
    error == VERIFICATION_REQUIRED
}
pub fn access_state(error: &str) -> &'static str {
    match error {
        VERIFICATION_REQUIRED => "captcha",
        ACCESS_UNAVAILABLE | LIVE_ACCESS_UNAVAILABLE => "unavailable",
        RATE_LIMITED => "cooldown",
        LOGIN_REQUIRED => "loginRequired",
        LOGIN_PROMPT => "loginPrompt",
        PAGE_UNAVAILABLE => "unavailable",
        _ => "",
    }
}
fn limited_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "请求过快",
        "频繁",
        "too many requests",
        "rate limit",
        "ip banned",
        "frequent",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}
fn login_required_text(text: &str) -> bool {
    [
        "请先登录",
        "登录后才能观看",
        "登录后才可观看",
        "登录后观看",
        "需要登录后",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}
fn room_error_text(room: &Value) -> String {
    format!(
        "{} {}",
        room["errorType"]["title"].as_str().unwrap_or_default(),
        room["errorType"]["content"].as_str().unwrap_or_default()
    )
}

pub fn parse_page(html: &str, quality: Option<&str>) -> Result<StreamInfo, String> {
    let state = super::kuaishou_login::initial_state(html).ok_or("快手页面缺少可验证的房间数据")?;
    parse_state(&state, quality)
}

/// This endpoint verifies room status only; it never supplies a recording source.
pub fn parse_user_info(data: &Value, principal_id: &str) -> Result<(String, bool), String> {
    const UNVERIFIED: &str = "快手用户信息未通过校验，直播状态未确认";
    let data = &data["data"];
    let user = &data["userInfo"];
    if principal_id.is_empty()
        || data["result"].as_i64() != Some(1)
        || user["id"].as_str() != Some(principal_id)
    {
        return Err(UNVERIFIED.into());
    }
    let living = user["living"].as_bool().ok_or(UNVERIFIED)?;
    let name = user["name"].as_str().unwrap_or_default().trim().to_owned();
    Ok((name, living))
}

/// Cookie headers are ordered from the most specific scope to the least specific.
/// The HTTP jar keeps the first occurrence of a name.
pub fn canonical_cookie_header(header: &str) -> Result<String, String> {
    if header.len() > 65536 || header.contains(['\r', '\n']) {
        return Err("快手 Cookie 格式无效".into());
    }
    let mut seen = std::collections::HashSet::new();
    let mut pairs = Vec::new();
    for pair in header
        .split(';')
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
    {
        let (name, value) = pair.split_once('=').ok_or("快手 Cookie 格式无效")?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
            || value.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err("快手 Cookie 格式无效".into());
        }
        if seen.insert(name) {
            pairs.push(pair);
        }
    }
    Ok(pairs.join("; "))
}

fn target_room(state: &Value) -> Option<&Value> {
    if let Some(rooms) = state.pointer("/liveroom/playList") {
        rooms.as_array().and_then(|rooms| rooms.first())
    } else {
        state.get("room")
    }
}

fn room_anchor(room: &Value) -> String {
    let mut anchor = text(&room["author"]["name"]);
    if anchor.is_empty() {
        anchor = text(&room["liveStream"]["user"]["user_name"]);
    }
    anchor
}

/// The requested room is first in the room playlist. Never accept a recommendation instead.
pub fn parse_state(state: &Value, quality: Option<&str>) -> Result<StreamInfo, String> {
    let room = target_room(state).ok_or("快手未返回目标房间，不能确认是否开播")?;
    parse_room(room, quality)
}
fn human_challenge_code(room: &Value) -> bool {
    let value = &room["errorType"]["type"];
    value.as_i64() == Some(400002) || value.as_str() == Some("400002")
}
pub fn parse_room(room: &Value, quality: Option<&str>) -> Result<StreamInfo, String> {
    let error = &room["errorType"];
    let code = error["type"]
        .as_i64()
        .or_else(|| error["type"].as_str()?.parse().ok());
    let title = error["title"].as_str().unwrap_or_default();
    let message = room_error_text(room);
    // A blocked business response is neither an offline room nor an HTTP 429.
    // Report it without starting a separate browser or mutating credentials.
    if human_challenge_code(room) {
        return Err(ACCESS_UNAVAILABLE.into());
    }
    if title.contains("请完成滑块验证") {
        return Err(PAGE_UNAVAILABLE.into());
    }
    if limited_text(&message) {
        return Err(RATE_LIMITED.into());
    }
    if login_required_text(&message) {
        return Err(LOGIN_REQUIRED.into());
    }
    if message.contains("登录") {
        return Err(LOGIN_PROMPT.into());
    }
    if code.is_some_and(|n| n != 0) || !title.is_empty() {
        return Err(format!(
            "快手未返回目标房间（返回码 {}）",
            code.map(|n| n.to_string()).unwrap_or_else(|| "未知".into())
        ));
    }
    // Access placeholders can retain author/offline fields even when their error is
    // content-only or has an unrecognized shape. They are not verified room state.
    if !message.trim().is_empty()
        || (!error.is_null() && !error.is_object())
        || (!error["type"].is_null() && code != Some(0))
    {
        return Err(PAGE_UNAVAILABLE.into());
    }
    let stream = &room["liveStream"];
    let anchor = room_anchor(room);
    if anchor.is_empty() {
        return Err("快手未返回主播信息（登录失效、访问受限或房间不存在）".into());
    }
    let mut info = StreamInfo {
        platform: "快手直播".into(),
        anchor_name: anchor,
        title: text(&stream["caption"]),
        ..Default::default()
    };
    if info.title.is_empty() {
        info.title = text(&stream["title"]);
    }
    let explicit_live = room["isLiving"]
        .as_bool()
        .or_else(|| stream["living"].as_bool());
    if explicit_live == Some(false) {
        return Ok(info);
    }
    if !stream.is_object() {
        return Err("快手返回空直播信息，不能当作未开播".into());
    }
    let index = quality_index(quality)?;
    let mut urls = Vec::new();
    collect_urls(&stream["playUrls"], &mut urls, 0);
    if urls.is_empty() {
        collect_urls(&stream["multiResolutionPlayUrls"], &mut urls, 0);
    }
    let mut hls = Vec::new();
    collect_urls(&stream["multiResolutionHlsPlayUrls"], &mut hls, 0);
    let pick = |mut list: Vec<(u64, String)>| {
        let has_bitrate = list.iter().any(|(n, _)| *n > 0);
        if has_bitrate {
            list.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        } else {
            list.reverse();
        }
        let limit = [u64::MAX, 2000, 1000, 800, 600][index];
        let selected = if has_bitrate {
            list.iter()
                .position(|(b, _)| *b <= limit)
                .unwrap_or(list.len().saturating_sub(1))
        } else {
            index.min(list.len().saturating_sub(1))
        };
        list.get(selected)
            .map(|(_, u)| u.clone())
            .unwrap_or_default()
    };
    info.flv_url = pick(urls);
    info.m3u8_url = pick(hls);
    info.record_url = if info.flv_url.is_empty() {
        info.m3u8_url.clone()
    } else {
        info.flv_url.clone()
    };
    if info.record_url.is_empty() {
        return Err("快手房间未返回可用直播流，状态未验证".into());
    }
    info.quality = quality_name(index).into();
    info.is_live = true;
    Ok(info)
}
fn collect_urls(value: &Value, out: &mut Vec<(u64, String)>, depth: usize) {
    if depth > 12 {
        return;
    }
    if let Some(url) = value["url"].as_str().filter(|u| validate_url(u).is_ok()) {
        if !out.iter().any(|(_, v)| v == url) {
            out.push((value["bitrate"].as_u64().unwrap_or(0), url.to_string()));
        }
    }
    match value {
        Value::Object(map) => {
            for child in map.values() {
                collect_urls(child, out, depth + 1);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_urls(child, out, depth + 1);
            }
        }
        _ => {}
    }
}

/// A bounded native request. Credentials never appear in Debug or serialized replies.
pub struct BrowserRequest {
    pub room_url: String,
    pub cookie: Option<String>,
    pub proxy: Option<String>,
    cancellation: CancellationToken,
    response: oneshot::Sender<Result<BrowserResponse, String>>,
}
impl BrowserRequest {
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }
    pub fn respond(self, result: Result<BrowserResponse, String>) {
        let _ = self.response.send(result);
    }
}
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserResponse {
    pub url: String,
    #[serde(default)]
    pub http_status: u16,
    pub payload: Value,
}
pub type BrowserRequests = mpsc::Receiver<BrowserRequest>;

/// Opaque keys keep proxy URLs and credential material out of profile paths and markers.
pub fn session_key(value: &str) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn principal_id(raw: &str) -> Result<String, String> {
    let url = validate_url(raw)?;
    let id = url
        .path()
        .strip_prefix("/u/")
        .unwrap_or("")
        .trim_end_matches('/');
    if url.scheme() != "https"
        || url.host_str() != Some("live.kuaishou.com")
        || url.port_or_known_default() != Some(443)
        || id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
    {
        return Err("请使用快手直播间的 HTTPS 地址".into());
    }
    Ok(id.to_owned())
}
pub fn detail_url(id: &str) -> Result<reqwest::Url, String> {
    principal_id(&format!("https://live.kuaishou.com/u/{id}"))?;
    let mut url = reqwest::Url::parse("https://live.kuaishou.com/live_api/liveroom/livedetail")
        .expect("static Kuaishou endpoint");
    url.query_pairs_mut()
        .append_pair("principalId", id)
        .append_pair("authToken", "");
    Ok(url)
}

/// Consume only the primary author's detail, never recommendation entries or stale navigations.
pub fn parse_browser_response(
    response: &BrowserResponse,
    id: &str,
    quality: Option<&str>,
) -> Result<StreamInfo, String> {
    if response.url != detail_url(id)?.as_str() {
        return Err("快手网页返回了其他房间，本次检测已停止".into());
    }
    if response.http_status == 429 {
        return Err(RATE_LIMITED.into());
    }
    if response.http_status >= 400 {
        return Err(PAGE_UNAVAILABLE.into());
    }
    let data = &response.payload["data"];
    let code = data["result"]
        .as_i64()
        .or_else(|| data["result"].as_str()?.parse().ok());
    if code == Some(400002) {
        return Err(VERIFICATION_REQUIRED.into());
    }
    let message = ["error_msg", "message", "errorMessage"]
        .iter()
        .filter_map(|key| data[*key].as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if limited_text(&message) {
        return Err(RATE_LIMITED.into());
    }
    if login_required_text(&message) {
        return Err(LOGIN_REQUIRED.into());
    }
    if !matches!(code, Some(1 | 671)) {
        return Err("快手网页未返回播放信息，请在设置中打开快手网页检查登录或验证状态".into());
    }
    let author = &data["author"];
    let matches = ["id", "principalId", "kwaiId", "kwai_id"]
        .iter()
        .any(|key| author[*key].as_str() == Some(id));
    if !matches || text(&author["name"]).is_empty() {
        return Err("快手网页未确认目标主播，本次检测已停止".into());
    }
    if code == Some(1)
        && (data["isLiving"].as_bool() == Some(false)
            || data["liveStream"]["living"].as_bool() == Some(false))
    {
        return Err(PAGE_UNAVAILABLE.into());
    }
    let room = serde_json::json!({
        "author": author,
        "liveStream": data["liveStream"],
        "isLiving": code == Some(1),
    });
    parse_room(&room, quality)
}

struct AccessState {
    cookie: Option<String>,
    login_required: bool,
    ready: Instant,
    failures: u32,
}
impl Default for AccessState {
    fn default() -> Self {
        Self {
            cookie: None,
            login_required: false,
            ready: Instant::now(),
            failures: 0,
        }
    }
}
impl AccessState {
    fn set_cookie(&mut self, cookie: Option<String>) {
        if self.cookie != cookie {
            self.cookie = cookie;
            self.login_required = false;
            // Credential edits never undo a server restriction.
        }
    }
    fn observe(&mut self, result: &Result<StreamInfo, String>) {
        match result {
            Err(error) if error == RATE_LIMITED || error.contains("HTTP 429") => {
                self.failures = self.failures.saturating_add(1);
                let seconds =
                    (600_u64 * (1_u64 << self.failures.saturating_sub(1).min(8))).min(14400);
                self.ready = Instant::now() + Duration::from_secs(seconds);
            }
            Err(error) if error == LOGIN_REQUIRED => self.login_required = true,
            Ok(_) => {
                self.failures = 0;
                self.ready = Instant::now();
            }
            _ => {}
        }
    }
}
#[derive(Clone, Default)]
pub struct KuaishouResolver {
    access: Arc<Mutex<AccessState>>,
    browser: Arc<StdMutex<Option<mpsc::Sender<BrowserRequest>>>>,
}
impl KuaishouResolver {
    pub fn attach_browser(&self) -> Result<BrowserRequests, String> {
        let mut browser = self.browser.lock().map_err(|_| BROWSER_UNAVAILABLE)?;
        if browser.is_some() {
            return Err("快手浏览器会话已连接".into());
        }
        let (send, receive) = mpsc::channel(1);
        *browser = Some(send);
        Ok(receive)
    }
    pub async fn reset_session(&self) {
        crate::network::prune_idle_clients();
        let mut state = self.access.lock().await;
        state.cookie = None;
        state.login_required = false;
    }
    pub async fn resolve(&self, request: &ResolveRequest) -> Result<StreamInfo, String> {
        let id = principal_id(&request.url)?;
        let cookie = request
            .cookie
            .as_deref()
            .filter(|cookie| !cookie.trim().is_empty())
            .map(canonical_cookie_header)
            .transpose()?;
        // Serialize requests across rooms and preserve server cooldowns.
        let mut state = self.access.lock().await;
        state.set_cookie(cookie);
        if Instant::now() < state.ready {
            return Err(RATE_LIMITED.into());
        }
        if state.login_required {
            return Err(LOGIN_REQUIRED.into());
        }
        let sender = self
            .browser
            .lock()
            .map_err(|_| BROWSER_UNAVAILABLE)?
            .clone()
            .ok_or(BROWSER_UNAVAILABLE)?;
        let cancellation = CancellationToken::new();
        let _cancel_on_drop = cancellation.clone().drop_guard();
        let (response, receive) = oneshot::channel();
        let message = BrowserRequest {
            room_url: format!("https://live.kuaishou.com/u/{id}"),
            cookie: state.cookie.clone(),
            proxy: request.proxy.clone(),
            cancellation,
            response,
        };
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            sender
                .send(message)
                .await
                .map_err(|_| BROWSER_UNAVAILABLE.to_owned())?;
            let response = receive
                .await
                .map_err(|_| BROWSER_UNAVAILABLE.to_owned())??;
            parse_browser_response(&response, &id, request.quality.as_deref())
        })
        .await
        .unwrap_or_else(|_| Err(BROWSER_TIMEOUT.into()));
        state.observe(&result);
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "explicit STREAMCAP_PROBE_CONFIG required; one saved monitored Kuaishou page, no login or recording"]
    async fn selected_kuaishou_conflict_readonly_probe() {
        use crate::platforms::http::PlatformHttp;
        use serde_json::json;
        use std::{path::PathBuf, time::Duration};
        let dir = PathBuf::from(std::env::var("STREAMCAP_PROBE_CONFIG").expect("explicit profile"));
        assert!(dir.is_absolute());
        let names = ["recordings.json", "user_settings.json", "cookies.json"];
        let before: Vec<_> = names
            .iter()
            .map(|name| std::fs::read(dir.join(name)).unwrap())
            .collect();
        let records: Value = serde_json::from_slice(&before[0]).unwrap();
        let settings: Value = serde_json::from_slice(&before[1]).unwrap();
        let cookies: Value = serde_json::from_slice(&before[2]).unwrap();
        let selected = records
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .find(|(_, r)| r["platform_key"] == "kuaishou" && r["monitor_status"] == true);
        let Some((index, record)) = selected else {
            println!(
                "KUAISHOU_CONFLICT_PROBE {}",
                json!({"probeCompleted":false,"reason":"no monitored saved target"})
            );
            return;
        };
        let url = reqwest::Url::parse(record["url"].as_str().unwrap()).unwrap();
        assert_eq!(url.host_str(), Some("live.kuaishou.com"));
        assert_eq!(url.scheme(), "https");
        let cookie = cookies["kuaishou"]
            .as_str()
            .filter(|s| !s.trim().is_empty());
        let proxy = if settings["enable_proxy"] == true {
            Some(
                settings["proxy_address"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .expect("configured proxy required"),
            )
        } else {
            None
        };
        let http =
            PlatformHttp::new("kuaishou.com", proxy, cookie, Duration::from_secs(20)).unwrap();
        let mut report = json!({"index":index,"requests":1,"cookieProvided":cookie.is_some(),"authentication":"not independently verified","transport":"unverified","businessReadable":false,"targetDataPresent":false,"conflictingSample":false});
        match http.get(url.clone(), "https://live.kuaishou.com/").await {
            Ok(page) => {
                report["transport"] = json!("reachable");
                if let Some(state) = crate::platforms::kuaishou_login::initial_state(&page) {
                    let room = state
                        .get("room")
                        .or_else(|| state.pointer("/liveroom/playList/0"))
                        .unwrap_or(&Value::Null);
                    let expected = url
                        .path()
                        .trim_end_matches('/')
                        .rsplit('/')
                        .next()
                        .unwrap_or("");
                    let identity =
                        room.pointer("/author/id").and_then(Value::as_str) == Some(expected);
                    report["targetDataPresent"] = json!(identity);
                    let flag = room["isLiving"]
                        .as_bool()
                        .or_else(|| room.pointer("/liveStream/living").and_then(Value::as_bool));
                    fn has_url(value: &Value) -> bool {
                        match value {
                            Value::Object(map) => {
                                map.get("url").and_then(Value::as_str).is_some_and(|s| {
                                    s.starts_with("https://") || s.starts_with("http://")
                                }) || map.values().any(has_url)
                            }
                            Value::Array(items) => items.iter().any(has_url),
                            _ => false,
                        }
                    }
                    let streams = has_url(&room["liveStream"]);
                    report["explicitLive"] = json!(flag);
                    report["streamAddressPresent"] = json!(streams);
                    report["conflictingSample"] = json!(identity && flag == Some(false) && streams);
                }
                match parse_page(&page, None) {
                    Ok(info) => {
                        report["businessReadable"] = json!(true);
                        report["parsedLive"] = json!(info.is_live);
                    }
                    Err(error) => {
                        report["accessState"] = json!(access_state(&error));
                        report["parserError"] = json!(error);
                    }
                }
            }
            Err(error) => {
                report["error"] = json!(error);
            }
        }
        for (name, bytes) in names.iter().zip(before) {
            assert_eq!(std::fs::read(dir.join(name)).unwrap(), bytes);
        }
        report["profileUnchanged"] = json!(true);
        println!("KUAISHOU_CONFLICT_PROBE {report}");
    }

    #[test]
    fn stream_addresses_alone_do_not_override_explicit_offline_or_access_failures() {
        let mut room = serde_json::json!({"author":{"id":"fixture","name":"Fixture"},"isLiving":false,"liveStream":{"playUrls":{"h264":{"adaptationSet":{"representation":[{"url":"https://example.invalid/stale.flv"}]}}}}});
        assert!(!parse_room(&room, None).unwrap().is_live);
        room["isLiving"] = serde_json::json!(true);
        assert!(parse_room(&room, None).unwrap().is_live);
        room["errorType"] = serde_json::json!({"code":400002,"title":"请完成滑块验证"});
        assert!(parse_room(&room, None).is_err());
    }
    #[test]
    fn blocked_business_responses_do_not_create_a_browser_gate_or_http_cooldown() {
        let mut state = AccessState::default();
        state.set_cookie(Some("did=original".into()));
        let before = state.ready;
        for error in [
            ACCESS_UNAVAILABLE,
            LIVE_ACCESS_UNAVAILABLE,
            PAGE_UNAVAILABLE,
            LOGIN_PROMPT,
        ] {
            state.observe(&Err(error.into()));
            assert_eq!(state.ready, before);
            assert_eq!(state.failures, 0);
            assert!(!state.login_required);
            assert_eq!(state.cookie.as_deref(), Some("did=original"));
            assert!(!verification_required(error));
        }
        assert!(verification_required(VERIFICATION_REQUIRED));
        assert_eq!(access_state(ACCESS_UNAVAILABLE), "unavailable");
        assert_eq!(access_state(VERIFICATION_REQUIRED), "captcha");
    }

    #[tokio::test(start_paused = true)]
    async fn credential_changes_and_session_reset_preserve_http_cooldown() {
        let resolver = KuaishouResolver::default();
        let deadline = {
            let mut state = resolver.access.lock().await;
            state.set_cookie(Some("did=original".into()));
            state.observe(&Err(RATE_LIMITED.into()));
            state.ready
        };
        resolver.reset_session().await;
        let request = ResolveRequest {
            account: None,
            url: "https://live.kuaishou.com/u/never-requested".into(),
            quality: None,
            proxy: None,
            cookie: Some("did=changed".into()),
            platform: Some("kuaishou".into()),
        };
        assert_eq!(resolver.resolve(&request).await.unwrap_err(), RATE_LIMITED);
        assert_eq!(resolver.access.lock().await.ready, deadline);
        tokio::time::advance(Duration::from_secs(600)).await;
        let mut state = resolver.access.lock().await;
        assert!(state.ready <= Instant::now());
        state.observe(&Err(RATE_LIMITED.into()));
        assert_eq!(state.failures, 2);
        assert_eq!(
            state.ready.duration_since(Instant::now()),
            Duration::from_secs(1200)
        );
    }

    #[test]
    fn login_failure_requires_new_credentials_but_does_not_clear_rate_limits() {
        let mut state = AccessState::default();
        state.set_cookie(Some("did=original".into()));
        state.observe(&Err(LOGIN_REQUIRED.into()));
        state.set_cookie(Some("did=original".into()));
        assert!(state.login_required);
        state.set_cookie(Some("did=changed".into()));
        assert!(!state.login_required);
        state.observe(&Ok(StreamInfo::default()));
        assert_eq!(state.failures, 0);
    }
}
