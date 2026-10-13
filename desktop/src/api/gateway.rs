//! Typed client for the Rust desktop API. UI tests intercept transport, never user data.
use gloo_net::http::{Request, Response};
use leptos::prelude::*;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use wasm_bindgen::prelude::*;

/// The native shell supplies the actual bound address before WASM starts.
pub fn gateway_base() -> String {
    let value = js_sys::Reflect::get(&window(), &JsValue::from_str("__STREAMCAP_RUNTIME__"))
        .ok()
        .and_then(|runtime| js_sys::Reflect::get(&runtime, &JsValue::from_str("apiOrigin")).ok())
        .and_then(|value| value.as_string())
        .unwrap_or_default();
    if let Ok(url) = web_sys::Url::new(&value) {
        if url.protocol() == "http:"
            && url.hostname() == "127.0.0.1"
            && url.port().parse::<u16>().is_ok_and(|p| p > 0)
        {
            return url.origin();
        }
    }
    String::new()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Recording {
    pub rec_id: String,
    pub url: String,
    pub streamer_name: String,
    pub platform: Option<String>,
    pub platform_key: Option<String>,
    pub record_format: Option<String>,
    pub quality: Option<String>,
    pub segment_record: Option<bool>,
    pub segment_time: Option<String>,
    #[serde(default)]
    pub monitor_status: bool,
    pub scheduled_recording: Option<bool>,
    pub scheduled_start_time: Option<String>,
    pub monitor_hours: Option<String>,
    pub enabled_message_push: Option<bool>,
    pub only_notify_no_record: Option<bool>,
    pub flv_use_direct_download: Option<bool>,
    pub video_bitrate: Option<i64>,
    #[serde(default)]
    pub is_live: bool,
    #[serde(default)]
    pub is_recording: bool,
    #[serde(default)]
    pub recorded_seconds: f64,
    #[serde(default)]
    pub recording_error: Option<String>,
    #[serde(default)]
    pub check_error: Option<String>,
    #[serde(default)]
    pub verification_required: bool,
    #[serde(default)]
    pub access_state: String,
    #[serde(default)]
    pub check_state: String,
    #[serde(default)]
    pub last_check_at: Option<i64>,
    #[serde(default)]
    pub last_success_at: Option<i64>,
    #[serde(default)]
    pub next_check_at: Option<i64>,
    #[serde(default)]
    pub queue_position: Option<usize>,
    #[serde(default)]
    pub check_wait_reason: String,
    #[serde(default)]
    pub check_ready_at: Option<i64>,
    #[serde(default)]
    pub recovery_at: Option<i64>,
    #[serde(default)]
    pub recovery_attempt: u32,
    pub live_title: Option<String>,
    pub speed: Option<String>,
    pub recording_dir: Option<String>,
    #[serde(default)]
    pub inherited_fields: Vec<String>,
}

impl Recording {
    pub fn task_phase(&self) -> TaskPhase {
        if self.is_recording {
            TaskPhase::Recording
        } else if self.needs_attention()
            || matches!(self.check_state.as_str(), "rechecking" | "recovering")
        {
            TaskPhase::Attention
        } else if self.is_live {
            TaskPhase::LiveIdle
        } else if self.monitor_status {
            TaskPhase::Waiting
        } else {
            TaskPhase::Paused
        }
    }
    pub fn needs_attention(&self) -> bool {
        !self.is_recording
            && (self.verification_required
                || self
                    .check_error
                    .as_ref()
                    .is_some_and(|error| !error.is_empty())
                || self
                    .recording_error
                    .as_ref()
                    .is_some_and(|error| !error.is_empty()))
    }
    pub fn name(&self) -> String {
        if self.streamer_name.trim().is_empty() {
            crate::app::i18n::t("未命名直播间").into()
        } else {
            self.streamer_name.clone()
        }
    }
    pub fn inherits(&self, field: &str) -> bool {
        self.inherited_fields.iter().any(|f| f == field)
    }
    pub fn status(&self) -> StatusKind {
        if self.is_recording {
            StatusKind::Recording
        } else if self.check_state == "rechecking" {
            StatusKind::Rechecking
        } else if self.check_wait_reason == "cooldown" && self.monitor_status {
            StatusKind::Cooldown
        } else if self.check_state == "recovering"
            && self.monitor_status
            && !self.verification_required
        {
            StatusKind::Recovering
        } else if self
            .recording_error
            .as_ref()
            .is_some_and(|error| !error.is_empty())
        {
            StatusKind::RecordingFailed
        } else if self.check_state == "checking" {
            StatusKind::Checking
        } else if self.verification_required {
            StatusKind::Verification
        } else if self.access_state == "cooldown" {
            StatusKind::Cooldown
        } else if self.access_state == "unavailable" {
            StatusKind::AccessRestricted
        } else if self.access_state == "loginRequired" {
            StatusKind::LoginRequired
        } else if self.access_state == "loginPrompt" {
            StatusKind::LoginPrompt
        } else if self
            .check_error
            .as_ref()
            .is_some_and(|error| !error.is_empty())
        {
            StatusKind::CheckFailed
        } else if self.check_state == "queued" && self.monitor_status {
            StatusKind::Queued
        } else if self.is_live {
            StatusKind::Live
        } else if self.check_state == "waiting" && self.monitor_status {
            StatusKind::Waiting
        } else if self.monitor_status {
            StatusKind::Monitoring
        } else {
            StatusKind::Stopped
        }
    }
}

/// A task belongs to exactly one phase. Monitoring intent is not a recording phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskPhase {
    Recording,
    Attention,
    LiveIdle,
    Waiting,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Cooldown,
    AccessRestricted,
    LoginRequired,
    LoginPrompt,
    Waiting,
    Queued,
    Checking,
    Rechecking,
    Recovering,
    RecordingFailed,
    Verification,
    CheckFailed,
    Recording,
    Live,
    Monitoring,
    Stopped,
}
impl StatusKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cooldown => crate::app::i18n::t("平台冷却中"),
            Self::AccessRestricted => crate::app::i18n::t("访问受限"),
            Self::LoginRequired => crate::app::i18n::t("需要登录"),
            Self::LoginPrompt => crate::app::i18n::t("登录提示"),
            Self::Waiting => crate::app::i18n::t("等待首检"),
            Self::Queued => crate::app::i18n::t("排队中"),
            Self::Checking => crate::app::i18n::t("检测中"),
            Self::Rechecking => crate::app::i18n::t("录制结束，复核中"),
            Self::Recovering => crate::app::i18n::t("等待恢复录制"),
            Self::RecordingFailed => crate::app::i18n::t("录制异常"),
            Self::Verification => crate::app::i18n::t("验证受限"),
            Self::CheckFailed => crate::app::i18n::t("检测异常"),
            Self::Recording => crate::app::i18n::t("录制中"),
            Self::Live => crate::app::i18n::t("直播未录制"),
            Self::Monitoring => crate::app::i18n::t("等待开播"),
            Self::Stopped => crate::app::i18n::t("已暂停"),
        }
    }
    pub fn class(self) -> &'static str {
        match self {
            Self::Verification
            | Self::CheckFailed
            | Self::RecordingFailed
            | Self::Cooldown
            | Self::LoginRequired
            | Self::LoginPrompt
            | Self::AccessRestricted => "badge attention",
            Self::Recording => "badge recording",
            Self::Live => "badge live",
            Self::Monitoring
            | Self::Waiting
            | Self::Queued
            | Self::Checking
            | Self::Rechecking
            | Self::Recovering => "badge monitoring",
            Self::Stopped => "badge stopped",
        }
    }
}

