use futures::StreamExt;
use std::{path::PathBuf, sync::atomic::Ordering, time::Duration};
use streamcap_core::{
    api,
    engine::{Engine, RecordOptions},
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
    let state = api::bootstrap(Workspace::from_repo_root(dir.path()))
        .await
        .unwrap();
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
    state.resolver.shutdown().await;
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
    let patch = serde_json::json!({"convert_to_mp4":true})
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
    let engine = Engine::new();
    let output = dir.path().join("capture.mkv");
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
