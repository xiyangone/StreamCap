use futures::StreamExt;
use std::{path::PathBuf, sync::atomic::Ordering, time::Duration};
use streamcap_core::{
    api,
    engine::RecordOptions,
    service::{Server, ServerOptions},
    Workspace,
};

#[tokio::test]
async fn service_shutdown_closes_sse_listener_and_preserves_data() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Workspace::from_repo_root(dir.path());
    let state = api::bootstrap(workspace.clone()).await.unwrap();
    std::fs::write(dir.path().join("downloads/keep.ts"), b"existing-media").unwrap();
    let server = Server::start(
        state.clone(),
        ServerOptions {
            port: 0,
            monitoring: true,
        },
    )
    .await
    .unwrap();
    let address = server.address();
    let base = format!("http://{address}");
    let client = reqwest::Client::new();
    let mut events = client
        .get(format!("{base}/api/events"))
        .send()
        .await
        .unwrap()
        .bytes_stream();
    tokio::time::timeout(Duration::from_secs(3), server.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert!(!state.recording_enabled.load(Ordering::SeqCst));
    assert!(!state.resolver.healthy().await);
    assert!(state.engine.active_ids().await.is_empty());
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(item) = events.next().await {
            if item.is_err() {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(client
        .get(format!("{base}/api/status"))
        .send()
        .await
        .is_err());
    assert_eq!(
        std::fs::read(dir.path().join("downloads/keep.ts")).unwrap(),
        b"existing-media"
    );
    server.shutdown().await.unwrap();
}
#[tokio::test]
async fn binding_failure_does_not_start_background_work() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let dir = tempfile::tempdir().unwrap();
    let workspace = Workspace::from_repo_root(dir.path());
    workspace.ensure_ready().unwrap();
    let journal = dir.path().join("config/media_jobs.json");
    let root = dir.path().join("downloads");
    let source = root.join("preserved.ts");
    std::fs::write(&source, b"preserved fixture").unwrap();
    let identity =
        streamcap_core::storage::source_identity(&std::fs::File::open(&source).unwrap()).unwrap();
    let history = serde_json::json!([{
        "job": {
            "id": "previous-completed-job", "taskId": null,
            "source": "preserved.ts", "output": "preserved.mp4", "state": "complete",
            "deleteOriginal": false, "sourceRemoved": false, "message": "原 TS 保留"
        },
        "input": {
            "root": root.canonicalize().unwrap(), "identity": identity,
            "options": {"delete_original": false, "minimum_free_bytes": 0}
        }
    }]);
    let original_journal = serde_json::to_vec_pretty(&history).unwrap();
    std::fs::write(&journal, &original_journal).unwrap();
    let state = api::bootstrap(workspace).await.unwrap();
    assert!(state.scheduler.postprocess.jobs().is_empty());
    assert_eq!(std::fs::read(&journal).unwrap(), original_journal);
    assert!(Server::start(
        state.clone(),
        ServerOptions {
            port,
            monitoring: true
        }
    )
    .await
    .is_err());
    assert!(state.engine.active_ids().await.is_empty());
    assert!(state.scheduler.postprocess.jobs().is_empty());
    assert_eq!(std::fs::read(&journal).unwrap(), original_journal);
    let server = Server::start(
        state.clone(),
        ServerOptions {
            port: 0,
            monitoring: false,
        },
    )
    .await
    .unwrap();
    let restored = state.scheduler.postprocess.jobs();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].id, "previous-completed-job");
    assert_eq!(state.scheduler.postprocess.pending(), 0);
    server.shutdown().await.unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), b"preserved fixture");
}
#[test]
fn invalid_config_is_not_replaced_and_unsupported_flags_cannot_enable() {
    use streamcap_core::config::{native_defaults, validate_settings, ConfigStore};
    let dir = tempfile::tempdir().unwrap();
    let ws = Workspace::from_repo_root(dir.path());
    ws.ensure_ready().unwrap();
    std::fs::write(ws.user_settings_path(), b"broken").unwrap();
    assert!(ConfigStore::load(ws.clone()).is_err());
    assert_eq!(std::fs::read(ws.user_settings_path()).unwrap(), b"broken");
    let values = native_defaults();
    assert_eq!(values["convert_to_mp4"], false);
    assert_eq!(values["delete_original"], false);
    let patch = serde_json::json!({"check_live_on_browser_refresh":true})
        .as_object()
        .unwrap()
        .clone();
    assert!(validate_settings(&patch).is_err());
}

