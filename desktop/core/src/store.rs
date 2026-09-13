//! 任务存储与事件广播：录制任务的内存态 + recordings.json 持久化 + SSE 事件源。

use crate::model::{detect_platform, Recording};
use crate::paths::Workspace;
use serde_json::Value;
use std::io::{self, Write};
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayEvent {
    pub topic: String,
    pub payload: Value,
}

#[derive(Clone)]
pub struct Store {
    inner: Arc<RwLock<Vec<Recording>>>,
    events: broadcast::Sender<GatewayEvent>,
    workspace: Workspace,
}

impl Store {
    pub fn new(workspace: Workspace) -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(RwLock::new(Vec::new())),
            events,
            workspace,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<GatewayEvent> {
        self.events.subscribe()
    }

    pub fn emit(&self, topic: &str, payload: Value) {
        // 无订阅者时 send 会失败，忽略即可（不是错误状态）
        let _ = self.events.send(GatewayEvent {
            topic: topic.to_string(),
            payload,
        });
    }

    pub fn snack(&self, text: impl Into<String>) {
        self.emit("snack", serde_json::json!({ "text": text.into() }));
    }

    /// 从 recordings.json 载入任务。
    ///
    /// 文件格式与 Python `save_recordings_config` 一致：**顶层数组 + snake_case**。
    /// 早期实现按 `{"recordings":[...]}` + camelCase 读取，会静默读到 0 条，
    /// 随后任何写盘都会用空数据覆盖用户的真实任务列表。
    pub async fn load(&self) -> io::Result<usize> {
        let path = self.workspace.recordings_path();
        if !path.exists() {
            return Ok(0);
        }

        let text = std::fs::read_to_string(&path)?;
        if text.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "recordings.json 为空，已中止载入以保护数据",
            ));
        }

        let items: Vec<crate::model::StoredRecording> = match serde_json::from_str(&text) {
            Ok(items) => items,
            Err(err) => {
                // 解析失败必须中止而不是当作空列表——否则随后的 persist 会清空用户数据
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("recordings.json 格式无法识别（已中止载入以保护数据）: {err}"),
                ));
            }
        };

        let loaded: Vec<Recording> = items
            .into_iter()
            .map(crate::model::Recording::from_stored)
            .collect();

        let count = loaded.len();
        *self.inner.write().await = loaded;
        log::info!("已载入 {count} 条录制任务");
        Ok(count)
    }

    /// 同目录临时文件 + 原子替换。写入失败不修改已有任务文件。
    fn persist_snapshot(&self, list: &[Recording]) -> io::Result<()> {
        let stored: Vec<_> = list.iter().map(Recording::to_stored).collect();
        let path = self.workspace.recordings_path();
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("任务文件缺少父目录"))?;
        std::fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".recordings-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(serde_json::to_string_pretty(&stored)?.as_bytes())?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, &path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }

    pub async fn persist(&self) -> io::Result<()> {
        let list = self.inner.read().await;
        self.persist_snapshot(&list)
    }

    pub async fn all(&self) -> Vec<Recording> {
        self.inner.read().await.clone()
    }

    pub async fn count(&self) -> (usize, usize) {
        let list = self.inner.read().await;
        (list.len(), list.iter().filter(|r| r.is_recording).count())
    }

    pub async fn get(&self, rec_id: &str) -> Option<Recording> {
        self.inner
            .read()
            .await
            .iter()
            .find(|r| r.rec_id == rec_id)
            .cloned()
    }

    /// Commit an inferred name before publishing it; never replace a user-supplied name.
    /// Runtime room/recording state stays out of the existing disk schema.
    pub async fn apply_stream_info(
        &self,
        rec_id: &str,
        info: &crate::resolver::StreamInfo,
    ) -> io::Result<Option<Recording>> {
        let mut list = self.inner.write().await;
        let Some(index) = list.iter().position(|rec| rec.rec_id == rec_id) else {
            return Ok(None);
        };
        let mut updated = list[index].clone();
        let anchor = info.anchor_name.trim();
        let name_changed = updated.streamer_name.trim().is_empty() && !anchor.is_empty();
        if name_changed {
            updated.streamer_name = anchor.to_string();
            updated.update_title();
        }
        updated.is_live = info.is_live;
        updated.live_title = (info.is_live && !info.title.is_empty()).then(|| info.title.clone());
        if !info.is_live {
            updated.recording_error = None;
        }
        if name_changed {
            let mut next = list.clone();
            next[index] = updated.clone();
            self.persist_snapshot(&next)?;
        }
        list[index] = updated.clone();
        drop(list);
        self.emit("update", serde_json::to_value(&updated)?);
        Ok(Some(updated))
    }

    pub async fn insert(&self, recordings: Vec<Recording>) -> io::Result<()> {
        let mut list = self.inner.write().await;
        let mut next = list.clone();
        for rec in &recordings {
            if next.iter().any(|old| old.url == rec.url) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "直播间地址已存在，未添加任何任务",
                ));
            }
            next.push(rec.clone());
        }
        self.persist_snapshot(&next)?;
        *list = next;
        drop(list);
        for rec in recordings {
            self.emit("update", serde_json::to_value(rec)?);
        }
        Ok(())
    }

    #[cfg(test)]
    pub async fn add_many(&self, recordings: Vec<Recording>) {
        self.insert(recordings)
            .await
            .expect("test recording insertion failed");
    }

    /// 在同一写锁内校验全部目标、写盘再发布，批量编辑不能部分落盘。
    pub async fn edit_many<F>(&self, ids: &[String], mutate: F) -> io::Result<Vec<Recording>>
    where
        F: Fn(&mut Recording),
    {
        if ids.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "请先选择任务"));
        }
        let mut list = self.inner.write().await;
        if ids
            .iter()
            .any(|id| !list.iter().any(|rec| &rec.rec_id == id))
        {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "选中的任务已不存在，请刷新列表",
            ));
        }
        if list
            .iter()
            .any(|rec| ids.contains(&rec.rec_id) && rec.is_recording)
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "请先停止录制，再编辑任务",
            ));
        }
        let mut next = list.clone();
        let mut edited = Vec::new();
        for rec in next.iter_mut().filter(|rec| ids.contains(&rec.rec_id)) {
            mutate(rec);
            edited.push(rec.clone());
        }
        self.persist_snapshot(&next)?;
        *list = next;
        drop(list);
        for rec in &edited {
            self.emit("update", serde_json::to_value(rec)?);
        }
        Ok(edited)
    }

    /// Update runtime state and broadcast only when the task still exists.
    pub async fn update<F>(&self, rec_id: &str, mutate: F) -> Option<Recording>
    where
        F: FnOnce(&mut Recording),
    {
        self.update_when(rec_id, |_| true, mutate).await
    }

    /// Old exit/sampling callbacks must not mutate a replacement recording attempt.
    pub(crate) async fn update_for_run<F>(
        &self,
        rec_id: &str,
        run_id: uuid::Uuid,
        mutate: F,
    ) -> Option<Recording>
    where
        F: FnOnce(&mut Recording),
    {
        self.update_when(rec_id, |rec| rec.recording_run == Some(run_id), mutate)
            .await
    }

    async fn update_when<P, F>(&self, rec_id: &str, predicate: P, mutate: F) -> Option<Recording>
    where
        P: FnOnce(&Recording) -> bool,
        F: FnOnce(&mut Recording),
    {
        let updated = {
            let mut list = self.inner.write().await;
            let slot = list.iter_mut().find(|r| r.rec_id == rec_id)?;
            if !predicate(slot) {
                return None;
            }
            mutate(slot);
            slot.clone()
        };
        self.emit(
            "update",
            serde_json::to_value(&updated).unwrap_or(Value::Null),
        );
        Some(updated)
    }

    pub async fn remove(&self, rec_ids: &[String]) -> io::Result<usize> {
        let mut list = self.inner.write().await;
        if list
            .iter()
            .any(|rec| rec_ids.contains(&rec.rec_id) && rec.is_recording)
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "请先停止录制，再删除任务",
            ));
        }
        let mut next = list.clone();
        next.retain(|rec| !rec_ids.contains(&rec.rec_id));
        let removed = list.len() - next.len();
        if removed > 0 {
            self.persist_snapshot(&next)?;
            *list = next;
            self.emit("delete", serde_json::json!(rec_ids));
        }
        Ok(removed)
    }
}

