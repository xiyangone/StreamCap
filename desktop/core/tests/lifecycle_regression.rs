//! Replays lifecycle races through real loopback HTTP and the native FLV recorder.
//! All state and media are confined to this test's temporary directory.
use axum::{body::Body, routing::get, Router};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use streamcap_core::{
    api::{self, ApiState},
    resolver::StreamInfo,
    service::{Server, ServerOptions},
    Recording, Workspace,
};

struct Fixture {
    _directory: tempfile::TempDir,
    workspace: Workspace,
    state: ApiState,
    server: Server,
    source: tokio::task::JoinHandle<()>,
    info: StreamInfo,
    base: String,
    client: reqwest::Client,
}
impl Fixture {
    async fn new(monitor: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::from_repo_root(directory.path());
        let state = api::bootstrap(workspace.clone()).await.unwrap();
        state
            .config
            .write()
            .await
            .update_user_config(
                json!({
                    "live_save_path": directory.path().join("downloads"),
                    "recording_space_threshold":"0.1", "loop_time_seconds":"4500",
                    "convert_to_mp4":false, "generate_time_subtitle_file":false,
                    "execute_custom_script":false, "system_notification_enabled":false
                })
                .as_object()
                .unwrap()
                .clone(),
            )
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source_url = format!("http://{}/source.flv", listener.local_addr().unwrap());
        let stream = Router::new().route(
            "/source.flv",
            get(|| async {
                Body::from_stream(futures::stream::unfold(0usize, |index| async move {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    let mut bytes = vec![0u8; 4096];
                    if index == 0 {
                        bytes[..9].copy_from_slice(&[b'F', b'L', b'V', 1, 5, 0, 0, 0, 9]);
                    }
                    Some((Ok::<_, std::io::Error>(bytes), index + 1))
                }))
            }),
        );
        let source = tokio::spawn(async move {
            axum::serve(listener, stream).await.unwrap();
        });
        let mut record = Recording::new("race".into(), source_url.clone(), "Fixture".into());
        record.monitor_status = monitor;
        record.record_format = Some("FLV".into());
        record.segment_record = Some(false);
        record.flv_use_direct_download = Some(true);
        state.store.insert(vec![record]).await.unwrap();
        let info = StreamInfo {
            anchor_name: "Fixture".into(),
            is_live: true,
            flv_url: source_url,
            ..Default::default()
        };
        let server = Server::start(
            state.clone(),
            ServerOptions {
                port: 0,
                monitoring: false,
            },
        )
        .await
        .unwrap();
        let base = format!("http://{}", server.address());
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(8))
            .build()
            .unwrap();
        Self {
            _directory: directory,
            workspace,
            state,
            server,
            source,
            info,
            base,
            client,
        }
    }
    async fn finish(self) {
        self.server.shutdown().await.unwrap();
        assert!(self.state.engine.active_ids().await.is_empty());
        self.source.abort();
        let _ = self.source.await;
    }
    fn mutation(&self, action: &str) -> reqwest::RequestBuilder {
        match action {
            "delete" => self
                .client
                .delete(format!("{}/api/recordings/race", self.base)),
            "delete-batch" => self
                .client
                .post(format!("{}/api/recordings/delete", self.base))
                .json(&json!({"recIds":["race"]})),
            "edit" => self
                .client
                .put(format!("{}/api/recordings/race", self.base))
                .json(&json!({"changes":{"streamerName":"Changed"}})),
            "edit-batch" => self
                .client
                .post(format!("{}/api/recordings/batch-edit", self.base))
                .json(&json!({"recIds":["race"],"changes":{"recordFormat":"MP4"}})),
            _ => unreachable!(),
        }
    }
    async fn written_bytes(&self) -> u64 {
        let process = self.state.engine.process("race").await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let length = std::fs::metadata(&process.output_path)
                    .map(|value| value.len())
                    .unwrap_or(0);
                if length > 13 {
                    return length;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn start_delete_and_edit_are_serialized_until_runtime_state_is_published() {
    for action in ["delete", "delete-batch", "edit", "edit-batch"] {
        let fixture = Fixture::new(true).await;
        let filesystem = fixture.state.engine.filesystem_guard().await;
        let mut start = Box::pin(
            fixture
                .state
                .scheduler
                .start_recording("race".into(), &fixture.info),
        );
        assert!(futures::poll!(start.as_mut()).is_pending());
        assert!(!fixture.state.store.get("race").await.unwrap().is_recording);
        assert!(!fixture.state.engine.is_recording("race").await);
        let request = fixture.mutation(action);
        let mut mutation = tokio::spawn(async move { request.send().await.unwrap() });
        assert!(
            tokio::time::timeout(Duration::from_millis(120), &mut mutation)
                .await
                .is_err(),
            "{action} crossed an in-flight start"
        );
        drop(filesystem);
        tokio::time::timeout(Duration::from_secs(3), start)
            .await
            .unwrap()
            .unwrap();
        let response = mutation.await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::CONFLICT, "{action}");
        let record = fixture.state.store.get("race").await.unwrap();
        assert!(record.is_recording && fixture.state.engine.is_recording("race").await);
        assert_eq!(record.streamer_name, "Fixture");
        assert_eq!(record.record_format.as_deref(), Some("FLV"));
        assert!(fixture.written_bytes().await > 13);
        let output = fixture
            .state
            .engine
            .process("race")
            .await
            .unwrap()
            .output_path
            .clone();
        let response = fixture
            .client
            .post(format!("{}/api/recordings/race/stop", fixture.base))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        assert!(fixture
            .mutation(action)
            .send()
            .await
            .unwrap()
            .status()
            .is_success());
        assert!(
            output.exists(),
            "task edits/deletion never remove recorded media"
        );
        fixture.finish().await;
    }
}

#[tokio::test]
async fn deleted_task_cannot_be_started_with_an_old_resolved_source() {
    let fixture = Fixture::new(true).await;
    assert_eq!(
        fixture.state.store.remove(&["race".into()]).await.unwrap(),
        1
    );
    assert!(fixture
        .state
        .scheduler
        .start_recording("race".into(), &fixture.info)
        .await
        .is_err());
    assert!(fixture.state.engine.active_ids().await.is_empty());
    fixture.finish().await;
}

#[cfg(windows)]
#[tokio::test]
async fn failed_monitor_persistence_does_not_publish_or_stop_recording() {
    use std::os::windows::fs::OpenOptionsExt;
    for monitored in [false, true] {
        let fixture = Fixture::new(monitored).await;
        if monitored {
            fixture
                .state
                .scheduler
                .start_recording("race".into(), &fixture.info)
                .await
                .unwrap();
            fixture.written_bytes().await;
        }
        let before = std::fs::read(fixture.workspace.recordings_path()).unwrap();
        let memory = fixture.state.store.get("race").await.unwrap();
        let mut events = fixture.state.store.subscribe();
        // Allow reads/writes but deny replacing the file, matching Windows sharing failures.
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1 | 2)
            .open(fixture.workspace.recordings_path())
            .unwrap();
        let response = fixture
            .client
            .post(format!("{}/api/recordings/race/monitor", fixture.base))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(fixture.state.store.get("race").await.unwrap(), memory);
        assert_eq!(
            std::fs::read(fixture.workspace.recordings_path()).unwrap(),
            before
        );
        assert!(
            events.try_recv().is_err(),
            "failed transition emitted an event"
        );
        assert_eq!(fixture.state.engine.is_recording("race").await, monitored);
        drop(lock);
        let response = fixture
            .client
            .post(format!("{}/api/recordings/race/monitor", fixture.base))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());
        let updated: Value = response.json().await.unwrap();
        assert_eq!(updated["monitorStatus"], !monitored);
        let reloaded = streamcap_core::store::Store::new(fixture.workspace.clone());
        reloaded.load().await.unwrap();
        assert_eq!(
            reloaded.get("race").await.unwrap().monitor_status,
            !monitored
        );
        fixture.finish().await;
    }
}

