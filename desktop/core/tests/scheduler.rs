use serde_json::json;
use std::{sync::Arc, time::Duration};
use streamcap_core::{api::bootstrap, Recording, Scheduler, Workspace};

#[tokio::test]
async fn recording_progress_is_published_each_second_without_waiting_for_disk_sampling() {
    use axum::{body::Body, routing::get, Router};
    let app = Router::new().route(
        "/fixture.flv",
        get(|| async {
            let bytes = futures::stream::unfold(0usize, |index| async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let chunk = if index == 0 {
                    b"FLV\x01\x05\x00\x00\x00\x09".to_vec()
                } else {
                    vec![0; 256]
                };
                Some((Ok::<_, std::io::Error>(chunk), index + 1))
            });
            ([("content-type", "video/x-flv")], Body::from_stream(bytes))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/fixture.flv", listener.local_addr().unwrap());
    let source = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let state = bootstrap(Workspace::from_repo_root(dir.path()))
        .await
        .unwrap();
    state.config.write().await.update_user_config(json!({"live_save_path":dir.path().join("downloads"),"convert_to_mp4":false,"generate_time_subtitle_file":false}).as_object().unwrap().clone()).unwrap();
    let mut r = Recording::new("progress".into(), url.clone(), "Fixture".into());
    r.record_format = Some("FLV".into());
    r.segment_record = Some(false);
    r.flv_use_direct_download = Some(true);
    state.store.insert(vec![r]).await.unwrap();
    let mut events = state.store.subscribe();
    state
        .scheduler
        .start_recording(
            "progress".into(),
            &streamcap_core::resolver::StreamInfo {
                is_live: true,
                record_url: url,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let started = tokio::time::Instant::now();
    let mut updates = Vec::new();
    while updates.len() < 4 {
        let event = tokio::time::timeout(Duration::from_secs(3), events.recv())
            .await
            .unwrap()
            .unwrap();
        if event.topic == "update" && event.payload["isRecording"] == true {
            updates.push(started.elapsed());
        }
    }
    state.scheduler.stop_recording("progress").await;
    streamcap_core::api::shutdown(&state).await.unwrap();
    source.abort();
    assert!(
        updates[1] < Duration::from_millis(1800),
        "first progress must not wait for the 2s scan: {updates:?}"
    );
    for pair in updates.windows(2) {
        assert!(
            pair[1] - pair[0] < Duration::from_millis(1800),
            "one-second progress: {updates:?}"
        );
    }
}

#[tokio::test]
async fn browser_access_failures_are_runtime_only_and_preserve_last_verified_room() {
    use streamcap_core::platforms::kuaishou;
    let dir = tempfile::tempdir().unwrap();
    let workspace = Workspace::from_repo_root(dir.path());
    let state = bootstrap(workspace.clone()).await.unwrap();
    let mut record = Recording::new(
        "access".into(),
        "https://live.kuaishou.com/u/fixture".into(),
        "Fixture".into(),
    );
    record.platform_key = Some("kuaishou".into());
    record.is_live = true;
    record.live_title = Some("Last verified title".into());
    record.last_success_at = Some(123);
    state.store.insert(vec![record.clone()]).await.unwrap();
    let before = std::fs::read(workspace.recordings_path()).unwrap();
    for (error, access, captcha) in [
        (kuaishou::PAGE_CHECK_REQUIRED, "pageCheck", false),
        (kuaishou::RATE_LIMITED, "cooldown", false),
        (kuaishou::LOGIN_PROMPT, "loginPrompt", false),
        (kuaishou::LOGIN_REQUIRED, "loginRequired", false),
        (kuaishou::VERIFICATION_REQUIRED, "captcha", true),
    ] {
        state.scheduler.report_kuaishou_access(&record, error).await;
        let current = state.store.get("access").await.unwrap();
        assert_eq!(current.access_state, access);
        assert_eq!(current.verification_required, captcha);
        assert!(current.is_live && !current.is_recording);
        assert_eq!(current.live_title, record.live_title);
        assert_eq!(current.last_success_at, Some(123));
        assert_eq!(std::fs::read(workspace.recordings_path()).unwrap(), before);
    }
    state
        .store
        .update("access", |r| {
            r.url = "https://live.kuaishou.com/u/replaced".into()
        })
        .await;
    let current = state.store.get("access").await.unwrap();
    state
        .scheduler
        .report_kuaishou_access(&record, kuaishou::RATE_LIMITED)
        .await;
    assert_eq!(state.store.get("access").await.unwrap(), current);
    state
        .store
        .apply_stream_info("access", &streamcap_core::resolver::StreamInfo::default())
        .await
        .unwrap();
    let recovered = state.store.get("access").await.unwrap();
    assert!(
        recovered.access_state.is_empty()
            && !recovered.verification_required
            && recovered.check_error.is_none()
    );
    streamcap_core::api::shutdown(&state).await.unwrap();
}

#[tokio::test]
async fn checks_publish_queue_running_completion_and_preserve_success_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let state = bootstrap(Workspace::from_repo_root(dir.path()))
        .await
        .unwrap();
    state
        .config
        .write()
        .await
        .update_user_config(
            json!({"platform_request_interval":"0"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
    let mut record = Recording::new(
        "phase".into(),
        "http://127.0.0.1:1/fixture.ts".into(),
        "Fixture".into(),
    );
    record.only_notify_no_record = Some(true);
    state.store.insert(vec![record]).await.unwrap();
    let mut events = state.store.subscribe();
    state.scheduler.check("phase".into()).await.unwrap();
    let mut phases = Vec::new();
    while let Ok(event) = events.try_recv() {
        if event.topic == "update" {
            if let Some(phase) = event.payload["checkState"].as_str() {
                phases.push(phase.to_owned());
            }
        }
    }
    assert!(
        phases
            .windows(3)
            .any(|p| p == ["queued", "checking", "idle"]),
        "{phases:?}"
    );
    let success = state.store.get("phase").await.unwrap();
    assert!(success.last_check_at.is_some() && success.last_success_at.is_some());
    state
        .store
        .update("phase", |record| record.quality = Some("INVALID".into()))
        .await;
    state.scheduler.check("phase".into()).await.unwrap();
    let failed = state.store.get("phase").await.unwrap();
    assert_eq!(failed.check_state, "idle");
    assert!(failed.check_error.is_some());
    assert_eq!(failed.last_success_at, success.last_success_at);
    streamcap_core::api::shutdown(&state).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cancelling_a_paced_check_leaves_no_stuck_queue_or_running_state() {
    let dir = tempfile::tempdir().unwrap();
    let state = bootstrap(Workspace::from_repo_root(dir.path()))
        .await
        .unwrap();
    state
        .config
        .write()
        .await
        .update_user_config(
            json!({"platform_request_interval":"30"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
    let mut record = Recording::new(
        "cancel-phase".into(),
        "http://127.0.0.1:1/fixture.ts".into(),
        "Fixture".into(),
    );
    record.only_notify_no_record = Some(true);
    state.store.insert(vec![record]).await.unwrap();
    state.scheduler.check("cancel-phase".into()).await.unwrap();
    let scheduler = state.scheduler.clone();
    let pending = tokio::spawn(async move { scheduler.check("cancel-phase".into()).await });
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        state.store.get("cancel-phase").await.unwrap().check_state,
        "queued"
    );
    state.scheduler.stop_recording("cancel-phase").await;
    pending.await.unwrap().unwrap();
    assert_eq!(
        state.store.get("cancel-phase").await.unwrap().check_state,
        "idle"
    );
    streamcap_core::api::shutdown(&state).await.unwrap();
}
#[tokio::test(start_paused = true)]
async fn committed_interval_changes_rearm_without_an_immediate_check() {
    let dir = tempfile::tempdir().unwrap();
    let state = bootstrap(Workspace::from_repo_root(dir.path()))
        .await
        .unwrap();
    state
        .config
        .write()
        .await
        .update_user_config(
            json!({"loop_time_seconds":"30"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
    let mut record = Recording::new(
        "timer".into(),
        "http://127.0.0.1:1/fixture.mp4".into(),
        "Timer".into(),
    );
    record.platform_key = Some("custom".into());
    record.monitor_status = true;
    state.store.insert(vec![record]).await.unwrap();
    let scheduler = Arc::new(Scheduler::new(
        state.store.clone(),
        state.engine.clone(),
        state.config.clone(),
        state.resolver.clone(),
        None,
        state.recording_enabled.clone(),
    ));
    let (stop, rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(scheduler.clone().run(rx));
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(
        state.store.get("timer").await.unwrap().is_live,
        "启动后应先检查监控任务"
    );
    state.store.update("timer", |r| r.is_live = false).await;
    tokio::time::advance(Duration::from_secs(20)).await;
    state
        .config
        .write()
        .await
        .update_user_config(
            json!({"loop_time_seconds":"4500"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
    scheduler.refresh_interval();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(4499)).await;
    tokio::task::yield_now().await;
    assert!(
        !state.store.get("timer").await.unwrap().is_live,
        "旧30秒定时器不能继续触发"
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    assert!(
        state.store.get("timer").await.unwrap().is_live,
        "新4500秒截止时间必须触发检测"
    );
    state.store.update("timer", |r| r.is_live = false).await;
    state
        .config
        .write()
        .await
        .update_user_config(
            json!({"loop_time_seconds":"30"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
    scheduler.refresh_interval();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(29)).await;
    assert!(!state.store.get("timer").await.unwrap().is_live);
    // The shared automatic queue may delay a short 30-second interval by up to 10 seconds.
    tokio::time::advance(Duration::from_secs(11)).await;
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    assert!(state.store.get("timer").await.unwrap().is_live);
    stop.send(true).unwrap();
    task.await.unwrap();
    streamcap_core::api::shutdown(&state).await.unwrap();
    scheduler.finish_background().await;
}

#[tokio::test(start_paused = true)]
async fn automatic_startup_is_one_room_at_a_time_and_each_room_keeps_4500_seconds() {
    let dir = tempfile::tempdir().unwrap();
    let state = bootstrap(Workspace::from_repo_root(dir.path()))
        .await
        .unwrap();
    state
        .config
        .write()
        .await
        .update_user_config(
            json!({"loop_time_seconds":"4500","platform_request_interval":"0"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
    let records = (0..3)
        .map(|index| {
            let mut record = Recording::new(
                format!("paced-{index}"),
                format!("http://127.0.0.1:1/{index}.mp4"),
                format!("Paced {index}"),
            );
            record.only_notify_no_record = Some(true);
            record
        })
        .collect();
    state.store.insert(records).await.unwrap();
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(state.scheduler.clone().run(receiver));
    for _ in 0..30 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        state.store.all().await.iter().filter(|r| r.is_live).count(),
        1,
        "startup must not launch all rooms"
    );
    for expected in [2, 3] {
        tokio::time::advance(Duration::from_secs(19)).await;
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            state.store.all().await.iter().filter(|r| r.is_live).count(),
            expected - 1
        );
        tokio::time::advance(Duration::from_secs(21)).await;
        for _ in 0..30 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            state.store.all().await.iter().filter(|r| r.is_live).count(),
            expected
        );
    }
    for index in 0..3 {
        state
            .store
            .update(&format!("paced-{index}"), |r| r.is_live = false)
            .await;
    }
    tokio::time::advance(Duration::from_secs(4419)).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert!(
        state.store.all().await.iter().all(|r| !r.is_live),
        "ordinary polling must not be accelerated"
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    for _ in 0..30 {
        tokio::task::yield_now().await;
    }
    assert!(state.store.get("paced-0").await.unwrap().is_live);
    assert!(!state.store.get("paced-1").await.unwrap().is_live);
    assert!(!state.store.get("paced-2").await.unwrap().is_live);
    assert_eq!(
        state.config.read().await.get_i64("loop_time_seconds", 0),
        4500
    );
    assert!(state.engine.active_ids().await.is_empty());
    stop.send(true).unwrap();
    task.await.unwrap();
    streamcap_core::api::shutdown(&state).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn queued_recovery_stays_staggered_and_shutdown_cancels_the_queue() {
    let dir = tempfile::tempdir().unwrap();
    let state = bootstrap(Workspace::from_repo_root(dir.path()))
        .await
        .unwrap();
    state
        .config
        .write()
        .await
        .update_user_config(
            json!({"loop_time_seconds":"4500","platform_request_interval":"0"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(state.scheduler.clone().run(receiver));
    tokio::task::yield_now().await;
    let records = (0..3)
        .map(|index| {
            let mut record = Recording::new(
                format!("resume-{index}"),
                format!("http://127.0.0.1:1/{index}.mp4"),
                format!("Resume {index}"),
            );
            record.only_notify_no_record = Some(true);
            record
        })
        .collect();
    state.store.insert(records).await.unwrap();
    state
        .scheduler
        .request_monitoring(["resume-0", "resume-1", "resume-2", "resume-1"].map(str::to_owned));
    for _ in 0..30 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        state.store.all().await.iter().filter(|r| r.is_live).count(),
        1
    );
    tokio::time::advance(Duration::from_secs(19)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        state.store.all().await.iter().filter(|r| r.is_live).count(),
        1
    );
    state
        .store
        .update("resume-1", |record| record.monitor_status = false)
        .await;
    state.scheduler.request_monitoring(["resume-1".into()]);
    tokio::time::advance(Duration::from_secs(21)).await;
    for _ in 0..30 {
        tokio::task::yield_now().await;
    }
    assert!(
        !state.store.get("resume-1").await.unwrap().is_live,
        "a paused queued room is never requested"
    );
    stop.send(true).unwrap();
    task.await.unwrap();
    tokio::time::advance(Duration::from_secs(100)).await;
    assert!(
        !state.store.get("resume-2").await.unwrap().is_live,
        "shutdown cancels the pending staggered check"
    );
    streamcap_core::api::shutdown(&state).await.unwrap();
}
#[tokio::test]
async fn filesystem_guard_serializes_mutating_operations() {
    let engine = streamcap_core::Engine::new();
    let guard = engine.filesystem_guard().await;
    let clone = engine.clone();
    let waiter = tokio::spawn(async move {
        let _guard = clone.filesystem_guard().await;
        true
    });
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());
    drop(guard);
    assert!(waiter.await.unwrap());
    assert!(!engine.protects_path(std::path::Path::new("absent")).await);
}

#[tokio::test]
async fn manual_and_scheduled_detection_persist_automatic_names_before_starting() {
    for manual in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::from_repo_root(dir.path());
        let state = bootstrap(workspace.clone()).await.unwrap();
        let mut record = Recording::new(
            "auto-name".into(),
            "http://127.0.0.1:1/fixture.flv".into(),
            String::new(),
        );
        record.platform_key = Some("custom".into());
        state.store.insert(vec![record]).await.unwrap();
        let scheduler = Scheduler::new(
            state.store.clone(),
            state.engine.clone(),
            state.config.clone(),
            state.resolver.clone(),
            None,
            state.recording_enabled.clone(),
        );
        let error = if manual {
            scheduler.force_start("auto-name".into()).await
        } else {
            scheduler.check("auto-name".into()).await
        }
        .unwrap_err();
        assert!(error.contains("ffmpeg"));
        let current = state.store.get("auto-name").await.unwrap();
        assert_eq!(current.streamer_name, "自定义直播", "manual={manual}");
        assert!(current
            .display_title
            .as_deref()
            .unwrap()
            .starts_with("自定义直播"));
        assert!(current.is_live);
        assert!(current
            .recording_error
            .as_deref()
            .unwrap()
            .contains("ffmpeg"));
        let reopened = streamcap_core::Store::new(workspace);
        reopened.load().await.unwrap();
        assert_eq!(
            reopened.get("auto-name").await.unwrap().streamer_name,
            "自定义直播"
        );
        streamcap_core::api::shutdown(&state).await.unwrap();
        scheduler.finish_background().await;
    }
}

#[tokio::test]
async fn manual_and_scheduled_detection_preserve_custom_names() {
    for manual in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::from_repo_root(dir.path());
        let state = bootstrap(workspace.clone()).await.unwrap();
        let mut record = Recording::new(
            "named".into(),
            "http://127.0.0.1:1/fixture.flv".into(),
            "我的自定义名称".into(),
        );
        record.platform_key = Some("custom".into());
        state.store.insert(vec![record]).await.unwrap();
        let before = std::fs::read(workspace.recordings_path()).unwrap();
        let scheduler = Scheduler::new(
            state.store.clone(),
            state.engine.clone(),
            state.config.clone(),
            state.resolver.clone(),
            None,
            state.recording_enabled.clone(),
        );
        let result = if manual {
            scheduler.force_start("named".into()).await
        } else {
            scheduler.check("named".into()).await
        };
        assert!(result.unwrap_err().contains("ffmpeg"));
        assert_eq!(
            state.store.get("named").await.unwrap().streamer_name,
            "我的自定义名称"
        );
        assert_eq!(std::fs::read(workspace.recordings_path()).unwrap(), before);
        streamcap_core::api::shutdown(&state).await.unwrap();
        scheduler.finish_background().await;
    }
}

#[tokio::test]
async fn failed_name_persistence_prevents_both_start_paths_from_claiming_success() {
    for manual in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::from_repo_root(dir.path());
        let state = bootstrap(workspace.clone()).await.unwrap();
        let mut record = Recording::new(
            "blocked".into(),
            "http://127.0.0.1:1/fixture.flv".into(),
            String::new(),
        );
        record.platform_key = Some("custom".into());
        state.store.insert(vec![record]).await.unwrap();
        let scheduler = Scheduler::new(
            state.store.clone(),
            state.engine.clone(),
            state.config.clone(),
            state.resolver.clone(),
            None,
            state.recording_enabled.clone(),
        );
        let backup = dir.path().join("fixture-original.json");
        std::fs::rename(workspace.recordings_path(), &backup).unwrap();
        std::fs::create_dir(workspace.recordings_path()).unwrap();
        let before = state.store.get("blocked").await.unwrap();
        let mut events = state.store.subscribe();
        let result = if manual {
            scheduler.force_start("blocked".into()).await
        } else {
            scheduler.check("blocked".into()).await
        };
        assert!(result.unwrap_err().contains("保存主播名称失败"));
        let current = state.store.get("blocked").await.unwrap();
        assert!(current.streamer_name.is_empty() && !current.is_live && !current.is_recording);
        let mut expected = before.clone();
        expected.check_state = "idle".into();
        expected.last_check_at = current.last_check_at;
        assert!(expected.last_check_at.is_some());
        assert_eq!(
            current, expected,
            "only attempt state may change when persistence fails"
        );
        let mut phases = Vec::new();
        while let Ok(event) = events.try_recv() {
            assert_eq!(event.topic, "update");
            let emitted = event.payload;
            let mut expected = serde_json::to_value(&before).unwrap();
            expected["checkState"] = emitted["checkState"].clone();
            expected["lastCheckAt"] = emitted["lastCheckAt"].clone();
            assert_eq!(
                emitted, expected,
                "never publish an unsaved name or recording success"
            );
            phases.push(emitted["checkState"].as_str().unwrap().to_owned());
        }
        assert_eq!(phases, ["queued", "checking", "idle"]);
        assert!(state.engine.active_ids().await.is_empty());
        std::fs::remove_dir(workspace.recordings_path()).unwrap();
        std::fs::rename(backup, workspace.recordings_path()).unwrap();
        streamcap_core::api::shutdown(&state).await.unwrap();
        scheduler.finish_background().await;
    }
}

#[tokio::test]
async fn parse_failure_does_not_erase_last_verified_live_state() {
    let dir = tempfile::tempdir().unwrap();
    let state = bootstrap(Workspace::from_repo_root(dir.path()))
        .await
        .unwrap();
    let mut record = Recording::new(
        "parse-error".into(),
        "https://media.invalid/not-a-stream".into(),
        "已确认主播".into(),
    );
    record.platform_key = Some("custom".into());
    record.is_live = true;
    record.live_title = Some("最近直播标题".into());
    record.recording_error = Some("播放地址返回 HTTP 404".into());
    state.store.insert(vec![record.clone()]).await.unwrap();
    let scheduler = Scheduler::new(
        state.store.clone(),
        state.engine.clone(),
        state.config.clone(),
        state.resolver.clone(),
        None,
        state.recording_enabled.clone(),
    );
    assert!(matches!(
        scheduler.check("parse-error".into()).await.unwrap(),
        streamcap_core::scheduler::CheckOutcome::Failed(_)
    ));
    let checked = state.store.get("parse-error").await.unwrap();
    assert!(checked.check_error.is_some());
    assert!(!checked.verification_required);
    record.check_error = checked.check_error.clone();
    assert_eq!(checked.is_live, record.is_live);
    assert_eq!(checked.live_title, record.live_title);
    assert_eq!(checked.recording_error, record.recording_error);
    assert_eq!(checked.check_error, record.check_error);
    assert!(scheduler.force_start("parse-error".into()).await.is_err());
    let after_manual = state.store.get("parse-error").await.unwrap();
    assert_eq!(after_manual.is_live, record.is_live);
    assert_eq!(after_manual.live_title, record.live_title);
    assert_eq!(after_manual.recording_error, record.recording_error);
    streamcap_core::api::shutdown(&state).await.unwrap();
    scheduler.finish_background().await;
}

#[tokio::test]
async fn conversion_preference_is_preserved_without_changing_detection_or_source_format() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Workspace::from_repo_root(dir.path());
    let state = bootstrap(workspace.clone()).await.unwrap();
    state.config.write().await.update_user_config(json!({"loop_time_seconds":"4500","video_format":"TS","segmented_recording_enabled":false,"convert_to_mp4":true}).as_object().unwrap().clone()).unwrap();
    let reloaded = streamcap_core::ConfigStore::load(workspace).unwrap();
    assert!(reloaded.get_bool("convert_to_mp4", false));
    assert_eq!(reloaded.get_i64("loop_time_seconds", 0), 4500);
    assert_eq!(reloaded.get_str("video_format", ""), "TS");
    assert!(!reloaded.get_bool("segmented_recording_enabled", true));
    assert_eq!(state.scheduler.postprocess.pending(), 0);
    streamcap_core::api::shutdown(&state).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn new_monitors_check_once_without_accelerating_existing_rooms() {
    let dir = tempfile::tempdir().unwrap();
    let state = bootstrap(Workspace::from_repo_root(dir.path()))
        .await
        .unwrap();
    state
        .config
        .write()
        .await
        .update_user_config(
            json!({"loop_time_seconds":"4500","platform_request_interval":"0"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
    let make = |id: &str| {
        let mut rec = Recording::new(id.into(), format!("http://127.0.0.1:1/{id}.mp4"), id.into());
        rec.only_notify_no_record = Some(true);
        rec
    };
    state.store.insert(vec![make("existing")]).await.unwrap();
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(state.scheduler.clone().run(receiver));
    for _ in 0..30 {
        tokio::task::yield_now().await;
    }
    assert!(state.store.get("existing").await.unwrap().is_live);
    state.store.update("existing", |r| r.is_live = false).await;
    tokio::time::advance(Duration::from_secs(100)).await;
    state.store.insert(vec![make("new")]).await.unwrap();
    state.scheduler.request_monitoring(vec!["new".into(); 5]);
    for _ in 0..30 {
        tokio::task::yield_now().await;
    }
    assert!(state.store.get("new").await.unwrap().is_live);
    assert!(!state.store.get("existing").await.unwrap().is_live);
    assert!(
        state.engine.active_ids().await.is_empty(),
        "仅通知模式不能自动录制"
    );
    state.store.update("new", |r| r.is_live = false).await;
    tokio::time::advance(Duration::from_secs(4399)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(!state.store.get("existing").await.unwrap().is_live);
    assert!(!state.store.get("new").await.unwrap().is_live);
    tokio::time::advance(Duration::from_secs(1)).await;
    for _ in 0..30 {
        tokio::task::yield_now().await;
    }
    assert!(state.store.get("existing").await.unwrap().is_live);
    assert!(
        !state.store.get("new").await.unwrap().is_live,
        "新增任务不能跟着旧周期重复检测"
    );
    state
        .store
        .update("new", |r| r.monitor_status = false)
        .await;
    state.scheduler.request_monitoring(["new".into()]);
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_secs(100)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(!state.store.get("new").await.unwrap().is_live);
    state.store.update("new", |r| r.monitor_status = true).await;
    state.scheduler.request_monitoring(["new".into()]);
    for _ in 0..30 {
        tokio::task::yield_now().await;
    }
    assert!(state.store.get("new").await.unwrap().is_live);
    stop.send(true).unwrap();
    task.await.unwrap();
    streamcap_core::api::shutdown(&state).await.unwrap();
}

#[tokio::test]
async fn queued_paused_and_outside_schedule_tasks_do_not_start_recording() {
    let dir = tempfile::tempdir().unwrap();
    let state = bootstrap(Workspace::from_repo_root(dir.path()))
        .await
        .unwrap();
    let mut paused = Recording::new(
        "paused".into(),
        "http://127.0.0.1:1/live.mp4".into(),
        "paused".into(),
    );
    paused.monitor_status = false;
    let mut scheduled = Recording::new(
        "scheduled".into(),
        "http://127.0.0.1:1/scheduled.mp4".into(),
        "scheduled".into(),
    );
    scheduled.scheduled_recording = Some(true);
    scheduled.scheduled_start_time = Some(
        (chrono::Local::now() + chrono::Duration::hours(6))
            .format("%H:%M")
            .to_string(),
    );
    scheduled.monitor_hours = Some("0.5".into());
    state.store.insert(vec![paused, scheduled]).await.unwrap();
    assert_eq!(
        state.scheduler.check("paused".into()).await.unwrap(),
        streamcap_core::scheduler::CheckOutcome::MonitoringPaused
    );
    assert_eq!(
        state.scheduler.check("scheduled".into()).await.unwrap(),
        streamcap_core::scheduler::CheckOutcome::OutsideSchedule
    );
    assert!(!state.store.get("paused").await.unwrap().is_live);
    assert!(!state.store.get("scheduled").await.unwrap().is_live);
    streamcap_core::api::shutdown(&state).await.unwrap();
}

#[tokio::test]
async fn verified_kuaishou_room_keeps_monitoring_intent_and_runtime_state_out_of_storage() {
    use streamcap_core::{resolver::StreamInfo, scheduler::CheckOutcome};
    for monitor in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::from_repo_root(dir.path());
        let state = bootstrap(workspace.clone()).await.unwrap();
        state
            .config
            .write()
            .await
            .update_user_config(
                json!({"loop_time_seconds":"4500","convert_to_mp4":true,"delete_original":true})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let mut record = Recording::new(
            "verify".into(),
            "https://live.kuaishou.com/u/fixture".into(),
            "Fixture".into(),
        );
        record.platform_key = Some("kuaishou".into());
        record.monitor_status = monitor;
        record.only_notify_no_record = Some(true);
        record.check_error =
            Some(streamcap_core::platforms::kuaishou::VERIFICATION_REQUIRED.into());
        record.verification_required = true;
        state.store.insert(vec![record]).await.unwrap();
        let expected = state.store.get("verify").await.unwrap();
        let info = StreamInfo {
            platform: "快手直播".into(),
            anchor_name: "Fixture".into(),
            is_live: true,
            record_url: "https://media.invalid/verified.flv".into(),
            ..Default::default()
        };
        let result = state
            .scheduler
            .accept_verified(&expected, &info)
            .await
            .unwrap();
        assert_eq!(
            result,
            if monitor {
                CheckOutcome::NotifyOnly
            } else {
                CheckOutcome::MonitoringPaused
            }
        );
        let current = state.store.get("verify").await.unwrap();
        assert_eq!(current.monitor_status, monitor);
        assert!(current.is_live);
        assert!(!current.is_recording);
        assert!(current.check_error.is_none());
        assert!(!current.verification_required);
        assert!(state.engine.active_ids().await.is_empty());
        state.store.persist().await.unwrap();
        let stored = std::fs::read_to_string(workspace.recordings_path()).unwrap();
        assert!(
            !stored.contains("checkError")
                && !stored.contains("verificationRequired")
                && !stored.contains("check_error")
        );
        assert_eq!(
            state.config.read().await.get_str("loop_time_seconds", ""),
            "4500"
        );
        assert!(state.config.read().await.get_bool("delete_original", false));
        state
            .store
            .update("verify", |r| {
                r.url = "https://live.kuaishou.com/u/changed".into()
            })
            .await;
        assert!(state
            .scheduler
            .accept_verified(&expected, &info)
            .await
            .unwrap_err()
            .contains("已更新"));
        streamcap_core::api::shutdown(&state).await.unwrap();
    }
}