#[cfg(test)]
fn to_camel(snake: &str) -> String {
    let mut out = String::with_capacity(snake.len());
    let mut upper = false;
    for ch in snake.chars() {
        if ch == '_' {
            upper = true;
        } else if upper {
            out.push(ch.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// 只更新继承项，显式覆盖的任务值不受全局修改影响。
pub async fn apply_global_defaults(store: &Store, config: &crate::config::ConfigStore) {
    let mut list = store.inner.write().await;
    for rec in list.iter_mut() {
        apply_defaults(rec, config);
        store.emit("update", serde_json::to_value(&*rec).unwrap_or(Value::Null));
    }
}

pub fn apply_defaults(rec: &mut Recording, config: &crate::config::ConfigStore) {
    for (field, _) in crate::model::INHERITABLE_FIELDS {
        let missing = match *field {
            "quality" => rec.quality.is_none(),
            "record_format" => rec.record_format.is_none(),
            "segment_time" => rec.segment_time.is_none(),
            "segment_record" => rec.segment_record.is_none(),
            "flv_use_direct_download" => rec.flv_use_direct_download.is_none(),
            "only_notify_no_record" => rec.only_notify_no_record.is_none(),
            _ => false,
        };
        if missing && !rec.is_inherited(field) {
            rec.inherited_fields.push((*field).into());
        }
    }
    if rec.is_inherited("quality") {
        rec.quality = Some(config.get_str("record_quality", "OD"));
    }
    if rec.is_inherited("record_format") {
        rec.record_format = Some(config.get_str("video_format", "TS"));
    }
    if rec.is_inherited("segment_time") {
        rec.segment_time = Some(config.get_str("video_segment_time", "1800"));
    }
    if rec.is_inherited("segment_record") {
        rec.segment_record = Some(config.get_bool("segmented_recording_enabled", false));
    }
    if rec.is_inherited("flv_use_direct_download") {
        rec.flv_use_direct_download = Some(config.get_bool("flv_use_direct_download", false));
    }
    if rec.is_inherited("only_notify_no_record") {
        rec.only_notify_no_record = Some(config.get_bool("only_notify_no_record", false));
    }
    if let Some((name, key)) = detect_platform(&rec.url) {
        rec.platform = Some(name);
        rec.platform_key = Some(key);
    }
    rec.update_title();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Workspace;
    use serde_json::json;

    fn temp_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::from_repo_root(dir.path());
        ws.ensure_ready().unwrap();
        (Store::new(ws), dir)
    }

    #[tokio::test]
    async fn add_persist_and_reload_roundtrip() {
        let (store, _guard) = temp_store();
        let mut rec = Recording::new(
            "r1".into(),
            "https://live.douyin.com/1".into(),
            "主播".into(),
        );
        rec.quality = Some("OD".into());
        rec.update_title();
        store.add_many(vec![rec]).await;

        let reopened = Store::new(store.workspace.clone());
        let loaded = reopened.load().await.unwrap();
        assert_eq!(loaded, 1);
        assert_eq!(reopened.all().await[0].streamer_name, "主播");
    }

    #[tokio::test]
    async fn remove_emits_delete_event() {
        let (store, _guard) = temp_store();
        let mut rx = store.subscribe();
        store
            .add_many(vec![Recording::new("r1".into(), "u".into(), "n".into())])
            .await;

        while rx.try_recv().is_ok() {}

        let removed = store.remove(&["r1".to_string()]).await.unwrap();
        assert_eq!(removed, 1);
        let event = rx.try_recv().unwrap();
        assert_eq!(event.topic, "delete");
    }

    #[tokio::test]
    async fn update_broadcasts_changed_recording() {
        let (store, _guard) = temp_store();
        store
            .add_many(vec![Recording::new("r1".into(), "u".into(), "n".into())])
            .await;
        let mut rx = store.subscribe();

        let updated = store
            .update("r1", |rec| {
                rec.is_live = true;
                rec.monitor_status = false;
            })
            .await
            .unwrap();

        assert!(updated.is_live);
        let event = rx.try_recv().unwrap();
        assert_eq!(event.topic, "update");
        assert_eq!(event.payload.get("isLive").unwrap(), &Value::Bool(true));
    }

    #[test]
    fn camel_conversion_matches_frontend_contract() {
        assert_eq!(to_camel("record_format"), "recordFormat");
        assert_eq!(to_camel("only_notify_no_record"), "onlyNotifyNoRecord");
        assert_eq!(to_camel("quality"), "quality");
    }

    /// 真实 Python 产物的同构样本：顶层数组 + snake_case + null 表示跟随全局。
    const PYTHON_FORMAT_FIXTURE: &str = r#"[
      {
        "rec_id": "6f1c0a52-0000-4000-8000-000000000001",
        "url": "https://live.example.com/room/1",
        "streamer_name": "示例主播",
        "record_format": null,
        "quality": "OD",
        "segment_record": true,
        "segment_time": "1800",
        "monitor_status": true,
        "scheduled_recording": false,
        "scheduled_start_time": null,
        "monitor_hours": "5,",
        "recording_dir": "Z:/demo/downloads/Example/主播",
        "enabled_message_push": false,
        "platform": "示例平台",
        "platform_key": "example",
        "last_duration": 1234.5,
        "only_notify_no_record": null,
        "flv_use_direct_download": null,
        "video_bitrate": null,
        "unknown_future_field": "must-survive"
      }
    ]"#;

    #[tokio::test]
    async fn loads_python_produced_recordings_file() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::from_repo_root(dir.path());
        ws.ensure_ready().unwrap();
        std::fs::write(ws.recordings_path(), PYTHON_FORMAT_FIXTURE).unwrap();

        let store = Store::new(ws);
        let count = store.load().await.unwrap();
        assert_eq!(count, 1, "应按 Python 的顶层数组格式读到任务");

        let rec = &store.all().await[0];
        assert_eq!(rec.streamer_name, "示例主播");
        assert_eq!(rec.platform_key.as_deref(), Some("example"));
        assert_eq!(rec.quality.as_deref(), Some("OD"));
        assert_eq!(rec.last_duration, Some(1234.5));

        // null 字段应标记为「跟随全局」
        // 真实数据里 monitor_hours 是字符串 "5,"，必须被宽容解析
        assert_eq!(rec.monitor_hours, Some(5), "字符串形式的数值也要能解析");
        assert!(rec.is_inherited("record_format"));
        assert!(rec.is_inherited("only_notify_no_record"));
        assert!(!rec.is_inherited("quality"), "有值字段不应被标记为跟随全局");
    }

    #[tokio::test]
    async fn persist_round_trips_in_python_format() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::from_repo_root(dir.path());
        ws.ensure_ready().unwrap();
        std::fs::write(ws.recordings_path(), PYTHON_FORMAT_FIXTURE).unwrap();

        let store = Store::new(ws.clone());
        store.load().await.unwrap();
        store.persist().await.unwrap();

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(ws.recordings_path()).unwrap()).unwrap();
        let array = written
            .as_array()
            .expect("磁盘格式必须是顶层数组，而非 {recordings:[...]}");

        let first = &array[0];
        assert!(first.get("rec_id").is_some(), "磁盘字段必须是 snake_case");
        assert!(
            first.get("recId").is_none(),
            "不应把 camelCase 写进磁盘文件"
        );
        assert!(
            first["record_format"].is_null(),
            "跟随全局的字段应写回 null"
        );
        assert_eq!(first["quality"], json!("OD"));
        assert_eq!(
            first["unknown_future_field"],
            json!("must-survive"),
            "未建模字段必须原样保留"
        );

        // 再次载入应完全一致（幂等）
        let reopened = Store::new(ws);
        assert_eq!(reopened.load().await.unwrap(), 1);
    }

    /// 真实文件里出现过的全部类型特征（字符串数值、尾逗号、null 继承、未知字段）
    const REAL_WORLD_FIXTURE: &str = r#"[
      {
        "rec_id": "15936169-a582-44b6-a38e-4c3c3b4e1b0a",
        "url": "https://live.kuaishou.com/u/demo",
        "streamer_name": "伤心猫♡东东电竞",
        "record_format": null,
        "quality": null,
        "segment_record": null,
        "segment_time": null,
        "monitor_status": true,
        "scheduled_recording": false,
        "scheduled_start_time": ",",
        "monitor_hours": "5,",
        "recording_dir": "E:/StreamCap_dev_Win_x64/downloads/快手直播/demo",
        "enabled_message_push": true,
        "platform": "快手直播",
        "platform_key": "kuaishou",
        "last_duration": 1664.814659,
        "only_notify_no_record": null,
        "flv_use_direct_download": null,
        "video_bitrate": null
      },
      {
        "rec_id": "464da528-cbc3-48e4-99a2-c1605e19aaaa",
        "url": "https://live.douyin.com/734054053077",
        "streamer_name": "小杜.",
        "record_format": null,
        "quality": "OD",
        "segment_record": true,
        "segment_time": "1800",
        "monitor_status": false,
        "scheduled_recording": false,
        "scheduled_start_time": null,
        "monitor_hours": 3,
        "recording_dir": null,
        "enabled_message_push": false,
        "platform": "抖音直播",
        "platform_key": "douyin",
        "last_duration": 0.0,
        "only_notify_no_record": null,
        "flv_use_direct_download": null,
        "video_bitrate": "2000"
      }
    ]"#;

    #[tokio::test]
    async fn loads_real_world_shaped_file() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::from_repo_root(dir.path());
        ws.ensure_ready().unwrap();
        std::fs::write(ws.recordings_path(), REAL_WORLD_FIXTURE).unwrap();

        let store = Store::new(ws.clone());
        assert_eq!(store.load().await.unwrap(), 2);

        let all = store.all().await;
        assert_eq!(all[0].monitor_hours, Some(5), "字符串 \"5,\" 应解析为 5");
        assert_eq!(all[0].last_duration, Some(1664.814659));
        assert_eq!(all[1].monitor_hours, Some(3));
        assert_eq!(
            all[1].video_bitrate,
            Some(2000),
            "字符串形式的码率也要能解析"
        );

        // 往返后仍能被解析（不破坏真实文件）
        store.persist().await.unwrap();
        let reopened = Store::new(ws);
        assert_eq!(reopened.load().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn corrupt_file_aborts_load_instead_of_wiping_data() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::from_repo_root(dir.path());
        ws.ensure_ready().unwrap();
        std::fs::write(ws.recordings_path(), "{ this is not valid json").unwrap();

        let store = Store::new(ws.clone());
        assert!(
            store.load().await.is_err(),
            "格式损坏必须报错，不能静默当作空列表"
        );

        // 关键：解析失败后不得写盘覆盖原始文件
        assert!(std::fs::read_to_string(ws.recordings_path())
            .unwrap()
            .contains("not valid json"));
    }
}

