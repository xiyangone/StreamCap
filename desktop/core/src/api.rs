//! Rust HTTP + SSE API。编辑契约使用 camelCase；用户任务文件保持 snake_case。

use crate::config::ConfigStore;
use crate::engine::Engine;
use crate::model::Recording;
use crate::paths::Workspace;
use crate::resolver::Resolver;
use crate::scheduler::Scheduler;
use crate::store::Store;
use axum::{
    extract::{Path as AxumPath, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{delete, get, post},
    Json, Router,
};
use futures::Stream;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::convert::Infallible;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

#[derive(Clone)]
pub struct ApiState {
    pub store: Store,
    pub engine: Engine,
    pub config: Arc<RwLock<ConfigStore>>,
    pub scheduler: Arc<Scheduler>,
    pub resolver: Resolver,
    pub workspace: Workspace,
    pub recording_enabled: Arc<AtomicBool>,
    pub storage: crate::storage::Storage,
    pub preview: crate::preview::Preview,
    pub tools: crate::tools::Tools,
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/status", get(status))
        .route(
            "/api/recordings",
            get(list_recordings).post(create_recordings),
        )
        .route("/api/recordings/delete", post(delete_many))
        .route("/api/recordings/batch-edit", post(batch_edit))
        .route(
            "/api/recordings/{rec_id}",
            delete(delete_one).put(update_one),
        )
        .route("/api/recordings/{rec_id}/monitor", post(toggle_monitor))
        .route("/api/recordings/{rec_id}/check", post(check_one))
        .route("/api/recordings/{rec_id}/start", post(start_recording))
        .route("/api/recordings/{rec_id}/stop", post(stop_recording))
        .route("/api/recordings/{rec_id}/files", get(list_recording_files))
        .route("/api/recordings/{rec_id}/preview", get(live_preview))
        .route("/api/settings", get(get_settings).put(update_settings))
        .route("/api/cookies", get(get_cookies).put(update_cookies))
        .route("/api/storage", get(list_storage).delete(delete_storage))
        .route("/api/videos", get(stream_video))
        .route("/api/media/info", get(media_info))
        .route("/api/media/preview", get(media_preview))
        .route("/api/media/jobs", get(media_jobs))
        .route("/api/media/transcode", get(media_transcode))
        .route(
            "/api/media/screenshot",
            post(save_screenshot).layer(axum::extract::DefaultBodyLimit::max(12 * 1024 * 1024)),
        )
        .route("/api/tools/status", get(tools_status))
        .route("/api/tools/install", post(install_tools))
        .route("/api/tools/update", get(check_update))
        .route("/api/accounts", get(account_summaries).put(save_account))
        .route("/api/automation/shutdown", post(schedule_shutdown))
        .route("/api/media/remux", post(remux_media))
        .route("/api/qr/kuaishou/start", post(qr_start))
        .route("/api/qr/kuaishou/status", get(qr_status))
        .route("/api/qr/kuaishou/cancel", post(qr_cancel))
        .route("/api/events", get(events))
        .layer(
            tower_http::cors::CorsLayer::new()
                .allow_origin(
                    crate::security::TRUSTED_UI_ORIGINS
                        .iter()
                        .map(|value| header::HeaderValue::from_static(value))
                        .collect::<Vec<_>>(),
                )
                .allow_methods([
                    axum::http::Method::GET,
                    axum::http::Method::HEAD,
                    axum::http::Method::POST,
                    axum::http::Method::PUT,
                    axum::http::Method::DELETE,
                    axum::http::Method::OPTIONS,
                ])
                .allow_headers([header::CONTENT_TYPE, header::RANGE])
                .expose_headers([
                    header::CONTENT_LENGTH,
                    header::CONTENT_RANGE,
                    header::ACCEPT_RANGES,
                ]),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.resolver.cancellation(),
            crate::security::guard,
        ))
        .with_state(state)
}

type ApiResult = Result<Json<Value>, ApiError>;

/// 统一错误响应：保留 HTTP 语义，消息回显给前端提示条。
pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "detail": self.1 }))).into_response()
    }
}

impl ApiError {
    fn not_found(msg: impl Into<String>) -> Self {
        Self(StatusCode::NOT_FOUND, msg.into())
    }
    fn bad_request(msg: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, msg.into())
    }
    fn storage(err: std::io::Error) -> Self {
        match err.kind() {
            std::io::ErrorKind::NotFound => Self::not_found(err.to_string()),
            std::io::ErrorKind::InvalidInput => Self::bad_request(err.to_string()),
            std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::WouldBlock => {
                Self(StatusCode::CONFLICT, err.to_string())
            }
            _ => Self::internal(format!("任务保存失败: {err}")),
        }
    }

    fn internal(msg: impl Into<String>) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, msg.into())
    }
}

// ---------------------------------------------------------------------------
// 状态与录制任务
// ---------------------------------------------------------------------------

async fn live_preview(
    State(state): State<ApiState>,
    AxumPath(rec_id): AxumPath<String>,
) -> Result<Response, ApiError> {
    let input = state
        .scheduler
        .preview_input(&rec_id)
        .await
        .map_err(ApiError::bad_request)?;
    let ffmpeg = crate::paths::find_ffmpeg(&state.workspace)
        .ok_or_else(|| ApiError::bad_request("直播预览需要 FFmpeg"))?;
    state
        .preview
        .live(ffmpeg, input)
        .await
        .map(IntoResponse::into_response)
        .map_err(ApiError::storage)
}

