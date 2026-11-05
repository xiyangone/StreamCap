//! Douyin room metadata and stream selection. Adapted from streamget (MIT).
use super::{
    douyin_sign,
    http::{validate_url, PlatformHttp, USER_AGENT},
    quality_index, quality_name, text,
};
use crate::resolver::{ResolveRequest, StreamInfo};
use reqwest::Url;
use serde_json::Value;
use std::time::Duration;

struct RoomContext<'a> {
    room: &'a Value,
    user: Option<&'a Value>,
    anchor: Option<&'a Value>,
    web_stream: Option<&'a Value>,
}

/// Metadata may be beside the room in SSR state, but never in another room's subtree.
fn room_context(value: &Value) -> Result<RoomContext<'_>, String> {
    const MISSING: &str = "抖音未返回目标房间，不能确认是否开播";
    if let Some(data) = value.get("data").filter(|data| data.get("data").is_some()) {
        let room = data["data"]
            .as_array()
            .and_then(|rooms| rooms.first())
            .filter(|room| room.is_object())
            .ok_or(MISSING)?;
        return Ok(RoomContext {
            room,
            user: data.get("user"),
            anchor: None,
            web_stream: None,
        });
    }
    let context = if let Some(store) = value.get("roomStore") {
        store.get("roomInfo").ok_or(MISSING)?
    } else if let Some(info) = value.get("roomInfo") {
        info
    } else {
        value
    };
    let room = if let Some(room) = context.get("room") {
        room
    } else if context.get("status").is_some() {
        context
    } else {
        return Err(MISSING.into());
    };
    Ok(RoomContext {
        room,
        user: None,
        anchor: context.get("anchor"),
        web_stream: context.get("web_stream_url"),
    })
}
pub fn parse_json(value: &Value, quality: Option<&str>) -> Result<StreamInfo, String> {
    let context = room_context(value)?;
    let room = context.room;
    let status = room
        .get("status")
        .and_then(Value::as_i64)
        .ok_or("抖音房间状态缺失")?;
    let mut info = StreamInfo {
        platform: "抖音直播".into(),
        anchor_name: context
            .user
            .map(|user| text(&user["nickname"]))
            .unwrap_or_default(),
        title: text(&room["title"]),
        ..Default::default()
    };
    if info.anchor_name.is_empty() {
        info.anchor_name = text(&room["owner"]["nickname"]);
    }
    if info.anchor_name.is_empty() {
        info.anchor_name = text(&room["anchor_name"]);
    }
    if info.anchor_name.is_empty() {
        if let Some(anchor) = context.anchor {
            info.anchor_name = text(&anchor["nickname"]);
        }
    }
    if info.anchor_name.is_empty() {
        return Err("抖音未返回主播信息，不能确认目标房间".into());
    }
    match status {
        4 => return Ok(info),
        2 => {}
        _ => return Err(format!("抖音返回未识别直播状态：{status}")),
    }
    let index = quality_index(quality)?;
    let stream = room
        .get("stream_url")
        .filter(|v| v.is_object())
        .or_else(|| room.get("web_stream_url").filter(|v| v.is_object()))
        .or_else(|| context.web_stream.filter(|v| v.is_object()))
        .ok_or("抖音显示在播但缺少流信息")?;
    let mut flv = Vec::<(u64, String)>::new();
    let mut hls = Vec::<(u64, String)>::new();
    if let Some(raw) = stream
        .pointer("/live_core_sdk_data/pull_data/stream_data")
        .and_then(Value::as_str)
    {
        if let Ok(data) = serde_json::from_str::<Value>(raw) {
            if let Some(entries) = data["data"].as_object() {
                for (name, entry) in entries {
                    let main = &entry["main"];
                    let sdk = main["sdk_params"]
                        .as_str()
                        .and_then(|s| serde_json::from_str::<Value>(s).ok())
                        .unwrap_or(Value::Null);
                    let bitrate = sdk["vbitrate"].as_u64().unwrap_or(match name.as_str() {
                        "origin" => u64::MAX,
                        "uhd" => 5_000_000,
                        "hd" => 3_000_000,
                        "sd" => 1_000_000,
                        "ld" => 500_000,
                        _ => 0,
                    });
                    add(&mut flv, bitrate, &main["flv"]);
                    add(&mut hls, bitrate, &main["hls"]);
                }
            }
        }
    }
    for (field, out) in [("flv_pull_url", &mut flv), ("hls_pull_url_map", &mut hls)] {
        if let Some(map) = stream[field].as_object() {
            for (name, value) in map {
                let rank = match name.as_str() {
                    "ORIGIN" | "FULL_HD1" => 9_000_000,
                    "UHD" | "HD1" => 5_000_000,
                    "HD" | "SD1" => 3_000_000,
                    "SD" | "SD2" => 1_000_000,
                    _ => 500_000,
                };
                add(out, rank, value);
            }
        }
    }
    info.flv_url = select(flv, index);
    info.m3u8_url = select(hls, index);
    if info.m3u8_url.is_empty() {
        info.m3u8_url = stream["hls_pull_url"]
            .as_str()
            .filter(|s| validate_url(s).is_ok())
            .unwrap_or_default()
            .into();
    }
    // Douyin can advertise an HLS URL that returns 404 while its FLV stream is live.
    // Keep both sources, but recommend FLV at the requested quality for recording.
    info.record_url = if !info.flv_url.is_empty() {
        info.flv_url.clone()
    } else {
        info.m3u8_url.clone()
    };
    if info.record_url.is_empty() {
        return Err("抖音显示在播但没有可用流地址（可能需要登录）".into());
    }
    info.is_live = true;
    info.quality = quality_name(index).into();
    Ok(info)
}
fn add(out: &mut Vec<(u64, String)>, rank: u64, value: &Value) {
    if let Some(url) = value
        .as_str()
        .filter(|u| !u.is_empty() && validate_url(u).is_ok())
    {
        if !out.iter().any(|(_, u)| u == url) {
            out.push((rank, url.into()));
        }
    }
}
fn select(mut urls: Vec<(u64, String)>, index: usize) -> String {
    urls.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    urls.get(index.min(urls.len().saturating_sub(1)))
        .map(|(_, u)| u.clone())
        .unwrap_or_default()
}

