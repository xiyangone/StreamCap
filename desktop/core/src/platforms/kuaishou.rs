//! In-process Kuaishou room parsing with bounded mobile-API backoff.
use super::{
    find_object,
    http::{body, transport_error, validate_url, PlatformHttp},
    json_after, quality_index, quality_name, text,
};
use crate::resolver::{ResolveRequest, StreamInfo};
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

pub fn parse_page(html: &str, quality: Option<&str>) -> Result<StreamInfo, String> {
    let data = json_after(html, "window.__INITIAL_STATE__=")
        .ok_or("快手页面缺少房间数据（可能需要重新登录）")?;
    let room = find_object(
        &data,
        &|v| {
            v.get("liveStream").is_some()
                && (v.get("author").is_some() || v.get("errorType").is_some())
        },
        0,
    )
    .ok_or("快手未返回目标房间，不能确认是否开播")?;
    parse_room(room, quality)
}
pub fn parse_room(room: &Value, quality: Option<&str>) -> Result<StreamInfo, String> {
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

const MOBILE_ENDPOINT: &str =
    "https://livev.m.chenzhongtech.com/rest/k/live/byUser?kpn=GAME_ZONE&captchaToken=";
const MOBILE_USER_AGENT: &str = "ios/7.830 (ios 17.0; ; iPhone 15 (A2846/A3089/A3090/A3092))";
struct MobileBackoff {
    ready: Instant,
    failures: u32,
    reason: String,
}
impl Default for MobileBackoff {
    fn default() -> Self {
        Self {
            ready: Instant::now(),
            failures: 0,
            reason: String::new(),
        }
    }
}
impl MobileBackoff {
    fn fail(&mut self, reason: String) {
        self.failures = self.failures.saturating_add(1);
        let seconds = (600_u64 * (1_u64 << self.failures.saturating_sub(1).min(8))).min(14400);
        self.ready = Instant::now() + Duration::from_secs(seconds);
        self.reason = reason;
    }
    fn clear(&mut self) {
        *self = Self::default();
    }
}
#[derive(Clone, Default)]
pub struct KuaishouResolver {
    mobile: Arc<Mutex<MobileBackoff>>,
}
impl KuaishouResolver {
    pub async fn resolve(&self, request: &ResolveRequest) -> Result<StreamInfo, String> {
        let url = validate_url(&request.url)?;
        let mobile_error = if let Some(eid) = url
            .path()
            .strip_prefix("/u/")
            .filter(|id| !id.is_empty() && !id.contains('/'))
        {
            // One attempt at a time across rooms; a blocked endpoint never receives a burst of retries.
            let mut state = self.mobile.lock().await;
            if Instant::now() < state.ready {
                Some(state.reason.clone())
            } else {
                match resolve_mobile(request, eid).await {
                    Ok(info) => {
                        state.clear();
                        return Ok(info);
                    }
                    Err(error) => {
                        state.fail(error.clone());
                        Some(error)
                    }
                }
            }
        } else {
            None
        };
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
        result.map_err(|error| match mobile_error {
            Some(mobile) => format!("{mobile}；{error}"),
            None => error,
        })
    }
}
async fn resolve_mobile(request: &ResolveRequest, eid: &str) -> Result<StreamInfo, String> {
    // This public share endpoint is on a different domain. Do not forward the user's kuaishou.com cookie.
    let http = PlatformHttp::new(
        "chenzhongtech.com",
        request.proxy.as_deref(),
        None,
        Duration::from_secs(18),
    )?;
    let response = http
        .client
        .post(MOBILE_ENDPOINT)
        .header("User-Agent", MOBILE_USER_AGENT)
        .header("Referer", "https://www.kuaishou.com/")
        .json(
            &json!({"source":5,"eid":eid,"shareMethod":"card","clientType":"WEB_OUTSIDE_SHARE_H5"}),
        )
        .send()
        .await
        .map_err(transport_error)?;
    let payload = body(response).await?;
    let value: Value = serde_json::from_str(&payload).map_err(|_| "快手移动接口未返回 JSON")?;
    parse_mobile(&value, request.quality.as_deref())
}
pub fn parse_mobile(value: &Value, quality: Option<&str>) -> Result<StreamInfo, String> {
    if !value["liveStream"].is_object() {
        let message = value["error_msg"]
            .as_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if message.contains("frequent") || message.contains("操作太快") || message.contains("频繁")
        {
            return Err("快手平台限制访问，移动接口已进入冷却；请稍后重试".into());
        }
        return Err(format!(
            "快手移动接口未返回房间数据（返回码 {}）",
            value["result"]
                .as_i64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "未知".into())
        ));
    }
    parse_room(value, quality)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mobile_backoff_grows_is_bounded_and_resets() {
        let mut backoff = MobileBackoff::default();
        assert!(backoff.ready <= Instant::now());
        backoff.fail("limited".into());
        assert_eq!(backoff.failures, 1);
        assert!(backoff.ready.duration_since(Instant::now()).as_secs() >= 599);
        for _ in 0..10 {
            backoff.fail("limited".into());
        }
        let remaining = backoff.ready.duration_since(Instant::now()).as_secs();
        assert!((14399..=14400).contains(&remaining));
        backoff.clear();
        assert_eq!(backoff.failures, 0);
        assert!(backoff.ready <= Instant::now());
    }
}
