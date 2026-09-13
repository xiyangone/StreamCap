//! Deterministic parsing and login tests. All URLs and credentials below are synthetic.
use serde_json::{json, Value};
use streamcap_core::{
    platforms::{
        custom, douyin, kuaishou,
        kuaishou_login::{LoginEndpoints, LoginManager},
    },
    resolver::ResolveRequest,
    Resolver,
};
use tokio_util::sync::CancellationToken;

fn room() -> Value {
    json!({"data":{"user":{"nickname":"Fixture"},"data":[{"status":2,"title":"Live","owner":{"nickname":"Fixture"},"stream_url":{"flv_pull_url":{"FULL_HD1":"https://media.invalid/a.flv","SD1":"https://media.invalid/b.flv"},"hls_pull_url_map":{"FULL_HD1":"https://media.invalid/a.m3u8"}}}]}})
}
#[test]
fn douyin_live_quality_and_offline() {
    let value = room();
    let info = douyin::parse_json(&value, Some("OD")).unwrap();
    assert!(info.is_live);
    assert_eq!(info.anchor_name, "Fixture");
    assert!(info.flv_url.ends_with("a.flv"));
    let mut offline = value;
    offline["data"]["data"][0]["status"] = json!(4);
    assert!(!douyin::parse_json(&offline, None).unwrap().is_live);
}
#[test]
fn douyin_embedded_json_and_escaped_streams() {
    let value = room()["data"]["data"][0].clone();
    let payload = format!("0:{}", json!({"room":value}));
    let html = format!(
        "<script>self.__next_f.push({})</script>",
        json!([1, payload])
    );
    assert!(douyin::parse_page(&html, None).unwrap().is_live);
}
#[test]
fn malformed_data_is_not_offline() {
    assert!(douyin::parse_json(&json!({"data":{"data":[]}}), None).is_err());
    assert!(douyin::parse_page("captcha", None).is_err());
    assert!(kuaishou::parse_page("<script>window.__INITIAL_STATE__={\"r\":{\"liveStream\":null,\"author\":{},\"errorType\":{}}};</script>",None).is_err());
}
#[test]
fn kuaishou_bitrate_selection() {
    let room = json!({"author":{"name":"Fixture"},"isLiving":true,"liveStream":{"playUrls":{"h264":{"adaptationSet":{"representation":[{"bitrate":3000,"url":"https://media.invalid/h.flv"},{"bitrate":800,"url":"https://media.invalid/l.flv"}]}}}}});
    assert!(kuaishou::parse_room(&room, Some("HD"))
        .unwrap()
        .flv_url
        .ends_with("l.flv"));
    let html = format!(
        "<script>window.__INITIAL_STATE__={};(function(){{}})()</script>",
        json!({"room":room})
    );
    assert!(kuaishou::parse_page(&html, None).unwrap().is_live);
}
#[test]
fn custom_urls_reject_credentials_and_unknown_pages() {
    let req = |url: &str| ResolveRequest {
        url: url.into(),
        quality: None,
        proxy: None,
        cookie: None,
        platform: None,
    };
    assert!(
        custom::resolve(&req("https://media.invalid/live.m3u8"))
            .unwrap()
            .is_live
    );
    assert!(custom::resolve(&req("https://media.invalid/room")).is_err());
    assert!(custom::resolve(&req("https://user:pass@media.invalid/live.m3u8")).is_err());
}
#[tokio::test]
async fn shutdown_cancels_resolver_and_unsupported_has_no_fallback() {
    let resolver = Resolver::new();
    let req = ResolveRequest {
        url: "https://live.bilibili.com/1".into(),
        quality: None,
        proxy: None,
        cookie: None,
        platform: Some("bilibili".into()),
    };
    assert!(resolver
        .resolve(req.clone())
        .await
        .unwrap_err()
        .contains("尚未迁移"));
    resolver.shutdown().await;
    assert!(!resolver.healthy().await);
    assert!(resolver.resolve(req).await.unwrap_err().contains("退出"));
}

