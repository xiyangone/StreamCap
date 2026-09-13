//! 录制任务模型：字段与 JSON 契约与 Python 侧 `app/models/recording/recording_model.py` 对齐，
//! 前端使用的 camelCase 序列化由 serde rename 保证。

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// 宽容解析整数：真实数据里 Python 侧不做类型校验，实测存在
/// `"monitor_hours": "5,"`（字符串带尾逗号）这类值。
/// 解析规则必须比 Python 更宽松，否则会拒绝载入用户的合法数据。
fn lenient_i32<'de, D>(deserializer: D) -> Result<Option<i32>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match Option::<Value>::deserialize(deserializer)? {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => n.as_i64().map(|v| v as i32),
        Some(Value::String(s)) => s
            .trim()
            .trim_end_matches(',')
            .trim()
            .parse::<i64>()
            .ok()
            .map(|v| v as i32),
        _ => None,
    })
}

/// 宽容解析 64 位整数：同 lenient_i32，用于码率等可能被存成字符串的字段。
fn lenient_i64<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match Option::<Value>::deserialize(deserializer)? {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().trim_end_matches(',').trim().parse::<i64>().ok(),
        _ => None,
    })
}

/// 宽容解析浮点：同上，兼容字符串形式的数值。
fn lenient_f64<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(match Option::<Value>::deserialize(deserializer)? {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().trim_end_matches(',').trim().parse::<f64>().ok(),
        _ => None,
    })
}