async fn status(State(state): State<ApiState>) -> Json<Value> {
    let (total, active) = state.store.count().await;
    Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "activeRecordings": active,
        "totalRecordings": total,
        "resolverReady": state.resolver.healthy().await,
        "resolverMode": "native",
        "pendingMediaJobs": state.scheduler.postprocess.pending(),
        "postprocessReady": state.scheduler.postprocess.ready(),
        "supportedPlatforms": Resolver::supported_platforms(),
    }))
}

async fn list_recordings(State(state): State<ApiState>) -> Json<Value> {
    let list = state.store.all().await;
    Json(serde_json::to_value(list).unwrap_or(Value::Array(vec![])))
}

#[derive(Debug, Deserialize)]
struct CreatePayload {
    #[serde(default)]
    url: Option<String>,
    #[serde(default, rename = "streamerName")]
    streamer_name: Option<String>,
    #[serde(default)]
    items: Option<Vec<CreateItem>>,
}

#[derive(Debug, Deserialize)]
struct CreateItem {
    url: String,
    #[serde(default)]
    quality: Option<String>,
    #[serde(default, rename = "streamerName")]
    streamer_name: Option<String>,
}

async fn create_recordings(
    State(state): State<ApiState>,
    Json(payload): Json<CreatePayload>,
) -> ApiResult {
    let items: Vec<CreateItem> = match (payload.items, payload.url) {
        (Some(items), _) if !items.is_empty() => items,
        (_, Some(url)) => vec![CreateItem {
            url,
            streamer_name: payload.streamer_name,
            quality: None,
        }],
        _ => return Err(ApiError::bad_request("url is required")),
    };

    let config = state.config.read().await;
    let mut created: Vec<Recording> = Vec::new();

    for item in items {
        let url = item.url.trim().to_string();
        validate_url(&url)?;
        if let Some(quality) = &item.quality {
            validate_quality(quality)?;
        }

        let mut rec = Recording::new(
            uuid::Uuid::new_v4().to_string(),
            url.clone(),
            item.streamer_name.unwrap_or_default().trim().to_string(),
        );
        rec.quality = item.quality;
        crate::store::apply_defaults(&mut rec, &config);
        created.push(rec);
    }
    drop(config);

    state
        .store
        .insert(created.clone())
        .await
        .map_err(ApiError::storage)?;
    state
        .store
        .snack(format!("已添加 {} 个录制任务", created.len()));

    Ok(Json(json!({ "created": created })))
}