#[cfg(test)]
mod resolved_state_tests {
    use super::*;
    use crate::resolver::StreamInfo;

    async fn fixture(name: &str) -> (Store, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::from_repo_root(directory.path());
        workspace.ensure_ready().unwrap();
        let store = Store::new(workspace);
        let mut record = Recording::new(
            "name".into(),
            "https://media.invalid/live.flv".into(),
            name.into(),
        );
        record
            .extra
            .insert("preserved".into(), serde_json::json!({"value":7}));
        store.insert(vec![record]).await.unwrap();
        (store, directory)
    }

    fn resolved(live: bool, name: &str) -> StreamInfo {
        StreamInfo {
            is_live: live,
            anchor_name: name.into(),
            title: "直播标题不是主播名".into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn names_are_saved_while_offline_and_runtime_fields_never_enter_the_disk_schema() {
        let (store, _directory) = fixture("  ").await;
        store
            .update("name", |r| {
                r.recording_error = Some("旧错误".into());
                r.recording_run = Some(uuid::Uuid::new_v4());
            })
            .await;
        let updated = store
            .apply_stream_info("name", &resolved(false, "  自动主播  "))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.streamer_name, "自动主播");
        assert!(!updated.is_live);
        assert!(updated.live_title.is_none());
        assert!(updated.recording_error.is_none());
        assert!(updated.display_title.unwrap().starts_with("自动主播"));
        let raw: Value =
            serde_json::from_slice(&std::fs::read(store.workspace.recordings_path()).unwrap())
                .unwrap();
        let object = raw[0].as_object().unwrap();
        for key in [
            "recordingError",
            "recording_error",
            "recordingRun",
            "recording_run",
            "isLive",
            "is_live",
            "liveTitle",
        ] {
            assert!(!object.contains_key(key), "runtime field leaked: {key}");
        }
        assert_eq!(raw[0]["preserved"]["value"], 7);
        let reopened = Store::new(store.workspace.clone());
        reopened.load().await.unwrap();
        assert_eq!(
            reopened.get("name").await.unwrap().streamer_name,
            "自动主播"
        );
    }

    #[tokio::test]
    async fn manual_names_and_missing_anchor_metadata_never_trigger_a_disk_rewrite() {
        for (original, anchor) in [
            ("我的手动备注", "平台新昵称"),
            ("未命名直播间", "平台主播"),
            ("", "  "),
        ] {
            let (store, _directory) = fixture(original).await;
            let before = std::fs::read(store.workspace.recordings_path()).unwrap();
            let updated = store
                .apply_stream_info("name", &resolved(true, anchor))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(updated.streamer_name, original);
            assert_eq!(
                std::fs::read(store.workspace.recordings_path()).unwrap(),
                before
            );
            assert!(updated.is_live);
            assert_eq!(updated.live_title.as_deref(), Some("直播标题不是主播名"));
        }
    }

    #[tokio::test]
    async fn failed_name_commit_does_not_publish_or_mutate_the_original_task() {
        let (store, directory) = fixture("").await;
        let original = store.get("name").await.unwrap();
        let backup = directory.path().join("fixture-original.json");
        std::fs::rename(store.workspace.recordings_path(), &backup).unwrap();
        let bytes = std::fs::read(&backup).unwrap();
        std::fs::create_dir(store.workspace.recordings_path()).unwrap();
        let mut events = store.subscribe();
        assert!(store
            .apply_stream_info("name", &resolved(true, "自动主播"))
            .await
            .is_err());
        assert_eq!(store.get("name").await.unwrap(), original);
        assert_eq!(std::fs::read(backup).unwrap(), bytes);
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn late_callbacks_cannot_change_a_new_recording_attempt() {
        let (store, _directory) = fixture("保留名字").await;
        let old = uuid::Uuid::new_v4();
        let current = uuid::Uuid::new_v4();
        store
            .update("name", |r| {
                r.recording_run = Some(current);
                r.is_live = true;
                r.is_recording = true;
            })
            .await;
        let mut events = store.subscribe();
        assert!(store
            .update_for_run("name", old, |r| {
                r.is_recording = false;
                r.recording_error = Some("旧进程错误".into());
            })
            .await
            .is_none());
        assert!(events.try_recv().is_err());
        let state = store.get("name").await.unwrap();
        assert!(state.is_recording && state.recording_error.is_none());
        assert!(store
            .update_for_run("name", current, |r| {
                r.is_recording = false;
                r.recording_run = None;
                r.recording_error = Some("本次录制失败".into());
            })
            .await
            .is_some());
        let event = events.try_recv().unwrap();
        assert_eq!(event.payload["recordingError"], "本次录制失败");
        assert!(event.payload.get("recordingRun").is_none());
        assert!(store.get("name").await.unwrap().is_live);
    }
}