/// 可继承字段 -> 对应全局设置键。
/// 语义：值等于当前全局值即视为「跟随全局」，持久化时写 null，之后改设置页自动生效。
pub const INHERITABLE_FIELDS: &[(&str, &str)] = &[
    ("record_format", "video_format"),
    ("quality", "record_quality"),
    ("segment_record", "segmented_recording_enabled"),
    ("segment_time", "video_segment_time"),
    ("flv_use_direct_download", "flv_use_direct_download"),
    ("only_notify_no_record", "only_notify_no_record"),
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Recording {
    pub rec_id: String,
    pub url: String,
    pub streamer_name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_key: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment_record: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment_time: Option<String>,

    #[serde(default)]
    pub monitor_status: bool,
    #[serde(default)]
    pub scheduled_recording: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_start_time: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor_hours: Option<i32>,

    #[serde(default)]
    pub enabled_message_push: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only_notify_no_record: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flv_use_direct_download: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_bitrate: Option<i64>,

    // --- 运行期状态（不持久化语义由 store 控制）---
    #[serde(default)]
    pub is_live: bool,
    #[serde(default)]
    pub is_recording: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_error: Option<String>,
    /// Internal attempt identity; never sent to the UI or persisted with user tasks.
    #[serde(skip)]
    pub(crate) recording_run: Option<uuid::Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_dir: Option<String>,

    /// 当前跟随全局设置的字段名集合，持久化时这些字段写 null。
    #[serde(default, skip_deserializing)]
    pub inherited_fields: Vec<String>,

    /// 上次录制时长（秒），与 Python 同名字段对应。
    #[serde(skip)]
    pub last_duration: Option<f64>,

    /// 磁盘上存在但本模块未建模的字段，重写时原样保留。
    #[serde(skip)]
    pub extra: serde_json::Map<String, Value>,
}

impl Recording {
    pub fn new(rec_id: String, url: String, streamer_name: String) -> Self {
        Self {
            rec_id,
            url,
            streamer_name,
            platform: None,
            platform_key: None,
            record_format: None,
            quality: None,
            segment_record: None,
            segment_time: None,
            monitor_status: true,
            scheduled_recording: None,
            scheduled_start_time: None,
            monitor_hours: None,
            enabled_message_push: None,
            only_notify_no_record: None,
            flv_use_direct_download: None,
            video_bitrate: None,
            is_live: false,
            is_recording: false,
            recording_error: None,
            recording_run: None,
            live_title: None,
            speed: None,
            display_title: None,
            recording_dir: None,
            inherited_fields: Vec::new(),
            last_duration: None,
            extra: serde_json::Map::new(),
        }
    }

    /// 标题：`主播名 - 清晰度`，与 Python 的 update_title 一致。
    pub fn update_title(&mut self) {
        let quality = self.quality.clone().unwrap_or_default();
        let title = format!("{} - {}", self.streamer_name, quality);
        self.display_title = Some(title);
    }

    pub fn title(&self) -> String {
        self.display_title.clone().unwrap_or_else(|| {
            format!(
                "{} - {}",
                self.streamer_name,
                self.quality.clone().unwrap_or_default()
            )
        })
    }

    pub fn is_inherited(&self, field: &str) -> bool {
        self.inherited_fields.iter().any(|f| f == field)
    }

    /// 持久化视图始终使用磁盘模型，避免 API 字段泄漏进用户文件。
    pub fn to_storage(&self) -> serde_json::Value {
        serde_json::to_value(self.to_stored()).expect("recording is JSON serializable")
    }
}

/// recordings.json 的磁盘表示：**必须与 Python `Recording.to_dict()` 完全一致**。
///
/// 关键约束（曾因此造成数据不兼容）：
/// - 文件顶层是**数组**，不是 `{"recordings": [...]}`
/// - 字段是 **snake_case**，不是 API 面向前端的 camelCase
/// - 继承字段写 `null` 表示「跟随全局」
/// - 未知字段通过 `extra` 原样保留，避免重写时丢数据
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StoredRecording {
    #[serde(default)]
    pub rec_id: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub streamer_name: String,
    #[serde(default)]
    pub record_format: Option<String>,
    #[serde(default)]
    pub quality: Option<String>,
    #[serde(default)]
    pub segment_record: Option<bool>,
    #[serde(default)]
    pub segment_time: Option<String>,
    #[serde(default)]
    pub monitor_status: bool,
    #[serde(default)]
    pub scheduled_recording: Option<bool>,
    #[serde(default)]
    pub scheduled_start_time: Option<String>,
    #[serde(default, deserialize_with = "lenient_i32")]
    pub monitor_hours: Option<i32>,
    #[serde(default)]
    pub recording_dir: Option<String>,
    #[serde(default)]
    pub enabled_message_push: Option<bool>,
    #[serde(default)]
    pub only_notify_no_record: Option<bool>,
    #[serde(default)]
    pub flv_use_direct_download: Option<bool>,
    #[serde(default, deserialize_with = "lenient_i64")]
    pub video_bitrate: Option<i64>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub platform_key: Option<String>,
    #[serde(default, deserialize_with = "lenient_f64")]
    pub last_duration: Option<f64>,
    /// 保留 Python 侧可能存在而此处未建模的字段
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl Recording {
    /// 转成磁盘表示：跟随全局的字段写 null，不落运行期状态。
    pub fn to_stored(&self) -> StoredRecording {
        let inherited = |field: &str| self.is_inherited(field);
        StoredRecording {
            rec_id: self.rec_id.clone(),
            url: self.url.clone(),
            streamer_name: self.streamer_name.clone(),
            record_format: if inherited("record_format") {
                None
            } else {
                self.record_format.clone()
            },
            quality: if inherited("quality") {
                None
            } else {
                self.quality.clone()
            },
            segment_record: if inherited("segment_record") {
                None
            } else {
                self.segment_record
            },
            segment_time: if inherited("segment_time") {
                None
            } else {
                self.segment_time.clone()
            },
            monitor_status: self.monitor_status,
            scheduled_recording: self.scheduled_recording,
            scheduled_start_time: self.scheduled_start_time.clone(),
            monitor_hours: self.monitor_hours,
            recording_dir: self.recording_dir.clone(),
            enabled_message_push: self.enabled_message_push,
            only_notify_no_record: if inherited("only_notify_no_record") {
                None
            } else {
                self.only_notify_no_record
            },
            flv_use_direct_download: if inherited("flv_use_direct_download") {
                None
            } else {
                self.flv_use_direct_download
            },
            video_bitrate: self.video_bitrate,
            platform: self.platform.clone(),
            platform_key: self.platform_key.clone(),
            last_duration: self.last_duration,
            extra: self.extra.clone(),
        }
    }

    /// 从磁盘表示还原；null 字段标记为「跟随全局」。
    pub fn from_stored(stored: StoredRecording) -> Self {
        let mut rec = Recording::new(stored.rec_id, stored.url, stored.streamer_name);
        rec.record_format = stored.record_format;
        rec.quality = stored.quality;
        rec.segment_record = stored.segment_record;
        rec.segment_time = stored.segment_time;
        rec.monitor_status = stored.monitor_status;
        rec.scheduled_recording = stored.scheduled_recording;
        rec.scheduled_start_time = stored.scheduled_start_time;
        rec.monitor_hours = stored.monitor_hours;
        rec.recording_dir = stored.recording_dir;
        rec.enabled_message_push = stored.enabled_message_push;
        rec.only_notify_no_record = stored.only_notify_no_record;
        rec.flv_use_direct_download = stored.flv_use_direct_download;
        rec.video_bitrate = stored.video_bitrate;
        rec.platform = stored.platform;
        rec.platform_key = stored.platform_key;
        rec.last_duration = stored.last_duration;
        rec.extra = stored.extra;

        // 值为 null 的可继承字段即「跟随全局」
        for (field, _) in INHERITABLE_FIELDS {
            let is_null = match *field {
                "record_format" => rec.record_format.is_none(),
                "quality" => rec.quality.is_none(),
                "segment_record" => rec.segment_record.is_none(),
                "segment_time" => rec.segment_time.is_none(),
                "only_notify_no_record" => rec.only_notify_no_record.is_none(),
                "flv_use_direct_download" => rec.flv_use_direct_download.is_none(),
                _ => false,
            };
            if is_null {
                rec.inherited_fields.push((*field).to_string());
            }
        }

        if rec.display_title.is_none() {
            rec.update_title();
        }
        rec
    }
}
pub fn detect_platform(url: &str) -> Option<(String, String)> {
    const RULES: &[(&str, &str, &str)] = &[
        ("douyin.com", "抖音", "douyin"),
        ("tiktok.com", "TikTok", "tiktok"),
        ("kuaishou.com", "快手", "kuaishou"),
        ("huya.com", "虎牙", "huya"),
        ("douyu.com", "斗鱼", "douyu"),
        ("bilibili.com", "哔哩哔哩", "bilibili"),
        ("yy.com", "YY", "yy"),
        ("xiaohongshu.com", "小红书", "xhs"),
        ("twitch.tv", "Twitch", "twitch"),
        ("youtube.com", "YouTube", "youtube"),
        ("chzzk.naver.com", "CHZZK", "chzzk"),
        ("sooplive", "SOOP", "soop"),
        ("pandalive", "PandaLive", "pandalive"),
        ("flextv", "FlexTV", "flextv"),
        ("winktv", "WinkTV", "winktv"),
        ("popkontv", "PopkonTV", "popkontv"),
        ("showroom", "SHOWROOM", "showroom"),
        ("twitcasting", "TwitCasting", "twitcasting"),
    ];

    let lower = url.to_lowercase();
    RULES
        .iter()
        .find(|(needle, _, _)| lower.contains(needle))
        .map(|(_, name, key)| ((*name).to_string(), (*key).to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_follows_streamer_and_quality() {
        let mut rec = Recording::new("id".into(), "https://example.com".into(), "主播A".into());
        rec.quality = Some("OD".into());
        rec.update_title();
        assert_eq!(rec.title(), "主播A - OD");
    }

    #[test]
    fn inherited_fields_serialize_as_null() {
        let mut rec = Recording::new("id".into(), "u".into(), "n".into());
        rec.quality = Some("HD".into());
        rec.inherited_fields.push("quality".into());

        let stored = rec.to_storage();
        assert!(stored.get("quality").unwrap().is_null());
        assert!(!stored.as_object().unwrap().contains_key("isLive"));
    }

    #[test]
    fn detect_platform_matches_known_hosts() {
        let (name, key) = detect_platform("https://live.douyin.com/123").unwrap();
        assert_eq!(name, "抖音");
        assert_eq!(key, "douyin");
        assert!(detect_platform("https://unknown.example.com/x").is_none());
    }
}
