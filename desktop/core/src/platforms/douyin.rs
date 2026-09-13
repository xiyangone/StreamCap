//! Douyin room metadata and stream selection. Adapted from streamget (MIT).
use super::{
    douyin_sign, find_object,
    http::{validate_url, PlatformHttp, USER_AGENT},
    json_after, quality_index, quality_name, text,
};
use crate::resolver::{ResolveRequest, StreamInfo};
use reqwest::Url;
use serde_json::Value;
use std::time::Duration;

fn room_in(value: &Value) -> Option<&Value> {
    find_object(
        value,
        &|v| {
            v.get("status").is_some()
                && (v.get("stream_url").is_some()
                    || v.get("owner").is_some()
                    || v.get("id_str").is_some())
        },
        0,
    )
}
pub fn parse_json(value: &Value, quality: Option<&str>) -> Result<StreamInfo, String> {
    let room = value
        .pointer("/data/data/0")
        .or_else(|| room_in(value))
        .ok_or("抖音未返回目标房间，不能确认是否开播")?;
    let status = room
        .get("status")
        .and_then(Value::as_i64)
        .ok_or("抖音房间状态缺失")?;
    let mut info = StreamInfo {
        platform: "抖音直播".into(),
        anchor_name: text(&value["data"]["user"]["nickname"]),
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
        if let Some(user) = find_object(
            value,
            &|v| v.get("nickname").is_some() && v.get("avatar_thumb").is_some(),
            0,
        ) {
            info.anchor_name = text(&user["nickname"]);
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
        .or_else(|| {
            find_object(
                value,
                &|v| v.get("web_stream_url").is_some_and(Value::is_object),
                0,
            )
            .and_then(|v| v.get("web_stream_url"))
        })
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

pub fn parse_page(html: &str, quality: Option<&str>) -> Result<StreamInfo, String> {
    if let Some(start) = html.find("id=\"RENDER_DATA\"") {
        if let Some(body) = html[start..]
            .split_once('>')
            .and_then(|(_, s)| s.split_once("</script>").map(|(s, _)| s))
        {
            let decoded = percent_encoding::percent_decode_str(body).decode_utf8_lossy();
            if let Ok(data) = serde_json::from_str::<Value>(&decoded) {
                if room_in(&data).is_some() {
                    return parse_json(&data, quality);
                }
            }
        }
    }
    for segment in ["self.__pace_f.push(", "self.__next_f.push("]
        .into_iter()
        .flat_map(|marker| html.split(marker).skip(1))
        .take(500)
    {
        if let Some(Ok(Value::Array(parts))) = serde_json::Deserializer::from_str(segment)
            .into_iter::<Value>()
            .next()
        {
            for part in parts.iter().filter_map(Value::as_str) {
                for marker in ["\"roomStore\":", "\"room\":"] {
                    if let Some(value) = json_after(part, marker) {
                        if let Ok(info) = parse_json(&value, quality) {
                            return Ok(info);
                        }
                    }
                }
            }
        }
    }
    for marker in ["\"roomStore\":", "\"room\":"] {
        if let Some(value) = json_after(html, marker) {
            if let Ok(info) = parse_json(&value, quality) {
                return Ok(info);
            }
        }
    }
    Err("抖音页面没有可验证的房间数据（登录限制或页面结构变化）".into())
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
        Err(e) => e,
    };
    let page = http.get(url, "https://live.douyin.com/").await?;
    parse_page(&page, request.quality.as_deref()).map_err(|e| format!("{api_error}；{e}"))
}
