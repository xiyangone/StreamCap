//! Human-readable labels and pure presentation helpers.
use crate::api::gateway::NewRecording;
use crate::app::i18n::t;
use std::collections::HashSet;

pub const QUALITY_OPTIONS: &[(&str, &str)] = &[
    ("OD", "原画"),
    ("UHD", "超清"),
    ("HD", "高清"),
    ("SD", "标清"),
    ("LD", "流畅"),
];
pub const FORMAT_OPTIONS: &[&str] = &[
    "TS", "FLV", "MKV", "MOV", "MP4", "NUT", "MP3", "M4A", "AAC", "WAV", "WMA",
];
pub const PLATFORMS: &[(&str, &str)] = &[
    ("douyin", "抖音直播"),
    ("kuaishou", "快手直播"),
    ("bilibili", "哔哩哔哩"),
    ("huya", "虎牙直播"),
    ("douyu", "斗鱼直播"),
    ("tiktok", "TikTok"),
    ("yy", "YY直播"),
    ("rednote", "小红书"),
    ("bigo", "Bigo"),
    ("blued", "Blued"),
    ("soop", "SOOP"),
    ("netease", "网易CC"),
    ("qiandurebo", "千度热播"),
    ("pandatv", "PandaTV"),
    ("maoerfm", "猫耳FM"),
    ("look", "LOOK"),
    ("winktv", "WinkTV"),
    ("flextv", "FlexTV"),
    ("popkontv", "PopkonTV"),
    ("twitcasting", "TwitCasting"),
    ("baidu", "百度直播"),
    ("weibo", "微博直播"),
    ("kugou", "酷狗直播"),
    ("twitch", "Twitch"),
    ("liveme", "LiveMe"),
    ("huajiao", "花椒直播"),
    ("showroom", "SHOWROOM"),
    ("acfun", "AcFun"),
    ("inke", "映客直播"),
    ("yinbo", "音播直播"),
    ("changliao", "畅聊直播"),
    ("zhihu", "知乎直播"),
    ("chzzk", "CHZZK"),
    ("haixiu", "嗨秀直播"),
    ("vvxq", "VV星球"),
    ("17live", "17Live"),
    ("langlive", "浪Live"),
    ("piaopiao", "漂漂直播"),
    ("sixroom", "六间房"),
    ("lehai", "乐嗨直播"),
    ("huamao", "花猫直播"),
    ("shopee", "Shopee"),
    ("youtube", "YouTube"),
    ("taobao", "淘宝直播"),
    ("jd", "京东直播"),
    ("faceit", "FACEIT"),
    ("lianjie", "连接直播"),
    ("migu", "咪咕直播"),
    ("laixiu", "来秀直播"),
    ("picarto", "Picarto"),
    ("xindongrebo", "心动热播"),
    ("custom", "自定义流"),
];
pub fn quality_label(code: Option<&str>) -> String {
    let code = code.unwrap_or("").trim().to_ascii_uppercase();
    QUALITY_OPTIONS
        .iter()
        .find(|(key, _)| *key == code)
        .map(|(_, name)| t(name).into())
        .unwrap_or_else(|| {
            if code.is_empty() {
                t("跟随全局").into()
            } else {
                code
            }
        })
}
pub fn format_label(code: Option<&str>) -> String {
    code.filter(|s| !s.is_empty())
        .map(str::to_ascii_uppercase)
        .unwrap_or_else(|| t("跟随全局").into())
}
pub fn platform_label(key: &str) -> String {
    PLATFORMS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, name)| t(name).into())
        .unwrap_or_else(|| key.to_string())
}
pub fn human_size(bytes: u64) -> String {
    let mut size = bytes as f64;
    let mut unit = 0;
    let units = ["B", "KB", "MB", "GB", "TB"];
    while size >= 1024.0 && unit < units.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", units[unit])
    }
}
pub fn modified_time(value: Option<f64>) -> String {
    value
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .map(|seconds| {
            let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(seconds * 1000.0));
            format!(
                "{}-{:02}-{:02} {:02}:{:02}",
                date.get_full_year(),
                date.get_month() + 1,
                date.get_date(),
                date.get_hours(),
                date.get_minutes()
            )
        })
        .unwrap_or_else(|| "—".into())
}
pub fn valid_url(url: &str) -> bool {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    rest.is_some_and(|s| {
        let host = s.split(['/', '?', '#']).next().unwrap_or("");
        !host.is_empty() && !host.contains('@') && !url.chars().any(char::is_whitespace)
    })
}
pub fn parse_recordings(raw: &str) -> Result<Vec<NewRecording>, String> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for (index, line) in raw
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
    {
        let normalized = line.replace('，', ",");
        let parts: Vec<_> = normalized.split(',').map(str::trim).collect();
        let (quality, url, name) = match parts.as_slice() {
            [url] => (None, *url, None),
            [url, name] if valid_url(url) => (None, *url, Some(*name)),
            [quality, url] => (Some(*quality), *url, None),
            [quality, url, name] => (Some(*quality), *url, Some(*name)),
            _ => {
                return Err(crate::tr_format!(
                    "第 {} 行格式不正确，请每行填写一个地址",
                    index + 1
                ))
            }
        };
        if !valid_url(url) {
            return Err(crate::tr_format!(
                "第 {} 行不是有效的 HTTP / HTTPS 地址",
                index + 1
            ));
        }
        if !seen.insert(url.to_string()) {
            return Err(crate::tr_format!("第 {} 行的地址重复", index + 1));
        }
        let quality = quality.map(|q| {
            match q {
                "0" => "OD",
                "1" => "UHD",
                "2" => "HD",
                "3" => "SD",
                "4" => "LD",
                _ => q,
            }
            .to_ascii_uppercase()
        });
        if quality
            .as_ref()
            .is_some_and(|q| !QUALITY_OPTIONS.iter().any(|(code, _)| code == q))
        {
            return Err(crate::tr_format!("第 {} 行的清晰度无效", index + 1));
        }
        items.push(NewRecording {
            url: url.into(),
            streamer_name: name.filter(|s| !s.is_empty()).map(str::to_string),
            quality,
        });
    }
    if items.is_empty() {
        return Err(t("请至少填写一个直播间地址").into());
    }
    Ok(items)
}