#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG; uses generated audio, adjacent ffprobe and a random local HTTP port only"]
async fn ffmpeg_is_stopped_with_a_valid_completed_file() {
    use axum::{body::Body, routing::get, Router};
    let ffmpeg =
        PathBuf::from(std::env::var("STREAMCAP_TEST_FFMPEG").expect("explicit FFmpeg path"));
    assert!(ffmpeg.is_file());
    let dir = tempfile::tempdir().unwrap();
    // One minute of silence, served slowly enough to stop an actively recording process.
    let data_len = 8000_u32 * 2 * 60;
    let mut header = Vec::new();
    header.extend(b"RIFF");
    header.extend((data_len + 36).to_le_bytes());
    header.extend(b"WAVEfmt ");
    header.extend(16_u32.to_le_bytes());
    header.extend(1_u16.to_le_bytes());
    header.extend(1_u16.to_le_bytes());
    header.extend(8000_u32.to_le_bytes());
    header.extend(16000_u32.to_le_bytes());
    header.extend(2_u16.to_le_bytes());
    header.extend(16_u16.to_le_bytes());
    header.extend(b"data");
    header.extend(data_len.to_le_bytes());
    let app = Router::new().route(
        "/audio.wav",
        get(move || {
            let header = header.clone();
            async move {
                let stream = futures::stream::unfold((0, header), |(index, header)| async move {
                    if index > 600 {
                        return None;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    let bytes = if index == 0 {
                        header.clone()
                    } else {
                        vec![0_u8; 1600]
                    };
                    Some((Ok::<_, std::io::Error>(bytes), (index + 1, header)))
                });
                ([("Content-Type", "audio/wav")], Body::from_stream(stream))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let source = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let state = streamcap_core::api::bootstrap(Workspace::from_repo_root(dir.path()))
        .await
        .unwrap();
    state
        .config
        .write()
        .await
        .update_user_config(
            serde_json::json!({"live_save_path":dir.path().to_string_lossy()})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
    let engine = state.engine.clone();
    let api = Server::start(
        state,
        ServerOptions {
            port: 0,
            monitoring: false,
        },
    )
    .await
    .unwrap();
    let output = dir.path().join("downloads/capture.mkv");
    let options = RecordOptions {
        record_url: format!("http://127.0.0.1:{port}/audio.wav"),
        save_path: output.to_string_lossy().into(),
        format: "MKV".into(),
        segment_record: false,
        segment_time: None,
        headers: None,
        proxy: None,
        platform_key: Some("custom".into()),
        video_bitrate: None,
        is_overseas: false,
    };
    engine.start(&ffmpeg, "fixture", &options).await.unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let client = reqwest::Client::new();
    for target in ["downloads/capture.mkv", "downloads"] {
        assert_eq!(
            client
                .delete(format!(
                    "http://{}/api/storage?path={target}",
                    api.address()
                ))
                .send()
                .await
                .unwrap()
                .status(),
            409
        );
    }
    assert!(output.exists());
    tokio::time::timeout(Duration::from_secs(12), engine.stop_all(8))
        .await
        .unwrap();
    assert!(engine.active_ids().await.is_empty());
    assert!(engine
        .start(&ffmpeg, "late", &options)
        .await
        .err()
        .unwrap()
        .contains("关闭"));
    let data = std::fs::read(&output).unwrap();
    assert!(data.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) && data.len() > 1024);
    let mut probe = tokio::process::Command::new(ffmpeg.with_file_name(if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    }));
    probe
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "json",
        ])
        .arg(&output);
    #[cfg(windows)]
    probe.creation_flags(0x08000000);
    let result = probe.output().await.unwrap();
    assert!(
        result.status.success(),
        "ffprobe must read the completed recording"
    );
    let metadata: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let duration = metadata["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    assert!(
        duration > 0.5,
        "Matroska duration must be finalized, not an unclosed live container"
    );
    let mut decoder = tokio::process::Command::new(&ffmpeg);
    decoder
        .args(["-v", "error", "-i"])
        .arg(&output)
        .args(["-f", "null", "-"]);
    #[cfg(windows)]
    decoder.creation_flags(0x08000000);
    let decoded = decoder.output().await.unwrap();
    assert!(
        decoded.status.success() && decoded.stderr.is_empty(),
        "stopped recording must decode without errors"
    );
    api.shutdown().await.unwrap();
    source.abort();
}

#[tokio::test]
async fn native_configuration_and_fifteen_tasks_round_trip_without_losing_preferences() {
    use serde_json::{json, Value};
    use streamcap_core::config::{disable_unimplemented, UNSUPPORTED_FLAGS};
    let directory = tempfile::tempdir().unwrap();
    let workspace = Workspace::from_repo_root(directory.path());
    workspace.ensure_ready().unwrap();
    let mut settings=json!({"record_quality":"OD","video_format":"TS","loop_time_seconds":"450","video_segment_time":"1800","recording_space_threshold":"2.0","theme_mode":"light","theme_color":"blue","enable_proxy":false,"segmented_recording_enabled":false,"convert_to_mp4":true,"delete_original":true,"unknown_config":{"credential":"fixture-only"}}).as_object().unwrap().clone();
    disable_unimplemented(&mut settings);
    std::fs::write(
        workspace.user_settings_path(),
        serde_json::to_vec_pretty(&settings).unwrap(),
    )
    .unwrap();
    let tasks:Vec<Value>=(0..15).map(|index|{let platform=if index<7{"douyin"}else{"kuaishou"};json!({"rec_id":format!("fixture-{index}"),"url":format!("https://media.invalid/{index}.m3u8"),"streamer_name":format!("Fixture {index}"),"record_format":null,"quality":null,"segment_record":null,"segment_time":null,"monitor_status":index<12,"scheduled_recording":false,"scheduled_start_time":null,"monitor_hours":5,"recording_dir":directory.path().join(format!("downloads/previous-{index}")),"enabled_message_push":false,"only_notify_no_record":null,"flv_use_direct_download":null,"video_bitrate":null,"platform":platform,"platform_key":platform,"last_duration":12.5,"unknown_recording":{"keep":"fixture"}})}).collect();
    std::fs::write(
        workspace.recordings_path(),
        serde_json::to_vec_pretty(&tasks).unwrap(),
    )
    .unwrap();
    let state = api::bootstrap(workspace.clone()).await.unwrap();
    assert_eq!(state.store.all().await.len(), 15);
    api::shutdown(&state).await.unwrap();
    let round_trip: Value =
        serde_json::from_slice(&std::fs::read(workspace.recordings_path()).unwrap()).unwrap();
    let mut expected = tasks;
    expected.sort_by_key(|task| task["rec_id"].as_str().unwrap().to_owned());
    let mut actual = round_trip.as_array().unwrap().clone();
    actual.sort_by_key(|task| task["rec_id"].as_str().unwrap().to_owned());
    assert_eq!(
        actual, expected,
        "all task fields, inherited nulls, paths and unknown fields must survive restart"
    );
    let saved: Value =
        serde_json::from_slice(&std::fs::read(workspace.user_settings_path()).unwrap()).unwrap();
    assert_eq!(saved, Value::Object(settings));
    for flag in UNSUPPORTED_FLAGS {
        assert_eq!(saved[*flag], false);
    }
    assert_eq!(
        actual
            .iter()
            .filter(|task| task["monitor_status"] == true)
            .count(),
        12
    );
    assert_eq!(saved["unknown_config"]["credential"], "fixture-only");
}
#[tokio::test]
async fn shutdown_reports_persistence_failure_but_still_closes_listener() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = Workspace::from_repo_root(directory.path());
    let state = api::bootstrap(workspace.clone()).await.unwrap();
    let server = Server::start(
        state,
        ServerOptions {
            port: 0,
            monitoring: false,
        },
    )
    .await
    .unwrap();
    let address = server.address();
    std::fs::create_dir(workspace.recordings_path()).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), server.shutdown())
            .await
            .unwrap()
            .is_err()
    );
    assert!(reqwest::get(format!("http://{address}/api/status"))
        .await
        .is_err());
    assert!(workspace.recordings_path().is_dir());
}