#[cfg(test)]
mod detection_status_tests {
    use super::*;
    #[test]
    fn recording_live_error_and_monitoring_phases_never_overlap() {
        for live in [false, true] {
            for monitor in [false, true] {
                let mut record = Recording {
                    is_live: live,
                    monitor_status: monitor,
                    is_recording: true,
                    ..Default::default()
                };
                assert_eq!(record.task_phase(), TaskPhase::Recording);
                assert_eq!(record.status(), StatusKind::Recording);
                record.is_recording = false;
                record.recording_error = Some("encoder failed".into());
                assert_eq!(record.task_phase(), TaskPhase::Attention);
                assert_eq!(record.status(), StatusKind::RecordingFailed);
                record.recording_error = None;
                assert_eq!(
                    record.task_phase(),
                    if live {
                        TaskPhase::LiveIdle
                    } else if monitor {
                        TaskPhase::Waiting
                    } else {
                        TaskPhase::Paused
                    }
                );
                record.check_error = Some("empty response".into());
                assert_eq!(record.task_phase(), TaskPhase::Attention);
                assert_eq!(record.status(), StatusKind::CheckFailed);
                record.check_state = "rechecking".into();
                assert_eq!(record.task_phase(), TaskPhase::Attention);
                assert_eq!(record.status(), StatusKind::Rechecking);
            }
        }
    }
    #[test]
    fn page_rate_limit_login_and_captcha_have_distinct_statuses() {
        for (access, expected) in [
            ("unavailable", StatusKind::AccessRestricted),
            ("cooldown", StatusKind::Cooldown),
            ("loginRequired", StatusKind::LoginRequired),
            ("loginPrompt", StatusKind::LoginPrompt),
        ] {
            let record = Recording {
                access_state: access.into(),
                check_error: Some("unreadable".into()),
                ..Default::default()
            };
            assert_eq!(record.status(), expected);
            assert!(!record.verification_required);
        }
    }
    #[test]
    fn active_check_is_visible_over_previous_errors_but_not_an_active_recording() {
        let mut record = Recording {
            check_state: "checking".into(),
            check_error: Some("previous failure".into()),
            verification_required: true,
            ..Default::default()
        };
        assert_eq!(record.status(), StatusKind::Checking);
        record.check_state = "idle".into();
        assert_eq!(record.status(), StatusKind::Verification);
        record.verification_required = false;
        assert_eq!(record.status(), StatusKind::CheckFailed);
        record.check_state = "checking".into();
        record.is_recording = true;
        assert_eq!(record.status(), StatusKind::Recording);
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    pub ok: bool,
    pub version: String,
    #[serde(default)]
    pub build_id: String,
    pub active_recordings: usize,
    pub total_recordings: usize,
    #[serde(default)]
    pub resolver_ready: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StorageItem {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    #[serde(default = "known_size")]
    pub size_known: bool,
    #[serde(default)]
    pub size_error: Option<String>,
    pub path: String,
    pub modified: Option<f64>,
}
fn known_size() -> bool {
    true
}
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageListing {
    pub root: String,
    pub items: Vec<StorageItem>,
    pub total_size: u64,
    #[serde(default = "known_size")]
    pub total_size_known: bool,
}
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPayload {
    pub user_config: Map<String, Value>,
    pub default_config: Map<String, Value>,
}
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ReleaseInfo {
    pub tag_name: String,
    pub html_url: String,
    pub body: Option<String>,
}
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheck {
    pub release: ReleaseInfo,
    pub current_version: String,
}
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountSummary {
    pub username: String,
    pub account_type: String,
    pub has_password: bool,
    pub has_access_token: bool,
}
#[derive(Debug, Clone, Deserialize)]
pub struct RecordingFile {
    pub name: String,
    pub size: u64,
    pub path: String,
    #[serde(default)]
    pub modified: f64,
}
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RecordingFiles {
    #[serde(default)]
    pub files: Vec<RecordingFile>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QrSnapshot {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub image_base64: String,
    #[serde(default)]
    pub seconds_left: i64,
    pub cookies: Option<String>,
}
impl QrSnapshot {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.state.as_str(),
            "success" | "error" | "expired" | "cancelled"
        )
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum NoticeKind {
    Success,
    Error,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub message: String,
    pub kind: NoticeKind,
}
#[derive(Debug, Deserialize)]
struct BackendNotice {
    text: String,
    kind: NoticeKind,
}

#[cfg(test)]
mod response_contract_tests {
    use super::*;

    #[test]
    fn native_update_accounts_and_notices_use_the_shared_contract() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../tests/api-contracts.json")).unwrap();
        let update: UpdateCheck = serde_json::from_value(fixture["updateCheck"].clone()).unwrap();
        assert_eq!(update.release.tag_name, "v0.1.1-fixture");
        assert_eq!(update.current_version, "0.1.0");
        assert!(
            serde_json::from_value::<UpdateCheck>(fixture["updateCheck"]["release"].clone())
                .is_err()
        );
        let accounts: HashMap<String, AccountSummary> =
            serde_json::from_value(fixture["accounts"].clone()).unwrap();
        assert_eq!(accounts["douyin"].username, "fixture-account");
        assert!(accounts["douyin"].has_password && accounts["douyin"].has_access_token);
        assert!(serde_json::from_value::<HashMap<String, AccountSummary>>(
            json!({"accounts":fixture["accounts"]})
        )
        .is_err());
        let error: BackendNotice =
            serde_json::from_value(fixture["backgroundError"].clone()).unwrap();
        let success: BackendNotice =
            serde_json::from_value(fixture["backgroundSuccess"].clone()).unwrap();
        assert_eq!(error.kind, NoticeKind::Error);
        assert_eq!(success.kind, NoticeKind::Success);
    }
}

/// Keep only the latest delta for each entity while a snapshot is in flight.
/// A newer request supersedes older responses without starving under frequent SSE updates.
#[derive(Debug)]
struct SnapshotMerge<T> {
    generation: u64,
    pending: bool,
    loaded: bool,
    error: Option<String>,
    changes: Vec<(String, Option<T>)>,
}
impl<T> Default for SnapshotMerge<T> {
    fn default() -> Self {
        Self {
            generation: 0,
            pending: false,
            loaded: false,
            error: None,
            changes: Vec::new(),
        }
    }
}
impl<T> SnapshotMerge<T> {
    fn begin(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.pending = true;
        self.changes.clear();
        self.generation
    }
    fn observe(&mut self, id: String, value: Option<T>) {
        if !self.pending {
            return;
        }
        if let Some((_, current)) = self.changes.iter_mut().find(|(key, _)| key == &id) {
            *current = value;
        } else {
            self.changes.push((id, value));
        }
    }
    fn needs_refresh(&self) -> bool {
        !self.loaded || self.error.is_some()
    }
    fn fail(&mut self, generation: u64, error: String) -> bool {
        if generation != self.generation || !self.pending {
            return false;
        }
        self.pending = false;
        self.error = Some(error);
        self.changes.clear();
        true
    }
    fn finish(
        &mut self,
        generation: u64,
        mut list: Vec<T>,
        key: impl Fn(&T) -> &str,
    ) -> Option<Vec<T>> {
        if generation != self.generation || !self.pending {
            return None;
        }
        self.pending = false;
        self.loaded = true;
        self.error = None;
        for (id, value) in self.changes.drain(..) {
            match value {
                None => list.retain(|item| key(item) != id),
                Some(value) => {
                    if let Some(item) = list.iter_mut().find(|item| key(item) == id) {
                        *item = value;
                    } else {
                        list.push(value);
                    }
                }
            }
        }
        Some(list)
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::SnapshotMerge;
    type Item = (String, u32);
    fn item(id: &str, value: u32) -> Item {
        (id.into(), value)
    }
    fn key(item: &Item) -> &str {
        &item.0
    }

    #[test]
    fn delayed_snapshot_cannot_restore_deleted_entities_or_old_values() {
        let mut sync = SnapshotMerge::default();
        let generation = sync.begin();
        sync.observe("removed".into(), None);
        sync.observe("updated".into(), Some(item("updated", 2)));
        sync.observe("added".into(), Some(item("added", 3)));
        assert_eq!(
            sync.finish(
                generation,
                vec![item("removed", 0), item("updated", 0), item("untouched", 1)],
                key
            )
            .unwrap(),
            vec![item("updated", 2), item("untouched", 1), item("added", 3)]
        );
        assert!(!sync.pending && sync.changes.is_empty());
    }
    #[test]
    fn frequent_events_are_coalesced_without_starving_initial_snapshot() {
        let mut sync = SnapshotMerge::default();
        let generation = sync.begin();
        for value in 0..10_000 {
            sync.observe("progress".into(), Some(item("progress", value)));
        }
        assert_eq!(sync.changes.len(), 1);
        assert_eq!(
            sync.finish(generation, vec![item("untouched", 1)], key)
                .unwrap(),
            vec![item("untouched", 1), item("progress", 9_999)]
        );
    }
    #[test]
    fn older_requests_and_errors_cannot_overwrite_a_newer_refresh() {
        let mut sync = SnapshotMerge::default();
        let old = sync.begin();
        let current = sync.begin();
        sync.observe("removed".into(), None);
        assert!(!sync.fail(old, "old error".into()));
        assert!(sync.finish(old, vec![item("removed", 0)], key).is_none());
        assert_eq!(
            sync.finish(current, vec![item("removed", 0), item("kept", 1)], key)
                .unwrap(),
            vec![item("kept", 1)]
        );
    }
    #[test]
    fn completed_or_failed_requests_do_not_keep_an_event_backlog() {
        let mut sync = SnapshotMerge::default();
        let generation = sync.begin();
        sync.observe("job".into(), Some(item("job", 1)));
        assert!(sync.fail(generation, "request failed".into()));
        sync.observe("job".into(), Some(item("job", 2)));
        assert!(sync.changes.is_empty());
        assert!(sync.finish(generation, vec![], key).is_none());
    }
    #[test]
    fn superseded_startup_responses_never_complete_initialization() {
        for old_succeeds in [false, true] {
            for newest_finishes_first in [false, true] {
                let mut sync = SnapshotMerge::default();
                let old = sync.begin();
                let current = sync.begin();
                if newest_finishes_first {
                    assert!(sync.fail(current, "current failure".into()));
                }
                if old_succeeds {
                    assert!(sync.finish(old, vec![item("old", 0)], key).is_none());
                } else {
                    assert!(!sync.fail(old, "old failure".into()));
                }
                if !newest_finishes_first {
                    assert!(sync.fail(current, "current failure".into()));
                }
                assert!(!sync.loaded && !sync.pending && sync.needs_refresh());
                assert_eq!(sync.error.as_deref(), Some("current failure"));
                let retry = sync.begin();
                assert!(sync.pending && sync.needs_refresh());
                assert_eq!(sync.error.as_deref(), Some("current failure"));
                assert_eq!(sync.finish(retry, vec![], key), Some(vec![]));
                assert!(sync.loaded && !sync.needs_refresh());
                assert!(sync.error.is_none());
            }
        }
    }
    #[test]
    fn failed_refresh_preserves_loaded_data_and_retries_until_current_success() {
        let mut sync = SnapshotMerge::default();
        let initial = sync.begin();
        assert!(sync.finish(initial, vec![item("kept", 1)], key).is_some());
        let failed = sync.begin();
        assert!(sync.fail(failed, "refresh failed".into()));
        assert!(sync.loaded && sync.needs_refresh());
        let recovery = sync.begin();
        assert_eq!(sync.error.as_deref(), Some("refresh failed"));
        assert!(sync.finish(recovery, vec![item("kept", 2)], key).is_some());
        assert!(!sync.fail(failed, "late failure".into()));
        assert!(sync.loaded && !sync.pending && !sync.needs_refresh());
        assert!(sync.error.is_none());
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AppState {
    pub status: RwSignal<GatewayStatus>,
    pub recordings: RwSignal<Vec<Recording>>,
    pub recording_list: Memo<Vec<RecordingListEntry>>,
    recordings_index: Memo<HashMap<String, usize>>,
    pub media_jobs: RwSignal<Vec<MediaJob>>,
    recordings_snapshot: StoredValue<SnapshotMerge<Recording>>,
    jobs_snapshot: StoredValue<SnapshotMerge<MediaJob>>,
    pub settings: RwSignal<Map<String, Value>>,
    pub settings_version: RwSignal<u32>,
    pub settings_loading: RwSignal<bool>,
    pub settings_error: RwSignal<Option<String>>,
    settings_sync: StoredValue<SettingsSync>,
    appearance_writes: StoredValue<AppearanceWrites>,
    grid_view_writes: StoredValue<GridViewWrites>,
    pub notice: RwSignal<Option<Notice>>,
    pub loading: RwSignal<bool>,
    pub error: RwSignal<Option<String>>,
    pub events_connected: RwSignal<bool>,
    pub events_revision: RwSignal<u32>,
    pub theme: RwSignal<String>,
    pub grid_view: RwSignal<bool>,
    pub grid_view_saving: RwSignal<bool>,
    pub now: RwSignal<i64>,
}
impl AppState {
    pub fn recording(self, id: &str) -> Option<Recording> {
        let index = self.recordings_index.with(|index| index.get(id).copied())?;
        self.recordings
            .with(|items| items.get(index).filter(|r| r.rec_id == id).cloned())
    }
    pub fn recording_untracked(self, id: &str) -> Option<Recording> {
        let index = self
            .recordings_index
            .with_untracked(|index| index.get(id).copied())?;
        self.recordings
            .with_untracked(|items| items.get(index).filter(|r| r.rec_id == id).cloned())
    }
    fn snapshots_need_refresh(self) -> bool {
        self.recordings_snapshot
            .with_value(SnapshotMerge::needs_refresh)
            || self.jobs_snapshot.with_value(SnapshotMerge::needs_refresh)
    }
    fn snapshot_pending(self) -> bool {
        self.recordings_snapshot.with_value(|sync| sync.pending)
            || self.jobs_snapshot.with_value(|sync| sync.pending)
    }
    fn snapshot_error(self) -> Option<String> {
        self.recordings_snapshot
            .with_value(|sync| sync.error.clone())
            .or_else(|| self.jobs_snapshot.with_value(|sync| sync.error.clone()))
    }
    fn publish_snapshot_result(self) {
        if self
            .recordings_snapshot
            .with_value(|sync| sync.loaded || sync.error.is_some())
        {
            self.loading.set(false);
        }
        let error = self.snapshot_error();
        if error.is_some() || self.status.get_untracked().ok {
            self.error.set(error);
        }
    }
    fn recount_recordings(self) {
        let (total, active) = self.recordings.with_untracked(|list| {
            (
                list.len(),
                list.iter().filter(|record| record.is_recording).count(),
            )
        });
        if self.status.with_untracked(|status| {
            status.total_recordings != total || status.active_recordings != active
        }) {
            self.status.update(|status| {
                status.total_recordings = total;
                status.active_recordings = active;
            });
        }
    }
    pub fn is_dark(self) -> bool {
        self.theme.get() == "dark"
    }
    pub fn notify(self, message: impl Into<String>) {
        self.publish_notice(Notice {
            message: message.into(),
            kind: NoticeKind::Success,
        });
    }
    pub fn fail(self, message: impl Into<String>) {
        self.publish_notice(Notice {
            message: message.into(),
            kind: NoticeKind::Error,
        });
    }
    fn publish_notice(self, notice: Notice) {
        if !notice.message.trim().is_empty()
            && self
                .notice
                .with_untracked(|current| current.as_ref() != Some(&notice))
        {
            self.notice.set(Some(notice));
        }
    }
    fn background_notice(self, notice: BackendNotice) {
        if notice.kind == NoticeKind::Success
            && self.notice.with_untracked(|current| {
                current
                    .as_ref()
                    .is_some_and(|n| n.kind == NoticeKind::Error)
            })
        {
            return;
        }
        self.publish_notice(Notice {
            message: crate::app::i18n::message(notice.text),
            kind: notice.kind,
        });
    }
    pub fn setting(self, key: &str, fallback: &str) -> String {
        self.settings.with(|map| {
            map.get(key)
                .and_then(Value::as_str)
                .unwrap_or(fallback)
                .to_string()
        })
    }
    fn publish_grid_view(self) {
        let Some(grid) = self
            .grid_view_writes
            .try_update_value(|writes| writes.visible())
        else {
            return;
        };
        self.grid_view.set(grid);
        if self.settings.with_untracked(|settings| {
            !settings.is_empty()
                && settings.get("is_grid_view").and_then(Value::as_bool) != Some(grid)
        }) {
            self.settings.update(|settings| {
                settings.insert("is_grid_view".into(), Value::Bool(grid));
            });
            self.settings_version
                .update(|version| *version = version.wrapping_add(1));
        }
    }
    fn apply_settings(self, payload: SettingsPayload) {
        let mut settings = payload.default_config;
        settings.extend(payload.user_config);
        let desired = self
            .appearance_writes
            .with_value(|writes| writes.desired.clone());
        if let Some(desired) = desired {
            self.theme.set(desired.theme);
        } else if let Some(theme) = settings.get("theme_mode").and_then(Value::as_str) {
            self.theme.set(normalize_theme(theme));
        }
        let Some(grid) = self.grid_view_writes.try_update_value(|writes| {
            writes.snapshot(settings.get("is_grid_view").and_then(Value::as_bool))
        }) else {
            return;
        };
        self.grid_view.set(grid);
        settings.insert("is_grid_view".into(), Value::Bool(grid));
        if let Some(language) = settings
            .get("language")
            .and_then(Value::as_str)
            .filter(|v| matches!(*v, "en" | "zh_CN"))
        {
            if language != crate::app::i18n::language() {
                crate::app::i18n::set_language(language);
            }
        }
        if self.settings.with_untracked(|current| current != &settings) {
            self.settings.set(settings);
            self.settings_version
                .update(|version| *version = version.wrapping_add(1));
        }
    }
}

/// List identity, ordering and filters exclude high-frequency recording progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingListEntry {
    pub rec_id: String,
    pub name: String,
    pub platform_key: String,
    pub search_text: String,
    pub phase: TaskPhase,
    pub priority: u8,
}
impl From<&Recording> for RecordingListEntry {
    fn from(record: &Recording) -> Self {
        Self {
            rec_id: record.rec_id.clone(),
            name: record.name().to_lowercase(),
            platform_key: record.platform_key.as_deref().unwrap_or("custom").into(),
            search_text: format!(
                "{}\n{}\n{}",
                record.name(),
                record.url,
                record.live_title.as_deref().unwrap_or("")
            )
            .to_lowercase(),
            phase: record.task_phase(),
            priority: crate::app::labels::recording_priority(record),
        }
    }
}

#[derive(Default)]
struct SettingsSync {
    generation: u64,
    pending: usize,
    loaded: bool,
}
impl SettingsSync {
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }
    fn begin(&mut self, force: bool) -> Option<u64> {
        if !force && (self.loaded || self.pending > 0) {
            return None;
        }
        self.generation = self.generation.wrapping_add(1);
        self.pending += 1;
        Some(self.generation)
    }
    fn finish(&mut self, generation: u64, success: bool) -> bool {
        self.pending = self.pending.saturating_sub(1);
        if self.generation != generation {
            return false;
        }
        self.loaded = success;
        true
    }
}
#[derive(Default)]
struct GridViewWrites {
    confirmed: Option<bool>,
    desired: Option<bool>,
    pending: Option<bool>,
    running: bool,
}
impl GridViewWrites {
    /// Only a confirmed idle choice or an identical in-flight intent is a no-op.
    fn push(&mut self, grid: bool) -> Option<bool> {
        if self.running && self.desired == Some(grid)
            || !self.running && self.desired.is_none() && self.confirmed == Some(grid)
        {
            return None;
        }
        self.desired = Some(grid);
        self.pending = Some(grid);
        Some(!std::mem::replace(&mut self.running, true))
    }
    fn next(&mut self) -> Option<bool> {
        let next = self.pending.take();
        if next.is_none() {
            self.running = false;
        }
        next
    }
    fn saved(&mut self, grid: bool) {
        self.confirmed = Some(grid);
        if self.pending == Some(grid) {
            self.pending = None;
        }
        if self.desired == Some(grid) && self.pending.is_none() {
            self.desired = None;
        }
    }
    /// An errored write may have committed. Only a fresh read can reconcile it.
    fn reconcile(&mut self, attempted: bool, observed: Option<bool>) -> bool {
        let Some(observed) = observed else {
            return false;
        };
        self.confirmed = Some(observed);
        if observed == attempted {
            self.saved(observed);
            true
        } else {
            if self.pending.is_none() {
                self.desired = None;
            }
            false
        }
    }
    fn snapshot(&mut self, observed: Option<bool>) -> bool {
        if !self.running {
            if let Some(observed) = observed {
                self.confirmed = Some(observed);
                if self.desired == Some(observed) {
                    self.desired = None;
                }
            }
        }
        self.visible()
    }
    fn visible(&self) -> bool {
        self.desired.or(self.confirmed).unwrap_or(true)
    }
}
#[cfg(test)]
mod grid_view_write_tests {
    use super::*;

    fn writes() -> GridViewWrites {
        GridViewWrites {
            confirmed: Some(true),
            ..Default::default()
        }
    }

    #[test]
    fn failed_first_save_rolls_back_and_the_same_choice_can_retry() {
        let mut writes = writes();
        assert_eq!(writes.push(false), Some(true));
        assert_eq!(writes.next(), Some(false));
        assert!(!writes.visible());

        assert!(!writes.reconcile(false, Some(true)));
        assert!(writes.visible());
        assert_eq!(writes.next(), None);
        assert_eq!(writes.push(false), Some(true));
        assert_eq!(writes.next(), Some(false));
        writes.saved(false);
        assert!(!writes.visible());
        assert_eq!(writes.confirmed, Some(false));
        assert_eq!(writes.next(), None);
        assert_eq!(
            writes.push(false),
            None,
            "only confirmed idle choices are skipped"
        );
    }

    #[test]
    fn unverified_failure_keeps_a_retryable_choice_without_claiming_success() {
        let mut writes = writes();
        assert_eq!(writes.push(false), Some(true));
        assert_eq!(writes.next(), Some(false));
        assert!(!writes.reconcile(false, None));
        assert_eq!(writes.next(), None);
        assert_eq!(writes.confirmed, Some(true));
        assert_eq!(writes.desired, Some(false));
        assert!(
            !writes.snapshot(Some(true)),
            "an unrelated read cannot erase the local intent"
        );
        assert_eq!(
            writes.push(false),
            Some(true),
            "the selected option remains retryable"
        );
    }

    #[test]
    fn a_timed_out_write_is_acknowledged_only_when_readback_confirms_it() {
        let mut writes = writes();
        assert_eq!(writes.push(false), Some(true));
        assert_eq!(writes.next(), Some(false));
        assert!(writes.reconcile(false, Some(false)));
        assert_eq!(writes.confirmed, Some(false));
        assert_eq!(writes.desired, None);
        assert_eq!(writes.next(), None);
    }

    #[test]
    fn rapid_choices_share_one_worker_and_only_the_last_pending_choice_is_written() {
        let mut writes = writes();
        assert_eq!(writes.push(false), Some(true));
        assert_eq!(writes.next(), Some(false));
        assert_eq!(writes.push(true), Some(false));
        assert_eq!(writes.push(false), Some(false));
        assert_eq!(writes.push(true), Some(false));
        assert_eq!(
            writes.push(true),
            None,
            "identical pending intent is coalesced"
        );
        writes.saved(false);
        assert!(
            writes.visible(),
            "the earlier acknowledgement cannot revert the last click"
        );
        assert_eq!(writes.next(), Some(true));
        writes.saved(true);
        assert_eq!(writes.next(), None);
        assert_eq!(writes.confirmed, Some(true));
        assert_eq!(writes.desired, None);
    }

    #[test]
    fn returning_to_the_inflight_value_does_not_send_a_redundant_write() {
        let mut writes = writes();
        assert_eq!(writes.push(false), Some(true));
        assert_eq!(writes.next(), Some(false));
        assert_eq!(writes.push(true), Some(false));
        assert_eq!(writes.push(false), Some(false));
        writes.saved(false);
        assert_eq!(writes.next(), None);
        assert!(!writes.visible());
    }

    #[test]
    fn a_failed_earlier_write_does_not_discard_a_newer_queued_choice() {
        let mut writes = writes();
        assert_eq!(writes.push(false), Some(true));
        assert_eq!(writes.next(), Some(false));
        assert_eq!(writes.push(true), Some(false));
        assert!(!writes.reconcile(false, Some(true)));
        assert!(writes.visible());
        assert_eq!(writes.desired, Some(true));
        assert_eq!(writes.next(), Some(true));
        writes.saved(true);
        assert_eq!(writes.next(), None);
    }

    #[test]
    fn settings_reads_cannot_overwrite_pending_or_just_confirmed_grid_choices() {
        let mut writes = writes();
        let mut sync = SettingsSync::default();
        let before_choice = sync.begin(false).unwrap();
        assert_eq!(writes.push(false), Some(true));
        sync.invalidate();
        assert!(!sync.finish(before_choice, true));
        assert_eq!(writes.next(), Some(false));

        let while_saving = sync.begin(true).unwrap();
        assert!(sync.finish(while_saving, true));
        assert!(!writes.snapshot(Some(true)));
        assert_eq!(writes.confirmed, Some(true));
        assert_eq!(writes.desired, Some(false));

        let before_acknowledgement = sync.begin(true).unwrap();
        writes.saved(false);
        sync.invalidate();
        assert_eq!(writes.next(), None);
        assert!(!sync.finish(before_acknowledgement, true));
        let current = sync.begin(true).unwrap();
        assert!(sync.finish(current, true));
        assert!(!writes.snapshot(Some(false)));
        assert_eq!(writes.confirmed, Some(false));
    }

    #[test]
    fn a_new_choice_supersedes_an_older_verification_read() {
        let mut writes = writes();
        let mut sync = SettingsSync::default();
        assert_eq!(writes.push(false), Some(true));
        assert_eq!(writes.next(), Some(false));
        let verification = sync.begin(true).unwrap();
        assert_eq!(writes.push(true), Some(false));
        sync.invalidate();
        assert!(!sync.finish(verification, true));
        assert!(writes.visible());
        assert_eq!(writes.next(), Some(true));
        writes.saved(true);
        assert_eq!(writes.next(), None);
    }

    #[test]
    fn application_owned_grid_writes_complete_after_either_entry_unmounts() {
        let app = Owner::new();
        let (writes, selected) = app.with(|| (StoredValue::new(writes()), RwSignal::new(true)));
        let recordings_page = app.with(Owner::new);
        recordings_page.with(|| {
            assert_eq!(
                writes.try_update_value(|writes| writes.push(false)),
                Some(Some(true))
            );
            selected.set(false);
        });
        assert_eq!(
            writes.try_update_value(GridViewWrites::next),
            Some(Some(false))
        );
        recordings_page.cleanup();

        let settings_page = app.with(Owner::new);
        settings_page.with(|| {
            assert_eq!(
                writes.try_update_value(|writes| writes.push(true)),
                Some(Some(false))
            );
            selected.set(true);
        });
        settings_page.cleanup();
        writes.update_value(|writes| writes.saved(false));
        assert_eq!(
            writes.try_update_value(GridViewWrites::next),
            Some(Some(true))
        );
        writes.update_value(|writes| writes.saved(true));
        assert_eq!(writes.try_update_value(GridViewWrites::next), Some(None));
        assert_eq!(
            writes.with_value(GridViewWrites::visible),
            selected.get_untracked()
        );
        assert_eq!(writes.with_value(|writes| writes.confirmed), Some(true));
        app.cleanup();
        assert!(writes.try_update_value(GridViewWrites::next).is_none());
    }
}
#[derive(Debug, Clone, PartialEq)]
struct Appearance {
    theme: String,
}
#[derive(Default)]
struct AppearanceWrites {
    pending: Option<Appearance>,
    desired: Option<Appearance>,
    running: bool,
}
impl AppearanceWrites {
    fn push(&mut self, value: Appearance) -> bool {
        self.desired = Some(value.clone());
        self.pending = Some(value);
        !std::mem::replace(&mut self.running, true)
    }
    fn next(&mut self) -> Option<Appearance> {
        let next = self.pending.take();
        if next.is_none() {
            self.running = false;
        }
        next
    }
    fn saved(&mut self, value: &Appearance) {
        if self.desired.as_ref() == Some(value) && self.pending.is_none() {
            self.desired = None;
        }
    }
}

#[cfg(test)]
mod settings_sync_tests {
    use super::*;

    #[test]
    fn themes_resolve_to_explicit_light_or_dark_only() {
        assert_eq!(normalize_theme("dark"), "dark");
        for value in ["light", "system", "", "purple", "invalid"] {
            assert_eq!(normalize_theme(value), "light");
        }
    }

    #[test]
    fn only_the_latest_settings_response_can_publish_success_or_failure() {
        let mut sync = SettingsSync::default();
        let old = sync.begin(false).unwrap();
        assert_eq!(sync.begin(false), None, "startup readers share one request");
        let latest = sync.begin(true).unwrap();
        assert!(sync.finish(latest, true));
        assert!(!sync.finish(old, false));
        assert!(sync.loaded);
        assert_eq!(sync.pending, 0);
        assert_eq!(sync.begin(false), None);

        let failed = sync.begin(true).unwrap();
        assert!(sync.finish(failed, false));
        assert!(!sync.loaded);
        let retry = sync.begin(false).unwrap();
        assert!(sync.finish(retry, true));
    }

    #[test]
    fn a_local_appearance_change_invalidates_an_inflight_settings_read() {
        let mut sync = SettingsSync::default();
        let old = sync.begin(false).unwrap();
        sync.generation = sync.generation.wrapping_add(1);
        assert!(!sync.finish(old, true));
        assert!(!sync.loaded);
        let latest = sync.begin(true).unwrap();
        assert!(sync.finish(latest, true));
        assert_eq!(sync.pending, 0);
    }

    #[test]
    fn appearance_writes_are_serial_and_coalesce_to_the_latest_choice() {
        let dark = Appearance {
            theme: "dark".into(),
        };
        let light = Appearance {
            theme: "light".into(),
        };
        let mut writes = AppearanceWrites::default();
        assert!(writes.push(dark.clone()));
        assert_eq!(writes.next(), Some(dark.clone()));
        assert!(!writes.push(dark.clone()));
        assert!(!writes.push(light.clone()));
        writes.saved(&dark);
        assert_eq!(writes.desired, Some(light.clone()));
        assert_eq!(writes.next(), Some(light.clone()));
        writes.saved(&light);
        assert_eq!(writes.desired, None);
        assert_eq!(writes.next(), None);
        assert!(writes.push(dark));
    }

    #[test]
    fn a_failed_appearance_write_keeps_the_local_choice() {
        let local = Appearance {
            theme: "dark".into(),
        };
        let mut writes = AppearanceWrites::default();
        assert!(writes.push(local.clone()));
        assert_eq!(writes.next(), Some(local.clone()));
        assert_eq!(writes.next(), None);
        assert_eq!(writes.desired, Some(local));
        assert!(!writes.running);
    }

    #[test]
    fn list_projection_changes_for_order_and_filters_but_not_progress() {
        let mut record = Recording {
            rec_id: "room".into(),
            streamer_name: "Room".into(),
            is_recording: true,
            ..Default::default()
        };
        let original = RecordingListEntry::from(&record);
        record.recorded_seconds = 123.0;
        record.speed = Some("2 MB/s".into());
        assert_eq!(RecordingListEntry::from(&record), original);
        record.is_recording = false;
        assert_ne!(RecordingListEntry::from(&record), original);
        record.is_recording = true;
        record.live_title = Some("new searchable title".into());
        assert_ne!(RecordingListEntry::from(&record), original);
        record.live_title = None;
        record.streamer_name = "Renamed".into();
        assert_ne!(RecordingListEntry::from(&record), original);
    }
}

pub fn normalize_theme(value: &str) -> String {
    if matches!(value, "light" | "dark") {
        value.into()
    } else {
        "light".into()
    }
}
pub fn provide_app_state() -> AppState {
    let appearance = window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|storage| storage.get_item("streamcap.appearance").ok().flatten())
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or(Value::Null);
    let recordings = RwSignal::new(Vec::<Recording>::new());
    let recording_list = Memo::new(move |_| {
        recordings.with(|items| {
            items
                .iter()
                .map(RecordingListEntry::from)
                .collect::<Vec<_>>()
        })
    });
    let recordings_index = Memo::new(move |_| {
        recording_list.with(|items| {
            items
                .iter()
                .enumerate()
                .map(|(index, record)| (record.rec_id.clone(), index))
                .collect()
        })
    });
    let state = AppState {
        status: RwSignal::new(GatewayStatus::default()),
        recordings,
        recording_list,
        recordings_index,
        media_jobs: RwSignal::new(Vec::new()),
        recordings_snapshot: StoredValue::new(SnapshotMerge::default()),
        jobs_snapshot: StoredValue::new(SnapshotMerge::default()),
        settings: RwSignal::new(Map::new()),
        settings_version: RwSignal::new(0),
        settings_loading: RwSignal::new(true),
        settings_error: RwSignal::new(None),
        settings_sync: StoredValue::new(SettingsSync::default()),
        appearance_writes: StoredValue::new(AppearanceWrites::default()),
        grid_view_writes: StoredValue::new(GridViewWrites::default()),
        notice: RwSignal::new(None),
        loading: RwSignal::new(true),
        error: RwSignal::new(None),
        events_connected: RwSignal::new(false),
        events_revision: RwSignal::new(0),
        theme: RwSignal::new(normalize_theme(
            appearance["theme"].as_str().unwrap_or("light"),
        )),
        grid_view: RwSignal::new(true),
        grid_view_saving: RwSignal::new(false),
        now: RwSignal::new((js_sys::Date::now() / 1000.0) as i64),
    };
    let clock = StoredValue::new_local(Some(gloo_timers::callback::Interval::new(
        1000,
        move || {
            let _ = state.now.try_set((js_sys::Date::now() / 1000.0) as i64);
        },
    )));
    on_cleanup(move || clock.update_value(|timer| *timer = None));
    provide_context(state);
    state
}
pub fn app_state() -> AppState {
    use_context::<AppState>().expect("AppState context missing")
}

pub async fn sleep_ms(ms: u32) {
    let (sender, receiver) = futures::channel::oneshot::channel();
    let timer = gloo_timers::callback::Timeout::new(ms, move || {
        let _ = sender.send(());
    });
    let _ = receiver.await;
    drop(timer);
}

async fn decode<T: DeserializeOwned>(response: Response) -> Result<T, String> {
    if !response.ok() {
        let status = response.status();
        let detail = response
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| v.get("detail").and_then(Value::as_str).map(str::to_owned));
        return Err(detail
            .unwrap_or_else(|| crate::tr_format!("操作未完成（HTTP {status}），请刷新后重试")));
    }
    response
        .json::<T>()
        .await
        .map_err(|_| crate::app::i18n::t("服务返回的数据格式不正确").into())
}
async fn call<T: DeserializeOwned>(
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<T, String> {
    call_controlled(method, path, body, None).await
}
struct ParentCancellation {
    signal: web_sys::AbortSignal,
    handler: Closure<dyn FnMut(web_sys::Event)>,
}
struct RequestCancellation {
    controller: web_sys::AbortController,
    parent: Option<ParentCancellation>,
}
impl RequestCancellation {
    fn new(parent: Option<web_sys::AbortController>) -> Result<Self, String> {
        let error = || crate::app::i18n::t("无法创建请求").to_string();
        let controller = web_sys::AbortController::new().map_err(|_| error())?;
        let parent = if let Some(parent) = parent {
            let signal = parent.signal();
            if signal.aborted() {
                controller.abort();
                None
            } else {
                let child = controller.clone();
                let handler = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| child.abort());
                signal
                    .add_event_listener_with_callback("abort", handler.as_ref().unchecked_ref())
                    .map_err(|_| error())?;
                Some(ParentCancellation { signal, handler })
            }
        } else {
            None
        };
        Ok(Self { controller, parent })
    }
}
impl Drop for RequestCancellation {
    fn drop(&mut self) {
        if let Some(parent) = &self.parent {
            let _ = parent.signal.remove_event_listener_with_callback(
                "abort",
                parent.handler.as_ref().unchecked_ref(),
            );
        }
        self.controller.abort();
    }
}
async fn call_controlled<T: DeserializeOwned>(
    method: &str,
    path: &str,
    body: Option<Value>,
    controller: Option<web_sys::AbortController>,
) -> Result<T, String> {
    let base = gateway_base();
    if base.is_empty() {
        return Err(crate::app::i18n::t("原生运行配置未就绪，请从桌面程序启动").into());
    }
    let url = format!("{base}{path}");
    // A page can cancel all of its requests, but each timeout cancels only its own fetch.
    let cancellation = RequestCancellation::new(controller)?;
    let builder = match method {
        "POST" => Request::post(&url),
        "PUT" => Request::put(&url),
        "DELETE" => Request::delete(&url),
        _ => Request::get(&url),
    }
    .abort_signal(Some(&cancellation.controller.signal()));
    let request = match body {
        Some(body) => builder.json(&body),
        None => builder.build(),
    }
    .map_err(|_| crate::app::i18n::t("无法构造请求").to_string())?;
    let timeout_ms = if path == "/api/tools/install" {
        600_000
    } else if path.starts_with("/api/storage/size?") {
        120_000
    } else if method == "GET" {
        12_000
    } else {
        120_000
    };
    let timeout = cancellation.controller.clone();
    let timer = gloo_timers::callback::Timeout::new(timeout_ms, move || timeout.abort());
    let response = request.send().await.map_err(|_| {
        crate::app::i18n::t("本地服务未响应；请刷新确认操作结果后再重试").to_string()
    })?;
    let result = decode(response).await;
    drop(timer);
    result
}

pub async fn fetch_status() -> Result<GatewayStatus, String> {
    call("GET", "/api/status", None).await
}
pub async fn fetch_recordings() -> Result<Vec<Recording>, String> {
    call("GET", "/api/recordings", None).await
}
async fn fetch_settings() -> Result<SettingsPayload, String> {
    call("GET", "/api/settings", None).await
}
pub async fn refresh_settings(state: AppState, force: bool) -> Result<(), String> {
    if let Some(payload) = refresh_settings_snapshot(state, force).await? {
        state.apply_settings(payload);
    }
    Ok(())
}
async fn refresh_settings_snapshot(
    state: AppState,
    force: bool,
) -> Result<Option<SettingsPayload>, String> {
    let Some(Some(generation)) = state
        .settings_sync
        .try_update_value(|sync| sync.begin(force))
    else {
        return Ok(None);
    };
    state.settings_loading.set(true);
    let result = fetch_settings().await;
    if state
        .settings_sync
        .try_update_value(|sync| sync.finish(generation, result.is_ok()))
        != Some(true)
    {
        return Ok(None);
    }
    state.settings_loading.set(false);
    match result {
        Ok(payload) => {
            state.settings_error.set(None);
            Ok(Some(payload))
        }
        Err(error) => {
            state.settings_error.set(Some(error.clone()));
            Err(error)
        }
    }
}
pub async fn refresh_recordings(state: AppState) -> Result<(), String> {
    let Some(generation) = state
        .recordings_snapshot
        .try_update_value(SnapshotMerge::begin)
    else {
        return Ok(());
    };
    let list = match fetch_recordings().await {
        Ok(list) => list,
        Err(error) => {
            if state
                .recordings_snapshot
                .try_update_value(|sync| sync.fail(generation, error.clone()))
                == Some(true)
            {
                state.publish_snapshot_result();
                return Err(error);
            }
            return Ok(());
        }
    };
    let Some(list) = state
        .recordings_snapshot
        .try_update_value(|sync| sync.finish(generation, list, |record| &record.rec_id))
        .flatten()
    else {
        return Ok(());
    };
    if state.recordings.with_untracked(|current| current != &list) {
        state.recordings.set(list);
    }
    state.recount_recordings();
    state.publish_snapshot_result();
    let Some(generation) = state.jobs_snapshot.try_update_value(SnapshotMerge::begin) else {
        return Ok(());
    };
    let jobs = match call::<MediaJobs>("GET", "/api/media/jobs", None).await {
        Ok(jobs) => jobs,
        Err(error) => {
            if state
                .jobs_snapshot
                .try_update_value(|sync| sync.fail(generation, error.clone()))
                == Some(true)
            {
                state.publish_snapshot_result();
                return Err(error);
            }
            return Ok(());
        }
    };
    if let Some(mut jobs) = state
        .jobs_snapshot
        .try_update_value(|sync| sync.finish(generation, jobs.jobs, |job| &job.id))
        .flatten()
    {
        if jobs.len() > 256 {
            jobs.drain(..jobs.len() - 256);
        }
        state.media_jobs.set(jobs);
        state.publish_snapshot_result();
    }
    Ok(())
}

pub async fn poll_connection(state: AppState) {
    loop {
        match fetch_status().await {
            Ok(status) => {
                let recovered = !state.status.get_untracked().ok;
                state.status.set(status);
                state.error.set(state.snapshot_error());
                let refresh = state.snapshots_need_refresh()
                    || recovered
                    || !state.events_connected.get_untracked();
                // An ignored old response is not initialization; only applied snapshots count.
                if refresh && !state.snapshot_pending() {
                    let _ = refresh_recordings(state).await;
                }
                if !state.settings_sync.with_value(|sync| sync.loaded) || refresh {
                    let _ = refresh_settings(state, refresh).await;
                }
            }
            Err(error) => {
                state.status.update(|s| {
                    s.ok = false;
                    s.resolver_ready = false;
                });
                state.error.set(Some(error));
                state.loading.set(false);
            }
        }
        sleep_ms(if state.snapshots_need_refresh() {
            1500
        } else {
            5000
        })
        .await;
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NewRecording {
    pub url: String,
    pub streamer_name: Option<String>,
    pub quality: Option<String>,
}
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ImportResult {
    #[serde(default)]
    pub created: Vec<Recording>,
    #[serde(default)]
    pub skipped: Vec<Value>,
}
pub async fn create_recordings(items: Vec<NewRecording>) -> Result<ImportResult, String> {
    call("POST", "/api/recordings", Some(json!({"items":items}))).await
}
pub async fn delete_recording(id: &str) -> Result<(), String> {
    call::<Value>("DELETE", &format!("/api/recordings/{}", encode(id)), None)
        .await
        .map(|_| ())
}
pub async fn delete_recordings(ids: &[String]) -> Result<(), String> {
    call::<Value>(
        "POST",
        "/api/recordings/delete",
        Some(json!({"recIds":ids})),
    )
    .await
    .map(|_| ())
}
pub async fn toggle_monitor(id: &str) -> Result<(), String> {
    call::<Value>(
        "POST",
        &format!("/api/recordings/{}/monitor", encode(id)),
        None,
    )
    .await
    .map(|_| ())
}
pub async fn check_recording(id: &str) -> Result<(), String> {
    call::<Value>(
        "POST",
        &format!("/api/recordings/{}/check", encode(id)),
        None,
    )
    .await
    .map(|_| ())
}
pub async fn start_recording(id: &str) -> Result<(), String> {
    call::<Value>(
        "POST",
        &format!("/api/recordings/{}/start", encode(id)),
        None,
    )
    .await
    .map(|_| ())
}
pub async fn stop_recording(id: &str) -> Result<(), String> {
    call::<Value>(
        "POST",
        &format!("/api/recordings/{}/stop", encode(id)),
        None,
    )
    .await
    .map(|_| ())
}
pub async fn update_recording(
    id: &str,
    changes: Map<String, Value>,
    follow: Vec<String>,
) -> Result<(), String> {
    call::<Value>(
        "PUT",
        &format!("/api/recordings/{}", encode(id)),
        Some(json!({"changes":changes,"followGlobal":follow})),
    )
    .await
    .map(|_| ())
}
pub async fn batch_edit(
    ids: Vec<String>,
    changes: Map<String, Value>,
    follow: Vec<String>,
) -> Result<(), String> {
    call::<Value>(
        "POST",
        "/api/recordings/batch-edit",
        Some(json!({"recIds":ids,"changes":changes,"followGlobal":follow})),
    )
    .await
    .map(|_| ())
}
pub async fn save_settings(patch: Value) -> Result<(), String> {
    call::<Value>("PUT", "/api/settings", Some(json!({"userConfig":patch})))
        .await
        .map(|_| ())
}
pub async fn fetch_cookies() -> Result<Map<String, Value>, String> {
    #[derive(Deserialize)]
    struct Cookies {
        cookies: Map<String, Value>,
    }
    call::<Cookies>("GET", "/api/cookies", None)
        .await
        .map(|c| c.cookies)
}
pub async fn save_cookies(patch: Value) -> Result<(), String> {
    call::<Value>("PUT", "/api/cookies", Some(json!({"cookies":patch})))
        .await
        .map(|_| ())
}
pub async fn fetch_storage(
    subfolder: Option<String>,
    abort: web_sys::AbortController,
) -> Result<StorageListing, String> {
    let query = subfolder
        .filter(|s| !s.is_empty())
        .map(|s| format!("?subfolder={}", encode(&s)))
        .unwrap_or_default();
    call_controlled("GET", &format!("/api/storage{query}"), None, Some(abort)).await
}
pub async fn fetch_storage_size(
    path: &str,
    abort: web_sys::AbortController,
) -> Result<u64, String> {
    let value: Value = call_controlled(
        "GET",
        &format!("/api/storage/size?path={}", encode(path)),
        None,
        Some(abort),
    )
    .await?;
    value["size"]
        .as_u64()
        .ok_or_else(|| crate::app::i18n::t("体积信息无效").into())
}
pub async fn delete_storage(path: &str) -> Result<(), String> {
    call::<Value>(
        "DELETE",
        &format!("/api/storage?path={}", encode(path)),
        None,
    )
    .await
    .map(|_| ())
}
pub fn video_url(path: &str) -> String {
    let base = gateway_base();
    if base.is_empty() {
        return String::new();
    }
    format!("{base}/api/videos?path={}", encode(path))
}
pub async fn fetch_recording_files(
    id: &str,
    abort: web_sys::AbortController,
) -> Result<RecordingFiles, String> {
    call_controlled(
        "GET",
        &format!("/api/recordings/{}/files", encode(id)),
        None,
        Some(abort),
    )
    .await
}
pub async fn qr_start() -> Result<QrSnapshot, String> {
    call("POST", "/api/qr/kuaishou/start", None).await
}
pub async fn qr_status(id: &str) -> Result<QrSnapshot, String> {
    call(
        "GET",
        &format!("/api/qr/kuaishou/status?sessionId={}", encode(id)),
        None,
    )
    .await
}
pub async fn qr_cancel(id: &str) -> Result<(), String> {
    call::<Value>(
        "POST",
        &format!("/api/qr/kuaishou/cancel?sessionId={}", encode(id)),
        None,
    )
    .await
    .map(|_| ())
}
fn encode(value: &str) -> String {
    js_sys::encode_uri_component(value)
        .as_string()
        .unwrap_or_default()
}

pub fn save_grid_view(state: AppState, grid: bool) {
    let Some(Some(start)) = state
        .grid_view_writes
        .try_update_value(|writes| writes.push(grid))
    else {
        return;
    };
    state.settings_sync.update_value(SettingsSync::invalidate);
    state.publish_grid_view();
    state.grid_view_saving.set(true);
    if !start {
        return;
    }
    // The queue and signals belong to AppState, not either page's reactive owner.
    leptos::task::spawn_local(async move {
        while let Some(Some(grid)) = state
            .grid_view_writes
            .try_update_value(GridViewWrites::next)
        {
            let result = match save_settings(json!({"is_grid_view":grid})).await {
                Ok(()) => {
                    let _ = state
                        .grid_view_writes
                        .try_update_value(|writes| writes.saved(grid));
                    Ok(())
                }
                Err(error) => match refresh_settings_snapshot(state, true).await {
                    Ok(Some(payload)) => {
                        let observed = payload
                            .user_config
                            .get("is_grid_view")
                            .or_else(|| payload.default_config.get("is_grid_view"))
                            .and_then(Value::as_bool);
                        let verified = state
                            .grid_view_writes
                            .try_update_value(|writes| writes.reconcile(grid, observed))
                            == Some(true);
                        state.apply_settings(payload);
                        if verified {
                            Ok(())
                        } else {
                            Err(error)
                        }
                    }
                    // Keep an unconfirmed choice retryable when verification also fails
                    // or a newer settings generation supersedes this verification read.
                    Ok(None) | Err(_) => Err(error),
                },
            };
            let Some(complete) = state
                .grid_view_writes
                .try_update_value(|writes| writes.desired.is_none())
            else {
                return;
            };
            // Reads started while this request was pending cannot undo its acknowledgement.
            state.settings_sync.update_value(SettingsSync::invalidate);
            state.publish_grid_view();
            match result {
                Ok(()) if complete => state.notify(crate::app::i18n::t("偏好设置已保存")),
                Err(error) => state.fail(error),
                _ => {}
            }
        }
        let _ = state.grid_view_saving.try_set(false);
        let _ = refresh_settings(state, true).await;
    });
}

pub fn save_appearance(state: AppState) {
    let value = Appearance {
        theme: state.theme.get_untracked(),
    };
    state
        .settings_sync
        .update_value(|sync| sync.generation = sync.generation.wrapping_add(1));
    if state
        .appearance_writes
        .try_update_value(|writes| writes.push(value))
        != Some(true)
    {
        return;
    }
    leptos::task::spawn_local(async move {
        while let Some(Some(value)) = state
            .appearance_writes
            .try_update_value(AppearanceWrites::next)
        {
            match save_settings(json!({"theme_mode":value.theme})).await {
                Ok(()) => {
                    let _ = state
                        .appearance_writes
                        .try_update_value(|writes| writes.saved(&value));
                }
                Err(error) => {
                    state.fail(crate::tr_format!("外观已本地应用，配置同步失败：{error}"))
                }
            }
        }
        let _ = refresh_settings(state, true).await;
    });
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MediaJob {
    pub id: String,
    #[serde(default)]
    pub stage_percent: Option<u8>,
    #[serde(default)]
    pub stage_eta_seconds: Option<u64>,
    #[serde(default)]
    pub started_at: Option<i64>,
    #[serde(default)]
    pub finished_at: Option<i64>,
    #[serde(default)]
    pub elapsed_seconds: u64,
    #[serde(default)]
    pub queue_position: Option<usize>,
    pub task_id: Option<String>,
    pub source: String,
    pub output: String,
    pub state: String,
    #[serde(default)]
    pub source_removed: bool,
    #[serde(default)]
    pub delete_original: bool,
    pub message: String,
}
impl MediaJob {
    pub fn pending(&self) -> bool {
        matches!(
            self.state.as_str(),
            "waiting" | "running" | "verifying" | "cleaning"
        )
    }
    pub fn needs_attention(&self) -> bool {
        matches!(
            self.state.as_str(),
            "failed" | "cleanupFailed" | "cancelled"
        )
    }
    pub fn output_ready(&self) -> bool {
        matches!(self.state.as_str(), "complete" | "cleanupFailed")
    }
}

fn upsert_media_job(jobs: &mut Vec<MediaJob>, job: MediaJob) {
    if let Some(existing) = jobs.iter_mut().find(|item| item.id == job.id) {
        *existing = job;
        return;
    }
    while jobs.len() >= 256 {
        let Some(index) = jobs.iter().position(|item| !item.pending()) else {
            break;
        };
        jobs.remove(index);
    }
    jobs.push(job);
}

#[cfg(test)]
mod media_job_tests {
    use super::*;

    #[test]
    fn history_eviction_never_hides_a_running_or_queued_job() {
        let job = |id: usize, state: &str| {
            serde_json::from_value(json!({
                "id":id.to_string(), "state":state, "taskId":null,
                "source":"sample.ts", "output":"sample.mp4", "message":""
            }))
            .unwrap()
        };
        let mut jobs = vec![job(0, "running"), job(1, "waiting")];
        jobs.extend((2..256).map(|id| job(id, "complete")));
        upsert_media_job(&mut jobs, job(256, "waiting"));
        assert_eq!(jobs.len(), 256);
        assert_eq!(jobs[0].id, "0");
        assert_eq!(jobs[1].id, "1");
        assert!(!jobs.iter().any(|item| item.id == "2"));
        upsert_media_job(&mut jobs, job(0, "complete"));
        assert_eq!(jobs.len(), 256);
        assert!(jobs[0].output_ready());
    }
}
#[derive(Deserialize)]
struct MediaJobs {
    jobs: Vec<MediaJob>,
}
pub async fn remux_file(path: &str) -> Result<(), String> {
    let _: Value = call("POST", "/api/media/remux", Some(json!({"path":path}))).await?;
    Ok(())
}

type EventHandler = (&'static str, Closure<dyn FnMut(web_sys::MessageEvent)>);

pub struct EventConnection {
    source: web_sys::EventSource,
    messages: Vec<EventHandler>,
    _open: Closure<dyn FnMut(web_sys::Event)>,
    _error: Closure<dyn FnMut(web_sys::Event)>,
    retry_timer: Rc<RefCell<Option<gloo_timers::callback::Timeout>>>,
    _watchdog: gloo_timers::callback::Interval,
}
impl Drop for EventConnection {
    fn drop(&mut self) {
        self.retry_timer.borrow_mut().take();
        self.source.close();
        self.source.set_onopen(None);
        self.source.set_onerror(None);
        for (event, handler) in &self.messages {
            let _ = self
                .source
                .remove_event_listener_with_callback(event, handler.as_ref().unchecked_ref());
        }
    }
}
pub fn subscribe_events(state: AppState) -> Option<EventConnection> {
    let base = gateway_base();
    if base.is_empty() {
        return None;
    }
    let source = web_sys::EventSource::new(&format!("{base}/api/events")).ok()?;
    let retry_timer = Rc::new(RefCell::new(None::<gloo_timers::callback::Timeout>));
    let last_event = Rc::new(std::cell::Cell::new(js_sys::Date::now()));
    let opened_at = last_event.clone();
    let connected_timer = retry_timer.clone();
    let on_open = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        opened_at.set(js_sys::Date::now());
        connected_timer.borrow_mut().take();
        state.events_connected.set(true);
        leptos::task::spawn_local(async move {
            let _ = refresh_recordings(state).await;
            let _ =
                refresh_settings(state, state.settings_sync.with_value(|sync| sync.loaded)).await;
        });
    });
    let failed_timer = retry_timer.clone();
    let on_error = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        state.events_connected.set(false);
        if failed_timer.borrow().is_none() {
            *failed_timer.borrow_mut() =
                Some(gloo_timers::callback::Timeout::new(3000, move || {
                    let _ = state
                        .events_revision
                        .try_update(|value| *value = value.wrapping_add(1));
                }));
        }
    });
    source.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    source.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    let watched_source = source.clone();
    let watched_event = last_event.clone();
    let watchdog = gloo_timers::callback::Interval::new(5000, move || {
        if js_sys::Date::now() - watched_event.get() > 20000.0 {
            watched_event.set(js_sys::Date::now());
            watched_source.close();
            let _ = state.events_connected.try_set(false);
            let _ = state
                .events_revision
                .try_update(|value| *value = value.wrapping_add(1));
        }
    });
    let mut messages = Vec::new();
    for event_name in [
        "update",
        "delete",
        "settings",
        "mediaJob",
        "resync",
        "snack",
        "heartbeat",
    ] {
        let observed_at = last_event.clone();
        let handler = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(
            move |event: web_sys::MessageEvent| {
                observed_at.set(js_sys::Date::now());
                let Some(text) = event.data().as_string() else {
                    return;
                };
                match event_name {
                    "snack" => {
                        if let Ok(notice) = serde_json::from_str::<BackendNotice>(&text) {
                            state.background_notice(notice);
                        }
                    }
                    "resync" => {
                        leptos::task::spawn_local(async move {
                            let _ = refresh_recordings(state).await;
                            let _ = refresh_settings(state, true).await;
                        });
                    }
                    "update" => {
                        if let Ok(rec) = serde_json::from_str::<Recording>(&text) {
                            state.recordings_snapshot.update_value(|sync| {
                                sync.observe(rec.rec_id.clone(), Some(rec.clone()))
                            });
                            if state.recording_untracked(&rec.rec_id).as_ref() != Some(&rec) {
                                state.recordings.update(|list| {
                                    if let Some(existing) =
                                        list.iter_mut().find(|r| r.rec_id == rec.rec_id)
                                    {
                                        *existing = rec;
                                    } else {
                                        list.push(rec);
                                    }
                                });
                                state.recount_recordings();
                            }
                        }
                    }
                    "delete" => {
                        if let Ok(ids) = serde_json::from_str::<Vec<String>>(&text) {
                            state.recordings_snapshot.update_value(|sync| {
                                for id in &ids {
                                    sync.observe(id.clone(), None);
                                }
                            });
                            state
                                .recordings
                                .update(|list| list.retain(|r| !ids.contains(&r.rec_id)));
                            state.recount_recordings();
                        }
                    }
                    "mediaJob" => {
                        if let Ok(job) = serde_json::from_str::<MediaJob>(&text) {
                            state.jobs_snapshot.update_value(|sync| {
                                sync.observe(job.id.clone(), Some(job.clone()))
                            });
                            state.media_jobs.update(|jobs| {
                                upsert_media_job(jobs, job);
                            });
                        }
                    }
                    "settings" => {
                        leptos::task::spawn_local(async move {
                            let _ = refresh_settings(state, true).await;
                        });
                    }
                    _ => {}
                }
            },
        );
        let _ =
            source.add_event_listener_with_callback(event_name, handler.as_ref().unchecked_ref());
        messages.push((event_name, handler));
    }
    Some(EventConnection {
        source,
        messages,
        _open: on_open,
        _error: on_error,
        retry_timer,
        _watchdog: watchdog,
    })
}

pub async fn tool_status() -> Result<Value, String> {
    call("GET", "/api/tools/status", None).await
}
pub async fn install_tools() -> Result<(), String> {
    call::<Value>("POST", "/api/tools/install", Some(json!({})))
        .await
        .map(|_| ())
}
pub async fn check_update() -> Result<UpdateCheck, String> {
    call("GET", "/api/tools/update", None).await
}
pub async fn accounts() -> Result<HashMap<String, AccountSummary>, String> {
    call("GET", "/api/accounts", None).await
}
pub async fn save_account(platform: &str, changes: Value) -> Result<(), String> {
    call::<Value>(
        "PUT",
        "/api/accounts",
        Some(json!({"platform":platform,"changes":changes})),
    )
    .await
    .map(|_| ())
}
pub async fn shutdown_timer(hours: Option<f64>) -> Result<(), String> {
    call::<Value>(
        "POST",
        "/api/automation/shutdown",
        Some(json!({"hours":hours})),
    )
    .await
    .map(|_| ())
}
pub async fn save_screenshot(path: &str, data: &str) -> Result<String, String> {
    let value: Value = call(
        "POST",
        "/api/media/screenshot",
        Some(json!({"path":path,"pngBase64":data})),
    )
    .await?;
    value["path"]
        .as_str()
        .map(str::to_owned)
        .ok_or(crate::app::i18n::t("截图保存结果无效").into())
}