async fn delete_one(
    State(state): State<ApiState>,
    AxumPath(rec_id): AxumPath<String>,
) -> ApiResult {
    let removed = state
        .store
        .remove(std::slice::from_ref(&rec_id))
        .await
        .map_err(ApiError::storage)?;
    if removed == 0 {
        return Err(ApiError::not_found(format!(
            "recording not found: {rec_id}"
        )));
    }
    state.store.snack("已删除录制任务");
    Ok(Json(json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
struct DeleteManyPayload {
    #[serde(default, rename = "recIds")]
    rec_ids: Vec<String>,
}

async fn delete_many(
    State(state): State<ApiState>,
    Json(payload): Json<DeleteManyPayload>,
) -> ApiResult {
    let removed = state
        .store
        .remove(&payload.rec_ids)
        .await
        .map_err(ApiError::storage)?;
    Ok(Json(json!({ "ok": true, "removed": removed })))
}

async fn toggle_monitor(
    State(state): State<ApiState>,
    AxumPath(rec_id): AxumPath<String>,
) -> ApiResult {
    let Some(rec) = state.store.get(&rec_id).await else {
        return Err(ApiError::not_found(format!(
            "recording not found: {rec_id}"
        )));
    };

    let now_monitoring = !rec.monitor_status;
    state
        .store
        .update(&rec_id, |r| {
            r.monitor_status = now_monitoring;
        })
        .await;

    if !now_monitoring {
        state.scheduler.stop_recording(&rec_id).await;
    }

    state.store.persist().await.map_err(ApiError::storage)?;
    let updated = state.store.get(&rec_id).await;
    Ok(Json(serde_json::to_value(updated).unwrap_or(Value::Null)))
}

async fn check_one(State(state): State<ApiState>, AxumPath(rec_id): AxumPath<String>) -> ApiResult {
    match state.scheduler.check(rec_id.clone()).await {
        Ok(outcome) => {
            if let crate::scheduler::CheckOutcome::Failed(ref err) = outcome {
                return Err(ApiError::bad_request(err.clone()));
            }
            state.store.snack(outcome.message());
            let updated = state.store.get(&rec_id).await;
            Ok(Json(serde_json::to_value(updated).unwrap_or(Value::Null)))
        }
        Err(err) => Err(ApiError::not_found(err)),
    }
}

#[derive(Debug, Deserialize)]
struct UpdatePayload {
    #[serde(default)]
    changes: Map<String, Value>,
    #[serde(default, rename = "followGlobal")]
    follow_global: Vec<String>,
}

/// 手动开始录制（对应源项目卡片的「开始录制」按钮）。
async fn start_recording(
    State(state): State<ApiState>,
    AxumPath(rec_id): AxumPath<String>,
) -> ApiResult {
    match state.scheduler.force_start(rec_id.clone()).await {
        Ok(outcome) => {
            state.store.snack(outcome.message());
            let updated = state.store.get(&rec_id).await;
            Ok(Json(serde_json::to_value(updated).unwrap_or(Value::Null)))
        }
        Err(err) => Err(ApiError::bad_request(err)),
    }
}

/// 停止录制（对应源项目卡片的「停止录制」按钮）。
async fn stop_recording(
    State(state): State<ApiState>,
    AxumPath(rec_id): AxumPath<String>,
) -> ApiResult {
    if state.store.get(&rec_id).await.is_none() {
        return Err(ApiError::not_found(format!(
            "recording not found: {rec_id}"
        )));
    }
    state.scheduler.stop_recording(&rec_id).await;
    state.store.snack("已停止录制");
    let updated = state.store.get(&rec_id).await;
    Ok(Json(serde_json::to_value(updated).unwrap_or(Value::Null)))
}

/// 列出某个任务已产出的录制文件，供卡片「预览」使用。
fn is_media_name(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "ts" | "mp4"
                    | "m4v"
                    | "flv"
                    | "mkv"
                    | "mov"
                    | "webm"
                    | "nut"
                    | "mp3"
                    | "m4a"
                    | "aac"
                    | "wav"
                    | "wma"
                    | "ogg"
                    | "ogv"
            )
        })
}
async fn list_recording_files(
    State(state): State<ApiState>,
    AxumPath(rec_id): AxumPath<String>,
) -> ApiResult {
    let rec = state
        .store
        .get(&rec_id)
        .await
        .ok_or_else(|| ApiError::not_found("recording not found"))?;
    let Some(dir) = rec.recording_dir else {
        return Ok(Json(json!({"dir":Value::Null,"files":[]})));
    };
    let root = state.config.read().await.recordings_root();
    let directory = std::path::PathBuf::from(&dir);
    let mut files=state.storage.blocking(move|stop|{
        crate::storage::check_cancel(&stop)?;
        if !directory.exists(){return Ok(Vec::<Value>::new())}
        let root_real=root.canonicalize()?;let dir_real=directory.canonicalize()?;
        let relative=dir_real.strip_prefix(&root_real).map_err(|_|std::io::Error::new(std::io::ErrorKind::InvalidInput,"此任务目录不在当前录制根目录内"))?.to_string_lossy().replace('\\',"/");
        for ancestor in directory.ancestors(){
            if ancestor==root{break}
            if crate::storage::is_link(&std::fs::symlink_metadata(ancestor)?){return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput,"不能通过链接预览录制文件"))}
            if ancestor.canonicalize()?==root_real{break}
        }
        let listing=crate::storage::list(&root,&relative,&stop)?;
        Ok(listing.items.into_iter().filter(|item|!item.is_dir && is_media_name(&item.name)).map(|item|json!({"name":item.name,"size":item.size,"path":item.path,"modified":item.modified.unwrap_or(0.0)})).collect::<Vec<_>>())
    }).await.map_err(ApiError::storage)?;
    files.sort_by(|a, b| {
        b["modified"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&a["modified"].as_f64().unwrap_or(0.0))
    });
    Ok(Json(json!({"dir":dir,"files":files})))
}

const EDIT_FIELDS: &[&str] = &[
    "url",
    "streamerName",
    "quality",
    "recordFormat",
    "segmentRecord",
    "segmentTime",
    "videoBitrate",
    "scheduledRecording",
    "scheduledStartTime",
    "monitorHours",
    "enabledMessagePush",
    "onlyNotifyNoRecord",
    "flvUseDirectDownload",
];
const BATCH_FIELDS: &[&str] = &[
    "quality",
    "recordFormat",
    "segmentRecord",
    "segmentTime",
    "videoBitrate",
    "scheduledRecording",
    "scheduledStartTime",
    "monitorHours",
    "enabledMessagePush",
    "onlyNotifyNoRecord",
    "flvUseDirectDownload",
];

fn validate_url(value: &str) -> Result<(), ApiError> {
    let parsed = reqwest::Url::parse(value)
        .map_err(|_| ApiError::bad_request("请输入完整的 HTTP 或 HTTPS 直播间地址"))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(ApiError::bad_request(
            "请输入不含账号密码的 HTTP 或 HTTPS 直播间地址",
        ));
    }
    Ok(())
}

fn validate_quality(value: &str) -> Result<(), ApiError> {
    if !["OD", "UHD", "HD", "SD", "LD"].contains(&value) {
        return Err(ApiError::bad_request("不支持的清晰度"));
    }
    Ok(())
}

