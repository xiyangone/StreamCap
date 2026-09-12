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
use std::path::PathBuf;
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
        .route("/api/settings", get(get_settings).put(update_settings))
        .route("/api/cookies", get(get_cookies).put(update_cookies))
        .route("/api/storage", get(list_storage).delete(delete_storage))
        .route("/api/videos", get(stream_video))
        .route("/api/qr/kuaishou/start", post(qr_start))
        .route("/api/qr/kuaishou/status", get(qr_status))
        .route("/api/qr/kuaishou/cancel", post(qr_cancel))
        .route("/api/events", get(events))
        .layer(axum::middleware::from_fn_with_state(
            state.resolver.cancellation(),
            reject_when_stopping,
        ))
        .layer(
            tower_http::cors::CorsLayer::new()
                .allow_origin(
                    TRUSTED_UI_ORIGINS
                        .iter()
                        .map(|value| header::HeaderValue::from_static(value))
                        .collect::<Vec<_>>(),
                )
                .allow_methods([
                    axum::http::Method::GET,
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
        .with_state(state)
}

const TRUSTED_UI_ORIGINS: &[&str] = &[
    "http://tauri.localhost",
    "https://tauri.localhost",
    "tauri://localhost",
    "http://localhost:1420",
    "http://127.0.0.1:1420",
];

async fn reject_when_stopping(
    State(stop): State<tokio_util::sync::CancellationToken>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if request.headers().get(header::ORIGIN).is_some_and(|origin| {
        !origin
            .to_str()
            .ok()
            .is_some_and(|value| TRUSTED_UI_ORIGINS.contains(&value))
    }) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"detail":"不允许此网页访问本地 StreamCap 服务"})),
        )
            .into_response();
    }
    tokio::select! {biased;
        _=stop.cancelled()=>(StatusCode::SERVICE_UNAVAILABLE,Json(json!({"detail":"应用正在退出"}))).into_response(),
        response=next.run(request)=>response,
    }
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