#[tokio::test]
async fn concurrent_monitor_toggles_do_not_lose_transitions() {
    let fixture = Fixture::new(false).await;
    let barrier = Arc::new(tokio::sync::Barrier::new(12));
    let mut toggles = Vec::new();
    for _ in 0..12 {
        let request = fixture
            .client
            .post(format!("{}/api/recordings/race/monitor", fixture.base));
        let barrier = barrier.clone();
        toggles.push(tokio::spawn(async move {
            barrier.wait().await;
            let response = request.send().await.unwrap();
            assert!(response.status().is_success());
            response.json::<Value>().await.unwrap()["monitorStatus"]
                .as_bool()
                .unwrap()
        }));
    }
    let mut enabled = 0;
    for toggle in toggles {
        enabled += usize::from(toggle.await.unwrap());
    }
    assert_eq!(enabled, 6);
    assert!(
        !fixture
            .state
            .store
            .get("race")
            .await
            .unwrap()
            .monitor_status
    );
    let reloaded = streamcap_core::store::Store::new(fixture.workspace.clone());
    reloaded.load().await.unwrap();
    assert!(!reloaded.get("race").await.unwrap().monitor_status);
    fixture.finish().await;
}

#[tokio::test]
async fn preview_lists_only_immediate_final_media_files() {
    let fixture = Fixture::new(false).await;
    let root = fixture.state.config.read().await.recordings_root();
    let folder = root.join("preview");
    std::fs::create_dir_all(folder.join("nested.ts/deeper")).unwrap();
    for name in [
        "valid.mp4",
        "next.ts",
        "notes.txt",
        ".streamcap-remux-working.mp4",
        ".streamcap-subtitle-working.ts",
        "nested.ts/deeper/unrelated.mp4",
    ] {
        std::fs::write(folder.join(name), b"fixture").unwrap();
    }
    fixture
        .state
        .store
        .update("race", |record| {
            record.recording_dir = Some(folder.to_string_lossy().into_owned())
        })
        .await;
    let response = fixture
        .client
        .get(format!("{}/api/recordings/race/files", fixture.base))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let result: Value = response.json().await.unwrap();
    let mut paths = result["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect::<Vec<_>>();
    paths.sort();
    assert_eq!(paths, ["preview/next.ts", "preview/valid.mp4"]);
    fixture.finish().await;
}
