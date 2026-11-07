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

fn browser_detail(id: &str, result: i64) -> kuaishou::BrowserResponse {
    kuaishou::BrowserResponse {
        url: kuaishou::detail_url(id).unwrap().to_string(),
        http_status: 200,
        payload: json!({"data":{"result":result,"author":{"id":id,"name":"Fixture"},
        "liveStream":{"playUrls":[
            {"bitrate":2000,"url":"https://media.invalid/high.flv"},
            {"bitrate":800,"url":"https://media.invalid/low.flv"}
        ]}}}),
    }
}

#[test]
fn browser_detail_requires_the_requested_primary_author_and_current_navigation() {
    let reply = browser_detail("fixture", 1);
    let info = kuaishou::parse_browser_response(&reply, "fixture", Some("HD")).unwrap();
    assert!(info.is_live);
    assert!(info.record_url.ends_with("low.flv"));
    let mut wrong = reply.clone();
    wrong.payload["data"]["author"]["id"] = json!("other");
    wrong.payload["data"]["recommendList"] = json!([reply.payload["data"].clone()]);
    assert!(kuaishou::parse_browser_response(&wrong, "fixture", None).is_err());
    wrong = reply.clone();
    wrong.url = kuaishou::detail_url("old-room").unwrap().to_string();
    assert!(kuaishou::parse_browser_response(&wrong, "fixture", None).is_err());
    wrong = reply;
    wrong.payload["data"]["author"] = json!({"name":"No verified identity"});
    assert!(kuaishou::parse_browser_response(&wrong, "fixture", None).is_err());
}

#[test]
fn browser_detail_separates_offline_captcha_rate_limits_and_missing_business_data() {
    let mut reply = browser_detail("fixture", 671);
    let offline = kuaishou::parse_browser_response(&reply, "fixture", None).unwrap();
    assert!(!offline.is_live);
    assert!(
        offline.record_url.is_empty(),
        "offline replies cannot reuse stale sources"
    );
    reply.payload["data"]["result"] = json!(400002);
    reply.payload["data"]["author"] = json!({});
    assert_eq!(
        kuaishou::parse_browser_response(&reply, "fixture", None).unwrap_err(),
        kuaishou::VERIFICATION_REQUIRED
    );
    reply.http_status = 429;
    assert_eq!(
        kuaishou::parse_browser_response(&reply, "fixture", None).unwrap_err(),
        kuaishou::RATE_LIMITED
    );
    reply = browser_detail("fixture", 1);
    reply.payload["data"]["liveStream"]["living"] = json!(false);
    assert!(kuaishou::parse_browser_response(&reply, "fixture", None).is_err());
    reply.payload["data"]["liveStream"] = Value::Null;
    assert!(kuaishou::parse_browser_response(&reply, "fixture", None).is_err());
    for invalid in [
        Value::Null,
        json!("not-json"),
        json!({"result":1}),
        json!({"data":{"result":2}}),
    ] {
        reply.payload = invalid;
        assert!(kuaishou::parse_browser_response(&reply, "fixture", None).is_err());
    }
}

