//! Native configuration, bounded validation and atomic persistence. Existing user preferences keep their keys.

use crate::paths::Workspace;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::io;
use std::path::Path;

pub const UNSUPPORTED_FLAGS: &[&str] = &[
    "convert_to_mp4",
    "delete_original",
    "generate_time_subtitle_file",
    "execute_custom_script",
    "scheduled_shutdown_enabled",
    "system_notification_enabled",
    "stream_start_notification_enabled",
    "stream_end_notification_enabled",
    "only_notify_no_record",
    "flv_use_direct_download",
    "force_https_recording",
    "dingtalk_enabled",
    "wechat_enabled",
    "feishu_enabled",
    "bark_enabled",
    "ntfy_enabled",
    "serverchan_enabled",
    "telegram_enabled",
    "email_enabled",
    "check_live_on_browser_refresh",
];

pub fn native_defaults() -> Map<String, Value> {
    let mut values: Map<String, Value> =
        serde_json::from_str(include_str!("../../../config/default_settings.json"))
            .expect("validated bundled defaults");
    disable_unimplemented(&mut values);
    values
}
pub fn disable_unimplemented(values: &mut Map<String, Value>) {
    for key in UNSUPPORTED_FLAGS {
        values.insert((*key).into(), Value::Bool(false));
    }
}
pub fn read_json_object(path: &Path) -> io::Result<Map<String, Value>> {
    if !path.exists() {
        return Ok(Map::new());
    }
    let text = std::fs::read_to_string(path)?;
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "配置必须是 JSON 对象，已停止以保护数据",
        )),
        Err(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "配置 JSON 无效，已停止以保护数据",
        )),
    }
}
fn write_json(path: &Path, value: &Value) -> io::Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "配置路径缺少目录"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(serde_json::to_string_pretty(value)?.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
pub fn validate_settings(patch: &Map<String, Value>) -> io::Result<()> {
    let invalid = |message: &str| io::Error::new(io::ErrorKind::InvalidInput, message);
    for key in UNSUPPORTED_FLAGS {
        if patch.get(*key).is_some_and(|v| v != &Value::Bool(false)) {
            return Err(invalid("此功能尚未迁移到原生版，不能启用"));
        }
    }
    for (key, min, max) in [
        ("loop_time_seconds", 30_u64, 86400_u64),
        ("video_segment_time", 1, 86400),
    ] {
        if let Some(value) = patch.get(key) {
            let n = value.as_str().and_then(|s| s.parse::<u64>().ok());
            if !n.is_some_and(|n| n >= min && n <= max) {
                return Err(invalid("检测间隔或分段时长超出允许范围"));
            }
        }
    }
    if patch.get("recording_space_threshold").is_some_and(|v| {
        !v.as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .is_some_and(|v| v.is_finite() && v >= 0.1)
    }) {
        return Err(invalid("空间保护阈值至少为 0.1 GB"));
    }
    if patch.get("video_format").is_some_and(|v| {
        !v.as_str()
            .is_some_and(|s| crate::engine::SUPPORTED_RECORD_FORMATS.contains(&s))
    }) {
        return Err(invalid("此录制格式尚未迁移到原生版"));
    }
    if patch
        .get("record_quality")
        .is_some_and(|v| !matches!(v.as_str(), Some("OD" | "UHD" | "HD" | "SD" | "LD")))
    {
        return Err(invalid("清晰度无效"));
    }
    if patch
        .get("theme_mode")
        .is_some_and(|v| !matches!(v.as_str(), Some("light" | "dark" | "system")))
    {
        return Err(invalid("主题模式无效"));
    }
    let defaults = native_defaults();
    for (key, value) in patch {
        if let Some(default) = defaults.get(key) {
            if default.is_boolean() && !value.is_boolean()
                || default.is_string() && !value.is_string()
            {
                return Err(invalid("配置字段类型无效"));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct ConfigStore {
    workspace: Workspace,
    user_config: Map<String, Value>,
    default_config: Map<String, Value>,
}

impl ConfigStore {
    pub fn load(workspace: Workspace) -> io::Result<Self> {
        workspace.ensure_ready()?;
        let mut default_config = native_defaults();
        default_config.extend(read_json_object(&workspace.default_settings_path())?);
        disable_unimplemented(&mut default_config);
        let mut user_config = read_json_object(&workspace.user_settings_path())?;

        // 与 Python `init_user_config` 一致：首次运行（文件缺失或为空）时把默认设置复制为
        // 用户设置。否则设置页会读到空配置并渲染成空白表单。
        if user_config.is_empty() && !default_config.is_empty() {
            write_json(
                &workspace.user_settings_path(),
                &Value::Object(default_config.clone()),
            )?;
            user_config = default_config.clone();
        }

        Ok(Self {
            workspace,
            user_config,
            default_config,
        })
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// 读取设置：用户配置优先，其次默认配置。
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.user_config
            .get(key)
            .or_else(|| self.default_config.get(key))
    }

    pub fn get_str(&self, key: &str, fallback: &str) -> String {
        match self.get(key) {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => fallback.to_string(),
        }
    }

    pub fn get_i64(&self, key: &str, fallback: i64) -> i64 {
        match self.get(key) {
            Some(Value::Number(n)) => n.as_i64().unwrap_or(fallback),
            Some(Value::String(s)) => s.parse().unwrap_or(fallback),
            _ => fallback,
        }
    }

    pub fn get_bool(&self, key: &str, fallback: bool) -> bool {
        match self.get(key) {
            Some(Value::Bool(b)) => *b,
            _ => fallback,
        }
    }

    pub fn user_config(&self) -> &Map<String, Value> {
        &self.user_config
    }

    pub fn default_config(&self) -> &Map<String, Value> {
        &self.default_config
    }

    /// 合并写入用户设置并落盘。
    pub fn update_user_config(&mut self, patch: Map<String, Value>) -> io::Result<Vec<String>> {
        validate_settings(&patch)?;
        let changed: Vec<String> = patch.keys().cloned().collect();
        let mut updated = self.user_config.clone();
        updated.extend(patch);
        write_json(
            &self.workspace.user_settings_path(),
            &Value::Object(updated.clone()),
        )?;
        self.user_config = updated;
        Ok(changed)
    }

    pub fn load_cookies(&self) -> io::Result<Map<String, Value>> {
        read_json_object(&self.workspace.cookies_path())
    }

    /// 写入 Cookie：空值表示清除该平台。
    pub fn update_cookies(&mut self, patch: Map<String, Value>) -> io::Result<Vec<String>> {
        let mut cookies = self.load_cookies()?;
        let changed: Vec<String> = patch.keys().cloned().collect();
        for (key, value) in patch {
            let empty = matches!(&value, Value::Null)
                || matches!(&value, Value::String(s) if s.trim().is_empty());
            if empty {
                cookies.remove(&key);
            } else {
                cookies.insert(key, value);
            }
        }
        write_json(&self.workspace.cookies_path(), &Value::Object(cookies))?;
        Ok(changed)
    }

    /// 解析 provider 所需的 Cookie 表（平台键 -> cookie 串）。
    pub fn cookies_for_resolver(&self) -> HashMap<String, String> {
        self.load_cookies()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
            .collect()
    }

    /// 录制输出根目录：live_save_path 优先，否则用默认目录。
    pub fn recordings_root(&self) -> std::path::PathBuf {
        match self.get("live_save_path") {
            Some(Value::String(s)) if !s.trim().is_empty() => std::path::PathBuf::from(s),
            _ => self.workspace.default_recordings_dir(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_workspace() -> (Workspace, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::from_repo_root(dir.path());
        fs::create_dir_all(ws.config_dir()).unwrap();
        (ws, dir)
    }

    #[test]
    fn user_config_overrides_default() {
        let (ws, _guard) = temp_workspace();
        fs::write(
            ws.default_settings_path(),
            r#"{"video_format":"TS","loop_time_seconds":"600"}"#,
        )
        .unwrap();
        fs::write(ws.user_settings_path(), r#"{"video_format":"MP4"}"#).unwrap();

        let store = ConfigStore::load(ws).unwrap();
        assert_eq!(store.get_str("video_format", "X"), "MP4");
        assert_eq!(store.get_str("loop_time_seconds", "1"), "600");
    }

    #[test]
    fn update_user_config_persists_and_reloads() {
        let (ws, _guard) = temp_workspace();
        let mut store = ConfigStore::load(ws.clone()).unwrap();

        let mut patch = Map::new();
        patch.insert("video_format".into(), Value::String("MKV".into()));
        let changed = store.update_user_config(patch).unwrap();
        assert_eq!(changed, vec!["video_format".to_string()]);

        let reloaded = ConfigStore::load(ws).unwrap();
        assert_eq!(reloaded.get_str("video_format", "X"), "MKV");
    }

    #[test]
    fn cookies_empty_value_removes_key() {
        let (ws, _guard) = temp_workspace();
        fs::write(ws.cookies_path(), r#"{"douyin":"abc","kuaishou":"xyz"}"#).unwrap();
        let mut store = ConfigStore::load(ws).unwrap();

        let mut patch = Map::new();
        patch.insert("douyin".into(), Value::String(String::new()));
        store.update_cookies(patch).unwrap();

        let cookies = store.load_cookies().unwrap();
        assert!(!cookies.contains_key("douyin"));
        assert!(cookies.contains_key("kuaishou"));
    }

    #[test]
    fn recordings_root_falls_back_to_default_dir() {
        let (ws, _guard) = temp_workspace();
        let store = ConfigStore::load(ws.clone()).unwrap();
        assert_eq!(store.recordings_root(), ws.default_recordings_dir());
    }
}
