//! Isolated feature contracts. No platform, account, real notification, or power action is used.
use axum::{routing::post, Json, Router};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use streamcap_core::{
    api,
    config::ConfigStore,
    notifications::Notifications,
    service::{Server, ServerOptions},
    Recording, Store, Workspace,
};
use tokio::sync::RwLock;
async fn server() -> (tempfile::TempDir, Server, String) {
    let root = tempfile::tempdir().unwrap();
    let state = api::bootstrap(Workspace::from_repo_root(root.path()))
        .await
        .unwrap();
    let server = Server::start(
        state,
        ServerOptions {
            port: 0,
            monitoring: false,
        },
    )
    .await
    .unwrap();
    let base = format!("http://{}", server.address());
    (root, server, base)
}
fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}
#[tokio::test]
async fn account_updates_are_redacted_targeted_and_serialized() {
    let (root, server, base) = server().await;
    let path = root.path().join("config/accounts.json");
    std::fs::write(&path,br#"{"pandalive":{"username":"fixture","password":"fixture-old","access_token":"fixture-token","extra":"keep"},"douyin":{"username":"other","password":"other-secret"}}"#).unwrap();
    let before = std::fs::read(&path).unwrap();
    let result: Value = client()
        .get(format!("{base}/api/accounts"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let text = result.to_string();
    assert!(!text.contains("fixture-old"));
    assert!(!text.contains("fixture-token"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let response = client()
        .put(format!("{base}/api/accounts"))
        .json(&json!({"platform":"pandatv","changes":{"password":"fixture-new"}}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(stored.get("pandalive").is_none());
    assert_eq!(stored["pandatv"]["password"], "fixture-new");
    assert_eq!(stored["pandatv"]["accessToken"], "fixture-token");
    assert_eq!(stored["pandatv"]["extra"], "keep");
    assert_eq!(stored["douyin"]["password"], "other-secret");
    let http = client();
    let a = http
        .put(format!("{base}/api/accounts"))
        .json(&json!({"platform":"douyin","changes":{"username":"changed"}}));
    let b = http
        .put(format!("{base}/api/accounts"))
        .json(&json!({"platform":"bilibili","changes":{"username":"added"}}));
    let (a, b) = tokio::join!(a.send(), b.send());
    assert!(a.unwrap().status().is_success());
    assert!(b.unwrap().status().is_success());
    let stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored["douyin"]["username"], "changed");
    assert_eq!(stored["bilibili"]["username"], "added");
    server.shutdown().await.unwrap();
}
#[tokio::test]
async fn cookie_aliases_are_read_without_rewriting_and_cleared_together() {
    let (root, server, base) = server().await;
    let path = root.path().join("config/cookies.json");
    std::fs::write(
        &path,
        br#"{"pandalive":"fixture-old","douyin":"fixture-keep"}"#,
    )
    .unwrap();
    let before = std::fs::read(&path).unwrap();
    let response: Value = client()
        .get(format!("{base}/api/cookies"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["cookies"]["pandatv"], "fixture-old");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(client()
        .put(format!("{base}/api/cookies"))
        .json(&json!({"cookies":{"pandatv":""}}))
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    let saved: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(saved.get("pandalive").is_none());
    assert!(saved.get("pandatv").is_none());
    assert_eq!(saved["douyin"], "fixture-keep");
    server.shutdown().await.unwrap();
}
#[test]
fn invalid_integer_settings_and_unsafe_scripts_are_rejected() {
    for patch in [
        json!({"smtp_port":1.5}),
        json!({"platform_max_concurrent_requests":"2.5"}),
        json!({"platform_request_interval":0.5}),
        json!({"generate_time_subtitle_file":"yes"}),
    ] {
        assert!(streamcap_core::config::validate_settings(patch.as_object().unwrap()).is_err());
    }
    assert!(
        streamcap_core::automation::ScriptSpec::parse(r#"["C:/Tools/{room}.exe","{file}"]"#)
            .is_err()
    );
    assert!(
        streamcap_core::automation::ScriptSpec::parse(r#"["C:/Tools/script.cmd","{file}"]"#)
            .is_err()
    );
}
#[tokio::test]
async fn invalid_notification_channel_does_not_block_a_valid_local_channel() {
    let received = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let capture = received.clone();
    let app = Router::new().route(
        "/wechat",
        post(move |Json(body): Json<Value>| {
            let capture = capture.clone();
            async move {
                capture.lock().unwrap().push(body);
                Json(json!({"errcode":0}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let workspace = Workspace::from_repo_root(root.path());
    let mut config = ConfigStore::load(workspace.clone()).unwrap();
    config.update_user_config(json!({"stream_start_notification_enabled":true,"system_notification_enabled":false,"dingtalk_enabled":true,"dingtalk_webhook_url":"not-a-url","wechat_enabled":true,"wechat_webhook_url":format!("http://{address}/wechat")}).as_object().unwrap().clone()).unwrap();
    let config = Arc::new(RwLock::new(config));
    let store = Store::new(workspace);
    let mut events = store.subscribe();
    let notifications = Notifications::new(config, store);
    let record = Recording::new(
        "fixture".into(),
        "https://example.test/live".into(),
        "Fixture".into(),
    );
    notifications.changed(&record, true).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if !received.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(received.lock().unwrap()[0]["msgtype"], "text");
    let event = events.recv().await.unwrap();
    assert_eq!(event.topic, "snack");
    assert!(!event.payload.to_string().contains("not-a-url"));
    notifications.shutdown().await;
    handle.abort();
    let _ = handle.await;
}
#[tokio::test]
async fn shutdown_api_only_schedules_cancellable_core_events() {
    let (_root, server, base) = server().await;
    let http = client();
    for value in [json!({"hours":0}), json!({"hours":169})] {
        assert_eq!(
            http.post(format!("{base}/api/automation/shutdown"))
                .json(&value)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    for value in [json!({"hours":2}), json!({"hours":null})] {
        assert!(http
            .post(format!("{base}/api/automation/shutdown"))
            .json(&value)
            .send()
            .await
            .unwrap()
            .status()
            .is_success());
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn screenshots_validate_png_and_never_overwrite_sources() {
    use base64::Engine;
    let (root, server, base) = server().await;
    let media = root.path().join("downloads");
    std::fs::create_dir_all(&media).unwrap();
    std::fs::write(media.join("fixture.ts"), b"source-unchanged").unwrap();
    server
        .state()
        .config
        .write()
        .await
        .update_user_config(json!({"live_save_path":media}).as_object().unwrap().clone())
        .unwrap();
    let broken = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\ninvalid");
    assert_eq!(
        client()
            .post(format!("{base}/api/media/screenshot"))
            .json(&json!({"path":"fixture.ts","pngBase64":broken}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 2, 2);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[255u8; 16]).unwrap();
    }
    let response=client().post(format!("{base}/api/media/screenshot")).json(&json!({"path":"fixture.ts","pngBase64":base64::engine::general_purpose::STANDARD.encode(bytes)})).send().await.unwrap();
    assert!(response.status().is_success());
    assert_eq!(
        std::fs::read(media.join("fixture.ts")).unwrap(),
        b"source-unchanged"
    );
    assert_eq!(std::fs::read_dir(media).unwrap().count(), 2);
    server.shutdown().await.unwrap();
}
