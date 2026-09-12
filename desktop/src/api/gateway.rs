//! Typed client for the Rust desktop API. UI tests intercept transport, never user data.
use gloo_net::http::{Request, Response};
use leptos::prelude::*;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::prelude::*;

pub const GATEWAY_BASE: &str = "http://127.0.0.1:6059";

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
    pub monitor_hours: Option<i32>,
    pub enabled_message_push: Option<bool>,
    pub only_notify_no_record: Option<bool>,
    pub flv_use_direct_download: Option<bool>,
    pub video_bitrate: Option<i64>,
    #[serde(default)]
    pub is_live: bool,
    #[serde(default)]
    pub is_recording: bool,
    pub live_title: Option<String>,
    pub speed: Option<String>,
    pub recording_dir: Option<String>,
    #[serde(default)]
    pub inherited_fields: Vec<String>,
}

impl Recording {
    pub fn name(&self) -> String {
        if self.streamer_name.trim().is_empty() {
            "未命名直播间".into()
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
        } else if self.is_live {
            StatusKind::Live
        } else if self.monitor_status {
            StatusKind::Monitoring
        } else {
            StatusKind::Stopped
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Recording,
    Live,
    Monitoring,
    Stopped,
}
impl StatusKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Recording => "录制中",
            Self::Live => "直播中",
            Self::Monitoring => "监控中",
            Self::Stopped => "已暂停",
        }
    }
    pub fn class(self) -> &'static str {
        match self {
            Self::Recording => "badge recording",
            Self::Live => "badge live",
            Self::Monitoring => "badge monitoring",
            Self::Stopped => "badge stopped",
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    pub ok: bool,
    pub version: String,
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
    pub path: String,
    pub modified: Option<f64>,
}
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageListing {
    pub root: String,
    pub items: Vec<StorageItem>,
    pub total_size: u64,
}
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPayload {
    pub user_config: Map<String, Value>,
    pub default_config: Map<String, Value>,
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
    pub dir: Option<String>,
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NoticeKind {
    Success,
    Error,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub message: String,
    pub kind: NoticeKind,
}

#[derive(Debug, Clone, Copy)]
pub struct AppState {
    pub status: RwSignal<GatewayStatus>,
    pub recordings: RwSignal<Vec<Recording>>,
    pub settings: RwSignal<Map<String, Value>>,
    pub settings_version: RwSignal<u32>,
    pub notice: RwSignal<Option<Notice>>,
    pub loading: RwSignal<bool>,
    pub error: RwSignal<Option<String>>,
    pub events_connected: RwSignal<bool>,
    pub events_revision: RwSignal<u32>,
    pub theme: RwSignal<String>,
    pub system_dark: RwSignal<bool>,
    pub accent: RwSignal<String>,
    pub grid_view: RwSignal<bool>,
}
impl AppState {
    pub fn is_dark(self) -> bool {
        match self.theme.get().as_str() {
            "dark" => true,
            "system" => self.system_dark.get(),
            _ => false,
        }
    }
    pub fn notify(self, message: impl Into<String>) {
        self.notice.set(Some(Notice {
            message: message.into(),
            kind: NoticeKind::Success,
        }));
    }
    pub fn fail(self, message: impl Into<String>) {
        self.notice.set(Some(Notice {
            message: message.into(),
            kind: NoticeKind::Error,
        }));
    }
    pub fn setting(self, key: &str, fallback: &str) -> String {
        self.settings.with(|map| {
            map.get(key)
                .and_then(Value::as_str)
                .unwrap_or(fallback)
                .to_string()
        })
    }
    pub fn apply_settings(self, payload: SettingsPayload) {
        let mut settings = payload.default_config;
        settings.extend(payload.user_config);
        if let Some(theme) = settings.get("theme_mode").and_then(Value::as_str) {
            self.theme.set(normalize_theme(theme));
        }
        if let Some(accent) = settings.get("theme_color").and_then(Value::as_str) {
            self.accent.set(normalize_accent(accent));
        }
        if let Some(grid) = settings.get("is_grid_view").and_then(Value::as_bool) {
            self.grid_view.set(grid);
        }
        self.settings.set(settings);
    }
}

pub fn normalize_theme(value: &str) -> String {
    if matches!(value, "light" | "dark" | "system") {
        value.into()
    } else {
        "light".into()
    }
}
pub fn normalize_accent(value: &str) -> String {
    if matches!(
        value,
        "blue" | "teal" | "purple" | "green" | "orange" | "red" | "indigo"
    ) {
        value.into()
    } else {
        "blue".into()
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
    let state = AppState {
        status: RwSignal::new(GatewayStatus::default()),
        recordings: RwSignal::new(Vec::new()),
        settings: RwSignal::new(Map::new()),
        settings_version: RwSignal::new(0),
        notice: RwSignal::new(None),
        loading: RwSignal::new(true),
        error: RwSignal::new(None),
        events_connected: RwSignal::new(false),
        events_revision: RwSignal::new(0),
        theme: RwSignal::new(normalize_theme(
            appearance["theme"].as_str().unwrap_or("light"),
        )),
        system_dark: RwSignal::new(
            window()
                .match_media("(prefers-color-scheme: dark)")
                .ok()
                .flatten()
                .is_some_and(|query| query.matches()),
        ),
        accent: RwSignal::new(normalize_accent(
            appearance["accent"].as_str().unwrap_or("blue"),
        )),
        grid_view: RwSignal::new(true),
    };
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
        return Err(detail.unwrap_or_else(|| format!("操作未完成（HTTP {status}），请刷新后重试")));
    }
    response
        .json::<T>()
        .await
        .map_err(|_| "服务返回的数据格式不正确".into())
}
async fn call<T: DeserializeOwned>(
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<T, String> {
    let url = format!("{GATEWAY_BASE}{path}");
    let abort = web_sys::AbortController::new().map_err(|_| "无法创建请求".to_string())?;
    let builder = match method {
        "POST" => Request::post(&url),
        "PUT" => Request::put(&url),
        "DELETE" => Request::delete(&url),
        _ => Request::get(&url),
    }
    .abort_signal(Some(&abort.signal()));
    let request = match body {
        Some(body) => builder.json(&body),
        None => builder.build(),
    }
    .map_err(|_| "无法构造请求".to_string())?;
    let timer = gloo_timers::callback::Timeout::new(12_000, move || abort.abort());
    let response = request
        .send()
        .await
        .map_err(|_| "本地服务未响应；请刷新确认操作结果后再重试".to_string())?;
    drop(timer);
    decode(response).await
}

pub async fn fetch_status() -> Result<GatewayStatus, String> {
    call("GET", "/api/status", None).await
}
pub async fn fetch_recordings() -> Result<Vec<Recording>, String> {
    call("GET", "/api/recordings", None).await
}
pub async fn fetch_settings() -> Result<SettingsPayload, String> {
    call("GET", "/api/settings", None).await
}
pub async fn refresh_recordings(state: AppState) -> Result<(), String> {
    let list = fetch_recordings().await?;
    state.status.update(|s| {
        s.total_recordings = list.len();
        s.active_recordings = list.iter().filter(|r| r.is_recording).count();
    });
    state.recordings.set(list);
    state.loading.set(false);
    Ok(())
}

pub async fn poll_connection(state: AppState) {
    let mut initialized = false;
    loop {
        match fetch_status().await {
            Ok(status) => {
                let recovered = !state.status.get_untracked().ok;
                state.status.set(status);
                state.error.set(None);
                if !initialized || recovered || !state.events_connected.get_untracked() {
                    initialized = match refresh_recordings(state).await {
                        Ok(()) => true,
                        Err(error) => {
                            state.loading.set(false);
                            state.error.set(Some(error));
                            false
                        }
                    };
                    if let Ok(settings) = fetch_settings().await {
                        state.apply_settings(settings);
                    }
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
        sleep_ms(if initialized { 5000 } else { 1500 }).await;
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NewRecording {
    pub url: String,
    pub streamer_name: Option<String>,
    pub quality: Option<String>,
}
pub async fn create_recordings(items: Vec<NewRecording>) -> Result<(), String> {
    call::<Value>("POST", "/api/recordings", Some(json!({"items":items})))
        .await
        .map(|_| ())
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
pub async fn fetch_storage(subfolder: Option<String>) -> Result<StorageListing, String> {
    let query = subfolder
        .filter(|s| !s.is_empty())
        .map(|s| format!("?subfolder={}", encode(&s)))
        .unwrap_or_default();
    call("GET", &format!("/api/storage{query}"), None).await
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
    format!("{GATEWAY_BASE}/api/videos?path={}", encode(path))
}
pub async fn fetch_recording_files(id: &str) -> Result<RecordingFiles, String> {
    call(
        "GET",
        &format!("/api/recordings/{}/files", encode(id)),
        None,
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

pub fn save_appearance(state: AppState) {
    let theme = state.theme.get_untracked();
    let accent = state.accent.get_untracked();
    if let Ok(Some(storage)) = window().local_storage() {
        let _ = storage.set_item(
            "streamcap.appearance",
            &json!({"theme":theme,"accent":accent}).to_string(),
        );
    }
    leptos::task::spawn_local(async move {
        match save_settings(json!({"theme_mode":theme,"theme_color":accent})).await {
            Ok(()) => {
                if let Ok(settings) = fetch_settings().await {
                    state.apply_settings(settings);
                }
            }
            Err(error) => state.fail(format!("外观已本地应用，配置同步失败：{error}")),
        }
    });
}

type EventHandler = (&'static str, Closure<dyn FnMut(web_sys::MessageEvent)>);

pub struct EventConnection {
    source: web_sys::EventSource,
    messages: Vec<EventHandler>,
    _open: Closure<dyn FnMut(web_sys::Event)>,
    _error: Closure<dyn FnMut(web_sys::Event)>,
    retry_timer: Rc<RefCell<Option<gloo_timers::callback::Timeout>>>,
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
    let source = web_sys::EventSource::new(&format!("{GATEWAY_BASE}/api/events")).ok()?;
    let retry_timer = Rc::new(RefCell::new(None::<gloo_timers::callback::Timeout>));
    let connected_timer = retry_timer.clone();
    let on_open = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        connected_timer.borrow_mut().take();
        state.events_connected.set(true);
        leptos::task::spawn_local(async move {
            if let Err(error) = refresh_recordings(state).await {
                state.error.set(Some(error));
            }
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
    let mut messages = Vec::new();
    for event_name in ["update", "delete", "settings"] {
        let handler = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(
            move |event: web_sys::MessageEvent| {
                let Some(text) = event.data().as_string() else {
                    return;
                };
                match event_name {
                    "update" => {
                        if let Ok(rec) = serde_json::from_str::<Recording>(&text) {
                            state.recordings.update(|list| {
                                if let Some(existing) =
                                    list.iter_mut().find(|r| r.rec_id == rec.rec_id)
                                {
                                    *existing = rec;
                                } else {
                                    list.push(rec);
                                }
                            });
                        }
                    }
                    "delete" => {
                        if let Ok(ids) = serde_json::from_str::<Vec<String>>(&text) {
                            state
                                .recordings
                                .update(|list| list.retain(|r| !ids.contains(&r.rec_id)));
                        }
                    }
                    "settings" => {
                        state.settings_version.update(|v| *v += 1);
                        leptos::task::spawn_local(async move {
                            if let Ok(settings) = fetch_settings().await {
                                state.apply_settings(settings);
                            }
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
    })
}