/// Keep failures and active captures visible in both dashboard and task sorting.
pub fn recording_priority(record: &crate::api::gateway::Recording) -> u8 {
    if record.needs_attention() {
        0
    } else if record.is_recording {
        1
    } else if record.is_live && record.monitor_status {
        2
    } else if record.monitor_status {
        3
    } else {
        4
    }
}

pub fn monitoring_label(record: &crate::api::gateway::Recording) -> &'static str {
    if !record.monitor_status {
        t("监控已暂停")
    } else if record.verification_required {
        t("等待手动验证")
    } else if record
        .check_error
        .as_ref()
        .is_some_and(|error| !error.is_empty())
    {
        t("等待检测恢复")
    } else if record.only_notify_no_record == Some(true) {
        t("仅通知，不录制")
    } else if record.scheduled_recording == Some(true) {
        t("按时间段自动监控")
    } else {
        t("自动监控，开播即录")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn monitoring_labels_and_attention_order_are_explicit() {
        use crate::api::gateway::Recording;
        let mut record = Recording::default();
        assert_eq!(monitoring_label(&record), "监控已暂停");
        record.monitor_status = true;
        assert_eq!(monitoring_label(&record), "自动监控，开播即录");
        record.only_notify_no_record = Some(true);
        assert_eq!(monitoring_label(&record), "仅通知，不录制");
        record.only_notify_no_record = Some(false);
        record.scheduled_recording = Some(true);
        assert_eq!(monitoring_label(&record), "按时间段自动监控");
        record.verification_required = true;
        assert_eq!(monitoring_label(&record), "等待手动验证");
        assert_eq!(recording_priority(&record), 0);
        assert!(record.needs_attention());
        record.verification_required = false;
        record.check_error = Some("检测超时".into());
        assert_eq!(monitoring_label(&record), "等待检测恢复");
        assert_eq!(recording_priority(&record), 0);
        record.check_error = None;
        record.recording_error = Some("fixture".into());
        assert_eq!(recording_priority(&record), 0);
        record.is_recording = true;
        assert_eq!(recording_priority(&record), 1);
        assert_eq!(duration(3661.9), "01:01:01");
        assert_eq!(duration(f64::INFINITY), "00:00:00");
    }
    #[test]
    fn labels_are_readable() {
        assert_eq!(quality_label(Some("OD")), "原画");
        assert_eq!(quality_label(Some("ld")), "流畅");
        assert_eq!(format_label(Some("ts")), "TS");
    }
    #[test]
    fn imports_keep_per_line_names_and_quality() {
        let items = parse_recordings("https://live.douyin.com/1,主播甲\n2,https://live.bilibili.com/2,主播乙\nhttps://www.huya.com/3").unwrap();
        assert_eq!(items[0].streamer_name.as_deref(), Some("主播甲"));
        assert_eq!(items[1].quality.as_deref(), Some("HD"));
        assert_eq!(items[2].streamer_name, None);
        assert_eq!(items[2].quality, None);
    }
    #[test]
    fn imports_reject_duplicates_and_invalid_input() {
        assert!(parse_recordings("file:///private").is_err());
        assert!(parse_recordings("https://live.douyin.com/1\nhttps://live.douyin.com/1").is_err());
        assert!(parse_recordings("OD,https://live.douyin.com/1").is_ok());
        assert!(!valid_url("https://user:password@live.example.com"));
        assert!(!valid_url("https://"));
    }
    #[test]
    fn file_sizes_do_not_overflow() {
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(0), "0 B");
    }
}

pub fn duration(seconds: f64) -> String {
    let seconds = if seconds.is_finite() {
        seconds.max(0.0) as u64
    } else {
        0
    };
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    )
}
