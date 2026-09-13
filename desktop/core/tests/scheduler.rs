use serde_json::json;
use std::{sync::Arc, time::Duration};
use streamcap_core::{api::bootstrap, Recording, Scheduler, Workspace};
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
    tokio::task::yield_now().await;
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
        let mut events = state.store.subscribe();
        let result = if manual {
            scheduler.force_start("blocked".into()).await
        } else {
            scheduler.check("blocked".into()).await
        };
        assert!(result.unwrap_err().contains("保存主播名称失败"));
        let current = state.store.get("blocked").await.unwrap();
        assert!(current.streamer_name.is_empty() && !current.is_live && !current.is_recording);
        assert!(events.try_recv().is_err());
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
    assert_eq!(state.store.get("parse-error").await.unwrap(), record);
    assert!(scheduler.force_start("parse-error".into()).await.is_err());
    assert_eq!(state.store.get("parse-error").await.unwrap(), record);
    streamcap_core::api::shutdown(&state).await.unwrap();
    scheduler.finish_background().await;
}