fn first_json(source: &str) -> Option<Value> {
    serde_json::Deserializer::from_str(source.trim_start())
        .into_iter::<Value>()
        .next()?
        .ok()
}

fn page_frame(source: &str) -> Option<(Option<Value>, usize)> {
    let body = if source.starts_with(['{', '[']) {
        source
    } else {
        let (id, body) = source.split_once(':')?;
        if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        body
    };
    if let Some(text) = body.strip_prefix('T') {
        let (length, text) = text.split_once(',')?;
        let length = usize::from_str_radix(length, 16).ok()?;
        let consumed = (source.len() - text.len()).checked_add(length)?;
        source.get(consumed..)?;
        return Some((None, consumed));
    }
    let mut values = serde_json::Deserializer::from_str(body).into_iter::<Value>();
    let value = values.next()?.ok()?;
    Some((
        Some(value),
        source.len() - body.len() + values.byte_offset(),
    ))
}

/// The decoded page frame must own its room container. Component trees and route
/// collections do not identify which nested room belongs to the requested page.
fn parse_page_data(value: &Value, quality: Option<&str>) -> Option<Result<StreamInfo, String>> {
    let direct_room = value.get("status").is_some()
        && [
            "owner",
            "id_str",
            "anchor_name",
            "stream_url",
            "web_stream_url",
        ]
        .iter()
        .any(|key| value.get(*key).is_some());
    if direct_room
        || value.pointer("/data/data").is_some()
        || ["roomStore", "roomInfo", "room"]
            .iter()
            .any(|key| value.get(*key).is_some())
    {
        Some(parse_json(value, quality))
    } else {
        None
    }
}