fn validate_changes(
    changes: &Map<String, Value>,
    follow: &[String],
    allowed: &[&str],
) -> Result<(), ApiError> {
    for (key, value) in changes {
        if !allowed.contains(&key.as_str()) {
            return Err(ApiError::bad_request(format!("不支持的编辑字段: {key}")));
        }
        match key.as_str() {
            "url" => validate_url(
                value
                    .as_str()
                    .ok_or_else(|| ApiError::bad_request("地址必须是文本"))?,
            )?,
            "quality" => validate_quality(value.as_str().unwrap_or(""))?,
            "streamerName" if !value.is_string() => {
                return Err(ApiError::bad_request("主播名称必须是文本"))
            }
            "recordFormat"
                if !crate::engine::SUPPORTED_RECORD_FORMATS
                    .contains(&value.as_str().unwrap_or("")) =>
            {
                return Err(ApiError::bad_request("不支持的录制格式"))
            }
            "scheduledRecording"
            | "enabledMessagePush"
            | "onlyNotifyNoRecord"
            | "flvUseDirectDownload"
                if !value.is_boolean() =>
            {
                return Err(ApiError::bad_request("开关必须为布尔值"));
            }
            "scheduledStartTime" | "monitorHours" if !value.is_string() => {
                return Err(ApiError::bad_request("定时字段必须是文本"));
            }
            "segmentRecord" if !value.is_boolean() => {
                return Err(ApiError::bad_request("分段录制必须是布尔值"))
            }
            "segmentTime"
                if value
                    .as_str()
                    .and_then(|s| s.parse::<u64>().ok())
                    .filter(|n| *n > 0 && *n <= 86400)
                    .is_none() =>
            {
                return Err(ApiError::bad_request("分段时长应为 1–86400 秒"))
            }
            "videoBitrate"
                if !value.is_null()
                    && value
                        .as_i64()
                        .filter(|n| *n > 0 && *n <= 1_000_000)
                        .is_none() =>
            {
                return Err(ApiError::bad_request("码率应为正整数，或留空复制源流"))
            }
            _ => {}
        }
    }
    for field in follow {
        if !crate::model::INHERITABLE_FIELDS
            .iter()
            .any(|(name, _)| name == field)
            || !allowed.contains(&to_camel(field).as_str())
        {
            return Err(ApiError::bad_request(format!("不支持的继承字段: {field}")));
        }
        if changes.contains_key(&to_camel(field)) {
            return Err(ApiError::bad_request("同一字段不能同时覆盖并跟随全局"));
        }
    }
    if changes.is_empty() && follow.is_empty() {
        return Err(ApiError::bad_request("没有需要保存的修改"));
    }
    Ok(())
}

fn apply_edit(
    rec: &mut Recording,
    changes: &Map<String, Value>,
    follow: &[String],
    allowed: &[&str],
    config: &ConfigStore,
) {
    apply_changes(rec, changes, allowed);
    rec.inherited_fields
        .retain(|field| !changes.contains_key(&to_camel(field)));
    for field in follow {
        if !rec.is_inherited(field) {
            rec.inherited_fields.push(field.clone());
        }
    }
    crate::store::apply_defaults(rec, config);
}

async fn update_one(
    State(state): State<ApiState>,
    AxumPath(rec_id): AxumPath<String>,
    Json(payload): Json<UpdatePayload>,
) -> ApiResult {
    validate_changes(&payload.changes, &payload.follow_global, EDIT_FIELDS)?;
    let config = state.config.read().await;
    let updated = state
        .store
        .edit_many(&[rec_id], |rec| {
            apply_edit(
                rec,
                &payload.changes,
                &payload.follow_global,
                EDIT_FIELDS,
                &config,
            );
        })
        .await
        .map_err(ApiError::storage)?;
    Ok(Json(
        serde_json::to_value(&updated[0]).map_err(|e| ApiError::internal(e.to_string()))?,
    ))
}

fn apply_changes(rec: &mut Recording, changes: &Map<String, Value>, allowed: &[&str]) {
    for (key, value) in changes {
        if !allowed.contains(&key.as_str()) {
            continue;
        }
        match key.as_str() {
            "url" => set_string(&mut rec.url, value),
            "streamerName" => set_string(&mut rec.streamer_name, value),
            "quality" => set_opt_string(&mut rec.quality, value),
            "recordFormat" => set_opt_string(&mut rec.record_format, value),
            "segmentTime" => set_opt_string(&mut rec.segment_time, value),
            "scheduledStartTime" => set_opt_string(&mut rec.scheduled_start_time, value),
            "segmentRecord" => rec.segment_record = value.as_bool(),
            "monitorStatus" => rec.monitor_status = value.as_bool().unwrap_or(rec.monitor_status),
            "scheduledRecording" => rec.scheduled_recording = value.as_bool(),
            "enabledMessagePush" => rec.enabled_message_push = value.as_bool(),
            "onlyNotifyNoRecord" => rec.only_notify_no_record = value.as_bool(),
            "flvUseDirectDownload" => rec.flv_use_direct_download = value.as_bool(),
            "monitorHours" => rec.monitor_hours = value.as_str().map(str::to_owned),
            "videoBitrate" => rec.video_bitrate = value.as_i64(),
            _ => {}
        }
    }
}

fn set_string(slot: &mut String, value: &Value) {
    if let Some(text) = value.as_str() {
        *slot = text.to_string();
    }
}

fn set_opt_string(slot: &mut Option<String>, value: &Value) {
    *slot = value
        .as_str()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
}

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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BatchEditPayload {
    rec_ids: Vec<String>,
    #[serde(default)]
    changes: Map<String, Value>,
    #[serde(default)]
    follow_global: Vec<String>,
}

