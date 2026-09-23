//! Native API integration tests using isolated directories and real loopback HTTP.
//!
//! 每个测试在独立端口启动真实 HTTP 服务，用真实 HTTP 请求验证，
//! 不使用 mock —— 契约错误必须在测试里暴露，而不是在打包后的 exe 里。

use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use streamcap_core::api::{bootstrap, router, ApiState};
use streamcap_core::paths::Workspace;
use tokio::sync::RwLock;

/// 在临时目录启动一个后端，返回 (base_url, 临时目录守卫)。
async fn start_backend() -> (String, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("创建临时目录");
    let workspace = Workspace::from_repo_root(dir.path());
    seed_defaults(&workspace);

    let resolver = streamcap_core::Resolver::with_login_endpoints(
        streamcap_core::platforms::kuaishou_login::LoginEndpoints::loopback("http://127.0.0.1:1/")
            .unwrap(),
    );
    let state = streamcap_core::api::bootstrap_with_resolver(workspace, resolver)
        .await
        .expect("后端初始化失败");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state);

    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    (format!("http://{addr}"), dir)
}

#[tokio::test]
async fn missing_recording_roots_never_rewrite_settings_or_historical_paths_at_bootstrap() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = Workspace {
        resource_dir: directory.path().join("StreamCap_new"),
        user_data_dir: directory.path().join("profile"),
    };
    workspace.ensure_ready().unwrap();
    let missing = directory.path().join("StreamCap_old/downloads");
    let settings =
        serde_json::to_vec_pretty(&json!({"live_save_path":missing,"loop_time_seconds":"4500"}))
            .unwrap();
    std::fs::write(workspace.user_settings_path(), &settings).unwrap();
    let mut recording = streamcap_core::Recording::new(
        "history".into(),
        "http://127.0.0.1:1/live.ts".into(),
        "History".into(),
    );
    recording.recording_dir = Some(missing.join("room").to_string_lossy().into());
    let recordings = serde_json::to_vec_pretty(&json!([recording.to_storage()])).unwrap();
    std::fs::write(workspace.recordings_path(), &recordings).unwrap();
    let state = bootstrap(workspace.clone()).await.unwrap();
    assert!(!missing.exists());
    assert_eq!(
        std::fs::read(workspace.user_settings_path()).unwrap(),
        settings
    );
    assert_eq!(
        std::fs::read(workspace.recordings_path()).unwrap(),
        recordings
    );
    assert_eq!(
        state.store.get("history").await.unwrap().recording_dir,
        recording.recording_dir
    );
    streamcap_core::api::shutdown(&state).await.unwrap();
}

