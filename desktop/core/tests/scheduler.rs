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
