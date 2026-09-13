//! Browser-to-loopback boundary. No remote Host/Origin is trusted.
use axum::{
    extract::{Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;
#[cfg(debug_assertions)]
pub const TRUSTED_UI_ORIGINS: &[&str] = &[
    "http://tauri.localhost",
    "https://tauri.localhost",
    "tauri://localhost",
    "http://localhost:1420",
    "http://127.0.0.1:1420",
];
#[cfg(not(debug_assertions))]
pub const TRUSTED_UI_ORIGINS: &[&str] = &[
    "http://tauri.localhost",
    "https://tauri.localhost",
    "tauri://localhost",
];
pub fn loopback_host(value: &str) -> bool {
    let Ok(authority) = value.parse::<axum::http::uri::Authority>() else {
        return false;
    };
    matches!(authority.host(), "127.0.0.1" | "localhost" | "[::1]") && !value.contains('@')
}
pub async fn guard(
    State(stop): State<CancellationToken>,
    request: Request,
    next: Next,
) -> Response {
    let host_ok = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(loopback_host)
        .unwrap_or_else(|| {
            request
                .uri()
                .authority()
                .is_some_and(|a| loopback_host(a.as_str()))
        });
    let origin_ok = request.headers().get(header::ORIGIN).is_none_or(|h| {
        h.to_str()
            .ok()
            .is_some_and(|o| TRUSTED_UI_ORIGINS.contains(&o))
    });
    if !host_ok || !origin_ok {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"detail":"不允许此来源访问本地 StreamCap 服务"})),
        )
            .into_response();
    }
    let mut response = tokio::select! {biased;
        _=stop.cancelled()=>(StatusCode::SERVICE_UNAVAILABLE,Json(json!({"detail":"应用正在退出"}))).into_response(),
        result=next.run(request)=>result,
    };
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