async fn batch_edit(
    State(state): State<ApiState>,
    Json(payload): Json<BatchEditPayload>,
) -> ApiResult {
    validate_changes(&payload.changes, &payload.follow_global, BATCH_FIELDS)?;
    let config = state.config.read().await;
    let updated = state
        .store
        .edit_many(&payload.rec_ids, |rec| {
            apply_edit(
                rec,
                &payload.changes,
                &payload.follow_global,
                BATCH_FIELDS,
                &config,
            );
        })
        .await
        .map_err(ApiError::storage)?;
    Ok(Json(json!({ "updated": updated.len() })))
}

// ---------------------------------------------------------------------------
// 设置与 Cookie
// ---------------------------------------------------------------------------

async fn get_settings(State(state): State<ApiState>) -> Json<Value> {
    let config = state.config.read().await;
    Json(json!({
        "userConfig": config.user_config(),
        "defaultConfig": config.default_config(),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsPayload {
    #[serde(rename = "userConfig")]
    user_config: Map<String, Value>,
}

async fn update_settings(
    State(state): State<ApiState>,
    Json(payload): Json<SettingsPayload>,
) -> ApiResult {
    let changed = {
        let mut config = state.config.write().await;
        config
            .update_user_config(payload.user_config)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::InvalidInput {
                    ApiError::bad_request(e.to_string())
                } else {
                    ApiError::internal(format!("写入设置失败: {e}"))
                }
            })?
    };

    let config = state.config.read().await;
    crate::store::apply_global_defaults(&state.store, &config).await;
    drop(config);
    if changed.iter().any(|key| key == "loop_time_seconds") {
        state.scheduler.refresh_interval();
    }
    state.store.emit("settings", json!({ "changed": changed }));
    state.store.snack("设置已保存");
    Ok(Json(json!({ "ok": true, "changed": changed })))
}

async fn get_cookies(State(state): State<ApiState>) -> ApiResult {
    let config = state.config.read().await;
    let cookies = config
        .cookie_view()
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(json!({ "cookies": cookies })))
}

#[derive(Debug, Deserialize)]
struct CookiesPayload {
    #[serde(default)]
    cookies: Map<String, Value>,
}

async fn update_cookies(
    State(state): State<ApiState>,
    Json(payload): Json<CookiesPayload>,
) -> ApiResult {
    let changed = {
        let mut config = state.config.write().await;
        config
            .update_cookies(payload.cookies)
            .map_err(ApiError::storage)?
    };
    state.store.snack("Cookie 已保存");
    Ok(Json(json!({ "ok": true, "changed": changed })))
}

// ---------------------------------------------------------------------------
// 存储
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SubfolderQuery {
    #[serde(default)]
    subfolder: Option<String>,
}

async fn list_storage(
    State(state): State<ApiState>,
    Query(query): Query<SubfolderQuery>,
) -> ApiResult {
    let root = state.config.read().await.recordings_root();
    let sub = query.subfolder.unwrap_or_default();
    let listing = state
        .storage
        .blocking(move |stop| crate::storage::list(&root, &sub, &stop))
        .await
        .map_err(ApiError::storage)?;
    Ok(Json(
        serde_json::to_value(listing).map_err(|e| ApiError::internal(e.to_string()))?,
    ))
}
#[derive(Debug, Deserialize)]
struct PathQuery {
    path: String,
}
async fn delete_storage(
    State(state): State<ApiState>,
    Query(query): Query<PathQuery>,
) -> ApiResult {
    let root = state.config.read().await.recordings_root();
    let relative = query.path;
    let guard = state.engine.filesystem_guard().await;
    let checked_root = root.clone();
    let checked_relative = relative.clone();
    let target = state
        .storage
        .blocking(move |_| crate::storage::checked_target(&checked_root, &checked_relative, false))
        .await
        .map_err(ApiError::storage)?;
    if state.engine.protects_path(&target).await {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "正在录制的文件及其父目录不能回收".into(),
        ));
    }
    let receipt = state
        .storage
        .blocking(move |stop| {
            let _guard = guard;
            crate::storage::recycle(&root, &relative, &stop)
        })
        .await
        .map_err(ApiError::storage)?;
    state.store.snack("已移至回收站");
    Ok(Json(
        json!({"ok":true,"recycled":receipt.recycled,"recycledTo":receipt.recycled_to}),
    ))
}

