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
    // A free platform can run at its deadline; there is no extra global 20–40 second wait.
    tokio::time::advance(Duration::from_secs(1)).await;
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    assert!(state.store.get("timer").await.unwrap().is_live);
    stop.send(true).unwrap();
    task.await.unwrap();
    streamcap_core::api::shutdown(&state).await.unwrap();
    scheduler.finish_background().await;
}

async fn settle_checks() {
    for _ in 0..40 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn completed_automatic_checks_are_not_readmitted_before_the_interval() {
    let dir = tempfile::tempdir().unwrap();
    let state = bootstrap(Workspace::from_repo_root(dir.path()))
        .await
        .unwrap();
    state
        .config
        .write()
        .await
        .update_user_config(
            json!({"loop_time_seconds":"30","platform_request_interval":"0"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
    state
        .store
        .insert(
            (0..7)
                .map(|index| {
                    let mut record = Recording::new(
                        format!("batch-{index}"),
                        format!("http://127.0.0.1:1/{index}.mp4"),
                        "Fixture".into(),
                    );
                    record.only_notify_no_record = Some(true);
                    record
                })
                .collect(),
        )
        .await
        .unwrap();
    let mut events = state.store.subscribe();
    let (stop, rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(state.scheduler.clone().run(rx));
    let mut counts = std::collections::HashMap::<String, usize>::new();
    for (seconds, expected) in [(0, 1), (29, 1), (1, 2)] {
        tokio::time::advance(Duration::from_secs(seconds)).await;
        settle_checks().await;
        while let Ok(event) = events.try_recv() {
            if event.topic == "update" && event.payload["checkState"] == "checking" {
                let id = event.payload["recId"].as_str().unwrap().to_owned();
                *counts.entry(id).or_default() += 1;
            }
        }
        for index in 0..7 {
            assert_eq!(
                counts.get(&format!("batch-{index}")).copied().unwrap_or(0),
                expected,
                "batch-{index}: each completed check must rearm before readmission"
            );
        }
    }
    stop.send(true).unwrap();
    task.await.unwrap();
    streamcap_core::api::shutdown(&state).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn login_and_unknown_access_states_keep_standard_checks_without_unlocking_captcha() {
    use streamcap_core::platforms::kuaishou::{
        ACCESS_UNAVAILABLE, LOGIN_PROMPT, LOGIN_REQUIRED, PAGE_UNAVAILABLE, VERIFICATION_REQUIRED,
    };

    let dir = tempfile::tempdir().unwrap();
    let state = bootstrap(Workspace::from_repo_root(dir.path()))
        .await
        .unwrap();
    state
        .config
        .write()
        .await
        .update_user_config(
            json!({"loop_time_seconds":"30","platform_request_interval":"0"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
    let cases = [
        ("prompt", LOGIN_PROMPT),
        ("login", LOGIN_REQUIRED),
        ("access", ACCESS_UNAVAILABLE),
        ("page", PAGE_UNAVAILABLE),
        ("captcha", VERIFICATION_REQUIRED),
    ];
    state
        .store
        .insert(
            cases
                .iter()
                .map(|(id, error)| {
                    let mut record = Recording::new(
                        (*id).into(),
                        format!("https://live.kuaishou.com/u/{id}"),
                        "Fixture".into(),
                    );
                    record.platform_key = Some("kuaishou".into());
                    // Resolver preflight rejects quality before any HTTP access.
                    record.quality = Some("INVALID".into());
                    record.check_error = Some((*error).into());
                    record.access_state =
                        streamcap_core::platforms::kuaishou::access_state(error).into();
                    record.verification_required = *error == VERIFICATION_REQUIRED;
                    record.is_live = true;
                    record.live_title = Some("Last confirmed title".into());
                    record.last_success_at = Some(123);
                    record
                })
                .collect(),
        )
        .await
        .unwrap();
    let mut events = state.store.subscribe();
    let (stop, rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(state.scheduler.clone().run(rx));
    let mut counts = std::collections::HashMap::<String, usize>::new();
    for (seconds, expected) in [(0, 1), (29, 1), (1, 2)] {
        tokio::time::advance(Duration::from_secs(seconds)).await;
        settle_checks().await;
        while let Ok(event) = events.try_recv() {
            assert_ne!(event.topic, "kuaishouVerificationRequired");
            if event.topic == "update" && event.payload["checkState"] == "checking" {
                let id = event.payload["recId"].as_str().unwrap().to_owned();
                *counts.entry(id).or_default() += 1;
            }
        }
        for (id, _) in cases {
            assert_eq!(
                counts.get(id).copied().unwrap_or(0),
                if id == "captcha" { 0 } else { expected },
                "{id}"
            );
            let current = state.store.get(id).await.unwrap();
            assert!(current.monitor_status && current.is_live && !current.is_recording);
            assert_eq!(current.live_title.as_deref(), Some("Last confirmed title"));
            assert_eq!(current.last_success_at, Some(123));
            assert!(current.recovery_at.is_none());
            assert_eq!(current.verification_required, id == "captcha");
            if id == "captcha" {
                assert_eq!(current.access_state, "captcha");
                assert_eq!(current.check_wait_reason, "verification");
            }
        }
    }
    stop.send(true).unwrap();
    task.await.unwrap();
    streamcap_core::api::shutdown(&state).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn automatic_checks_use_platform_spacing_and_keep_each_room_interval() {
    let dir = tempfile::tempdir().unwrap();
    let state = bootstrap(Workspace::from_repo_root(dir.path()))
        .await
        .unwrap();
    state
        .config
        .write()
        .await
        .update_user_config(
            json!({"loop_time_seconds":"4500",
        "platform_request_interval":"3", "platform_max_concurrent_requests":"1"})
            .as_object()
            .unwrap()
            .clone(),
        )
        .unwrap();
    state
        .store
        .insert(
            (0..3)
                .map(|index| {
                    let mut r = Recording::new(
                        format!("paced-{index}"),
                        format!("http://127.0.0.1:1/{index}.mp4"),
                        format!("Paced {index}"),
                    );
                    r.only_notify_no_record = Some(true);
                    r
                })
                .collect(),
        )
        .await
        .unwrap();
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(state.scheduler.clone().run(receiver));
    settle_checks().await;
    assert!(state.store.get("paced-0").await.unwrap().is_live);
    let waiting = state.store.get("paced-1").await.unwrap();
    assert_eq!(waiting.queue_position, Some(1));
    assert_eq!(waiting.check_wait_reason, "platformSpacing");
    assert!(waiting.check_ready_at.is_some());
    tokio::time::advance(Duration::from_secs(2)).await;
    settle_checks().await;
    assert!(!state.store.get("paced-1").await.unwrap().is_live);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle_checks().await;
    assert!(state.store.get("paced-1").await.unwrap().is_live);
    tokio::time::advance(Duration::from_secs(3)).await;
    settle_checks().await;
    assert!(state.store.get("paced-2").await.unwrap().is_live);
    for index in 0..3 {
        state
            .store
            .update(&format!("paced-{index}"), |r| r.is_live = false)
            .await;
    }
    tokio::time::advance(Duration::from_secs(4493)).await;
    settle_checks().await;
    assert!(state.store.all().await.iter().all(|r| !r.is_live));
    tokio::time::advance(Duration::from_secs(1)).await;
    settle_checks().await;
    assert!(state.store.get("paced-0").await.unwrap().is_live);
    assert!(!state.store.get("paced-1").await.unwrap().is_live);
    assert_eq!(
        state.config.read().await.get_i64("loop_time_seconds", 0),
        4500
    );
    stop.send(true).unwrap();
    task.await.unwrap();
    streamcap_core::api::shutdown(&state).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn pausing_a_queued_monitor_and_shutdown_cancel_pending_checks() {
    let dir = tempfile::tempdir().unwrap();
    let state = bootstrap(Workspace::from_repo_root(dir.path()))
        .await
        .unwrap();
    state
        .config
        .write()
        .await
        .update_user_config(
            json!({"loop_time_seconds":"4500",
        "platform_request_interval":"30", "platform_max_concurrent_requests":"1"})
            .as_object()
            .unwrap()
            .clone(),
        )
        .unwrap();
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(state.scheduler.clone().run(receiver));
    state
        .store
        .insert(
            (0..3)
                .map(|index| {
                    let mut r = Recording::new(
                        format!("resume-{index}"),
                        format!("http://127.0.0.1:1/{index}.mp4"),
                        format!("Resume {index}"),
                    );
                    r.only_notify_no_record = Some(true);
                    r
                })
                .collect(),
        )
        .await
        .unwrap();
    state
        .scheduler
        .request_monitoring(["resume-0", "resume-1", "resume-2", "resume-1"].map(str::to_owned));
    settle_checks().await;
    assert_eq!(
        state.store.all().await.iter().filter(|r| r.is_live).count(),
        1
    );
    state.scheduler.toggle_monitor("resume-1").await.unwrap();
    settle_checks().await;
    let paused = state.store.get("resume-1").await.unwrap();
    assert!(!paused.is_live && !paused.monitor_status);
    assert!(paused.queue_position.is_none() && paused.next_check_at.is_none());
    stop.send(true).unwrap();
    task.await.unwrap();
    tokio::time::advance(Duration::from_secs(100)).await;
    settle_checks().await;
    let pending = state.store.get("resume-2").await.unwrap();
    assert!(!pending.is_live && pending.queue_position.is_none());
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
        expected.next_check_at = current.next_check_at;
        assert!(expected.last_check_at.is_some());
        assert_eq!(
            serde_json::to_value(&current).unwrap(),
            serde_json::to_value(&expected).unwrap(),
            "only attempt state may change when persistence fails"
        );
        let mut phases = Vec::new();
        while let Ok(event) = events.try_recv() {
            assert_eq!(event.topic, "update");
            let emitted = event.payload;
            let mut expected = serde_json::to_value(&before).unwrap();
            expected["checkState"] = emitted["checkState"].clone();
            expected["lastCheckAt"] = emitted["lastCheckAt"].clone();
            for key in ["nextCheckAt", "checkReadyAt", "checkWaitReason"] {
                expected[key] = emitted[key].clone();
            }
            assert_eq!(
                emitted, expected,
                "never publish an unsaved name or recording success"
            );
            if matches!(
                emitted["checkState"].as_str(),
                Some("queued" | "checking" | "idle")
            ) {
                phases.push(emitted["checkState"].as_str().unwrap().to_owned());
            }
        }
        phases.dedup();
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
async fn failed_kuaishou_check_has_no_browser_event_or_credential_write() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Workspace::from_repo_root(dir.path());
    let state = bootstrap(workspace.clone()).await.unwrap();
    let mut record = Recording::new(
        "access".into(),
        "https://live.kuaishou.com/u/never-requested".into(),
        "Fixture".into(),
    );
    record.platform_key = Some("kuaishou".into());
    // Invalid quality is rejected before the HTTP request, keeping this test fully offline.
    record.quality = Some("INVALID".into());
    record.monitor_status = true;
    record.is_live = true;
    record.live_title = Some("Last verified title".into());
    record.last_success_at = Some(123);
    state.store.insert(vec![record.clone()]).await.unwrap();
    let saved = std::fs::read(workspace.recordings_path()).unwrap();
    let cookies = state.config.read().await.cookies_for_resolver().unwrap();
    let mut events = state.store.subscribe();
    assert!(matches!(
        state.scheduler.check("access".into()).await.unwrap(),
        streamcap_core::scheduler::CheckOutcome::Failed(_)
    ));
    let checked = state.store.get("access").await.unwrap();
    assert!(checked.check_error.is_some());
    assert!(checked.is_live && !checked.is_recording);
    assert_eq!(checked.live_title, record.live_title);
    assert_eq!(checked.last_success_at, Some(123));
    assert_eq!(std::fs::read(workspace.recordings_path()).unwrap(), saved);
    assert_eq!(
        state.config.read().await.cookies_for_resolver().unwrap(),
        cookies
    );
    let mut had_update = false;
    while let Ok(event) = events.try_recv() {
        assert_ne!(event.topic, "kuaishouVerificationRequired");
        had_update |= event.topic == "update";
    }
    assert!(had_update);
    streamcap_core::api::shutdown(&state).await.unwrap();
}
