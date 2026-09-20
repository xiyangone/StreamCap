//! Real media fixtures are synthesized locally; no platform or user recording is accessed.
use axum::{body::Body, routing::get, Router};
use futures::StreamExt;
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use streamcap_core::{
    api,
    model::MediaJobState,
    service::{Server, ServerOptions},
    Recording, Scheduler, Workspace,
};

fn ffmpeg() -> PathBuf {
    PathBuf::from(std::env::var("STREAMCAP_TEST_FFMPEG").expect("explicit local FFmpeg path"))
}
fn command(path: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(path);
    command.kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    command
}
async fn sample_ts(seconds: &str) -> Vec<u8> {
    let output = command(&ffmpeg())
        .args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=10",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            seconds,
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-g",
            "10",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-f",
            "mpegts",
            "pipe:1",
        ])
        .output()
        .await
        .unwrap();
    assert!(output.status.success(), "local TS synthesis failed");
    assert!(output.stdout.len() > 1880);
    output.stdout
}
async fn state(root: &Path, convert: bool) -> api::ApiState {
    let mut state = api::bootstrap(Workspace::from_repo_root(root))
        .await
        .unwrap();
    state.config.write().await.update_user_config(json!({"live_save_path":root.join("downloads"),"video_format":"TS","convert_to_mp4":convert,"segmented_recording_enabled":false,"loop_time_seconds":"4500","folder_name_platform":false,"folder_name_author":false,"folder_name_time":false}).as_object().unwrap().clone()).unwrap();
    std::fs::create_dir_all(root.join("downloads")).unwrap();
    state.scheduler = Arc::new(Scheduler::new(
        state.store.clone(),
        state.engine.clone(),
        state.config.clone(),
        state.resolver.clone(),
        Some(ffmpeg()),
        state.recording_enabled.clone(),
    ));
    state
}
async fn wait_jobs(state: &api::ApiState) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let jobs = state.scheduler.postprocess.jobs();
            if !jobs.is_empty() && jobs.iter().all(|j| !j.state.pending()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("media jobs did not finish");
}
async fn check_mp4(path: &Path) {
    let result = command(&streamcap_core::paths::adjacent_ffprobe(&ffmpeg()).unwrap())
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=format_name,duration:stream=codec_type,codec_name",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .await
        .unwrap();
    assert!(result.status.success());
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let streams = value["streams"].as_array().unwrap();
    assert!(streams.iter().any(|s| s["codec_name"] == "h264"));
    assert!(streams.iter().any(|s| s["codec_name"] == "aac"));
    let result = command(&ffmpeg())
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "null", "-"])
        .output()
        .await
        .unwrap();
    assert!(result.status.success(), "MP4 must decode");
}
struct Source {
    url: String,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Source {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn source(data: Vec<u8>) -> Source {
    let bytes = Arc::new(data);
    let app = Router::new().route(
        "/live.ts",
        get(move || {
            let bytes = bytes.clone();
            async move {
                let chunks = futures::stream::unfold((bytes, 0), |(bytes, offset)| async move {
                    if offset >= bytes.len() {
                        return None;
                    }
                    tokio::time::sleep(Duration::from_millis(15)).await;
                    let end = (offset + 4096).min(bytes.len());
                    Some((
                        Ok::<_, std::io::Error>(bytes[offset..end].to_vec()),
                        (bytes, end),
                    ))
                });
                ([("content-type", "video/mp2t")], Body::from_stream(chunks))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/live.ts", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Source { url, server }
}
async fn add(state: &api::ApiState, url: String, segment: bool) {
    let mut record = Recording::new("media".into(), url, "Fixture".into());
    record.platform_key = Some("custom".into());
    record.record_format = Some("TS".into());
    record.segment_record = Some(segment);
    record.segment_time = Some("2".into());
    state.store.insert(vec![record]).await.unwrap();
}
fn recordings(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root.join("downloads"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "ts"))
        .collect()
}

#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG and rustc; real local recording with a gated slow subtitle probe"]
async fn media_slow_finish_publishes_stop_releases_global_locks_and_is_awaited_at_shutdown() {
    let tools = tempfile::tempdir().unwrap();
    let local_ffmpeg = tools.path().join(if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    });
    std::fs::copy(ffmpeg(), &local_ffmpeg).unwrap();
    let probe = local_ffmpeg.with_file_name(if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    });
    let source_file = tools.path().join("slow_probe.rs");
    std::fs::write(
        &source_file,
        r#"
fn main() {
    let root = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
    std::fs::write(root.join("entered"), b"ready").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
    while !root.join("release").exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    println!("2.0");
}
"#,
    )
    .unwrap();
    let compiled = command(Path::new("rustc"))
        .arg("--crate-name")
        .arg("slow_probe")
        .arg(&source_file)
        .arg("-o")
        .arg(&probe)
        .output()
        .await
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    for manual in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut state = state(root.path(), false).await;
        state
            .config
            .write()
            .await
            .update_user_config(
                json!({"generate_time_subtitle_file":true})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        state.scheduler = Arc::new(Scheduler::new(
            state.store.clone(),
            state.engine.clone(),
            state.config.clone(),
            state.resolver.clone(),
            Some(local_ffmpeg.clone()),
            state.recording_enabled.clone(),
        ));
        let stream = source(sample_ts(if manual { "20" } else { "2.2" }).await).await;
        add(&state, stream.url.clone(), false).await;
        state
            .scheduler
            .start_recording(
                "media".into(),
                &streamcap_core::resolver::StreamInfo {
                    is_live: true,
                    record_url: stream.url.clone(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        if manual {
            tokio::time::timeout(Duration::from_secs(15), async {
                while !recordings(root.path())
                    .iter()
                    .any(|p| p.metadata().is_ok_and(|m| m.len() > 1880))
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
            assert!(tokio::time::timeout(
                Duration::from_secs(3),
                state.scheduler.stop_recording("media")
            )
            .await
            .unwrap());
        }
        tokio::time::timeout(Duration::from_secs(15), async {
            while !tools.path().join("entered").exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            !state.store.get("media").await.unwrap().is_recording,
            "manual={manual}: pending subtitles are not recording"
        );
        assert!(state.engine.active_ids().await.is_empty());
        assert!(state.store.get("media").await.unwrap().speed.is_none());
        let guard =
            tokio::time::timeout(Duration::from_millis(500), state.engine.filesystem_guard())
                .await
                .expect("slow subtitles must not hold the filesystem lock");
        let outputs: Vec<_> = recordings(root.path())
            .into_iter()
            .map(|path| path.canonicalize().unwrap())
            .collect();
        assert!(!outputs.is_empty());
        for output in &outputs {
            assert!(
                state.engine.protects_path(output).await,
                "unfinished media remains protected"
            );
        }
        drop(guard);
        tokio::time::timeout(
            Duration::from_millis(500),
            state.scheduler.stop_recording("unrelated"),
        )
        .await
        .expect("unrelated operations must not wait for subtitles");
        let closing = state.clone();
        let shutdown = tokio::spawn(async move { api::shutdown(&closing).await });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !shutdown.is_finished(),
            "shutdown must await the tracked finish task"
        );
        std::fs::write(tools.path().join("release"), b"continue").unwrap();
        tokio::time::timeout(Duration::from_secs(8), shutdown)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        for output in outputs {
            assert!(output.exists());
            assert!(output.with_extension("srt").exists());
            assert!(!state.engine.protects_path(&output).await);
        }
        std::fs::remove_file(tools.path().join("entered")).unwrap();
        std::fs::remove_file(tools.path().join("release")).unwrap();
    }
}

#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG; synthetic local TS only"]
async fn media_manual_conversion_preserves_ts_and_never_overwrites_mp4() {
    let directory = tempfile::tempdir().unwrap();
    let state = state(directory.path(), false).await;
    let bytes = sample_ts("3").await;
    let root = directory.path().join("downloads");
    let file = root.join("中文 录像.ts");
    std::fs::write(&file, &bytes).unwrap();
    let job = state
        .scheduler
        .postprocess
        .enqueue(root.clone(), "中文 录像.ts", None)
        .await
        .unwrap();
    let duplicate = state
        .scheduler
        .postprocess
        .enqueue(root.clone(), "中文 录像.ts", None)
        .await
        .unwrap();
    assert_eq!(job.id, duplicate.id);
    assert!(
        state
            .engine
            .protects_path(&root.canonicalize().unwrap())
            .await
    );
    wait_jobs(&state).await;
    assert_eq!(
        state.scheduler.postprocess.jobs()[0].state,
        MediaJobState::Complete
    );
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    check_mp4(&file.with_extension("mp4")).await;
    let before = std::fs::read(file.with_extension("mp4")).unwrap();
    assert!(state
        .scheduler
        .postprocess
        .enqueue(root, "中文 录像.ts", None)
        .await
        .is_err());
    assert_eq!(std::fs::read(file.with_extension("mp4")).unwrap(), before);
    api::shutdown(&state).await.unwrap();
}

#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG; synthetic invalid TS only"]
async fn media_failed_input_preserves_original_and_removes_only_own_temporary_file() {
    let directory = tempfile::tempdir().unwrap();
    let state = state(directory.path(), false).await;
    let root = directory.path().join("downloads");
    std::fs::write(root.join("bad.ts"), b"invalid TS fixture").unwrap();
    state
        .scheduler
        .postprocess
        .enqueue(root.clone(), "bad.ts", None)
        .await
        .unwrap();
    wait_jobs(&state).await;
    assert_eq!(
        state.scheduler.postprocess.jobs()[0].state,
        MediaJobState::Failed
    );
    assert_eq!(
        std::fs::read(root.join("bad.ts")).unwrap(),
        b"invalid TS fixture"
    );
    assert!(!root.join("bad.mp4").exists());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    assert!(state
        .scheduler
        .postprocess
        .enqueue(root, "../config/user_settings.json", None)
        .await
        .is_err());
    api::shutdown(&state).await.unwrap();
}

#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG; verifies growing local preview and manual stop conversion"]
async fn media_recording_preview_follows_growth_then_stop_converts_to_mp4() {
    let directory = tempfile::tempdir().unwrap();
    let state = state(directory.path(), true).await;
    let source = source(sample_ts("12").await).await;
    add(&state, source.url.clone(), false).await;
    let server = Server::start(
        state.clone(),
        ServerOptions {
            port: 0,
            monitoring: false,
        },
    )
    .await
    .unwrap();
    state.scheduler.force_start("media".into()).await.unwrap();
    let file = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(p) = recordings(directory.path())
                .into_iter()
                .find(|p| std::fs::metadata(p).unwrap().len() > 1880)
            {
                break p;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    let path = file.file_name().unwrap().to_string_lossy();
    let client = reqwest::Client::new();
    let info: serde_json::Value = client
        .get(format!("http://{}/api/media/info", server.address()))
        .query(&[("path", path.as_ref())])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["isRecording"], true);
    let response = client
        .get(format!("http://{}/api/media/transcode", server.address()))
        .query(&[("path", path.as_ref())])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.content_length().is_none());
    let mut stream = response.bytes_stream();
    let first = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first[0], 0x47);
    assert!(state
        .scheduler
        .postprocess
        .enqueue(directory.path().join("downloads"), path.as_ref(), None)
        .await
        .is_err());
    let size = std::fs::metadata(&file).unwrap().len();
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(std::fs::metadata(&file).unwrap().len() > size);
    assert!(state.scheduler.stop_recording("media").await);
    let before = std::fs::read(&file).unwrap();
    wait_jobs(&state).await;
    assert!(state
        .scheduler
        .postprocess
        .jobs()
        .iter()
        .all(|j| j.state == MediaJobState::Complete));
    check_mp4(&file.with_extension("mp4")).await;
    assert_eq!(std::fs::read(&file).unwrap(), before);
    drop(stream);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(state.preview.active(), 0);
    server.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG; natural completion and segments use only generated TS"]
async fn media_natural_end_and_segmented_output_convert_each_completed_file() {
    let directory = tempfile::tempdir().unwrap();
    let state = state(directory.path(), true).await;
    let source = source(sample_ts("5").await).await;
    add(&state, source.url.clone(), true).await;
    state.scheduler.force_start("media".into()).await.unwrap();
    state
        .store
        .update("media", |record| {
            record.live_title = Some("stale title before natural EOF".into());
            record.check_error = Some("stale detection before natural EOF".into());
        })
        .await;
    tokio::time::timeout(Duration::from_secs(18), async {
        loop {
            let record = state.store.get("media").await.unwrap();
            if !record.is_recording && record.live_title.is_none() && record.check_error.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    wait_jobs(&state).await;
    assert!(
        state.engine.active_ids().await.is_empty(),
        "post-EOF state refresh must not restart the finite source"
    );
    assert_eq!(
        state.config.read().await.get_i64("loop_time_seconds", 0),
        4500
    );
    let files = recordings(directory.path());
    assert!(
        files.len() >= 2,
        "fixture must produce multiple TS segments"
    );
    assert_eq!(state.scheduler.postprocess.jobs().len(), files.len());
    for file in files {
        assert!(file.exists());
        check_mp4(&file.with_extension("mp4")).await;
    }
    api::shutdown(&state).await.unwrap();
}

#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG; application exit with an active local recording"]
async fn media_shutdown_finishes_current_recording_and_waits_for_conversion() {
    let directory = tempfile::tempdir().unwrap();
    let state = state(directory.path(), true).await;
    let source = source(sample_ts("12").await).await;
    add(&state, source.url.clone(), false).await;
    state.scheduler.force_start("media".into()).await.unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    api::shutdown(&state).await.unwrap();
    assert!(state.engine.active_ids().await.is_empty());
    assert_eq!(state.scheduler.postprocess.pending(), 0);
    assert!(!state.scheduler.postprocess.jobs().is_empty());
    assert!(state
        .scheduler
        .postprocess
        .jobs()
        .iter()
        .all(|j| j.state == MediaJobState::Complete));
    for file in recordings(directory.path()) {
        check_mp4(&file.with_extension("mp4")).await;
    }
}

#[tokio::test]
async fn preview_rejects_traversal_invalid_time_and_releases_slots() {
    let directory = tempfile::tempdir().unwrap();
    let state = api::bootstrap(Workspace::from_repo_root(directory.path()))
        .await
        .unwrap();
    let root = directory.path().join("downloads");
    std::fs::write(root.join("finite.ts"), vec![0x47; 188 * 2]).unwrap();
    let missing_ffmpeg = root.join("must-not-execute.exe");
    for (path, start) in [
        ("../outside.ts", 0.0),
        ("finite.ts", -1.0),
        ("finite.ts", f64::NAN),
        ("finite.ts", f64::INFINITY),
    ] {
        assert!(state
            .preview
            .transcode(
                &state.storage,
                root.clone(),
                path.into(),
                missing_ffmpeg.clone(),
                start
            )
            .await
            .is_err());
        assert_eq!(state.preview.active(), 0);
    }
    api::shutdown(&state).await.unwrap();
}

#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG; verifies cancellation of a running local conversion"]
async fn media_running_job_cancels_without_leftover_process_or_partial_mp4() {
    let directory = tempfile::tempdir().unwrap();
    let state = state(directory.path(), false).await;
    let root = directory.path().join("downloads");
    let bytes = sample_ts("6").await;
    std::fs::write(root.join("cancel.ts"), &bytes).unwrap();
    state
        .scheduler
        .postprocess
        .enqueue(root.clone(), "cancel.ts", None)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if state.scheduler.postprocess.jobs()[0].state != MediaJobState::Waiting {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    state.scheduler.postprocess.shutdown(Duration::ZERO).await;
    assert_eq!(state.scheduler.postprocess.pending(), 0);
    assert_eq!(
        state.scheduler.postprocess.jobs()[0].state,
        MediaJobState::Cancelled
    );
    assert_eq!(std::fs::read(root.join("cancel.ts")).unwrap(), bytes);
    assert!(!root.join("cancel.mp4").exists());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    api::shutdown(&state).await.unwrap();
}

#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG; generated media only"]
async fn media_cleanup_is_opt_in_and_removes_only_validated_source() {
    let directory = tempfile::tempdir().unwrap();
    let state = state(directory.path(), false).await;
    state
        .config
        .write()
        .await
        .update_user_config(json!({"delete_original":true}).as_object().unwrap().clone())
        .unwrap();
    let root = directory.path().join("downloads");
    std::fs::write(root.join("cleanup.ts"), sample_ts("2").await).unwrap();
    std::fs::write(root.join("unrelated.ts"), b"untouched").unwrap();
    state
        .scheduler
        .postprocess
        .enqueue(root.clone(), "cleanup.ts", None)
        .await
        .unwrap();
    wait_jobs(&state).await;
    let jobs = state.scheduler.postprocess.jobs();
    assert_eq!(
        jobs[0].state,
        MediaJobState::Complete,
        "{}",
        jobs[0].message
    );
    assert!(jobs[0].source_removed);
    assert!(jobs[0].delete_original);
    assert!(!root.join("cleanup.ts").exists());
    check_mp4(&root.join("cleanup.mp4")).await;
    assert_eq!(
        std::fs::read(root.join("unrelated.ts")).unwrap(),
        b"untouched"
    );
    api::shutdown(&state).await.unwrap();
}
#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG; generated broken input only"]
async fn media_cleanup_never_deletes_after_failed_conversion() {
    let directory = tempfile::tempdir().unwrap();
    let state = state(directory.path(), false).await;
    state
        .config
        .write()
        .await
        .update_user_config(json!({"delete_original":true}).as_object().unwrap().clone())
        .unwrap();
    let root = directory.path().join("downloads");
    std::fs::write(root.join("broken.ts"), b"not a media stream").unwrap();
    state
        .scheduler
        .postprocess
        .enqueue(root.clone(), "broken.ts", None)
        .await
        .unwrap();
    wait_jobs(&state).await;
    let jobs = state.scheduler.postprocess.jobs();
    assert_eq!(jobs[0].state, MediaJobState::Failed);
    assert!(!jobs[0].source_removed);
    assert!(root.join("broken.ts").exists());
    assert!(!root.join("broken.mp4").exists());
    api::shutdown(&state).await.unwrap();
}
#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG; generated media, source held without delete sharing"]
async fn media_cleanup_reports_locked_source_without_discarding_mp4() {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let directory = tempfile::tempdir().unwrap();
        let state = state(directory.path(), false).await;
        state
            .config
            .write()
            .await
            .update_user_config(json!({"delete_original":true}).as_object().unwrap().clone())
            .unwrap();
        let root = directory.path().join("downloads");
        std::fs::write(root.join("locked.ts"), sample_ts("2").await).unwrap();
        let reader = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(root.join("locked.ts"))
            .unwrap();
        state
            .scheduler
            .postprocess
            .enqueue(root.clone(), "locked.ts", None)
            .await
            .unwrap();
        wait_jobs(&state).await;
        let jobs = state.scheduler.postprocess.jobs();
        assert_eq!(jobs[0].state, MediaJobState::CleanupFailed);
        assert!(root.join("locked.ts").exists());
        assert!(root.join("locked.mp4").exists());
        drop(reader);
        api::shutdown(&state).await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG; verifies every format using synthetic media only"]
async fn media_all_formats_and_segments_produce_readable_files() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path(), false).await;
    let source = source(sample_ts("1.4").await).await;
    for format in streamcap_core::engine::SUPPORTED_RECORD_FORMATS {
        for segmented in [false, true] {
            let id = format!("{format}-{segmented}");
            let fixture_url = format!("{}?case={id}", source.url);
            let mut record = Recording::new(id.clone(), fixture_url.clone(), id.clone());
            record.record_format = Some((*format).into());
            record.segment_record = Some(segmented);
            record.segment_time = Some("1".into());
            state.store.insert(vec![record]).await.unwrap();
            state
                .scheduler
                .start_recording(
                    id.clone(),
                    &streamcap_core::resolver::StreamInfo {
                        is_live: true,
                        record_url: fixture_url,
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let process = state.engine.process(&id).await.unwrap();
            tokio::time::timeout(Duration::from_secs(12), async {
                while state.engine.is_recording(&id).await {
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            })
            .await
            .unwrap();
            assert!(
                state
                    .store
                    .get(&id)
                    .await
                    .unwrap()
                    .recording_error
                    .is_none(),
                "{id}: {:?}",
                state.store.get(&id).await.unwrap().recording_error
            );
            let outputs =
                streamcap_core::media_safety::recording_outputs(&process.output_path).unwrap();
            assert!(!outputs.is_empty(), "{id}");
            for path in outputs {
                let result = command(&streamcap_core::paths::adjacent_ffprobe(&ffmpeg()).unwrap())
                    .args([
                        "-v",
                        "error",
                        "-show_entries",
                        "stream=codec_type",
                        "-of",
                        "json",
                    ])
                    .arg(&path)
                    .output()
                    .await
                    .unwrap();
                assert!(result.status.success(), "{id}: {}", path.display());
                let info: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
                let streams = info["streams"].as_array().unwrap();
                assert!(streams.iter().any(|stream| stream["codec_type"] == "audio"));
                if matches!(*format, "WAV" | "MP3" | "WMA" | "M4A" | "AAC") {
                    assert!(streams.iter().all(|stream| stream["codec_type"] == "audio"));
                } else {
                    assert!(streams.iter().any(|stream| stream["codec_type"] == "video"));
                }
            }
        }
    }
    api::shutdown(&state).await.unwrap();
}
#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG; verifies generated timestamp subtitles before source cleanup"]
async fn media_timestamp_subtitles_finish_before_verified_source_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path(), true).await;
    state
        .config
        .write()
        .await
        .update_user_config(
            json!({"generate_time_subtitle_file":true,"delete_original":true})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
    let source = source(sample_ts("2.2").await).await;
    add(&state, source.url.clone(), true).await;
    state
        .scheduler
        .start_recording(
            "media".into(),
            &streamcap_core::resolver::StreamInfo {
                is_live: true,
                record_url: source.url.clone(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while state.engine.is_recording("media").await {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    wait_jobs(&state).await;
    let jobs = state.scheduler.postprocess.jobs();
    assert!(!jobs.is_empty());
    for job in jobs {
        assert_eq!(job.state, MediaJobState::Complete);
        assert!(job.source_removed);
        let path = root
            .path()
            .join("downloads")
            .join(&job.output)
            .with_extension("srt");
        let text = std::fs::read_to_string(path).unwrap();
        assert!(text.starts_with("1\n00:00:00,000 --> "));
        assert!(text.contains(" --> "));
    }
    api::shutdown(&state).await.unwrap();
}
#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG; tests bounded compatibility and live preview against loopback media"]
async fn media_compatibility_and_live_preview_release_owned_ffmpeg() {
    use axum::body::to_bytes;
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path(), false).await;
    let original = root.path().join("downloads/fixture.ts");
    std::fs::write(&original, sample_ts("2.2").await).unwrap();
    let mkv = root.path().join("downloads/fixture.mkv");
    let result = command(&ffmpeg())
        .args(["-v", "error", "-i"])
        .arg(&original)
        .args(["-c", "copy", "-f", "matroska"])
        .arg(&mkv)
        .output()
        .await
        .unwrap();
    assert!(result.status.success());
    let before = std::fs::read(&mkv).unwrap();
    let response = state
        .preview
        .transcode(
            &state.storage,
            root.path().join("downloads"),
            "fixture.mkv".into(),
            ffmpeg(),
            0.0,
        )
        .await
        .unwrap();
    let bytes = tokio::time::timeout(
        Duration::from_secs(10),
        to_bytes(response.into_body(), 4 * 1024 * 1024),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(bytes.len() > 188);
    assert_eq!(bytes[0], 0x47);
    assert_eq!(std::fs::read(&mkv).unwrap(), before);
    let source = source(sample_ts("2.2").await).await;
    let response = state
        .preview
        .live(
            ffmpeg(),
            streamcap_core::preview::LiveInput {
                url: source.url.clone(),
                headers: None,
                proxy: None,
            },
        )
        .await
        .unwrap();
    let mut body = response.into_body().into_data_stream();
    let bytes = tokio::time::timeout(Duration::from_secs(10), body.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!bytes.is_empty());
    drop(body);
    state.preview.shutdown().await;
    assert_eq!(state.preview.active(), 0);
    api::shutdown(&state).await.unwrap();
}
#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG; restores only a cancelled synthetic job with an unchanged identity"]
async fn media_recovery_keeps_the_original_cleanup_preference() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path(), false).await;
    let directory = root.path().join("downloads");
    let source = sample_ts("1.2").await;
    std::fs::write(directory.join("resume.ts"), &source).unwrap();
    state
        .scheduler
        .postprocess
        .enqueue_manual(directory.clone(), "resume.ts", false)
        .await
        .unwrap();
    state.scheduler.postprocess.shutdown(Duration::ZERO).await;
    assert!(directory.join("resume.ts").is_file());
    assert!(!directory.join("resume.mp4").exists());
    state
        .config
        .write()
        .await
        .update_user_config(json!({"delete_original":true}).as_object().unwrap().clone())
        .unwrap();
    let processor = streamcap_core::postprocess::Postprocessor::new(
        state.engine.clone(),
        state.store.clone(),
        Some(ffmpeg()),
        state.config.clone(),
    );
    assert_eq!(processor.recover().await.unwrap(), 1);
    processor.shutdown(Duration::from_secs(15)).await;
    assert!(directory.join("resume.mp4").is_file());
    assert_eq!(std::fs::read(directory.join("resume.ts")).unwrap(), source);
    api::shutdown(&state).await.unwrap();
}

#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG only to synthesize and probe the FLV fixture; direct recording has no ffmpeg configured"]
async fn media_native_flv_download_needs_no_recording_subprocess() {
    let root = tempfile::tempdir().unwrap();
    let mut state = state(root.path(), false).await;
    let sample = command(&ffmpeg())
        .args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=10",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "1.5",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-c:a",
            "aac",
            "-f",
            "flv",
            "pipe:1",
        ])
        .output()
        .await
        .unwrap();
    assert!(sample.status.success());
    let bytes = sample.stdout;
    let source = source(bytes.clone()).await;
    let mut record = Recording::new("direct".into(), source.url.clone(), "Direct fixture".into());
    record.record_format = Some("FLV".into());
    record.segment_record = Some(false);
    record.flv_use_direct_download = Some(true);
    state.store.insert(vec![record]).await.unwrap();
    state.scheduler = Arc::new(Scheduler::new(
        state.store.clone(),
        state.engine.clone(),
        state.config.clone(),
        state.resolver.clone(),
        None,
        state.recording_enabled.clone(),
    ));
    state
        .scheduler
        .start_recording(
            "direct".into(),
            &streamcap_core::resolver::StreamInfo {
                is_live: true,
                flv_url: source.url.clone(),
                record_url: source.url.clone(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let process = state.engine.process("direct").await.unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        while state.engine.is_recording("direct").await {
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read(&process.output_path).unwrap(), bytes);
    assert!(state
        .store
        .get("direct")
        .await
        .unwrap()
        .recording_error
        .is_none());
    let probe = command(&streamcap_core::paths::adjacent_ffprobe(&ffmpeg()).unwrap())
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type",
            "-of",
            "json",
        ])
        .arg(&process.output_path)
        .output()
        .await
        .unwrap();
    assert!(probe.status.success());
    api::shutdown(&state).await.unwrap();
}

#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG; proves creating a loopback room starts recording automatically"]
async fn media_added_room_starts_without_manual_record_action() {
    let directory = tempfile::tempdir().unwrap();
    let state = state(directory.path(), true).await;
    let fixture = source(sample_ts("12").await).await;
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
    let created: serde_json::Value = client
        .post(format!("http://{}/api/recordings", server.address()))
        .json(&json!({"url":fixture.url,"streamerName":"automatic"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["created"][0]["recId"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(4), async {
        while !state.store.get(id).await.unwrap().is_recording {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("new monitored room must start without /start");
    tokio::time::sleep(Duration::from_millis(800)).await;
    server.shutdown().await.unwrap();
    let jobs = state.scheduler.postprocess.jobs();
    assert!(!jobs.is_empty());
    assert!(jobs.iter().all(|job| job.state == MediaJobState::Complete));
    assert_eq!(state.engine.active_ids().await.len(), 0);
    assert_eq!(
        state.config.read().await.get_i64("loop_time_seconds", 0),
        4500
    );
}

async fn preview_first_pixel(
    state: &api::ApiState,
    root: &Path,
    path: &str,
    start: f64,
) -> [u8; 3] {
    use tokio::io::AsyncWriteExt;
    let response = state
        .preview
        .transcode(
            &state.storage,
            root.to_path_buf(),
            path.into(),
            ffmpeg(),
            start,
        )
        .await
        .unwrap();
    let mut stream = response.into_body().into_data_stream();
    let mut prefix = Vec::new();
    tokio::time::timeout(Duration::from_secs(8), async {
        while prefix.len() < 12 * 1024 {
            match stream.next().await {
                Some(Ok(bytes)) => prefix.extend_from_slice(&bytes),
                Some(Err(error)) => panic!("{error}"),
                None => break,
            }
        }
    })
    .await
    .unwrap();
    drop(stream);
    assert!(
        prefix.len() < 256 * 1024,
        "seeking must not download the full source"
    );
    let mut child = command(&ffmpeg())
        .args([
            "-v",
            "error",
            "-f",
            "mpegts",
            "-i",
            "pipe:0",
            "-frames:v",
            "1",
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&prefix)
        .await
        .unwrap();
    let decoded = child.wait_with_output().await.unwrap();
    assert!(
        decoded.status.success(),
        "frame decode: {}",
        String::from_utf8_lossy(&decoded.stderr)
    );
    assert!(decoded.stdout.len() >= 3);
    [decoded.stdout[0], decoded.stdout[1], decoded.stdout[2]]
}

#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG; synthesizes a 600MiB TS and seeks without downloading it"]
async fn media_large_ts_seeks_forward_and_backward_without_full_conversion() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path(), false).await;
    let root = dir.path().join("downloads");
    let file = root.join("large-seek.ts");
    let result = command(&ffmpeg())
        .args([
            "-v",
            "error",
            "-nostdin",
            "-n",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:size=160x120:rate=10:d=15",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:size=160x120:rate=10:d=15",
            "-filter_complex",
            "[0:v][1:v]concat=n=2:v=1:a=0[v]",
            "-map",
            "[v]",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "10",
            "-pix_fmt",
            "yuv420p",
            "-muxrate",
            "170000000",
            "-f",
            "mpegts",
        ])
        .arg(&file)
        .output()
        .await
        .unwrap();
    assert!(result.status.success());
    assert!(std::fs::metadata(&file).unwrap().len() > 600 * 1024 * 1024);
    let before =
        streamcap_core::storage::source_identity(&std::fs::File::open(&file).unwrap()).unwrap();
    let info = state
        .preview
        .info(
            &state.storage,
            root.clone(),
            "large-seek.ts".into(),
            Some(ffmpeg()),
        )
        .await
        .unwrap();
    assert!(info.seekable);
    assert!((29.0..31.0).contains(&info.duration_seconds.unwrap()));
    let blue = preview_first_pixel(&state, &root, "large-seek.ts", 24.0).await;
    assert!(
        blue[2] > 200 && blue[0] < 40,
        "24s must be blue, not the first red frame: {blue:?}"
    );
    let red = preview_first_pixel(&state, &root, "large-seek.ts", 2.0).await;
    assert!(
        red[0] > 200 && red[2] < 40,
        "backward seek must return to red: {red:?}"
    );
    assert_eq!(
        streamcap_core::storage::source_identity(&std::fs::File::open(&file).unwrap()).unwrap(),
        before
    );
    assert_eq!(
        std::fs::read_dir(&root).unwrap().count(),
        1,
        "no full-file conversion or cache file"
    );
    state.preview.shutdown().await;
    assert_eq!(state.preview.active(), 0);
    api::shutdown(&state).await.unwrap();
}

#[tokio::test]
#[ignore = "requires STREAMCAP_TEST_FFMPEG; verifies local preview is not throttled to playback speed"]
async fn media_preview_buffers_ahead_and_keeps_source_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let state = state(temp.path(), false).await;
    let root = temp.path().join("downloads");
    let original = sample_ts("12").await;
    std::fs::write(root.join("buffer-ahead.ts"), &original).unwrap();
    let info = state
        .preview
        .info(
            &state.storage,
            root.clone(),
            "buffer-ahead.ts".into(),
            Some(ffmpeg()),
        )
        .await
        .unwrap();
    assert!(info.duration_seconds.unwrap() >= 11.5);
    let started = std::time::Instant::now();
    let response = state
        .preview
        .transcode(
            &state.storage,
            root.clone(),
            "buffer-ahead.ts".into(),
            ffmpeg(),
            0.0,
        )
        .await
        .unwrap();
    let bytes = tokio::time::timeout(
        Duration::from_secs(8),
        axum::body::to_bytes(response.into_body(), 32 * 1024 * 1024),
    )
    .await
    .expect("local buffering must not take the 12-second playback duration")
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(8));
    assert!(bytes.len() > 1880);
    assert_eq!(
        std::fs::read(root.join("buffer-ahead.ts")).unwrap(),
        original
    );
    state.preview.shutdown().await;
    assert_eq!(state.preview.active(), 0);
}
