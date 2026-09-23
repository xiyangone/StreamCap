//! Native configuration, bounded validation and atomic persistence. Existing user preferences keep their keys.

use crate::paths::Workspace;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::io;
use std::path::Path;

pub const UNSUPPORTED_FLAGS: &[&str] = &["check_live_on_browser_refresh"];

pub fn native_defaults() -> Map<String, Value> {
    let mut values: Map<String, Value> =
        serde_json::from_str(include_str!("../../../config/default_settings.json"))
            .expect("validated bundled defaults");
    disable_unimplemented(&mut values);
    values.insert("convert_to_mp4".into(), Value::Bool(false));
    values.insert("close_action".into(), Value::String("ask".into()));
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
    write_atomic(path, &serde_json::to_vec_pretty(value)?)
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
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
        file.write_all(bytes)?;
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
            return Err(invalid("浏览器刷新不再触发检测，请使用手动检测或监控间隔"));
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
        return Err(invalid("不支持的录制格式"));
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
    if patch
        .get("close_action")
        .is_some_and(|v| !matches!(v.as_str(), Some("ask" | "exit" | "tray")))
    {
        return Err(invalid("关闭窗口行为必须为每次询问、退出或最小化到托盘"));
    }
    for (key, min, max) in [
        ("platform_max_concurrent_requests", 1.0, 16.0),
        ("platform_request_interval", 0.0, 300.0),
        ("smtp_port", 1.0, 65535.0),
    ] {
        if let Some(value) = patch.get(key) {
            let n = value
                .as_f64()
                .or_else(|| value.as_str().and_then(|s| s.parse().ok()));
            if !n.is_some_and(|n| n.is_finite() && n.fract() == 0.0 && n >= min && n <= max) {
                return Err(invalid("并发、请求间隔或端口超出有效范围"));
            }
        }
    }
    if patch
        .get("default_live_source")
        .is_some_and(|v| !matches!(v.as_str(), Some("FLV" | "HLS")))
    {
        return Err(invalid("直播源偏好必须为 FLV 或 HLS"));
    }
    if let Some(Value::String(value)) = patch.get("custom_script_command") {
        if !value.is_empty() {
            crate::automation::ScriptSpec::parse(value).map_err(|e| invalid(&e))?;
        }
    }
    if let Some(Value::String(value)) = patch.get("scheduled_shutdown_time") {
        if chrono::NaiveTime::parse_from_str(value, "%H:%M")
            .or_else(|_| chrono::NaiveTime::parse_from_str(value, "%H:%M:%S"))
            .is_err()
        {
            return Err(invalid("关机时间应为 HH:MM"));
        }
    }
    if let Some(value) = patch.get("language") {
        if !matches!(value.as_str(), Some("zh_CN" | "en")) {
            return Err(invalid("语言选项无效"));
        }
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
        let changed: Vec<String> = patch
            .iter()
            .filter(|(key, value)| self.user_config.get(*key) != Some(*value))
            .map(|(key, _)| key.clone())
            .collect();
        if changed.is_empty() {
            return Ok(changed);
        }
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
        if patch.values().any(|value| {
            !value.is_null()
                && (!value.is_string()
                    || value
                        .as_str()
                        .is_some_and(|text| text.len() > 65536 || text.contains(['\r', '\n'])))
        }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Cookie 必须是没有换行的文本，且不超过 64 KiB",
            ));
        }
        let mut cookies = self.load_cookies()?;
        let changed: Vec<String> = patch.keys().cloned().collect();
        for (key, value) in patch {
            let key = crate::platforms::catalog::canonical_key(&key).to_string();
            cookies.retain(|stored, _| crate::platforms::catalog::canonical_key(stored) != key);
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

    /// View old persisted aliases through one canonical platform key, without rewriting the file.
    pub fn cookie_view(&self) -> io::Result<Map<String, Value>> {
        let mut normalized = Map::new();
        for (key, value) in self.load_cookies()? {
            if !value.is_null()
                && (!value.is_string()
                    || value
                        .as_str()
                        .is_some_and(|text| text.contains(['\r', '\n'])))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Cookie 配置字段不是文本，未发送匿名请求",
                ));
            }
            let canonical = crate::platforms::catalog::canonical_key(&key).to_owned();
            if canonical == key {
                normalized.insert(canonical, value);
            } else {
                normalized.entry(canonical).or_insert(value);
            }
        }
        Ok(normalized)
    }
    pub fn cookies_for_resolver(&self) -> io::Result<HashMap<String, String>> {
        Ok(self
            .cookie_view()?
            .into_iter()
            .filter_map(|(key, value)| value.as_str().map(|value| (key, value.to_owned())))
            .collect())
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

    #[test]
    fn atomic_write_preserves_existing_target_and_removes_failed_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("settings.json");
        write_atomic(&target, b"first").unwrap();
        write_atomic(&target, b"second").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"second");
        let occupied = dir.path().join("occupied");
        fs::create_dir(&occupied).unwrap();
        fs::write(occupied.join("kept"), b"preserved").unwrap();
        assert!(write_atomic(&occupied, b"not published").is_err());
        assert_eq!(fs::read(occupied.join("kept")).unwrap(), b"preserved");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

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

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlatformAccount {
    pub username: String,
    pub password: String,
    #[serde(alias = "access_token")]
    pub access_token: String,
    #[serde(alias = "account_type")]
    pub account_type: String,
}
impl ConfigStore {
    pub fn account(&self, platform: &str) -> io::Result<Option<PlatformAccount>> {
        let values = read_json_object(&self.workspace.config_dir().join("accounts.json"))?;
        let canonical = crate::platforms::catalog::canonical_key(platform);
        let value = values.get(canonical).or_else(|| {
            values
                .iter()
                .find(|(key, _)| crate::platforms::catalog::canonical_key(key) == canonical)
                .map(|(_, value)| value)
        });
        value
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "账号配置字段无效"))
    }
    pub fn account_summaries(&self) -> io::Result<Value> {
        let values = read_json_object(&self.workspace.config_dir().join("accounts.json"))?;
        let mut out = Map::new();
        for (key, value) in values {
            let account = serde_json::from_value::<PlatformAccount>(value)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "账号配置字段无效"))?;
            let canonical = crate::platforms::catalog::canonical_key(&key).to_owned();
            let summary = serde_json::json!({"username":account.username,"hasPassword":!account.password.is_empty(),"hasAccessToken":!account.access_token.is_empty(),"accountType":account.account_type});
            if canonical == key {
                out.insert(canonical, summary);
            } else {
                out.entry(canonical).or_insert(summary);
            }
        }
        Ok(Value::Object(out))
    }
    pub fn save_account(&self, platform: &str, patch: &Map<String, Value>) -> io::Result<()> {
        if crate::platforms::catalog::by_key(platform).is_none() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "平台无效"));
        }
        if patch
            .keys()
            .any(|k| !["username", "password", "accessToken", "accountType"].contains(&k.as_str()))
            || patch.values().any(|v| {
                !v.is_string()
                    || v.as_str()
                        .is_some_and(|s| s.contains('\0') || s.len() > 16384)
            })
        {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "账号字段无效"));
        }
        let mut accounts = read_json_object(&self.workspace.config_dir().join("accounts.json"))?;
        let canonical = crate::platforms::catalog::canonical_key(platform);
        self.account(platform)?;
        let mut value = accounts
            .get(canonical)
            .or_else(|| {
                accounts
                    .iter()
                    .find(|(key, _)| crate::platforms::catalog::canonical_key(key) == canonical)
                    .map(|(_, value)| value)
            })
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let object = value
            .as_object_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "账号配置字段无效"))?;
        for (old, new) in [
            ("access_token", "accessToken"),
            ("account_type", "accountType"),
        ] {
            if let Some(value) = object.remove(old) {
                object.entry(new).or_insert(value);
            }
        }
        value
            .as_object_mut()
            .expect("account object")
            .extend(patch.clone());
        accounts.retain(|key, _| {
            crate::platforms::catalog::canonical_key(key)
                != crate::platforms::catalog::canonical_key(platform)
        });
        accounts.insert(
            crate::platforms::catalog::canonical_key(platform).to_string(),
            value,
        );
        write_json(
            &self.workspace.config_dir().join("accounts.json"),
            &Value::Object(accounts),
        )
    }
}
