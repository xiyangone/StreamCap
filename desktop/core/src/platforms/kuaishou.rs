//! Authenticated website resolution. Human verification is explicit; no anonymous mobile retries.
use super::{
    http::{validate_url, PlatformHttp},
    quality_index, quality_name, text,
};
use crate::resolver::{ResolveRequest, StreamInfo};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{Mutex, OwnedSemaphorePermit, Semaphore},
    time::Instant,
};

pub const PAGE_CHECK_REQUIRED: &str = "快手接口暂不可读（400002），等待页面检查";
pub const VERIFICATION_REQUIRED: &str = "快手页面出现验证码，请在验证窗口手动完成";
pub const RATE_LIMITED: &str = "快手请求过快，已暂停请求并进入冷却；请勿反复刷新或重新登录";
pub const LOGIN_REQUIRED: &str = "快手目标页面明确要求登录，请在设置中检查登录状态";
pub const LOGIN_PROMPT: &str = "快手页面出现登录提示，但未确认登录失效；已停止本次检查";
pub const PAGE_UNAVAILABLE: &str = "快手页面暂不可读，已停止本次检查；未确认下播或需要验证码";
pub const PAGE_RETRY_LATER: &str = "快手页面检查处于冷却期，请等待下次轮询；未打开新窗口";
pub const PAGE_IN_PROGRESS: &str = "快手页面检查正在进行，未重复请求";