#[tokio::test]
async fn pending_storage_scan_is_cancelled_and_cannot_register_after_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let state =
        streamcap_core::api::bootstrap(streamcap_core::Workspace::from_repo_root(dir.path()))
            .await
            .unwrap();
    let worker_state = state.storage.clone();
    let started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let signal = started.clone();
    let work = tokio::spawn(async move {
        worker_state
            .blocking(move |stop| {
                signal.store(true, std::sync::atomic::Ordering::SeqCst);
                while !stop.is_cancelled() {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                streamcap_core::storage::check_cancel(&stop)
            })
            .await
    });
    while !started.load(std::sync::atomic::Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        streamcap_core::api::shutdown(&state),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(work.await.unwrap().is_err());
    assert!(state.storage.blocking(|_| Ok(())).await.is_err());
}

// Local-only recording sources: a dead HLS URL and a real, generated FLV payload.
struct RecordingSourceFixture {
    base: String,
    enabled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    flv_requests: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    hls_requests: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for RecordingSourceFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn fixture_command(executable: &std::path::Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(executable);
    command.kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    command
}

async fn recording_source_fixture(ffmpeg: &std::path::Path) -> RecordingSourceFixture {
    use axum::{body::Body, http::StatusCode, response::IntoResponse, routing::get, Router};
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize},
        Arc,
    };
    let generated = fixture_command(ffmpeg)
        .args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=channel_layout=mono:sample_rate=8000",
            "-t",
            "60",
            "-c:a",
            "aac",
            "-b:a",
            "32k",
            "-f",
            "flv",
            "pipe:1",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        generated.status.success(),
        "synthetic FLV generation failed"
    );
    assert!(generated.stdout.starts_with(b"FLV"));
    let flv = Arc::new(generated.stdout);
    let enabled = Arc::new(AtomicBool::new(true));
    let flv_requests = Arc::new(AtomicUsize::new(0));
    let hls_requests = Arc::new(AtomicUsize::new(0));
    let live_flag = enabled.clone();
    let flv_count = flv_requests.clone();
    let hls_count = hls_requests.clone();
    let app = Router::new()
        .route(
            "/live.flv",
            get(move || {
                let bytes = flv.clone();
                let flag = live_flag.clone();
                let count = flv_count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    if !flag.load(Ordering::SeqCst) {
                        return StatusCode::NOT_FOUND.into_response();
                    }
                    (
                        [("content-type", "video/x-flv")],
                        Body::from(bytes.as_ref().clone()),
                    )
                        .into_response()
                }
            }),
        )
        .route(
            "/missing.m3u8",
            get(move || {
                let count = hls_count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    StatusCode::NOT_FOUND
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    RecordingSourceFixture {
        base,
        enabled,
        flv_requests,
        hls_requests,
        server,
    }
}

async fn recording_scheduler_fixture(
    dir: &std::path::Path,
    ffmpeg: PathBuf,
    url: String,
) -> (api::ApiState, streamcap_core::Scheduler) {
    let state = api::bootstrap(Workspace::from_repo_root(dir))
        .await
        .unwrap();
    state.config.write().await.update_user_config(serde_json::json!({
        "live_save_path": dir.join("downloads").to_string_lossy(),
        "folder_name_platform": false, "folder_name_author": false, "folder_name_time": false,
        "filename_includes_title": false, "video_format": "MKV", "loop_time_seconds": "4500"
    }).as_object().unwrap().clone()).unwrap();
    let mut record = streamcap_core::Recording::new("recording-fixture".into(), url, String::new());
    record.platform_key = Some("custom".into());
    record.record_format = Some("MKV".into());
    state.store.insert(vec![record]).await.unwrap();
    let scheduler = streamcap_core::Scheduler::new(
        state.store.clone(),
        state.engine.clone(),
        state.config.clone(),
        state.resolver.clone(),
        Some(ffmpeg),
        state.recording_enabled.clone(),
    );
    (state, scheduler)
}

#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG; generated audio and loopback HTTP only"]
async fn ffmpeg_failure_keeps_live_state_and_retry_clears_the_error() {
    let ffmpeg =
        PathBuf::from(std::env::var("STREAMCAP_TEST_FFMPEG").expect("explicit FFmpeg path"));
    let source = recording_source_fixture(&ffmpeg).await;
    source.enabled.store(false, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let (state, scheduler) =
        recording_scheduler_fixture(dir.path(), ffmpeg, format!("{}/live.flv", source.base)).await;
    let id = "recording-fixture";
    scheduler.force_start(id.into()).await.unwrap();
    let failed = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let record = state.store.get(id).await.unwrap();
            if !record.is_recording && record.recording_error.is_some() {
                break record;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("FFmpeg failure must reach the task state");
    assert!(
        failed.is_live,
        "HTTP 404 is not proof that the streamer went offline"
    );
    assert_eq!(failed.streamer_name, "自定义直播");
    assert_eq!(
        failed.recording_error.as_deref(),
        Some("播放地址返回 HTTP 404")
    );
    assert!(failed.speed.is_none());
    assert!(state.engine.active_ids().await.is_empty());
    source.enabled.store(true, Ordering::SeqCst);
    scheduler.force_start(id.into()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let retried = state.store.get(id).await.unwrap();
    assert!(retried.is_live && retried.is_recording);
    assert!(retried.recording_error.is_none());
    assert!(scheduler.stop_recording(id).await);
    let stopped = state.store.get(id).await.unwrap();
    assert!(stopped.is_live && !stopped.is_recording && stopped.recording_error.is_none());
    // Restart before the old watcher/sampler's next tick; neither may touch this new attempt.
    let mut events = state.store.subscribe();
    scheduler.force_start(id.into()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(2200)).await;
    let restarted = state.store.get(id).await.unwrap();
    assert!(restarted.is_live && restarted.is_recording && restarted.recording_error.is_none());
    while let Ok(event) = events.try_recv() {
        if event.topic == "update" {
            assert!(event.payload.get("recordingError").is_none());
        }
    }
    assert!(scheduler.stop_recording(id).await);
    assert_eq!(
        state.config.read().await.get_i64("loop_time_seconds", 0),
        4500
    );
    api::shutdown(&state).await.unwrap();
    scheduler.finish_background().await;
    assert!(state.engine.active_ids().await.is_empty());
}

#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG; generated FLV, a loopback 404 HLS route and ffprobe only"]
async fn ffmpeg_douyin_uses_readable_flv_instead_of_advertised_dead_hls() {
    let ffmpeg =
        PathBuf::from(std::env::var("STREAMCAP_TEST_FFMPEG").expect("explicit FFmpeg path"));
    let source = recording_source_fixture(&ffmpeg).await;
    let hls = format!("{}/missing.m3u8", source.base);
    assert_eq!(
        reqwest::Client::new()
            .get(&hls)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let flv = format!("{}/live.flv", source.base);
    let metadata = serde_json::json!({"data":{"user":{"nickname":"本地测试主播"},"data":[{"status":2,"title":"合成音频", "owner":{"nickname":"本地测试主播"}, "stream_url":{"flv_pull_url":{"FULL_HD1":flv},"hls_pull_url_map":{"FULL_HD1":hls}}}]}});
    let info = streamcap_core::platforms::douyin::parse_json(&metadata, Some("OD")).unwrap();
    assert_eq!(info.pick_record_url(false).unwrap(), flv);
    let dir = tempfile::tempdir().unwrap();
    let (state, scheduler) = recording_scheduler_fixture(dir.path(), ffmpeg.clone(), flv).await;
    state
        .store
        .apply_stream_info("recording-fixture", &info)
        .await
        .unwrap();
    scheduler
        .start_recording("recording-fixture".into(), &info)
        .await
        .unwrap();
    let output = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let ready = std::fs::read_dir(dir.path().join("downloads"))
                .unwrap()
                .filter_map(Result::ok)
                .find(|entry| {
                    entry.path().extension().is_some_and(|ext| ext == "mkv")
                        && entry.metadata().is_ok_and(|m| m.len() > 1024)
                });
            if let Some(entry) = ready {
                break entry.path();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the selected FLV must produce a recording");
    assert!(scheduler.stop_recording("recording-fixture").await);
    assert_eq!(
        source.hls_requests.load(Ordering::SeqCst),
        1,
        "the recorder must not request the dead HLS URL"
    );
    assert!(source.flv_requests.load(Ordering::SeqCst) >= 1);
    let probe = fixture_command(&ffmpeg.with_file_name(if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    }))
    .args([
        "-v",
        "error",
        "-show_entries",
        "stream=codec_type",
        "-of",
        "json",
    ])
    .arg(&output)
    .output()
    .await
    .unwrap();
    assert!(probe.status.success());
    let streams: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
    assert!(streams["streams"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["codec_type"] == "audio"));
    let decoded = fixture_command(&ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(&output)
        .args(["-f", "null", "-"])
        .output()
        .await
        .unwrap();
    assert!(
        decoded.status.success(),
        "recorded FLV must decode after muxing"
    );
    api::shutdown(&state).await.unwrap();
    scheduler.finish_background().await;
}

#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG; tests process identity with generated local audio only"]
async fn ffmpeg_old_process_handle_cannot_reap_a_replacement_recording() {
    let ffmpeg =
        PathBuf::from(std::env::var("STREAMCAP_TEST_FFMPEG").expect("explicit FFmpeg path"));
    let source = recording_source_fixture(&ffmpeg).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = streamcap_core::Engine::new();
    let mut options = RecordOptions {
        record_url: format!("{}/live.flv", source.base),
        save_path: dir.path().join("old.mkv").to_string_lossy().into(),
        format: "MKV".into(),
        segment_record: false,
        segment_time: None,
        headers: None,
        proxy: None,
        platform_key: Some("custom".into()),
        video_bitrate: None,
        is_overseas: false,
    };
    let old = engine.start(&ffmpeg, "same-task", &options).await.unwrap();
    assert!(engine.stop("same-task", 5).await);
    options.save_path = dir.path().join("new.mkv").to_string_lossy().into();
    let new = engine.start(&ffmpeg, "same-task", &options).await.unwrap();
    assert_ne!(old.run_id, new.run_id);
    assert_eq!(
        engine.poll_state(&old).await,
        streamcap_core::engine::ProcessState::Unknown
    );
    assert!(engine.is_current(&new).await);
    assert_eq!(
        engine.poll_state(&new).await,
        streamcap_core::engine::ProcessState::Running
    );
    engine.stop_all(5).await;
    assert!(engine.active_ids().await.is_empty());
}
