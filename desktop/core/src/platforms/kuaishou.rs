//! Authenticated website resolution. Human verification is explicit; no anonymous mobile retries.
use super::{
    http::{validate_url, PlatformHttp},
    quality_index, quality_name, text,
};
use crate::resolver::{ResolveRequest, StreamInfo};
use serde_json::Value;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

pub const VERIFICATION_REQUIRED: &str = "快手需要完成滑块验证（400002），请在验证窗口手动完成";
const RATE_LIMITED: &str = "快手请求受限，已进入冷却，请稍后再检测";
pub fn verification_required(error: &str) -> bool {
    error == VERIFICATION_REQUIRED
}

pub fn parse_page(html: &str, quality: Option<&str>) -> Result<StreamInfo, String> {
    let state = super::kuaishou_login::initial_state(html).ok_or("快手页面缺少可验证的房间数据")?;
    parse_state(&state, quality)
}
/// The requested room is first in the room playlist. Never accept a recommendation instead.
pub fn parse_state(state: &Value, quality: Option<&str>) -> Result<StreamInfo, String> {
    let room = if let Some(rooms) = state.pointer("/liveroom/playList") {
        rooms.as_array().and_then(|rooms| rooms.first())
    } else {
        state.get("room")
    }
    .ok_or("快手未返回目标房间，不能确认是否开播")?;
    parse_room(room, quality)
}
pub fn parse_room(room: &Value, quality: Option<&str>) -> Result<StreamInfo, String> {
    let error = &room["errorType"];
    let code = error["type"]
        .as_i64()
        .or_else(|| error["type"].as_str()?.parse().ok());
    let title = error["title"].as_str().unwrap_or_default();
    if code == Some(400002) || title.contains("请完成滑块验证") {
        return Err(VERIFICATION_REQUIRED.into());
    }
    if title.contains("频繁") || title.to_ascii_lowercase().contains("frequent") {
        return Err(RATE_LIMITED.into());
    }
    if code.is_some_and(|n| n != 0) || !title.is_empty() {
        return Err(format!(
            "快手未返回目标房间（返回码 {}）",
            code.map(|n| n.to_string()).unwrap_or_else(|| "未知".into())
        ));
    }
    let stream = &room["liveStream"];
    let mut anchor = text(&room["author"]["name"]);
    if anchor.is_empty() {
        anchor = text(&stream["user"]["user_name"]);
    }
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
    verification: bool,
    ready: Instant,
    failures: u32,
}
impl Default for AccessState {
    fn default() -> Self {
        Self {
            cookie: None,
            verification: false,
            ready: Instant::now(),
            failures: 0,
        }
    }
}
impl AccessState {
    fn limit(&mut self) {
        self.failures = self.failures.saturating_add(1);
        let seconds = (600_u64 * (1_u64 << self.failures.saturating_sub(1).min(8))).min(14400);
        self.ready = Instant::now() + Duration::from_secs(seconds);
    }
}
#[derive(Clone, Default)]
pub struct KuaishouResolver {
    access: Arc<Mutex<AccessState>>,
}
impl KuaishouResolver {
    pub async fn reset_session(&self) {
        *self.access.lock().await = AccessState::default();
    }
    pub async fn resolve(&self, request: &ResolveRequest) -> Result<StreamInfo, String> {
        let url = validate_url(&request.url)?;
        if url.scheme() != "https"
            || url.host_str() != Some("live.kuaishou.com")
            || !url.path().starts_with("/u/")
        {
            return Err("请使用快手直播间的 HTTPS 地址".into());
        }
        // Serialize requests across rooms; a challenge blocks further platform traffic until the user acts.
        let mut state = self.access.lock().await;
        if state.cookie != request.cookie {
            *state = AccessState {
                cookie: request.cookie.clone(),
                ..Default::default()
            };
        }
        if state.verification {
            return Err(VERIFICATION_REQUIRED.into());
        }
        if Instant::now() < state.ready {
            return Err(RATE_LIMITED.into());
        }
        let http = PlatformHttp::new(
            "kuaishou.com",
            request.proxy.as_deref(),
            request.cookie.as_deref(),
            Duration::from_secs(20),
        )?;
        let result = match http.get(url, "https://live.kuaishou.com/").await {
            Ok(page) => parse_page(&page, request.quality.as_deref()),
            Err(error) => Err(error),
        };
        match &result {
            Err(error) if verification_required(error) => state.verification = true,
            Err(error) if error == RATE_LIMITED || error.contains("HTTP 429") => state.limit(),
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
        resolver.access.lock().await.verification = true;
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
            VERIFICATION_REQUIRED
        );
        resolver.reset_session().await;
        assert!(!resolver.access.lock().await.verification);
    }
}