#[derive(Debug, Clone, PartialEq)]
pub enum VerificationPage {
    Challenge,
    RateLimited,
    LoginRequired,
    Room(Box<crate::resolver::StreamInfo>),
    Unknown(String),
}
pub fn verification_required(error: &str) -> bool {
    error == VERIFICATION_REQUIRED
}
pub fn page_check_required(error: &str) -> bool {
    error == PAGE_CHECK_REQUIRED
}
pub fn access_state(error: &str) -> &'static str {
    match error {
        VERIFICATION_REQUIRED => "captcha",
        PAGE_CHECK_REQUIRED | PAGE_IN_PROGRESS => "pageCheck",
        RATE_LIMITED | PAGE_RETRY_LATER => "cooldown",
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
        "访问受限",
        "请求受限",
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

/// Evaluate the current, identity-bound room state, not the removed SSR script.
/// A 400002 response alone does not prove an interactive CAPTCHA is visible.
pub fn evaluate_verification_page(
    state: &Value,
    expected_id: &str,
    visible_text: &str,
    challenge_visible: bool,
    quality: Option<&str>,
) -> VerificationPage {
    let room = if let Some(rooms) = state.pointer("/liveroom/playList") {
        let index = match state.pointer("/liveroom/activeIndex") {
            None => 0,
            Some(value) => match value.as_u64().and_then(|n| usize::try_from(n).ok()) {
                Some(index) => index,
                None => return VerificationPage::Unknown("快手目标播放器尚未就绪".into()),
            },
        };
        rooms.as_array().and_then(|rooms| rooms.get(index))
    } else {
        state.get("room").filter(|room| room.is_object())
    };
    // Access failures describe this navigation, not the stream's live/offline state.
    // Limited pages often deliberately omit the author; classify them before identity checks.
    let error_text = room.map(room_error_text).unwrap_or_default();
    if limited_text(visible_text) || limited_text(&error_text) {
        return VerificationPage::RateLimited;
    }
    if challenge_visible {
        return VerificationPage::Challenge;
    }
    if login_required_text(visible_text) || login_required_text(&error_text) {
        return VerificationPage::LoginRequired;
    }
    let Some(room) = room else {
        return VerificationPage::Unknown("快手页面尚未返回目标房间数据，保留页面等待加载".into());
    };
    if expected_id.is_empty() || text(&room["author"]["id"]) != expected_id {
        return VerificationPage::Unknown("快手页面尚未确认目标主播，未应用页面结果".into());
    }
    match parse_room(room, quality) {
        Ok(info) => VerificationPage::Room(Box::new(info)),
        Err(error) if page_check_required(&error) && visible_offline_text(visible_text) => {
            if !explicitly_offline(room) {
                return VerificationPage::Unknown(
                    "页面文字显示未开播，但目标房间状态仍不明确".into(),
                );
            }
            let anchor = room_anchor(room);
            if anchor.is_empty() || !visible_text.contains(&anchor) {
                return VerificationPage::Unknown("页面显示未开播，但未能确认目标主播".into());
            }
            VerificationPage::Room(Box::new(StreamInfo {
                platform: "快手直播".into(),
                anchor_name: anchor,
                ..Default::default()
            }))
        }
        Err(error) => VerificationPage::Unknown(error),
    }
}

fn visible_offline_text(text: &str) -> bool {
    ["主播尚未开播", "主播暂未开播", "当前未开播", "直播已结束"]
        .iter()
        .any(|marker| text.contains(marker))
}

/// Cookie headers are ordered from the most specific scope to the least specific.
/// The WebView and the HTTP jar must both keep the first occurrence of a name.
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

fn explicitly_offline(room: &Value) -> bool {
    room["isLiving"]
        .as_bool()
        .or_else(|| room["liveStream"]["living"].as_bool())
        == Some(false)
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
pub fn parse_room(room: &Value, quality: Option<&str>) -> Result<StreamInfo, String> {
    let error = &room["errorType"];
    let code = error["type"]
        .as_i64()
        .or_else(|| error["type"].as_str()?.parse().ok());
    let title = error["title"].as_str().unwrap_or_default();
    let message = room_error_text(room);
    if limited_text(&message) {
        return Err(RATE_LIMITED.into());
    }
    if login_required_text(&message) {
        return Err(LOGIN_REQUIRED.into());
    }
    if code == Some(400002) || title.contains("请完成滑块验证") {
        return Err(PAGE_CHECK_REQUIRED.into());
    }
    if code.is_some_and(|n| n != 0) || !title.is_empty() {
        return Err(format!(
            "快手未返回目标房间（返回码 {}）",
            code.map(|n| n.to_string()).unwrap_or_else(|| "未知".into())
        ));
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

struct AccessState {
    cookie: Option<String>,
    page_cookie: Option<String>,
    page_required: bool,
    login_required: bool,
    ready: Instant,
    page_ready: Instant,
    failures: u32,
}
impl Default for AccessState {
    fn default() -> Self {
        Self {
            cookie: None,
            page_cookie: None,
            page_required: false,
            login_required: false,
            ready: Instant::now(),
            page_ready: Instant::now(),
            failures: 0,
        }
    }
}
impl AccessState {
    fn set_cookie(&mut self, cookie: Option<String>) {
        if self.cookie != cookie {
            self.cookie = cookie;
            self.page_cookie = None;
            self.page_required = false;
            self.login_required = false;
            // Credential edits never undo a server restriction or the page request budget.
        }
    }
    fn limit(&mut self) {
        self.failures = self.failures.saturating_add(1);
        let seconds = (600_u64 * (1_u64 << self.failures.saturating_sub(1).min(8))).min(14400);
        self.ready = Instant::now() + Duration::from_secs(seconds);
    }
}
#[derive(Clone)]
pub struct KuaishouResolver {
    access: Arc<Mutex<AccessState>>,
    pages: Arc<Semaphore>,
}
impl Default for KuaishouResolver {
    fn default() -> Self {
        Self {
            access: Arc::new(Mutex::new(AccessState::default())),
            pages: Arc::new(Semaphore::new(1)),
        }
    }
}
impl KuaishouResolver {
    pub async fn begin_page_check(
        &self,
        saved: Option<&str>,
        interval: Duration,
    ) -> Result<OwnedSemaphorePermit, String> {
        let permit = self
            .pages
            .clone()
            .try_acquire_owned()
            .map_err(|_| PAGE_IN_PROGRESS.to_string())?;
        let cookie = saved
            .filter(|s| !s.trim().is_empty())
            .map(canonical_cookie_header)
            .transpose()?;
        let mut state = self.access.lock().await;
        state.set_cookie(cookie);
        if state.ready > Instant::now() {
            return Err(RATE_LIMITED.into());
        }
        if state.login_required {
            return Err(LOGIN_REQUIRED.into());
        }
        if state.page_ready > Instant::now() {
            return Err(PAGE_RETRY_LATER.into());
        }
        state.page_ready = Instant::now() + interval;
        state.page_required = true;
        Ok(permit)
    }
    pub async fn page_problem(&self, error: &str) {
        let mut state = self.access.lock().await;
        match error {
            RATE_LIMITED => {
                state.limit();
                state.page_required = false;
            }
            LOGIN_REQUIRED => {
                state.login_required = true;
                state.page_required = false;
            }
            _ => {}
        }
    }
    /// Bind a verified browser transport to the unchanged saved configuration.
    /// A public-room session must not replace a user's stored login identity.
    pub async fn use_page_session(&self, saved: Option<&str>, page: &str) -> Result<(), String> {
        let cookie = saved
            .filter(|s| !s.trim().is_empty())
            .map(canonical_cookie_header)
            .transpose()?;
        let page_cookie = canonical_cookie_header(page)?;
        if page_cookie.is_empty() {
            return Err("快手页面会话为空".into());
        }
        let mut state = self.access.lock().await;
        state.set_cookie(cookie);
        state.page_cookie = Some(page_cookie);
        state.page_required = false;
        state.login_required = false;
        Ok(())
    }
    pub async fn reset_session(&self) {
        let mut state = self.access.lock().await;
        state.cookie = None;
        state.page_cookie = None;
        state.page_required = false;
        state.login_required = false;
    }
    pub async fn resolve(&self, request: &ResolveRequest) -> Result<StreamInfo, String> {
        let url = validate_url(&request.url)?;
        if url.scheme() != "https"
            || url.host_str() != Some("live.kuaishou.com")
            || !url.path().starts_with("/u/")
        {
            return Err("请使用快手直播间的 HTTPS 地址".into());
        }
        let cookie = request
            .cookie
            .as_deref()
            .filter(|cookie| !cookie.trim().is_empty())
            .map(canonical_cookie_header)
            .transpose()?;
        // Serialize requests across rooms; restricted responses wait for the official page.
        let mut state = self.access.lock().await;
        state.set_cookie(cookie);
        if Instant::now() < state.ready {
            return Err(RATE_LIMITED.into());
        }
        if state.login_required {
            return Err(LOGIN_REQUIRED.into());
        }
        if state.page_required || self.pages.available_permits() == 0 {
            return Err(PAGE_CHECK_REQUIRED.into());
        }
        let http = PlatformHttp::new(
            "kuaishou.com",
            request.proxy.as_deref(),
            state.page_cookie.as_deref().or(state.cookie.as_deref()),
            Duration::from_secs(20),
        )?;
        let result = match http.get(url, "https://live.kuaishou.com/").await {
            Ok(page) => parse_page(&page, request.quality.as_deref()),
            Err(error) if error.contains("HTTP 429") => Err(RATE_LIMITED.into()),
            Err(error) => Err(error),
        };
        match &result {
            Err(error) if page_check_required(error) => state.page_required = true,
            Err(error) if error == RATE_LIMITED || error.contains("HTTP 429") => state.limit(),
            Err(error) if error == LOGIN_REQUIRED => state.login_required = true,
            Ok(_) => {
                state.failures = 0;
                state.ready = Instant::now();
            }
            _ => {}
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn verified_page_transport_does_not_replace_saved_identity_and_reset_discards_it() {
        let resolver = KuaishouResolver::default();
        resolver
            .use_page_session(Some("userId=original; did=old"), "did=current")
            .await
            .unwrap();
        {
            let state = resolver.access.lock().await;
            assert_eq!(state.cookie.as_deref(), Some("userId=original; did=old"));
            assert_eq!(state.page_cookie.as_deref(), Some("did=current"));
            assert!(!state.page_required);
        }
        resolver.reset_session().await;
        assert!(resolver.access.lock().await.page_cookie.is_none());
    }
    #[test]
    fn cooldown_is_bounded_and_verification_is_not_offline() {
        let mut state = AccessState::default();
        state.limit();
        assert!(state.ready.duration_since(Instant::now()).as_secs() >= 599);
        for _ in 0..10 {
            state.limit();
        }
        assert!((14399..=14400).contains(&state.ready.duration_since(Instant::now()).as_secs()));
        assert!(verification_required(VERIFICATION_REQUIRED));
        assert!(!verification_required("平台请求超时"));
    }
    #[tokio::test]
    async fn blocked_rooms_do_not_send_another_request_and_session_reset_clears_gate() {
        let resolver = KuaishouResolver::default();
        resolver.access.lock().await.page_required = true;
        let request = ResolveRequest {
            account: None,
            url: "https://live.kuaishou.com/u/fixture".into(),
            quality: None,
            proxy: None,
            cookie: None,
            platform: Some("kuaishou".into()),
        };
        assert_eq!(
            resolver.resolve(&request).await.unwrap_err(),
            PAGE_CHECK_REQUIRED
        );
        resolver.reset_session().await;
        assert!(!resolver.access.lock().await.page_required);
    }

    #[tokio::test(start_paused = true)]
    async fn http_browser_and_credential_changes_share_rate_limit_cooldown() {
        let resolver = KuaishouResolver::default();
        let permit = resolver
            .begin_page_check(Some("did=fixture"), Duration::from_secs(4500))
            .await
            .unwrap();
        resolver.page_problem(RATE_LIMITED).await;
        drop(permit);
        assert_eq!(
            resolver
                .begin_page_check(Some("did=fixture"), Duration::from_secs(4500))
                .await
                .unwrap_err(),
            RATE_LIMITED
        );
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
        resolver
            .use_page_session(Some("did=changed"), "did=browser")
            .await
            .unwrap();
        assert_eq!(resolver.resolve(&request).await.unwrap_err(), RATE_LIMITED);
        tokio::time::advance(Duration::from_secs(600)).await;
        assert_eq!(
            resolver
                .begin_page_check(Some("did=changed"), Duration::from_secs(4500))
                .await
                .unwrap_err(),
            PAGE_RETRY_LATER
        );
        tokio::time::advance(Duration::from_secs(3900)).await;
        assert!(resolver
            .begin_page_check(Some("did=changed"), Duration::from_secs(4500))
            .await
            .is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_rooms_and_manual_reopens_cannot_create_more_page_requests() {
        let resolver = KuaishouResolver::default();
        let permit = resolver
            .begin_page_check(None, Duration::from_secs(4500))
            .await
            .unwrap();
        assert_eq!(
            resolver
                .begin_page_check(None, Duration::from_secs(4500))
                .await
                .unwrap_err(),
            PAGE_IN_PROGRESS
        );
        drop(permit);
        assert_eq!(
            resolver
                .begin_page_check(None, Duration::from_secs(4500))
                .await
                .unwrap_err(),
            PAGE_RETRY_LATER
        );
        resolver.page_problem(LOGIN_PROMPT).await;
        assert!(
            !resolver.access.lock().await.login_required,
            "a marketing/login prompt is not expired authentication"
        );
        resolver.page_problem(LOGIN_REQUIRED).await;
        assert_eq!(
            resolver
                .begin_page_check(None, Duration::from_secs(4500))
                .await
                .unwrap_err(),
            LOGIN_REQUIRED
        );
        resolver.reset_session().await;
        assert_eq!(
            resolver
                .begin_page_check(None, Duration::from_secs(4500))
                .await
                .unwrap_err(),
            PAGE_RETRY_LATER
        );
    }
}