#[test]
fn browser_room_urls_are_canonical_and_cannot_escape_the_official_endpoint() {
    assert_eq!(
        kuaishou::principal_id("https://live.kuaishou.com/u/fixture/?utm_source=test#ignored")
            .unwrap(),
        "fixture"
    );
    for invalid in [
        "http://live.kuaishou.com/u/fixture",
        "https://live.kuaishou.com.evil.invalid/u/fixture",
        "https://user:pass@live.kuaishou.com/u/fixture",
        "https://live.kuaishou.com:444/u/fixture",
        "https://live.kuaishou.com/u/",
        "https://live.kuaishou.com/u/a/b",
        "https://live.kuaishou.com/u/a%2Fb",
    ] {
        assert!(kuaishou::principal_id(invalid).is_err(), "{invalid}");
    }
    assert!(kuaishou::detail_url("a?principalId=b").is_err());
    let key = kuaishou::session_key("cookie:fixture-not-a-credential");
    assert_eq!(key.len(), 64);
    assert!(key.bytes().all(|byte| byte.is_ascii_hexdigit()));
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

fn douyin_pages(state: &Value) -> Vec<String> {
    let json = state.to_string();
    let encoded = percent_encoding::utf8_percent_encode(&json, percent_encoding::NON_ALPHANUMERIC);
    let mut pages = vec![
        format!("<script id=\"RENDER_DATA\">{encoded}</script>"),
        format!("<script>window.__INITIAL_STATE__ = {json};</script>"),
        format!("<script type=\"application/json\">{json}</script>"),
    ];
    for marker in ["__next_f", "__pace_f"] {
        pages.push(format!(
            "<script>self.{marker}.push({})</script>",
            json!([1, format!("0:{json}")])
        ));
    }
    pages
}

#[test]
fn douyin_never_fills_missing_target_data_from_recommendations() {
    let unrelated = json!({
        "status":2,
        "owner":{"nickname":"Unrelated room"},
        "stream_url":{"flv_pull_url":{"ORIGIN":"https://media.invalid/unrelated.flv"}}
    });
    let requested = json!({"status":2,"owner":{"nickname":"Requested room"}});
    let unrelated_store = json!({"roomInfo":{
        "room":unrelated,
        "anchor":{"nickname":"Unrelated room","avatar_thumb":{}},
        "web_stream_url":unrelated["stream_url"]
    }});
    for state in [
        json!({"data":{"data":[]},"recommendations":[unrelated]}),
        json!({"data":{"data":[requested]},"recommendations":[{"web_stream_url":unrelated["stream_url"]}]}),
        json!({"data":{"data":null},"roomStore":unrelated_store}),
        json!({"recommendations":[{"room":unrelated},{"roomStore":unrelated_store}]}),
        json!({"roomStore":{"roomInfo":null},"room":unrelated}),
        json!({"roomStore":{"roomInfo":{"room":requested}},"recommendations":[{"roomStore":unrelated_store}]}),
        json!({"roomStore":{"roomInfo":{"room":{"status":2,"stream_url":unrelated["stream_url"]}}},"recommendations":[{"nickname":"Unrelated room","avatar_thumb":{}}]}),
    ] {
        assert!(douyin::parse_json(&state, None).is_err(), "{state}");
        for page in douyin_pages(&state) {
            assert!(douyin::parse_page(&page, None).is_err(), "{page}");
        }
    }
}

#[test]
fn douyin_owned_room_shapes_keep_identity_quality_and_offline_state() {
    let mut api = room();
    api["data"]["user"]["nickname"] = json!("Requested room");
    api["data"]["data"][0]["owner"]["nickname"] = json!("Requested room");
    let direct = api["data"]["data"][0].clone();
    let store = json!({"roomInfo":{
        "room":{"id_str":"requested","status":2,"title":"Live"},
        "anchor":{"nickname":"Requested room","avatar_thumb":{}},
        "web_stream_url":direct["stream_url"]
    }});
    for state in [
        api,
        direct.clone(),
        json!({"room":direct}),
        store.clone(),
        json!({"roomStore":store}),
    ] {
        let expected = douyin::parse_json(&state, Some("UHD")).unwrap();
        assert_eq!(expected.anchor_name, "Requested room");
        assert!(expected.is_live);
        assert!(expected.record_url.ends_with("b.flv"));
        for page in douyin_pages(&state) {
            assert_eq!(douyin::parse_page(&page, Some("UHD")).unwrap(), expected);
        }
    }
    let offline = json!({
        "roomStore":{"roomInfo":{"room":{"status":4},"anchor":{"nickname":"Requested room"}}},
        "recommendations":[{"room":direct}]
    });
    for page in douyin_pages(&offline) {
        let info = douyin::parse_page(&page, None).unwrap();
        assert_eq!(info.anchor_name, "Requested room");
        assert!(!info.is_live);
        assert!(info.record_url.is_empty());
    }
}

#[test]
fn douyin_page_placeholders_do_not_promote_unowned_room_containers() {
    let store = json!({"roomInfo":{
        "room":{"id_str":"requested","status":4},
        "anchor":{"nickname":"Requested room"}
    }});
    let recommendation = json!({"status":2,"owner":{"nickname":"Unrelated room"},"stream_url":{"flv_pull_url":{"ORIGIN":"https://media.invalid/unrelated.flv"}}});
    for state in [
        json!({"recommendations":{"0":{"roomStore":store}}}),
        json!(["$","Recommendation",null,{"roomStore":store}]),
        json!({"0":{"room":recommendation}}),
    ] {
        for page in douyin_pages(&state) {
            assert!(douyin::parse_page(&page, None).is_err());
        }
    }
    let placeholder =
        json!({"roomStore":{"roomInfo":null},"recommendations":[{"room":recommendation}]});
    let html = format!(
        "<script>self.__pace_f.push({})</script><script>self.__pace_f.push({})</script>",
        json!([1, format!("0:{placeholder}")]),
        json!([1, format!("1:{}", json!({"roomStore":store}))])
    );
    assert_eq!(
        douyin::parse_page(&html, None).unwrap().anchor_name,
        "Requested room"
    );
    for page in douyin_pages(&json!({"0":{"roomStore":store},"1":{"room":recommendation}})) {
        assert!(douyin::parse_page(&page, None).is_err());
    }
}

#[test]
fn douyin_flight_chunks_reassemble_before_selecting_room_state() {
    let requested = json!({"roomStore":{"roomInfo":{
        "room":{"status":4},"anchor":{"nickname":"Requested room"}
    }}});
    let unrelated = json!({"room":{"status":2,"owner":{"nickname":"Unrelated room"},"stream_url":{"flv_pull_url":{"ORIGIN":"https://media.invalid/unrelated.flv"}}}});
    let false_frame = format!("text before a newline\n0:{unrelated}\n");
    for marker in ["__next_f", "__pace_f"] {
        for (state, expected) in [
            (requested.clone(), true),
            (
                json!({"data":{"data":[]},"recommendations":[unrelated]}),
                false,
            ),
        ] {
            let payload = format!("0:{state}\n");
            let split = payload
                .char_indices()
                .nth(payload.chars().count() / 2)
                .unwrap()
                .0;
            let html = format!(
                "<script>self.{marker}.push({})</script><script>self.{marker}.push({})</script>",
                json!([1, &payload[..split]]),
                json!([1, &payload[split..]])
            );
            let result = douyin::parse_page(&html, None);
            assert_eq!(result.is_ok(), expected);
            if let Ok(info) = result {
                assert_eq!(info.anchor_name, "Requested room");
                assert!(!info.is_live);
            }
        }
        let html = format!(
            "<script>self.{marker}.push({})</script>",
            json!([
                1,
                format!("a:T{:x},{false_frame}1:{requested}\n", false_frame.len())
            ])
        );
        assert_eq!(
            douyin::parse_page(&html, None).unwrap().anchor_name,
            "Requested room"
        );
        let visible_text = format!(
            "<p>self.{marker}.push({})</p>",
            json!([1, format!("0:{unrelated}")])
        );
        assert!(douyin::parse_page(&visible_text, None).is_err());
    }
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
fn kuaishou_user_info_requires_success_target_identity_and_boolean_status() {
    let valid =
        json!({"data":{"result":1,"userInfo":{"id":"fixture","name":"Fixture","living":true}}});
    for living in [false, true] {
        let mut data = valid.clone();
        data["data"]["userInfo"]["living"] = json!(living);
        assert_eq!(
            kuaishou::parse_user_info(&data, "fixture").unwrap(),
            ("Fixture".into(), living)
        );
    }
    for result in [json!(0), json!(2), json!(400002), json!("1"), Value::Null] {
        let mut data = valid.clone();
        data["data"]["result"] = result;
        assert!(kuaishou::parse_user_info(&data, "fixture").is_err());
    }
    for identity in [json!("other"), json!(""), json!(42), Value::Null] {
        let mut data = valid.clone();
        data["data"]["userInfo"]["id"] = identity;
        assert!(kuaishou::parse_user_info(&data, "fixture").is_err());
    }
    for living in [
        json!(0),
        json!(1),
        json!("false"),
        json!("true"),
        Value::Null,
    ] {
        let mut data = valid.clone();
        data["data"]["userInfo"]["living"] = living;
        assert!(kuaishou::parse_user_info(&data, "fixture").is_err());
    }
    assert!(kuaishou::parse_user_info(&valid, "").is_err());
    assert!(kuaishou::parse_user_info(&json!({}), "fixture").is_err());
    // Display metadata is not evidence of status and is not required for it.
    let no_name = json!({"data":{"result":1,"userInfo":{"id":"fixture","living":false}}});
    assert_eq!(
        kuaishou::parse_user_info(&no_name, "fixture").unwrap(),
        (String::new(), false)
    );
}

#[test]
fn kuaishou_live_access_restriction_keeps_existing_access_contract() {
    assert_eq!(
        kuaishou::access_state(kuaishou::LIVE_ACCESS_UNAVAILABLE),
        "unavailable"
    );
    assert!(!kuaishou::verification_required(
        kuaishou::LIVE_ACCESS_UNAVAILABLE
    ));
}

#[test]
fn custom_urls_reject_credentials_and_unknown_pages() {
    let req = |url: &str| ResolveRequest {
        account: None,
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
        account: None,
        url: "https://unsupported.example.test/1".into(),
        quality: None,
        proxy: None,
        cookie: None,
        platform: Some("unsupported-fixture".into()),
    };
    assert!(resolver
        .resolve(req.clone())
        .await
        .unwrap_err()
        .contains("不受支持"));
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
            account: None,
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

#[tokio::test]
#[ignore = "requires explicit STREAMCAP_PROBE_ROOM_URL; one read-only page request, no task changes"]
async fn selected_douyin_page_readonly_probe() {
    use streamcap_core::platforms::http::PlatformHttp;
    let url = std::env::var("STREAMCAP_PROBE_ROOM_URL").expect("explicit selected room");
    let url = reqwest::Url::parse(&url).unwrap();
    assert_eq!(url.host_str(), Some("live.douyin.com"));
    let http =
        PlatformHttp::new("douyin.com", None, None, std::time::Duration::from_secs(18)).unwrap();
    let page = http.get(url, "https://live.douyin.com/").await.unwrap();
    let result = douyin::parse_page(&page, None);
    println!(
        "SELECTED_PAGE {}",
        match &result {
            Ok(info) =>
                json!({"readable":true,"isLive":info.is_live,"anchorPresent":!info.anchor_name.is_empty()}),
            Err(error) => json!({"readable":false,"error":error,"bytes":page.len()}),
        }
    );
    assert!(
        result.is_ok(),
        "selected page must yield verified room state"
    );
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
fn kuaishou_challenge_has_priority_over_empty_author_and_false_live_flag() {
    let restricted = json!({"liveStream":{},"author":{},"isLiving":false,"errorType":{"type":400002,"title":"请完成滑块验证"}});
    for code in [json!(400002), json!("400002")] {
        let mut value = restricted.clone();
        value["errorType"]["type"] = code;
        assert_eq!(
            kuaishou::parse_room(&value, None).unwrap_err(),
            kuaishou::ACCESS_UNAVAILABLE
        );
    }
    let state = json!({"liveroom":{"playList":[restricted,{"author":{"name":"Recommendation"},"isLiving":false,"liveStream":{}}]}});
    assert_eq!(
        kuaishou::parse_state(&state, None).unwrap_err(),
        kuaishou::ACCESS_UNAVAILABLE
    );
    assert!(kuaishou::parse_state(&json!({"liveroom":{"playList":[]},"room":{"author":{"name":"Wrong room"},"isLiving":false}}),None).is_err());
    assert!(
        !kuaishou::parse_room(
            &json!({"author":{"name":"Fixture"},"isLiving":false,"liveStream":{},"errorType":{}}),
            None
        )
        .unwrap()
        .is_live
    );
}
#[test]
fn kuaishou_cookie_scope_order_is_preserved_by_the_http_client() {
    assert_eq!(
        kuaishou::canonical_cookie_header(
            "did=specific; userId=fixture; did=parent; kuaishou.live.web_st=a=b"
        )
        .unwrap(),
        "did=specific; userId=fixture; kuaishou.live.web_st=a=b"
    );
    assert!(kuaishou::canonical_cookie_header("did=fixture\r\nAuthorization: bad").is_err());
    assert!(kuaishou::canonical_cookie_header("invalid name=value").is_err());
    assert_eq!(kuaishou::canonical_cookie_header("").unwrap(), "");
}

#[test]
fn kuaishou_login_and_room_share_the_same_state_decoder() {
    let state = json!({"currentUser":{"userId":"42","name":"测试账号"},"liveroom":{"playList":[{"author":{"name":"测试主播"},"isLiving":false,"liveStream":{}}]}});
    for expression in [
        state.to_string(),
        format!(
            "JSON.parse({})",
            serde_json::to_string(&state.to_string()).unwrap()
        ),
        format!("JSON.parse('{state}')"),
    ] {
        let page = format!("<script>window.__INITIAL_STATE__ = {expression};</script>");
        assert_eq!(
            kuaishou::parse_page(&page, None).unwrap().anchor_name,
            "测试主播"
        );
        assert_eq!(
            streamcap_core::platforms::kuaishou_login::verify_login_page(&page, "42").unwrap(),
            Some("测试账号".into())
        );
    }
}

#[test]
fn kuaishou_server_state_allows_undefined_values_without_executing_javascript() {
    let state = r#"{"liveroom":{"playList":[{"author":{"id":"target","name":"undefined 主播"},"liveStream":{},"isLiving":false,"authToken":undefined,"optional":[undefined,"undefined",{"value":undefined}]}]}}"#;
    let page = format!(
        "<script>window.__INITIAL_STATE__={state};document.currentScript.remove();</script>"
    );
    let info = kuaishou::parse_page(&page, None).unwrap();
    assert_eq!(info.anchor_name, "undefined 主播");
    assert!(!info.is_live);
    for expression in [
        "undefinedValue",
        "(globalThis.sideEffect=true)",
        "(()=>false)()",
    ] {
        let page = page.replace(
            "\"authToken\":undefined",
            &format!("\"authToken\":{expression}"),
        );
        assert!(kuaishou::parse_page(&page, None).is_err());
    }
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
        json!({"user":{"isLoggedIn":true,"userInfoQuery":{"ownerInfo":{"userId":"42","name":"Fixture"}}}}),
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
        json!({"user":{"isLoggedIn":false,"userInfoQuery":{"ownerInfo":{"userId":"42"}}}}),
        json!({"user":{"loggedIn":"0","userInfoQuery":{"ownerInfo":{"userId":"42"}}}}),
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
    assert_eq!(
        verify_login_page(
            r#"<script>window.__BOOTSTRAP__={'currentUser':{'userId':'42','name':'Fixture \'quoted\''},'unused':undefined,'recommendations':[{'author':{'userId':'7'}}]};</script>"#,
            "42"
        ).unwrap(),
        Some("Fixture 'quoted'".into())
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
    for state in [
        json!({"room":{"author":{"userId":"fixture-user","name":"Fixture"}}}),
        json!({"currentUser":null,"recommendations":[{"author":{"userId":"fixture-user"}}]}),
        json!({"currentUser":{"userId":"another-user"},"recommendations":[{"author":{"userId":"fixture-user"}}]}),
        json!({"user":{"isLoggedIn":false,"userInfoQuery":{"ownerInfo":{"userId":"fixture-user"}}}}),
    ] {
        let page = format!("<script>window.__INITIAL_STATE__={state};</script>");
        let png="iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aK9sAAAAASUVORK5CYII=";
        let app=Router::new()
        .route("/rest/c/infra/ks/qr/start",post(move||async move{Json(json!({"result":1,"qrLoginToken":"fixture-token","qrLoginSignature":"fixture-sign","imageData":png}))}))
        .route("/rest/c/infra/ks/qr/scanResult",post(||async{Json(json!({"result":1}))}))
        .route("/rest/c/infra/ks/qr/acceptResult",post(||async{Json(json!({"result":1,"qrToken":"fixture-accepted"}))}))
        .route("/pass/kuaishou/login/qr/callback",post(||async{Json(json!({"result":1}))}))
        .route("/pass/kuaishou/login/passToken",post(||async{Json(json!({"result":1,"userId":"fixture-user","kuaishou.live.web_st":"fixture-session","kuaishou.live.web_ph":"fixture-proof"}))}))
        .route("/",get(move || {let page = page.clone(); async move {page}}));
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
}

#[test]
fn qr_verification_decodes_unicode_surrogate_pairs_without_accepting_malformed_strings() {
    use streamcap_core::platforms::kuaishou_login::verify_login_page;
    let page =
        r#"window.__BOOTSTRAP__={'currentUser':{'userId':'42','name':'\u68A6\uD83D\uDE00'}};"#;
    assert_eq!(verify_login_page(page, "42").unwrap(), Some("梦😀".into()));
    for name in [r"\uD83D", r"\uDE00", r"\uD83D\u0041", r"\uD83Dx"] {
        let page =
            format!("window.__BOOTSTRAP__={{'currentUser':{{'userId':'42','name':'{name}'}}}};");
        assert!(verify_login_page(&page, "42").is_err(), "{page}");
    }
}

#[test]
fn qr_verification_rejects_recommendations_and_unowned_identity_text() {
    use streamcap_core::platforms::kuaishou_login::verify_login_page;
    for state in [
        json!({"currentUser":null,"recommendations":[{"author":{"userId":"42"}}]}),
        json!({"currentUser":{"userId":"7"},"recommendations":[{"author":{"userId":"42"}}]}),
        json!({"isLoggedIn":true,"recommendations":[{"author":{"userId":"42"}}]}),
        json!({"isLoggedIn":true,"metadata":{"userId":"42"}}),
        json!({"currentUser":null,"isLoggedIn":true,"profile":{"userId":"42"}}),
        json!({"currentUser":{"userId":"7"},"bootstrap":{"uid":"42"}}),
        json!({"bootstrap":{"uid":"42"}}),
    ] {
        let page = format!("window.__INITIAL_STATE__={state};");
        assert!(verify_login_page(&page, "42").is_err(), "{page}");
    }
    for page in [
        r#"<html>{"userId":"42"}</html>"#,
        r#"<script>window.__BOOTSTRAP__={'currentUser':null,'recommendations':[{'author':{'userId':'42'}}]};</script>"#,
        r#"<script>window.__BOOTSTRAP__={'currentUser':{'userId':'7'},'recommendations':[{'author':{'userId':'42'}}]};</script>"#,
        r#"<script>window.__INITIAL_STATE__={"currentUser":null};</script><p>42</p>"#,
    ] {
        assert!(verify_login_page(page, "42").is_err(), "{page}");
    }
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

#[test]
fn kuaishou_business_restriction_is_not_rate_limiting_or_an_offline_room() {
    for code in [json!(400002), json!("400002")] {
        let room = json!({"author":{},"isLiving":false,"liveStream":{},"errorType":{"type":code,"title":"请求频繁，请完成人机验证"}});
        let error = kuaishou::parse_room(&room, None).unwrap_err();
        assert_eq!(error, kuaishou::ACCESS_UNAVAILABLE);
        assert_eq!(kuaishou::access_state(&error), "unavailable");
        assert!(!kuaishou::verification_required(&error));
        assert!(!error.contains("窗口"));
    }
    let limited = json!({"errorType":{"title":"HTTP 429 too many requests"}});
    assert_eq!(
        kuaishou::parse_room(&limited, None).unwrap_err(),
        kuaishou::RATE_LIMITED
    );
}

#[test]
fn kuaishou_http_login_failure_does_not_invent_a_human_challenge() {
    let login = json!({"author":{},"isLiving":false,"liveStream":{},"errorType":{"title":"请先登录后观看"}});
    let error = kuaishou::parse_room(&login, None).unwrap_err();
    assert_eq!(error, kuaishou::LOGIN_REQUIRED);
    assert_eq!(kuaishou::access_state(&error), "loginRequired");
    assert!(!kuaishou::verification_required(&error));
    let normal =
        json!({"author":{"id":"fixture","name":"Fixture"},"isLiving":false,"liveStream":{}});
    assert!(
        !kuaishou::parse_state(&json!({"room":normal}), None)
            .unwrap()
            .is_live
    );
}

#[test]
fn kuaishou_login_prompt_does_not_confirm_logout_or_offline() {
    for error in [
        json!({"title":"登录后享受更多权益"}),
        json!({"content":"扫码登录后享受更多权益"}),
    ] {
        let room = json!({
            "author":{"name":"Fixture"},
            "isLiving":false,
            "liveStream":{},
            "errorType":error
        });
        let error = kuaishou::parse_room(&room, None).unwrap_err();
        assert_eq!(error, kuaishou::LOGIN_PROMPT);
        assert_eq!(kuaishou::access_state(&error), "loginPrompt");
        assert!(!kuaishou::verification_required(&error));
    }
    let room = json!({
        "author":{"name":"Fixture"},
        "isLiving":false,
        "liveStream":{"caption":"扫码登录后享受更多权益"}
    });
    assert!(!kuaishou::parse_room(&room, None).unwrap().is_live);
}

#[test]
fn kuaishou_unknown_access_errors_do_not_confirm_offline() {
    for error in [
        json!({"content":"页面暂不可用"}),
        json!({"content":"请完成人机验证"}),
        json!({"title":"请完成滑块验证"}),
        json!({"type":"UNKNOWN_ACCESS"}),
        json!({"type":false}),
        json!("unavailable"),
    ] {
        let room = json!({
            "author":{"name":"Fixture"},
            "isLiving":false,
            "liveStream":{},
            "errorType":error
        });
        let error = kuaishou::parse_room(&room, None).unwrap_err();
        assert_eq!(error, kuaishou::PAGE_UNAVAILABLE);
        assert_eq!(kuaishou::access_state(&error), "unavailable");
        assert!(!kuaishou::verification_required(&error));
    }
    for error in [
        Value::Null,
        json!({}),
        json!({"type":0}),
        json!({"type":"0"}),
    ] {
        let room = json!({
            "author":{"name":"Fixture"},
            "isLiving":false,
            "liveStream":{},
            "errorType":error
        });
        assert!(!kuaishou::parse_room(&room, None).unwrap().is_live);
    }
}