async fn status(State(state): State<ApiState>) -> Json<Value> {
    let (total, active) = state.store.count().await;
    Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "activeRecordings": active,
        "totalRecordings": total,
        "resolverReady": state.resolver.healthy().await,
        "resolverMode": "native",
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
async fn list_recording_files(
    State(state): State<ApiState>,
    AxumPath(rec_id): AxumPath<String>,
) -> ApiResult {
    let Some(rec) = state.store.get(&rec_id).await else {
        return Err(ApiError::not_found(format!(
            "recording not found: {rec_id}"
        )));
    };

    let Some(dir) = rec.recording_dir.as_ref() else {
        return Ok(Json(json!({ "dir": Value::Null, "files": [] })));
    };

    let root = {
        let config = state.config.read().await;
        config.recordings_root()
    };
    let dir_path = std::path::PathBuf::from(dir);
    if !dir_path.is_dir() {
        return Ok(Json(json!({ "dir": dir, "files": [] })));
    }

    // 播放走 /api/videos?path=<相对录制根目录>
    let relative = dir_path
        .strip_prefix(&root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .ok();

    let mut files: Vec<Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let name = entry.file_name().to_string_lossy().to_string();
            let playable = relative
                .as_ref()
                .map(|base| format!("{base}/{name}"))
                .unwrap_or_else(|| name.clone());
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            files.push(json!({
                "name": name, "size": meta.len(),
                "path": playable, "modified": modified,
            }));
        }
    }

    // 最新产出的排在前面
    files.sort_by(|a, b| {
        let am = a.get("modified").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let bm = b.get("modified").and_then(|v| v.as_f64()).unwrap_or(0.0);
        bm.partial_cmp(&am).unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(Json(json!({ "dir": dir, "files": files })))
}

const EDIT_FIELDS: &[&str] = &[
    "url",
    "streamerName",
    "quality",
    "recordFormat",
    "segmentRecord",
    "segmentTime",
    "videoBitrate",
];
const BATCH_FIELDS: &[&str] = &[
    "quality",
    "recordFormat",
    "segmentRecord",
    "segmentTime",
    "videoBitrate",
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
            "onlyNotifyNoRecord" => rec.only_notify_no_record = value.as_bool(),
            "flvUseDirectDownload" => rec.flv_use_direct_download = value.as_bool(),
            "monitorHours" => rec.monitor_hours = value.as_i64().map(|v| v as i32),
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
    state.store.emit("settings", json!({ "changed": changed }));
    state.store.snack("设置已保存");
    Ok(Json(json!({ "ok": true, "changed": changed })))
}

async fn get_cookies(State(state): State<ApiState>) -> ApiResult {
    let config = state.config.read().await;
    let cookies = config
        .load_cookies()
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
            .map_err(|e| ApiError::internal(format!("写入 Cookie 失败: {e}")))?
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
    let root = {
        let config = state.config.read().await;
        config.recordings_root()
    };
    let target = match &query.subfolder {
        Some(sub) => root.join(sub),
        None => root.clone(),
    };

    if !target.exists() {
        return Ok(Json(
            json!({ "root": root.to_string_lossy(), "items": [], "totalSize": 0 }),
        ));
    }

    let mut items: Vec<Value> = Vec::new();
    let mut total: u64 = 0;

    let entries = std::fs::read_dir(&target).map_err(|e| ApiError::internal(e.to_string()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let relative = path
            .strip_prefix(&root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");

        if path.is_dir() {
            let size = dir_size(&path);
            total += size;
            items.push(json!({ "name": name, "isDir": true, "size": size, "path": relative }));
        } else {
            let meta = entry
                .metadata()
                .map_err(|e| ApiError::internal(e.to_string()))?;
            total += meta.len();
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64());
            items.push(json!({
                "name": name, "isDir": false, "size": meta.len(),
                "path": relative, "modified": modified,
            }));
        }
    }

    items.sort_by(|a, b| {
        let a_dir = a.get("isDir").and_then(|v| v.as_bool()).unwrap_or(false);
        let b_dir = b.get("isDir").and_then(|v| v.as_bool()).unwrap_or(false);
        b_dir.cmp(&a_dir).then_with(|| {
            a.get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or(""))
        })
    });

    Ok(Json(json!({
        "root": root.to_string_lossy(),
        "items": items,
        "totalSize": total,
    })))
}

fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let child = entry.path();
            if child.is_dir() {
                total += dir_size(&child);
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

#[derive(Debug, Deserialize)]
struct PathQuery {
    path: String,
}

async fn delete_storage(
    State(state): State<ApiState>,
    Query(query): Query<PathQuery>,
) -> ApiResult {
    let root = {
        let config = state.config.read().await;
        config.recordings_root()
    };
    let target =
        safe_join(&root, &query.path).ok_or_else(|| ApiError::bad_request("invalid path"))?;
    if !target.exists() {
        return Err(ApiError::not_found("file not found"));
    }

    let result = if target.is_dir() {
        std::fs::remove_dir_all(&target)
    } else {
        std::fs::remove_file(&target)
    };
    result.map_err(|e| ApiError::internal(format!("删除失败: {e}")))?;

    state.store.snack("已删除");
    Ok(Json(json!({ "ok": true })))
}

/// 防目录穿越：目标必须位于 root 之内。
fn safe_join(root: &std::path::Path, relative: &str) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let target = root.join(relative);
    // 目标可能尚不存在（父级规范化后仍需在 root 内）
    let normalized = target
        .parent()
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p.join(target.file_name().unwrap_or_default()))?;
    (normalized != root && normalized.starts_with(&root)).then_some(normalized)
}

async fn stream_video(
    State(state): State<ApiState>,
    Query(query): Query<PathQuery>,
    headers: HeaderMap,
) -> Response {
    let root = {
        let config = state.config.read().await;
        config.recordings_root()
    };
    let Some(target) = safe_join(&root, &query.path) else {
        return (StatusCode::BAD_REQUEST, "invalid path").into_response();
    };
    let Ok(data) = std::fs::read(&target) else {
        return (StatusCode::NOT_FOUND, "video not found").into_response();
    };

    let size = data.len();
    let mime = mime_for(&target);

    // Range 支持（<video> 拖动进度必需）
    if let Some(range) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        if let Some((start, end)) = parse_range(range, size) {
            let chunk = data[start..=end].to_vec();
            return Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_TYPE, mime)
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"))
                .header(header::CONTENT_LENGTH, chunk.len().to_string())
                .body(chunk.into())
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        }
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, size.to_string())
        .body(data.into())
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
        _ => "application/octet-stream",
    }
}

fn parse_range(header: &str, size: usize) -> Option<(usize, usize)> {
    let spec = header.strip_prefix("bytes=")?;
    let (start_raw, end_raw) = spec.split_once('-')?;
    let start: usize = start_raw.trim().parse().ok()?;
    let end: usize = if end_raw.trim().is_empty() {
        size.saturating_sub(1)
    } else {
        end_raw.trim().parse().ok()?
    };
    let end = end.min(size.saturating_sub(1));
    (start <= end && start < size).then_some((start, end))
}

// ---------------------------------------------------------------------------
// 扫码登录（代理到解析 sidecar）
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

    Ok(ApiState {
        store,
        engine,
        config,
        scheduler,
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
    tokio::join!(state.resolver.shutdown(), state.engine.stop_all(10));
    state.scheduler.finish_background().await;
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

        assert!(safe_join(root, "sub/a.mp4").is_some());
        assert!(safe_join(root, "../outside.mp4").is_none());
        assert!(safe_join(root, "sub/../../outside.mp4").is_none());
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