pub fn parse_page(html: &str, quality: Option<&str>) -> Result<StreamInfo, String> {
    let scripts = regex::Regex::new(r"(?is)<script\b([^>]*)>(.*?)</script\s*>")
        .expect("static script pattern");
    let render_id = regex::Regex::new(r#"(?i)\bid\s*=\s*["']RENDER_DATA["']"#)
        .expect("static render data pattern");
    let mut error = None;
    let mut parse = |value: &Value| match parse_page_data(value, quality) {
        Some(Ok(info)) => Some(info),
        Some(Err(reason)) => {
            error = Some(reason);
            None
        }
        None => None,
    };
    for script in scripts.captures_iter(html).take(500) {
        if render_id.is_match(&script[1]) {
            let decoded = percent_encoding::percent_decode_str(&script[2]).decode_utf8_lossy();
            if let Ok(data) = serde_json::from_str::<Value>(&decoded) {
                if let Some(info) = parse(&data) {
                    return Ok(info);
                }
            }
        }
    }
    for marker in ["self.__pace_f.push(", "self.__next_f.push("] {
        let mut payload = String::new();
        let mut chunks = 0;
        'scripts: for script in scripts.captures_iter(html).take(500) {
            for segment in script[2].split(marker).skip(1) {
                if chunks == 500 {
                    break 'scripts;
                }
                chunks += 1;
                if let Some(Ok(Value::Array(parts))) = serde_json::Deserializer::from_str(segment)
                    .into_iter::<Value>()
                    .next()
                {
                    if parts.first().and_then(Value::as_u64) == Some(1) {
                        if let Some(part) = parts.get(1).and_then(Value::as_str) {
                            if payload.len() + part.len() > super::http::MAX_RESPONSE {
                                return Err("抖音页面数据超过大小限制".into());
                            }
                            payload.push_str(part);
                        }
                    }
                }
            }
        }
        let mut remaining = payload.as_str();
        for _ in 0..500 {
            remaining = remaining.trim_start();
            if remaining.is_empty() {
                break;
            }
            if let Some((value, consumed)) = page_frame(remaining) {
                if let Some(value) = value {
                    if let Some(info) = parse(&value) {
                        return Ok(info);
                    }
                }
                remaining = &remaining[consumed..];
            } else if remaining
                .split_once(':')
                .is_some_and(|(_, body)| body.starts_with('T'))
            {
                // A truncated length-prefixed text record cannot be scanned for new frames.
                break;
            } else if let Some((_, tail)) = remaining.split_once('\n') {
                // Flight import records are not room state.
                remaining = tail;
            } else {
                break;
            }
        }
    }
    let assignment = regex::Regex::new(
        r"^(?:(?:var|let|const)\s+)?(?:(?:window|self)\.)?[A-Za-z_$][A-Za-z0-9_$]*\s*=\s*",
    )
    .expect("static assignment pattern");
    for script in scripts.captures_iter(html).take(500) {
        let source = script[2].trim_start();
        let source = assignment
            .find(source)
            .map(|matched| &source[matched.end()..])
            .unwrap_or(source);
        if let Some(value) = first_json(source) {
            if let Some(info) = parse(&value) {
                return Ok(info);
            }
        }
    }
    Err(error.unwrap_or_else(|| "抖音页面没有可验证的房间数据（登录限制或页面结构变化）".into()))
}
pub async fn resolve(request: &ResolveRequest) -> Result<StreamInfo, String> {
    let mut url = validate_url(&request.url)?;
    let http = PlatformHttp::new(
        "douyin.com",
        request.proxy.as_deref(),
        request.cookie.as_deref(),
        Duration::from_secs(18),
    )?;
    // The page route also handles the platform's own short-link redirect. Never synthesize credentials.
    if url.host_str() != Some("live.douyin.com") {
        let response = http
            .client
            .get(url)
            .header("Referer", "https://live.douyin.com/")
            .send()
            .await
            .map_err(super::http::transport_error)?;
        url = response.url().clone();
        let page = super::http::body(response).await?;
        if let Ok(info) = parse_page(&page, request.quality.as_deref()) {
            return Ok(info);
        }
        if url.host_str() != Some("live.douyin.com") {
            return Err("请使用抖音直播间地址；该分享或主页链接未返回房间".into());
        }
    }
    let rid = url.path().trim_matches('/');
    if rid.is_empty() || rid.contains('/') {
        return Err("抖音直播间地址缺少房间编号".into());
    }
    let mut endpoint = Url::parse("https://live.douyin.com/webcast/room/web/enter/")
        .map_err(|_| "抖音端点无效")?;
    endpoint.query_pairs_mut().extend_pairs([
        ("aid", "6383"),
        ("app_name", "douyin_web"),
        ("live_id", "1"),
        ("device_platform", "web"),
        ("language", "zh-CN"),
        ("browser_language", "zh-CN"),
        ("browser_platform", "Win32"),
        ("browser_name", "Chrome"),
        ("browser_version", "116.0.0.0"),
        ("web_rid", rid),
        ("is_need_double_stream", "false"),
        ("msToken", ""),
    ]);
    let sign = douyin_sign::sign(endpoint.query().unwrap_or_default(), USER_AGENT);
    endpoint.query_pairs_mut().append_pair("a_bogus", &sign);
    let api_error = match http.get(endpoint, "https://live.douyin.com/").await {
        Ok(body) => match serde_json::from_str::<Value>(&body) {
            Ok(data) => match parse_json(&data, request.quality.as_deref()) {
                Ok(info) => return Ok(info),
                Err(e) => e,
            },
            Err(_) => "抖音接口未返回 JSON".into(),
        },
        Err(e)
            if ["HTTP 401", "HTTP 403", "HTTP 429"]
                .iter()
                .any(|code| e.contains(code)) =>
        {
            return Err(e)
        }
        Err(e) => e,
    };
    let page = http.get(url, "https://live.douyin.com/").await?;
    parse_page(&page, request.quality.as_deref()).map_err(|e| format!("{api_error}；{e}"))
}