async fn media_info(State(state): State<ApiState>, Query(query): Query<PathQuery>) -> ApiResult {
    let root = state.config.read().await.recordings_root();
    let info = state
        .preview
        .info(&state.storage, root, query.path)
        .await
        .map_err(ApiError::storage)?;
    Ok(Json(
        serde_json::to_value(info).map_err(|_| ApiError::internal("预览信息序列化失败"))?,
    ))
}
async fn media_preview(State(state): State<ApiState>, Query(query): Query<PathQuery>) -> Response {
    let root = state.config.read().await.recordings_root();
    match state.preview.stream(&state.storage, root, query.path).await {
        Ok(response) => response,
        Err(error) => ApiError::storage(error).into_response(),
    }
}
async fn media_jobs(State(state): State<ApiState>) -> Json<Value> {
    Json(
        json!({"jobs":state.scheduler.postprocess.jobs(),"ready":state.scheduler.postprocess.ready(),"activePreviews":state.preview.active()}),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemuxPayload {
    path: String,
    #[serde(default, rename = "deleteOriginal")]
    delete_original: bool,
}
async fn remux_media(
    State(state): State<ApiState>,
    Json(payload): Json<RemuxPayload>,
) -> ApiResult {
    let root = state.config.read().await.recordings_root();
    let job = state
        .scheduler
        .postprocess
        .enqueue_manual(root, &payload.path, payload.delete_original)
        .await
        .map_err(ApiError::storage)?;
    Ok(Json(json!({"job":job})))
}

async fn media_transcode(
    State(state): State<ApiState>,
    Query(query): Query<PathQuery>,
) -> Response {
    let config = state.config.read().await;
    let root = config.recordings_root();
    let ffmpeg = crate::paths::find_ffmpeg(config.workspace());
    drop(config);
    let Some(ffmpeg) = ffmpeg else {
        return ApiError::bad_request("此格式预览需要 FFmpeg").into_response();
    };
    match state
        .preview
        .transcode(&state.storage, root, query.path, ffmpeg)
        .await
    {
        Ok(response) => response,
        Err(error) => ApiError::storage(error).into_response(),
    }
}
async fn tools_status(State(state): State<ApiState>) -> Json<Value> {
    let config = state.config.read().await;
    let path = crate::paths::find_ffmpeg(config.workspace());
    Json(
        json!({"installation":state.tools.status(),"ffmpegReady":path.is_some(),"ffprobeReady":path.as_ref().is_some_and(|p|crate::paths::adjacent_ffprobe(p).is_some())}),
    )
}
async fn install_tools(State(state): State<ApiState>) -> ApiResult {
    state.tools.install().map_err(ApiError::bad_request)?;
    Ok(Json(json!({"ok":true})))
}
async fn check_update() -> ApiResult {
    let release = crate::tools::check_update()
        .await
        .map_err(ApiError::bad_request)?;
    Ok(Json(
        json!({"release":release,"currentVersion":env!("CARGO_PKG_VERSION")}),
    ))
}
async fn account_summaries(State(state): State<ApiState>) -> ApiResult {
    let value = state
        .config
        .read()
        .await
        .account_summaries()
        .map_err(ApiError::storage)?;
    Ok(Json(value))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountPayload {
    platform: String,
    changes: Map<String, Value>,
}
async fn save_account(
    State(state): State<ApiState>,
    Json(payload): Json<AccountPayload>,
) -> ApiResult {
    state
        .config
        .write()
        .await
        .save_account(&payload.platform, &payload.changes)
        .map_err(ApiError::storage)?;
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShutdownPayload {
    hours: Option<f64>,
}
async fn schedule_shutdown(
    State(state): State<ApiState>,
    Json(payload): Json<ShutdownPayload>,
) -> ApiResult {
    if let Some(hours) = payload.hours {
        if !hours.is_finite() || hours <= 0.0 || hours > 168.0 {
            return Err(ApiError::bad_request(
                "关机倒计时应大于 0 且不超过 168 小时",
            ));
        }
        state
            .config
            .write()
            .await
            .update_user_config(
                json!({"quick_shutdown_hours":hours.to_string()})
                    .as_object()
                    .expect("object")
                    .clone(),
            )
            .map_err(ApiError::storage)?;
        state
            .store
            .emit("settings", json!({"changed":["quick_shutdown_hours"]}));
    }
    state
        .scheduler
        .automation
        .quick_shutdown(payload.hours)
        .map_err(ApiError::bad_request)?;
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScreenshotPayload {
    path: String,
    png_base64: String,
}
fn validate_screenshot(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("截图超过大小限制".into());
    }
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_limits(png::Limits {
        bytes: 32 * 1024 * 1024,
    });
    let mut reader = decoder.read_info().map_err(|_| "截图 PNG 无效")?;
    let info = reader.info();
    if info.width == 0
        || info.height == 0
        || info.width > 4096
        || info.height > 4096
        || u64::from(info.width) * u64::from(info.height) > 4 * 1024 * 1024
    {
        return Err("截图尺寸超过限制".into());
    }
    let size = reader
        .output_buffer_size()
        .filter(|size| *size <= 32 * 1024 * 1024)
        .ok_or("截图解码尺寸无效")?;
    let mut buffer = vec![0; size];
    reader
        .next_frame(&mut buffer)
        .map_err(|_| "截图 PNG 解码失败")?;
    Ok(())
}
async fn save_screenshot(
    State(state): State<ApiState>,
    Json(payload): Json<ScreenshotPayload>,
) -> ApiResult {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&payload.png_base64)
        .map_err(|_| ApiError::bad_request("截图编码无效"))?;
    validate_screenshot(&bytes).map_err(ApiError::bad_request)?;
    let root = state.config.read().await.recordings_root();
    let saved = state
        .storage
        .blocking(move |stop| {
            crate::storage::check_cancel(&stop)?;
            let source = crate::storage::checked_target(&root, &payload.path, false)?;
            let parent = source
                .parent()
                .ok_or_else(|| std::io::Error::other("截图目录无效"))?;
            let directory = parent.join("screenshots");
            if directory.exists() {
                if crate::storage::is_link(&std::fs::symlink_metadata(&directory)?) {
                    return Err(std::io::Error::other("截图目录不能是链接"));
                }
            } else {
                std::fs::create_dir(&directory)?;
            }
            let output = directory.join(format!(
                "截图_{}_{}.png",
                chrono::Local::now().format("%Y%m%d-%H%M%S"),
                uuid::Uuid::new_v4()
            ));
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&output)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            Ok(output.to_string_lossy().to_string())
        })
        .await
        .map_err(ApiError::storage)?;
    Ok(Json(json!({"path":saved})))
}
async fn stream_video(
    State(state): State<ApiState>,
    Query(query): Query<PathQuery>,
    method: axum::http::Method,
    headers: HeaderMap,
) -> Response {
    use futures::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let root = state.config.read().await.recordings_root();
    let opened = state
        .storage
        .blocking(move |stop| {
            crate::storage::check_cancel(&stop)?;
            let target = crate::storage::checked_target(&root, &query.path, false)?;
            let file = crate::storage::open_file(&root, &query.path)?;
            let size = file.metadata()?.len();
            Ok((file, size, mime_for(&target)))
        })
        .await;
    let (file, size, mime) = match opened {
        Ok(v) => v,
        Err(e) => return ApiError::storage(e).into_response(),
    };
    let range = if method == axum::http::Method::HEAD {
        None
    } else {
        headers.get(header::RANGE)
    };
    let selected = match range {
        Some(value) => match value.to_str().ok().and_then(|v| parse_range(v, size)) {
            Some(r) => Some(r),
            None => {
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                    .header(header::ACCEPT_RANGES, "bytes")
                    .header(header::CONTENT_LENGTH, "0")
                    .body(axum::body::Body::empty())
                    .unwrap()
            }
        },
        None => None,
    };
    let (start, length) = selected.map(|(s, e)| (s, e - s + 1)).unwrap_or((0, size));
    let mut file = tokio::fs::File::from_std(file);
    if start != 0 {
        if let Err(e) = file.seek(std::io::SeekFrom::Start(start)).await {
            return ApiError::storage(e).into_response();
        }
    }
    let mut response = Response::builder()
        .status(if selected.is_some() {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(header::CONTENT_TYPE, mime)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, length.to_string());
    if let Some((start, end)) = selected {
        response = response.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"));
    }
    let body = if method == axum::http::Method::HEAD || length == 0 {
        axum::body::Body::empty()
    } else {
        let stream = tokio_util::io::ReaderStream::with_capacity(file.take(length), 64 * 1024)
            .take_until(state.resolver.cancellation().cancelled_owned());
        axum::body::Body::from_stream(stream)
    };
    response
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn mime_for(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .as_deref()
    {
        Some("mp4") | Some("m4v") => "video/mp4",
        Some("ts") => "video/mp2t",
        Some("mkv") => "video/x-matroska",
        Some("flv") => "video/x-flv",
        Some("mov") => "video/quicktime",
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("webm") => "video/webm",
        Some("wav") => "audio/wav",
        Some("aac") => "audio/aac",
        Some("ogg") => "audio/ogg",
        Some("ogv") => "video/ogg",
        Some("flac") => "audio/flac",
        _ => "application/octet-stream",
    }
}

fn parse_range(header: &str, size: u64) -> Option<(u64, u64)> {
    if size == 0 {
        return None;
    }
    let spec = header.strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    if start.trim().is_empty() {
        let count: u64 = end.trim().parse().ok()?;
        return (count > 0).then_some((size.saturating_sub(count), size - 1));
    }
    let start: u64 = start.trim().parse().ok()?;
    let end = if end.trim().is_empty() {
        size - 1
    } else {
        end.trim().parse::<u64>().ok()?.min(size - 1)
    };
    (start < size && start <= end).then_some((start, end))
}

// ---------------------------------------------------------------------------
// 进程内扫码登录
// ---------------------------------------------------------------------------

async fn qr_start(State(state): State<ApiState>) -> ApiResult {
    let proxy = {
        let config = state.config.read().await;
        if config.get_bool("enable_proxy", false) {
            Some(config.get_str("proxy_address", ""))
        } else {
            None
        }
    };
    match state.resolver.qr_start(proxy.as_deref()).await {
        Ok(snapshot) => Ok(Json(serde_json::to_value(snapshot).unwrap_or(Value::Null))),
        Err(err) => Err(ApiError::internal(err)),
    }
}

async fn qr_status(State(state): State<ApiState>, Query(query): Query<QrQuery>) -> ApiResult {
    match state.resolver.qr_status(&query.session_id).await {
        Ok(snapshot) => Ok(Json(serde_json::to_value(snapshot).unwrap_or(Value::Null))),
        Err(err) => Err(ApiError::internal(err)),
    }
}

async fn qr_cancel(State(state): State<ApiState>, Query(query): Query<QrQuery>) -> ApiResult {
    state
        .resolver
        .qr_cancel(&query.session_id)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
struct QrQuery {
    #[serde(rename = "sessionId")]
    session_id: String,
}

// ---------------------------------------------------------------------------
// 事件流
// ---------------------------------------------------------------------------

async fn events(
    State(state): State<ApiState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let receiver = state.store.subscribe();

    let cancel = state.resolver.cancellation();
    let stream = futures::stream::unfold((receiver, cancel), |(mut rx, cancel)| async move {
        loop {
            let event =
                tokio::select! {biased;_=cancel.cancelled()=>return None,event=rx.recv()=>event};
            match event {
                Ok(event) => {
                    let payload =
                        serde_json::to_string(&event.payload).unwrap_or_else(|_| "null".into());
                    return Some((
                        Ok(Event::default().event(event.topic).data(payload)),
                        (rx, cancel),
                    ));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ---------------------------------------------------------------------------
// 服务启动
// ---------------------------------------------------------------------------

/// 组装一个完整的后端状态（配置、存储、解析器、引擎、调度器）。
pub async fn bootstrap(workspace: Workspace) -> anyhow::Result<ApiState> {
    bootstrap_with_resolver(workspace, Resolver::new()).await
}

pub async fn bootstrap_with_resolver(
    workspace: Workspace,
    resolver: Resolver,
) -> anyhow::Result<ApiState> {
    workspace.ensure_ready()?;

    let config = ConfigStore::load(workspace.clone())?;
    let store = Store::new(workspace.clone());
    let loaded = store.load().await?;
    log::info!("已载入 {loaded} 个录制任务");

    // 用全局设置补齐运行时生效值（磁盘上仍是 null，保持「跟随全局」语义）
    crate::store::apply_global_defaults(&store, &config).await;

    let engine = Engine::new();
    let recording_enabled = Arc::new(AtomicBool::new(true));
    let config = Arc::new(RwLock::new(config));

    let ffmpeg = crate::paths::find_ffmpeg(&workspace);
    if ffmpeg.is_none() {
        log::warn!("未找到 ffmpeg，录制功能将不可用");
    }

    let scheduler = Arc::new(Scheduler::new(
        store.clone(),
        engine.clone(),
        config.clone(),
        resolver.clone(),
        ffmpeg,
        recording_enabled.clone(),
    ));

    let tools = crate::tools::Tools::new(workspace.clone());
    let preview = crate::preview::Preview::new(engine.clone(), resolver.cancellation());
    Ok(ApiState {
        store,
        engine,
        config,
        scheduler,
        storage: crate::storage::Storage::new(resolver.cancellation()),
        preview,
        tools,
        resolver,
        workspace,
        recording_enabled,
    })
}

/// 退出前清理：停止全部录制。
pub async fn shutdown(state: &ApiState) -> std::io::Result<()> {
    state
        .recording_enabled
        .store(false, std::sync::atomic::Ordering::SeqCst);
    state.engine.begin_shutdown();
    state.resolver.begin_shutdown();
    tokio::join!(
        state.resolver.shutdown(),
        state.scheduler.stop_all_for_shutdown()
    );
    state.scheduler.finish_background().await;
    state.preview.shutdown().await;
    state.tools.shutdown().await;
    state.storage.shutdown().await;
    for rec in state.store.all().await {
        state
            .store
            .update(&rec.rec_id, |r| {
                r.is_recording = false;
                r.speed = None;
            })
            .await;
    }
    state.store.persist().await?;
    log::info!("原生后端退出收尾完成");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_parsing_handles_open_ended_and_clamped() {
        assert_eq!(parse_range("bytes=0-499", 1000), Some((0, 499)));
        assert_eq!(parse_range("bytes=500-", 1000), Some((500, 999)));
        assert_eq!(parse_range("bytes=0-99999", 1000), Some((0, 999)));
        assert_eq!(parse_range("bytes=1000-1200", 1000), None);
        assert_eq!(parse_range("garbage", 1000), None);
    }

    #[test]
    fn safe_join_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("a.mp4"), b"x").unwrap();

        assert!(crate::storage::checked_target(root, "sub/a.mp4", false).is_ok());
        assert!(crate::storage::checked_target(root, "../outside.mp4", false).is_err());
        assert!(crate::storage::checked_target(root, "sub/../../outside.mp4", false).is_err());
    }

    #[test]
    fn mime_mapping_covers_recorded_formats() {
        assert_eq!(mime_for(std::path::Path::new("a.mp4")), "video/mp4");
        assert_eq!(mime_for(std::path::Path::new("a.TS")), "video/mp2t");
        assert_eq!(
            mime_for(std::path::Path::new("a.unknown")),
            "application/octet-stream"
        );
    }

    #[test]
    fn camel_conversion_for_change_keys() {
        assert_eq!(to_camel("record_format"), "recordFormat");
        assert_eq!(to_camel("only_notify_no_record"), "onlyNotifyNoRecord");
    }

    #[test]
    fn apply_changes_ignores_unknown_and_respects_allowed() {
        let mut rec = Recording::new("id".into(), "u".into(), "n".into());
        let mut changes = Map::new();
        changes.insert("quality".into(), json!("HD"));
        changes.insert("evilField".into(), json!("x"));

        apply_changes(&mut rec, &changes, &["quality"]);
        assert_eq!(rec.quality.as_deref(), Some("HD"));
    }

    #[test]
    fn empty_string_change_clears_option_to_follow_global() {
        let mut rec = Recording::new("id".into(), "u".into(), "n".into());
        rec.quality = Some("HD".into());
        let mut changes = Map::new();
        changes.insert("quality".into(), json!(""));

        apply_changes(&mut rec, &changes, &["quality"]);
        assert!(rec.quality.is_none());
    }
}