#[tokio::test]
async fn import_counts_alias_duplicates_but_preserves_unknown_access_parameters() {
    let (base, directory) = start_backend().await;
    let client = reqwest::Client::new();
    let response: Value = client.post(format!("{base}/api/recordings"))
        .json(&json!({"items":[
            {"url":"https://live.douyin.com/123?access=one", "streamerName":"first"},
            {"url":"https://www.douyin.com/live/123?access=one&utm_source=share", "streamerName":"do not overwrite"},
            {"url":"https://live.douyin.com/123?access=two"},
            {"url":"https://live.douyin.com/123?access=one"}
        ]})).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(response["created"].as_array().unwrap().len(), 2);
    assert_eq!(response["skipped"].as_array().unwrap().len(), 2);
    assert_eq!(response["created"][0]["streamerName"], "first");
    let path = directory.path().join("config/recordings.json");
    let unchanged = std::fs::read(&path).unwrap();
    let second = response["created"][1]["recId"].as_str().unwrap();
    let edit = client
        .put(format!("{base}/api/recordings/{second}"))
        .json(&json!({"changes":{"url":"https://live.douyin.com/123?access=one"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(edit.status(), 409);
    assert_eq!(std::fs::read(path).unwrap(), unchanged);
}

fn seed_defaults(workspace: &Workspace) {
    workspace.ensure_ready().unwrap();
    std::fs::write(
        workspace.default_settings_path(),
        r#"{
            "video_format": "TS",
            "record_quality": "OD",
            "video_segment_time": "1800",
            "loop_time_seconds": "600",
            "recordings": []
        }"#,
    )
    .unwrap();
}

async fn get_json(url: &str) -> Value {
    reqwest::get(url).await.unwrap().json().await.unwrap()
}

#[tokio::test]
async fn status_matches_frontend_contract() {
    let (base, _guard) = start_backend().await;
    let status = get_json(&format!("{base}/api/status")).await;

    // 前端 api/gateway.rs 期望的字段名
    assert_eq!(status["ok"], json!(true));
    assert!(status["version"].is_string());
    assert_eq!(status["buildId"], env!("STREAMCAP_BUILD_ID"));
    assert!(status["activeRecordings"].is_number());
    assert!(status["totalRecordings"].is_number());
    assert!(status["resolverReady"].is_boolean());
}

#[tokio::test]
async fn create_then_list_then_delete_roundtrip() {
    let (base, _guard) = start_backend().await;
    let client = reqwest::Client::new();

    // 空列表
    let listed = get_json(&format!("{base}/api/recordings")).await;
    assert_eq!(listed.as_array().unwrap().len(), 0);

    // 批量创建（前端「每行一个地址」提交的形态）
    let created: Value = client
        .post(format!("{base}/api/recordings"))
        .json(&json!({ "items": [
            { "url": "https://live.douyin.com/111", "streamerName": "主播A" },
            { "url": "https://live.bilibili.com/222" }
        ]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(created["created"].as_array().unwrap().len(), 2);

    let listed = get_json(&format!("{base}/api/recordings")).await;
    let list = listed.as_array().unwrap();
    assert_eq!(list.len(), 2);

    // 字段名必须是 camelCase（前端 serde rename 依赖）
    let first = &list[0];
    assert!(first["recId"].is_string());
    assert!(first["streamerName"].is_string());
    assert!(first["monitorStatus"].is_boolean());
    assert!(first["isLive"].is_boolean());
    assert!(first["isRecording"].is_boolean());
    assert_eq!(first["platformKey"], json!("douyin"));
    assert!(first.get("rec_id").is_none(), "不应出现 snake_case 字段");

    // 默认值来自 default_settings.json
    assert_eq!(first["recordFormat"], json!("TS"));
    assert_eq!(first["quality"], json!("OD"));

    // 删除
    let rec_id = first["recId"].as_str().unwrap().to_string();
    let resp = client
        .delete(format!("{base}/api/recordings/{rec_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let listed = get_json(&format!("{base}/api/recordings")).await;
    assert_eq!(listed.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn delete_unknown_recording_returns_404() {
    let (base, _guard) = start_backend().await;
    let resp = reqwest::Client::new()
        .delete(format!("{base}/api/recordings/does-not-exist"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn monitor_toggle_flips_flag_and_reports_state() {
    let (base, _guard) = start_backend().await;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/api/recordings"))
        .json(&json!({ "url": "https://live.douyin.com/1" }))
        .send()
        .await
        .unwrap();
    let list = get_json(&format!("{base}/api/recordings")).await;
    let rec_id = list[0]["recId"].as_str().unwrap().to_string();
    assert_eq!(list[0]["monitorStatus"], json!(true));

    let toggled: Value = client
        .post(format!("{base}/api/recordings/{rec_id}/monitor"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(toggled["monitorStatus"], json!(false));

    let toggled: Value = client
        .post(format!("{base}/api/recordings/{rec_id}/monitor"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(toggled["monitorStatus"], json!(true));
}

#[tokio::test]
async fn update_recording_applies_changes() {
    let (base, _guard) = start_backend().await;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/api/recordings"))
        .json(&json!({ "url": "https://live.douyin.com/1", "streamerName": "主播" }))
        .send()
        .await
        .unwrap();
    let list = get_json(&format!("{base}/api/recordings")).await;
    let rec_id = list[0]["recId"].as_str().unwrap().to_string();

    let updated: Value = client
        .put(format!("{base}/api/recordings/{rec_id}"))
        .json(
            &json!({ "changes": { "quality": "UHD", "segmentRecord": true }, "followGlobal": [] }),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(updated["quality"], json!("UHD"));
    assert_eq!(updated["segmentRecord"], json!(true));
    assert_eq!(
        updated["displayTitle"],
        json!("主播 - UHD"),
        "标题应随清晰度更新"
    );
}

#[tokio::test]
async fn settings_read_and_write_roundtrip() {
    let (base, _guard) = start_backend().await;
    let client = reqwest::Client::new();

    let settings = get_json(&format!("{base}/api/settings")).await;
    assert_eq!(settings["userConfig"]["video_format"], json!("TS"));
    assert!(settings["defaultConfig"].is_object());

    client
        .put(format!("{base}/api/settings"))
        .json(&json!({ "userConfig": { "video_format": "MP4", "loop_time_seconds": "120" } }))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();

    let settings = get_json(&format!("{base}/api/settings")).await;
    assert_eq!(settings["userConfig"]["video_format"], json!("MP4"));
    assert_eq!(settings["userConfig"]["loop_time_seconds"], json!("120"));
}

#[tokio::test]
async fn settings_only_broadcast_changed_inherited_tasks_and_skip_noop_updates() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = Workspace::from_repo_root(directory.path());
    seed_defaults(&workspace);
    let state = bootstrap(workspace.clone()).await.unwrap();
    let inherited = streamcap_core::Recording::new(
        "inherited".into(),
        "http://127.0.0.1:1/inherited".into(),
        "Inherited".into(),
    );
    let mut explicit = streamcap_core::Recording::new(
        "explicit".into(),
        "http://127.0.0.1:1/explicit".into(),
        "Explicit".into(),
    );
    explicit.record_format = Some("MKV".into());
    explicit
        .inherited_fields
        .retain(|field| field != "record_format");
    state.store.insert(vec![inherited, explicit]).await.unwrap();
    {
        let config = state.config.read().await;
        streamcap_core::store::apply_global_defaults(&state.store, &config).await;
    }
    let stored = std::fs::read(workspace.recordings_path()).unwrap();
    let mut events = state.store.subscribe();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = router(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    for (patch, changed, updated_ids) in [
        (json!({"theme_mode":"dark"}), vec!["theme_mode"], vec![]),
        (json!({"theme_mode":"dark"}), vec![], vec![]),
        (
            json!({"video_format":"MP4"}),
            vec!["video_format"],
            vec!["inherited"],
        ),
        (json!({"video_format":"MP4"}), vec![], vec![]),
    ] {
        let response: Value = client
            .put(format!("{base}/api/settings"))
            .json(&json!({"userConfig":patch}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response["changed"], json!(changed));
        let mut updated = Vec::new();
        let mut settings_events = 0;
        while let Ok(event) = events.try_recv() {
            if event.topic == "update" {
                updated.push(event.payload["recId"].as_str().unwrap().to_owned());
            }
            if event.topic == "settings" {
                settings_events += 1;
            }
        }
        assert_eq!(updated, updated_ids);
        assert_eq!(settings_events, usize::from(!changed.is_empty()));
    }
    assert_eq!(
        state
            .store
            .get("inherited")
            .await
            .unwrap()
            .record_format
            .as_deref(),
        Some("MP4")
    );
    assert_eq!(
        state
            .store
            .get("explicit")
            .await
            .unwrap()
            .record_format
            .as_deref(),
        Some("MKV")
    );
    assert_eq!(std::fs::read(workspace.recordings_path()).unwrap(), stored);
    streamcap_core::api::shutdown(&state).await.unwrap();
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn cookies_write_read_and_clear() {
    let (base, _guard) = start_backend().await;
    let client = reqwest::Client::new();

    client
        .put(format!("{base}/api/cookies"))
        .json(&json!({ "cookies": { "douyin": "session=abc", "kuaishou": "kw=xyz" } }))
        .send()
        .await
        .unwrap();

    let cookies = get_json(&format!("{base}/api/cookies")).await;
    assert_eq!(cookies["cookies"]["douyin"], json!("session=abc"));

    // 空值清除该平台
    client
        .put(format!("{base}/api/cookies"))
        .json(&json!({ "cookies": { "douyin": "" } }))
        .send()
        .await
        .unwrap();

    let cookies = get_json(&format!("{base}/api/cookies")).await;
    assert!(cookies["cookies"].get("douyin").is_none());
    assert_eq!(cookies["cookies"]["kuaishou"], json!("kw=xyz"));
}

#[tokio::test]
async fn storage_lists_created_files() {
    let (base, _guard) = start_backend().await;

    // 在录制根目录写入一个文件（模拟已录制产物）
    let listing = get_json(&format!("{base}/api/storage")).await;
    let root = listing["root"].as_str().unwrap().to_string();
    std::fs::create_dir_all(Path::new(&root).join("抖音/主播A")).unwrap();
    std::fs::write(Path::new(&root).join("抖音/主播A/video.mp4"), b"0123456789").unwrap();

    let listing = get_json(&format!("{base}/api/storage")).await;
    let items = listing["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], json!("抖音"));
    assert_eq!(items[0]["isDir"], json!(true));
    assert_eq!(items[0]["size"], json!(10));
    assert_eq!(listing["totalSize"], json!(10));

    // 进入子目录
    let sub = get_json(&format!("{base}/api/storage?subfolder=%E6%8A%96%E9%9F%B3")).await;
    assert_eq!(sub["items"].as_array().unwrap().len(), 1);
    assert_eq!(sub["items"][0]["name"], json!("主播A"));
}

#[tokio::test]
async fn storage_delete_rejects_traversal() {
    let (base, _guard) = start_backend().await;
    let resp = reqwest::Client::new()
        .delete(format!("{base}/api/storage?path=../outside.txt"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn video_streaming_supports_range_requests() {
    let (base, _guard) = start_backend().await;
    let client = reqwest::Client::new();

    let listing = get_json(&format!("{base}/api/storage")).await;
    let root = listing["root"].as_str().unwrap().to_string();
    std::fs::write(Path::new(&root).join("clip.mp4"), b"0123456789").unwrap();

    // 完整请求
    let full = client
        .get(format!("{base}/api/videos?path=clip.mp4"))
        .send()
        .await
        .unwrap();
    assert_eq!(full.status(), 200);
    assert_eq!(full.headers()["accept-ranges"], "bytes");
    assert_eq!(full.bytes().await.unwrap().len(), 10);

    // Range 请求（<video> 拖动进度依赖）
    let partial = client
        .get(format!("{base}/api/videos?path=clip.mp4"))
        .header("Range", "bytes=2-5")
        .send()
        .await
        .unwrap();
    assert_eq!(partial.status(), 206);
    assert_eq!(partial.headers()["content-range"], "bytes 2-5/10");
    assert_eq!(partial.bytes().await.unwrap().as_ref(), b"2345");
}

#[tokio::test]
async fn sse_stream_delivers_snack_and_delete_events() {
    let (base, _guard) = start_backend().await;
    let client = reqwest::Client::new();

    // 先建立 SSE 连接
    let mut stream = client
        .get(format!("{base}/api/events"))
        .send()
        .await
        .unwrap()
        .bytes_stream();

    use futures::StreamExt;
    // 触发事件：创建任务 -> 产生 update 事件
    client
        .post(format!("{base}/api/recordings"))
        .json(&json!({ "url": "https://live.douyin.com/1" }))
        .send()
        .await
        .unwrap();

    let mut collected = String::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline && !collected.contains("event: update") {
        match tokio::time::timeout(std::time::Duration::from_secs(2), stream.next()).await {
            Ok(Some(Ok(chunk))) => collected.push_str(&String::from_utf8_lossy(&chunk)),
            _ => break,
        }
    }

    assert!(
        collected.contains("event: update"),
        "SSE 应推送 update 事件，实际收到: {collected:?}"
    );
}

#[tokio::test]
async fn recordings_persist_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Workspace::from_repo_root(dir.path());
    seed_defaults(&workspace);

    // 第一次启动：创建任务
    {
        let state = bootstrap(workspace.clone()).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(state);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        reqwest::Client::new()
            .post(format!("http://{addr}/api/recordings"))
            .json(
                &json!({ "url": "https://live.douyin.com/persist", "streamerName": "持久化主播" }),
            )
            .send()
            .await
            .unwrap();
    }

    // 第二次启动：应从 recordings.json 恢复
    let state = bootstrap(workspace).await.unwrap();
    let restored = state.store.all().await;
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].streamer_name, "持久化主播");
    assert_eq!(restored[0].platform_key.as_deref(), Some("douyin"));
}

#[tokio::test]
async fn qr_routes_use_in_process_sessions_and_expose_errors() {
    let (base, _guard) = start_backend().await;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{base}/api/qr/kuaishou/start"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let snapshot: Value = response.json().await.unwrap();
    assert_eq!(snapshot["state"], "error");
    let id = snapshot["sessionId"].as_str().unwrap();
    assert!(!id.is_empty());
    let status = get_json(&format!("{base}/api/qr/kuaishou/status?sessionId={id}")).await;
    assert_eq!(status["state"], "error");
    assert_eq!(
        client
            .post(format!("{base}/api/qr/kuaishou/cancel?sessionId={id}"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
}

/// 构造一个未使用的 ApiState 类型引用，确保公开导出稳定（编译期契约）。
#[allow(dead_code)]
fn _assert_public_types(state: &ApiState) {
    let _: &Arc<RwLock<streamcap_core::ConfigStore>> = &state.config;
    let _: &Arc<AtomicBool> = &state.recording_enabled;
}

// UI contracts: isolated workspaces only; never use the desktop's fixed business port.
async fn create_named(base: &str, suffix: &str) -> Value {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/recordings"))
        .json(&json!({"url": format!("https://live.douyin.com/{suffix}"), "streamerName": suffix}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    response.json::<Value>().await.unwrap()["created"][0].clone()
}

#[tokio::test]
async fn edit_contract_uses_camel_case_and_rejects_silent_noops() {
    let (base, _guard) = start_backend().await;
    let rec = create_named(&base, "field-contract").await;
    let endpoint = format!("{base}/api/recordings/{}", rec["recId"].as_str().unwrap());
    let client = reqwest::Client::new();
    let wrong = client
        .put(&endpoint)
        .json(&json!({"changes": {"record_format": "MKV"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 400);
    let good = client.put(&endpoint).json(&json!({"changes": {"recordFormat": "MKV", "segmentTime": "120", "videoBitrate": 2400}})).send().await.unwrap();
    assert_eq!(good.status(), 200);
    let updated: Value = good.json().await.unwrap();
    assert_eq!(updated["recordFormat"], "MKV");
    assert_eq!(updated["segmentTime"], "120");
    assert_eq!(updated["videoBitrate"], 2400);
    let clear: Value = client
        .put(&endpoint)
        .json(&json!({"changes": {"videoBitrate": null}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(clear.get("videoBitrate").is_none_or(Value::is_null));
}

#[tokio::test]
async fn batch_edit_requires_explicit_nonempty_targets() {
    let (base, _guard) = start_backend().await;
    create_named(&base, "batch-a").await;
    create_named(&base, "batch-b").await;
    let client = reqwest::Client::new();
    for body in [
        json!({"changes": {"quality": "HD"}}),
        json!({"recIds": [], "changes": {"quality": "HD"}}),
    ] {
        let response = client
            .post(format!("{base}/api/recordings/batch-edit"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_client_error());
    }
    let list = get_json(&format!("{base}/api/recordings")).await;
    assert!(list
        .as_array()
        .unwrap()
        .iter()
        .all(|rec| rec["quality"] == "OD"));
}

#[tokio::test]
async fn batch_edit_only_changes_selected_ids_and_is_atomic_for_missing_ids() {
    let (base, guard) = start_backend().await;
    let a = create_named(&base, "selected").await;
    create_named(&base, "unselected").await;
    let client = reqwest::Client::new();
    let endpoint = format!("{base}/api/recordings/batch-edit");
    let response = client
        .post(&endpoint)
        .json(&json!({"recIds": [a["recId"]], "changes": {"quality": "HD"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let list = get_json(&format!("{base}/api/recordings")).await;
    assert_eq!(list[0]["quality"], "HD");
    assert_eq!(list[1]["quality"], "OD");
    let path = guard.path().join("config/recordings.json");
    let before = std::fs::read(&path).unwrap();
    let response = client
        .post(&endpoint)
        .json(&json!({"recIds": [a["recId"], "missing"], "changes": {"quality": "SD"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    assert_eq!(std::fs::read(path).unwrap(), before);
    assert_eq!(get_json(&format!("{base}/api/recordings")).await, list);
}

#[tokio::test]
async fn inherited_values_follow_global_immediately_but_overrides_stay_fixed() {
    let (base, guard) = start_backend().await;
    let a = create_named(&base, "inherited").await;
    let b = create_named(&base, "override").await;
    assert!(a["inheritedFields"]
        .as_array()
        .unwrap()
        .contains(&json!("quality")));
    let client = reqwest::Client::new();
    let update = client
        .put(format!(
            "{base}/api/recordings/{}",
            b["recId"].as_str().unwrap()
        ))
        .json(&json!({"changes": {"quality": "UHD", "segmentRecord": false}}))
        .send()
        .await
        .unwrap();
    assert_eq!(update.status(), 200);
    let update = client.put(format!("{base}/api/settings"))
        .json(&json!({"userConfig": {"record_quality": "HD", "segmented_recording_enabled": true, "video_segment_time": "90"}})).send().await.unwrap();
    assert_eq!(update.status(), 200);
    let list = get_json(&format!("{base}/api/recordings")).await;
    assert_eq!(list[0]["quality"], "HD");
    assert_eq!(list[0]["segmentRecord"], true);
    assert_eq!(list[0]["segmentTime"], "90");
    assert_eq!(list[1]["quality"], "UHD");
    assert_eq!(list[1]["segmentRecord"], false);
    let reset: Value = client
        .put(format!(
            "{base}/api/recordings/{}",
            b["recId"].as_str().unwrap()
        ))
        .json(&json!({"followGlobal": ["quality", "segment_record"]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reset["quality"], "HD");
    assert_eq!(reset["segmentRecord"], true);
    let stored: Value = serde_json::from_slice(
        &std::fs::read(guard.path().join("config/recordings.json")).unwrap(),
    )
    .unwrap();
    assert!(stored.is_array());
    assert!(stored[1]["quality"].is_null());
    assert!(stored[1].get("inheritedFields").is_none());
    assert!(stored[1].get("recordFormat").is_none());
}

#[tokio::test]
async fn duplicate_creation_and_invalid_edit_leave_existing_tasks_untouched() {
    let (base, guard) = start_backend().await;
    let rec = create_named(&base, "duplicate").await;
    let path = guard.path().join("config/recordings.json");
    let original: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let client = reqwest::Client::new();
    let duplicate = client
        .post(format!("{base}/api/recordings"))
        .json(&json!({"items": [{"url": "https://live.douyin.com/new"}, {"url": rec["url"]}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate.status(), 200);
    let duplicate_body: Value = duplicate.json().await.unwrap();
    assert_eq!(duplicate_body["created"].as_array().unwrap().len(), 1);
    assert_eq!(duplicate_body["skipped"].as_array().unwrap().len(), 1);
    assert_eq!(duplicate_body["skipped"][0]["reason"], "duplicate");
    let after_import = std::fs::read(&path).unwrap();
    let imported: Value = serde_json::from_slice(&after_import).unwrap();
    assert_eq!(
        imported[0], original[0],
        "skipping duplicates must preserve every stored field"
    );
    for change in [
        json!({"quality":"BAD"}),
        json!({"videoBitrate": -1}),
        json!({"segmentTime":"0"}),
        json!({"segmentRecord":"true"}),
    ] {
        let response = client
            .put(format!(
                "{base}/api/recordings/{}",
                rec["recId"].as_str().unwrap()
            ))
            .json(&json!({"changes": change}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }
    assert_eq!(
        std::fs::read(&path).unwrap(),
        after_import,
        "invalid edits must not change any stored bytes"
    );
    let stored: Value = serde_json::from_slice(&after_import).unwrap();
    assert_eq!(stored.as_array().unwrap().len(), 2);
    assert!(stored
        .as_array()
        .unwrap()
        .iter()
        .any(|item| { item["rec_id"] == rec["recId"] && item["url"] == rec["url"] }));
    assert!(stored
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["url"] == "https://live.douyin.com/new"));
}

#[tokio::test]
async fn failed_disk_write_does_not_report_success_or_publish_new_tasks() {
    let (base, guard) = start_backend().await;
    std::fs::create_dir(guard.path().join("config/recordings.json")).unwrap();
    let response = reqwest::Client::new()
        .post(format!("{base}/api/recordings"))
        .json(&json!({"url":"https://live.douyin.com/write-failure"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 500);
    assert_eq!(get_json(&format!("{base}/api/recordings")).await, json!([]));
}

#[tokio::test]
async fn monitor_switch_is_persisted() {
    let (base, guard) = start_backend().await;
    let rec = create_named(&base, "monitor-persist").await;
    let response = reqwest::Client::new()
        .post(format!(
            "{base}/api/recordings/{}/monitor",
            rec["recId"].as_str().unwrap()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let stored: Value = serde_json::from_slice(
        &std::fs::read(guard.path().join("config/recordings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(stored[0]["monitor_status"], false);
}

#[tokio::test]
async fn foreign_web_pages_cannot_read_cookies_or_mutate_tasks() {
    let (base, _guard) = start_backend().await;
    let client = reqwest::Client::new();
    let allowed = client
        .get(format!("{base}/api/status"))
        .header("Origin", "http://tauri.localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), 200);
    assert_eq!(
        allowed.headers()["access-control-allow-origin"],
        "http://tauri.localhost"
    );
    let denied = client
        .get(format!("{base}/api/cookies"))
        .header("Origin", "https://foreign.invalid")
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);
    let denied = client
        .post(format!("{base}/api/recordings/anything/start"))
        .header("Origin", "https://foreign.invalid")
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);
    let preflight = client
        .request(reqwest::Method::OPTIONS, format!("{base}/api/cookies"))
        .header("Origin", "https://foreign.invalid")
        .header("Access-Control-Request-Method", "PUT")
        .send()
        .await
        .unwrap();
    assert!(!preflight
        .headers()
        .contains_key("access-control-allow-origin"));
}
#[tokio::test]
async fn native_settings_reject_unsupported_features_and_formats() {
    let (base, _guard) = start_backend().await;
    let client = reqwest::Client::new();
    for patch in [
        json!({"delete_original":"yes"}),
        json!({"video_format":"INVALID"}),
    ] {
        let response = client
            .put(format!("{base}/api/settings"))
            .json(&json!({"userConfig":patch}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }
}

#[tokio::test]
async fn native_close_preference_defaults_to_ask_and_validates_saved_values() {
    let (base, _guard) = start_backend().await;
    let client = reqwest::Client::new();
    let defaults = get_json(&format!("{base}/api/settings")).await;
    assert_eq!(defaults["defaultConfig"]["close_action"], "ask");
    for action in ["tray", "exit", "ask"] {
        let result = client
            .put(format!("{base}/api/settings"))
            .json(&json!({"userConfig":{"close_action":action}}))
            .send()
            .await
            .unwrap();
        assert_eq!(result.status(), 200);
        let settings = get_json(&format!("{base}/api/settings")).await;
        assert_eq!(settings["userConfig"]["close_action"], action);
    }
    for invalid in [json!(true), json!("always_kill"), Value::Null] {
        let result = client
            .put(format!("{base}/api/settings"))
            .json(&json!({"userConfig":{"close_action":invalid}}))
            .send()
            .await
            .unwrap();
        assert_eq!(result.status(), 400);
    }
    let settings = get_json(&format!("{base}/api/settings")).await;
    assert_eq!(settings["userConfig"]["close_action"], "ask");
}

#[tokio::test]
async fn interval_updates_are_committed_and_invalid_changes_do_not_publish() {
    let (base, _guard) = start_backend().await;
    let client = reqwest::Client::new();
    let result = client
        .put(format!("{base}/api/settings"))
        .json(&json!({"userConfig":{"loop_time_seconds":"4500"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), 200);
    let result = client
        .put(format!("{base}/api/settings"))
        .json(&json!({"userConfig":{"loop_time_seconds":"0"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), 400);
    let settings = get_json(&format!("{base}/api/settings")).await;
    assert_eq!(settings["userConfig"]["loop_time_seconds"], "4500");
}

#[tokio::test]
async fn media_conversion_and_explicit_cleanup_settings_are_valid() {
    assert!(streamcap_core::config::validate_settings(
        json!({"convert_to_mp4":true}).as_object().unwrap()
    )
    .is_ok());
    assert!(streamcap_core::config::validate_settings(
        json!({"delete_original":true}).as_object().unwrap()
    )
    .is_ok());
}

#[tokio::test]
async fn creating_and_resuming_a_monitor_schedules_checks_without_start_requests() {
    use streamcap_core::service::{Server, ServerOptions};
    let dir = tempfile::tempdir().unwrap();
    let state = bootstrap(Workspace::from_repo_root(dir.path()))
        .await
        .unwrap();
    state.config.write().await.update_user_config(json!({"loop_time_seconds":"4500","platform_request_interval":"0","only_notify_no_record":true}).as_object().unwrap().clone()).unwrap();
    let server = Server::start(
        state.clone(),
        ServerOptions {
            port: 0,
            monitoring: true,
        },
    )
    .await
    .unwrap();
    let client = reqwest::Client::new();
    let base = format!("http://{}", server.address());
    let created: Value = client
        .post(format!("{base}/api/recordings"))
        .json(&json!({"url":"http://127.0.0.1:1/fixture.mp4","streamerName":"automatic"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["created"][0]["recId"].as_str().unwrap();
    assert_eq!(created["created"][0]["monitorStatus"], true);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !state.store.get(id).await.unwrap().is_live {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(state.engine.active_ids().await.is_empty());
    assert!(client
        .post(format!("{base}/api/recordings/{id}/monitor"))
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    state.store.update(id, |r| r.is_live = false).await;
    assert!(client
        .post(format!("{base}/api/recordings/{id}/monitor"))
        .send()
        .await
        .unwrap()
        .status()
        .is_success());
    // A resumed room joins the 20-40 second automatic queue instead of causing a burst.
    // Advance only the test clock, never the real platform's configured interval.
    tokio::time::pause();
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(!state.store.get(id).await.unwrap().is_live);
    tokio::time::advance(std::time::Duration::from_secs(40)).await;
    for _ in 0..30 {
        tokio::task::yield_now().await;
    }
    assert!(state.store.get(id).await.unwrap().is_live);
    tokio::time::resume();
    assert_eq!(
        state.config.read().await.get_i64("loop_time_seconds", 0),
        4500
    );
    server.shutdown().await.unwrap();
}