#[tokio::test]
async fn qr_cookie_exchange_and_cancel_are_in_process() {
    use axum::{
        http::{HeaderMap, HeaderValue},
        routing::{get, post},
        Json, Router,
    };
    let png="iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aK9sAAAAASUVORK5CYII=";
    let app=Router::new()
        .route("/rest/c/infra/ks/qr/start",post(move ||async move{Json(json!({"result":1,"qrLoginToken":"fixture-token","qrLoginSignature":"fixture-sign","imageData":png}))}))
        .route("/rest/c/infra/ks/qr/scanResult",post(||async{Json(json!({"result":1}))}))
        .route("/rest/c/infra/ks/qr/acceptResult",post(||async{Json(json!({"result":1,"qrToken":"fixture-accepted"}))}))
        .route("/pass/kuaishou/login/qr/callback",post(||async{let mut headers=HeaderMap::new();headers.insert("set-cookie",HeaderValue::from_static("fixture_pass=demo; Path=/"));(headers,Json(json!({"result":1})))}))
        .route("/pass/kuaishou/login/passToken",post(||async{Json(json!({"result":1,"userId":"fixture-user","kuaishou.live.web_st":"fixture-session","kuaishou.live.web_ph":"fixture-proof"}))}))
        .route("/",get(||async{"<script>window.__INITIAL_STATE__={\"currentUser\":{\"userId\":\"fixture-user\",\"name\":\"Fixture\"}};</script>"}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let manager = LoginManager::with_endpoints(
        CancellationToken::new(),
        LoginEndpoints::loopback(&base).unwrap(),
    );
    let mut snapshot = manager.start(None).await.unwrap();
    for _ in 0..30 {
        if snapshot.is_terminal() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        snapshot = manager.status(&snapshot.session_id).await.unwrap();
    }
    assert_eq!(snapshot.state, "success");
    let cookie = snapshot.cookies.unwrap();
    assert!(cookie.contains("kuaishou.live.web_st=fixture-session"));
    assert!(cookie.contains("kuaishou.live.web_ph=fixture-proof"));
    assert!(cookie.contains("fixture_pass=demo"));
    let mut completed = Vec::new();
    for _ in 0..6 {
        let mut next = manager.start(None).await.unwrap();
        for _ in 0..100 {
            if next.is_terminal() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            next = manager.status(&next.session_id).await.unwrap();
        }
        assert_eq!(next.state, "success", "终态不应占满四个活动会话名额");
        assert_eq!(
            manager.status(&next.session_id).await.unwrap().state,
            "success",
            "终态读取可重试"
        );
        completed.push(next.session_id);
    }
    for id in completed {
        manager.cancel(&id).await.unwrap();
    }
    manager.cancel(&snapshot.session_id).await.unwrap();
    assert!(manager.status(&snapshot.session_id).await.is_err());
    manager.shutdown().await;
    assert_eq!(manager.active_tasks(), 0);
    server.abort();
}

#[tokio::test]
async fn qr_pending_request_is_cancelled_at_shutdown() {
    use axum::{routing::post, Json, Router};
    let app = Router::new().route(
        "/rest/c/infra/ks/qr/start",
        post(|| async {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            Json(json!({"result":707}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let manager = LoginManager::with_endpoints(
        CancellationToken::new(),
        LoginEndpoints::loopback(&base).unwrap(),
    );
    let task_manager = manager.clone();
    let start = tokio::spawn(async move { task_manager.start(None).await });
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    tokio::time::timeout(std::time::Duration::from_secs(1), manager.shutdown())
        .await
        .unwrap();
    assert_eq!(manager.active_tasks(), 0);
    assert!(start.await.unwrap().is_err());
    server.abort();
}

/// Opt-in read-only probe: exactly one saved room per supported platform, never starts recording or login.
#[tokio::test]
#[ignore = "requires explicit STREAMCAP_PROBE_CONFIG; reads two selected rooms without modifying user data"]
async fn native_platform_readonly_probe() {
    use std::path::PathBuf;
    let directory = PathBuf::from(
        std::env::var("STREAMCAP_PROBE_CONFIG").expect("explicit probe config directory"),
    );
    assert!(directory.is_absolute());
    let load = |name: &str| -> Value {
        serde_json::from_slice(&std::fs::read(directory.join(name)).unwrap()).unwrap()
    };
    let tasks = load("recordings.json");
    let settings = load("user_settings.json");
    let cookies = load("cookies.json");
    let resolver = Resolver::new();
    let mut unavailable = 0;
    for platform in ["douyin", "kuaishou"] {
        let task = tasks
            .as_array()
            .unwrap()
            .iter()
            .find(|task| task["platform_key"] == platform)
            .expect("selected platform must exist");
        let cookie = cookies[platform]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned);
        let cookie_provided = cookie.is_some();
        let proxy = if settings["enable_proxy"] == true {
            settings["proxy_address"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .map(str::to_owned)
        } else {
            None
        };
        let request = ResolveRequest {
            url: task["url"].as_str().unwrap().into(),
            quality: task["quality"]
                .as_str()
                .or(settings["record_quality"].as_str())
                .map(str::to_owned),
            proxy,
            cookie,
            platform: Some(platform.into()),
        };
        let report = match resolver.resolve(request).await {
            Ok(info) => {
                json!({"platform":platform,"ok":true,"cookieProvided":cookie_provided,"isLive":info.is_live,"anchorPresent":!info.anchor_name.is_empty(),"streamPresent":info.pick_record_url(false).is_some()})
            }
            Err(message) => {
                unavailable += 1;
                json!({"platform":platform,"ok":false,"cookieProvided":cookie_provided,"error":message})
            }
        };
        println!("NATIVE_PROBE {}", report);
    }
    resolver.shutdown().await;
    assert_eq!(
        unavailable, 0,
        "one or more platform responses remain unverified; see redacted NATIVE_PROBE output"
    );
}

#[test]
fn douyin_pace_room_store_preserves_anchor_and_explicit_offline() {
    let store = json!({"roomInfo":{"room":{"id_str":"fixture-room","status":4},"anchor":{"nickname":"Fixture","avatar_thumb":{}}}});
    let payload = format!("0:{}", json!({"roomStore":store}));
    let placeholder = format!("0:{}", json!({"roomStore":{"roomInfo":null}}));
    let html = format!(
        "<script>self.__pace_f.push({})</script><script>self.__pace_f.push({})</script>",
        json!([1, placeholder]),
        json!([1, payload])
    );
    let info = douyin::parse_page(&html, None).unwrap();
    assert!(!info.is_live);
    assert_eq!(info.anchor_name, "Fixture");
}
#[test]
fn douyin_pace_web_stream_url_uses_requested_quality() {
    let store = json!({"roomInfo":{"room":{"id_str":"fixture-room","status":2},"anchor":{"nickname":"Fixture","avatar_thumb":{}},"web_stream_url":{"flv_pull_url":{"FULL_HD1":"https://media.invalid/high.flv","SD1":"https://media.invalid/low.flv"}}}});
    let html = format!(
        "<script>self.__pace_f.push({})</script>",
        json!([1, format!("0:{}", json!({"roomStore":store}))])
    );
    let info = douyin::parse_page(&html, Some("UHD")).unwrap();
    assert!(info.is_live);
    assert!(info.record_url.ends_with("low.flv"));
}
#[test]
fn kuaishou_mobile_restriction_is_not_offline_and_streams_are_parsed() {
    let limited =
        json!({"result":2,"error_msg":"Your action is too frequent. Please try again later."});
    assert!(kuaishou::parse_mobile(&limited, None)
        .unwrap_err()
        .contains("限制访问"));
    let live = json!({"result":1,"liveStream":{"living":true,"user":{"user_name":"Fixture"},"multiResolutionPlayUrls":[{"urls":[{"bitrate":3000,"url":"https://media.invalid/high.flv"},{"bitrate":800,"url":"https://media.invalid/low.flv"}]}]}});
    let info = kuaishou::parse_mobile(&live, Some("HD")).unwrap();
    assert!(info.is_live);
    assert!(info.record_url.ends_with("low.flv"));
    let mut offline = live;
    offline["liveStream"]["living"] = json!(false);
    assert!(!kuaishou::parse_mobile(&offline, None).unwrap().is_live);
}
#[tokio::test]
async fn qr_concurrent_start_and_shutdown_leave_no_workers() {
    let manager = LoginManager::with_endpoints(
        CancellationToken::new(),
        LoginEndpoints::loopback("http://127.0.0.1:1/").unwrap(),
    );
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(17));
    let mut starts = Vec::new();
    for _ in 0..16 {
        let manager = manager.clone();
        let barrier = barrier.clone();
        starts.push(tokio::spawn(async move {
            barrier.wait().await;
            manager.start(None).await
        }));
    }
    barrier.wait().await;
    tokio::time::timeout(std::time::Duration::from_secs(2), manager.shutdown())
        .await
        .unwrap();
    for start in starts {
        let _ = start.await.unwrap();
    }
    assert_eq!(manager.active_tasks(), 0);
    assert!(manager.start(None).await.is_err());
}

#[test]
fn qr_account_verification_handles_identity_shapes_without_matching_recommendations() {
    use streamcap_core::platforms::kuaishou_login::verify_login_page;
    for state in [
        json!({"currentUser":{"userId":"42","name":"Fixture"}}),
        json!({"global":{"loginUser":{"id":42,"nickname":"Fixture"}}}),
        json!({"userStore":{"userInfo":{"principalId":"42","user_name":"Fixture"}}}),
        json!({"user":{"userInfoQuery":{"ownerInfo":{"originUserId":42,"name":"Fixture"}}}}),
    ] {
        let page = format!("<script>window.__INITIAL_STATE__ = {};</script>", state);
        assert_eq!(
            verify_login_page(&page, "42").unwrap(),
            Some("Fixture".into())
        );
    }
    for state in [
        json!({"recommendations":[{"author":{"userId":"42","name":"Unrelated"}}]}),
        json!({"currentUser":{"userId":"142","name":"Different"}}),
        json!({"currentUser":{"userId":"42","isLogin":false}}),
        json!({"isLoggedIn":false,"currentUser":{"userId":"42"}}),
        json!({"userStore":{"isLogin":false,"userInfo":{"userId":"42"}}}),
    ] {
        let page = format!("window.__INITIAL_STATE__={};", state);
        let error = verify_login_page(&page, "42").unwrap_err();
        assert!(error.contains("手机已确认"));
        assert!(error.contains("未保存"));
    }
    assert!(verify_login_page("<html>login challenge</html>", "42").is_err());
    assert!(verify_login_page(
        r#"window.__INITIAL_STATE__={"currentUser":{"userId":""}};"#,
        ""
    )
    .is_err());
    assert_eq!(
        verify_login_page(
            r#"<script>window.__BOOTSTRAP__={'isLoggedIn':true,'profile':{'userId':'42'}};</script>"#,
            "42"
        )
        .unwrap(),
        Some("42".into())
    );
    assert_eq!(
        verify_login_page(r#"<html>{"userId":"42"}</html>"#, "42").unwrap(),
        Some("42".into())
    );
    assert_eq!(
        verify_login_page(
            r#"window.__INITIAL_STATE__={"bootstrap":{"uid":"42"}};"#,
            "42"
        )
        .unwrap(),
        Some("42".into())
    );
    assert!(verify_login_page(
        r#"window.__INITIAL_STATE__={"recommendations":[{"uid":"42"}]};"#,
        "42"
    )
    .is_err());
    assert!(verify_login_page(
        r#"window.__INITIAL_STATE__={"home":{"homeLiveStream":[{"author":{"originUserId":42}}]}};"#,
        "42"
    )
    .is_err());
    assert_eq!(
        verify_login_page(
            r#"<script>window.__INITIAL_STATE__ = JSON.parse('{"auth":{"isLoggedIn":true,"profile":{"userId":"42"}}}');</script>"#,
            "42"
        )
        .unwrap(),
        Some("42".into())
    );
    assert!(verify_login_page(
        r#"<script>window.__BOOTSTRAP__={'recommendations':[{'author':{'userId':'42'}}]};</script>"#,
        "42"
    )
    .is_err());
}
#[tokio::test]
async fn qr_confirmed_but_unverified_never_exposes_or_saves_cookies() {
    use axum::{
        routing::{get, post},
        Json, Router,
    };
    let png="iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aK9sAAAAASUVORK5CYII=";
    let app=Router::new()
        .route("/rest/c/infra/ks/qr/start",post(move||async move{Json(json!({"result":1,"qrLoginToken":"fixture-token","qrLoginSignature":"fixture-sign","imageData":png}))}))
        .route("/rest/c/infra/ks/qr/scanResult",post(||async{Json(json!({"result":1}))}))
        .route("/rest/c/infra/ks/qr/acceptResult",post(||async{Json(json!({"result":1,"qrToken":"fixture-accepted"}))}))
        .route("/pass/kuaishou/login/qr/callback",post(||async{Json(json!({"result":1}))}))
        .route("/pass/kuaishou/login/passToken",post(||async{Json(json!({"result":1,"userId":"fixture-user","kuaishou.live.web_st":"fixture-session","kuaishou.live.web_ph":"fixture-proof"}))}))
        .route("/",get(||async{r#"<script>window.__INITIAL_STATE__={"room":{"author":{"userId":"fixture-user","name":"Fixture"}}};</script>"#}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let manager = LoginManager::with_endpoints(
        CancellationToken::new(),
        LoginEndpoints::loopback(&base).unwrap(),
    );
    let mut snapshot = manager.start(None).await.unwrap();
    for _ in 0..40 {
        if snapshot.is_terminal() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        snapshot = manager.status(&snapshot.session_id).await.unwrap();
    }
    assert_eq!(snapshot.state, "error");
    assert!(snapshot.message.contains("手机已确认"));
    assert!(snapshot.cookies.is_none());
    assert!(snapshot.username.is_none());
    assert!(snapshot.image_base64.is_empty());
    assert_eq!(snapshot.seconds_left, 0);
    manager.shutdown().await;
    assert_eq!(manager.active_tasks(), 0);
    server.abort();
}

#[test]
fn douyin_recommends_flv_without_changing_the_requested_quality() {
    let info = douyin::parse_json(&room(), Some("OD")).unwrap();
    assert_eq!(info.record_url, info.flv_url);
    assert!(info.record_url.ends_with("a.flv"));
    assert_eq!(
        info.pick_record_url(false).as_deref(),
        Some(info.flv_url.as_str())
    );
    let lower = douyin::parse_json(&room(), Some("UHD")).unwrap();
    assert!(lower.record_url.ends_with("b.flv"));
}

#[test]
fn douyin_uses_hls_only_when_no_valid_flv_is_provided() {
    let mut value = room();
    value["data"]["data"][0]["stream_url"]["flv_pull_url"] = json!({"FULL_HD1":"not-a-url"});
    let info = douyin::parse_json(&value, None).unwrap();
    assert!(info.flv_url.is_empty());
    assert_eq!(info.record_url, info.m3u8_url);
    assert!(info.record_url.ends_with("a.m3u8"));
    value["data"]["data"][0]["stream_url"]["hls_pull_url_map"] = json!({});
    assert!(douyin::parse_json(&value, None).is_err());
}
