//! Window close, tray hide and application exit have distinct, explicit lifecycles.
use serde::Serialize;
use std::{
    io,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use streamcap_core::service::Server;
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub struct Lifecycle {
    server: Arc<Server>,
    closing: AtomicBool,
    finished: AtomicBool,
    pending: AtomicBool,
    tray_available: AtomicBool,
    ui_ready: AtomicBool,
    action: tokio::sync::Mutex<()>,
    report: Option<PathBuf>,
    native_stop: CancellationToken,
    native_tasks: TaskTracker,
    shutdown_at: std::sync::Mutex<Option<std::time::Instant>>,
    system_shutdown: AtomicBool,
    power_committed: AtomicBool,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowStatus {
    pub maximized: bool,
    pub visible: bool,
    pub decorated: bool,
    pub close_pending: bool,
    pub closing: bool,
    pub tray_available: bool,
    pub shutdown_seconds: Option<u64>,
    pub system_shutdown: bool,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseRequest {
    pub active_recordings: usize,
    pub pending_media_jobs: usize,
    pub tray_available: bool,
}
#[derive(Debug, PartialEq)]
enum ClosePolicy {
    Ask,
    Exit,
    Tray,
}
impl ClosePolicy {
    fn parse(value: &str, tray_available: bool) -> Self {
        match value {
            "exit" => Self::Exit,
            "tray" if tray_available => Self::Tray,
            _ => Self::Ask,
        }
    }
}
impl Lifecycle {
    pub fn new(server: Arc<Server>, report: Option<PathBuf>) -> Self {
        Self {
            server,
            closing: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            pending: AtomicBool::new(false),
            tray_available: AtomicBool::new(false),
            ui_ready: AtomicBool::new(false),
            action: tokio::sync::Mutex::new(()),
            native_stop: CancellationToken::new(),
            native_tasks: TaskTracker::new(),
            shutdown_at: std::sync::Mutex::new(None),
            system_shutdown: AtomicBool::new(false),
            power_committed: AtomicBool::new(false),
            report,
        }
    }
    pub fn finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }
    pub fn set_tray_available(&self, value: bool) {
        self.tray_available.store(value, Ordering::SeqCst);
    }
    pub fn is_smoke(&self) -> bool {
        self.report.is_some()
    }
    pub async fn ready(&self, window: &WebviewWindow) {
        self.ui_ready.store(true, Ordering::SeqCst);
        if self.pending.load(Ordering::SeqCst) && !self.closing.load(Ordering::SeqCst) {
            let request = CloseRequest {
                active_recordings: self.server.state().engine.active_ids().await.len(),
                pending_media_jobs: self.server.state().scheduler.postprocess.pending(),
                tray_available: self.tray_available.load(Ordering::SeqCst),
            };
            let _ = window.emit("streamcap:close-requested", request);
        }
    }
}
pub fn window_status(window: &WebviewWindow) -> WindowStatus {
    let lifecycle = window.state::<Lifecycle>();
    let shutdown_seconds = lifecycle
        .shutdown_at
        .lock()
        .expect("shutdown timer")
        .map(|when| {
            when.saturating_duration_since(std::time::Instant::now())
                .as_secs()
                .saturating_add(1)
        });
    WindowStatus {
        maximized: window.is_maximized().unwrap_or(false),
        visible: window.is_visible().unwrap_or(false),
        decorated: window.is_decorated().unwrap_or(true),
        close_pending: lifecycle.pending.load(Ordering::SeqCst),
        closing: lifecycle.closing.load(Ordering::SeqCst),
        tray_available: lifecycle.tray_available.load(Ordering::SeqCst),
        shutdown_seconds,
        system_shutdown: lifecycle.system_shutdown.load(Ordering::SeqCst),
    }
}
pub fn emit_window_status(window: &WebviewWindow) {
    let _ = window.emit("streamcap:window-state", window_status(window));
}
pub fn show_main(app: &AppHandle) -> Result<(), String> {
    let window = app.get_webview_window("main").ok_or("主窗口不可用")?;
    window.unminimize().map_err(|_| "无法还原窗口")?;
    window.show().map_err(|_| "无法显示主窗口")?;
    window.set_focus().map_err(|_| "无法聚焦主窗口")?;
    emit_window_status(&window);
    Ok(())
}
pub fn on_close_requested(window: &WebviewWindow) {
    let state = window.state::<Lifecycle>();
    if state.closing.load(Ordering::SeqCst) || state.pending.swap(true, Ordering::SeqCst) {
        return;
    }
    // A failed frontend must not leave an uncloseable application; initialized windows always honor the preference.
    if !state.ui_ready.load(Ordering::SeqCst) {
        log::warn!("界面尚未就绪，关闭请求直接进入安全退出");
        request_exit(window.app_handle());
        return;
    }
    let window = window.clone();
    tauri::async_runtime::spawn(async move {
        let state = window.state::<Lifecycle>();
        let _action = state.action.lock().await;
        if state.closing.load(Ordering::SeqCst) {
            return;
        }
        let preference = state
            .server
            .state()
            .config
            .read()
            .await
            .get_str("close_action", "ask");
        let tray_available = state.tray_available.load(Ordering::SeqCst);
        match ClosePolicy::parse(&preference, tray_available) {
            ClosePolicy::Exit => request_exit(window.app_handle()),
            ClosePolicy::Tray => {
                if let Err(error) = window.hide() {
                    log::error!("无法隐藏到托盘: {error}");
                } else {
                    log::info!("主窗口已最小化到托盘，后台监控继续运行");
                }
                state.pending.store(false, Ordering::SeqCst);
                emit_window_status(&window);
            }
            ClosePolicy::Ask => {
                let payload = CloseRequest {
                    active_recordings: state.server.state().engine.active_ids().await.len(),
                    pending_media_jobs: state.server.state().scheduler.postprocess.pending(),
                    tray_available,
                };
                if let Err(error) = window.emit("streamcap:close-requested", payload) {
                    state.pending.store(false, Ordering::SeqCst);
                    log::error!("无法显示关闭确认: {error}");
                }
            }
        }
    });
}
pub async fn resolve_close(
    window: &WebviewWindow,
    choice: &str,
    remember: bool,
) -> Result<WindowStatus, String> {
    if !matches!(choice, "cancel" | "exit" | "tray") {
        return Err("关闭选择无效".into());
    }
    let state = window.state::<Lifecycle>();
    let _action = state.action.lock().await;
    if state.closing.load(Ordering::SeqCst) {
        return Err("应用正在退出".into());
    }
    if !state.pending.load(Ordering::SeqCst) {
        return Err("没有待处理的关闭请求".into());
    }
    if choice == "cancel" {
        state.pending.store(false, Ordering::SeqCst);
        return Ok(window_status(window));
    }
    if choice == "tray" && !state.tray_available.load(Ordering::SeqCst) {
        return Err("系统托盘不可用，请选择取消或退出应用".into());
    }
    if remember {
        let patch = serde_json::json!({"close_action":choice})
            .as_object()
            .expect("object")
            .clone();
        state
            .server
            .state()
            .config
            .write()
            .await
            .update_user_config(patch)
            .map_err(|_| "关闭偏好保存失败，窗口保持打开，请重试")?;
        state
            .server
            .state()
            .store
            .emit("settings", serde_json::json!({"changed":["close_action"]}));
    }
    if choice == "tray" {
        window
            .hide()
            .map_err(|_| "无法隐藏到系统托盘，窗口保持打开")?;
        state.pending.store(false, Ordering::SeqCst);
        log::info!("主窗口已最小化到托盘，后台监控继续运行");
    } else {
        request_exit(window.app_handle());
    }
    emit_window_status(window);
    Ok(window_status(window))
}
pub fn request_exit(app: &AppHandle) {
    let Some(state) = app.try_state::<Lifecycle>() else {
        app.exit(1);
        return;
    };
    if state.closing.swap(true, Ordering::SeqCst) {
        return;
    }
    crate::kuaishou_verification::begin_shutdown(app);
    state.native_stop.cancel();
    state.native_tasks.close();
    state.server.request_shutdown();
    if let Some(window) = app.get_webview_window("main") {
        emit_window_status(&window);
    }
    let server = state.server.clone();
    let native_tasks = state.native_tasks.clone();
    let known_terminal = server
        .state()
        .scheduler
        .postprocess
        .jobs()
        .into_iter()
        .filter(|job| !job.state.pending())
        .map(|job| job.id)
        .collect::<std::collections::HashSet<_>>();
    let report = state.report.clone();
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        native_tasks.wait().await;
        crate::kuaishou_verification::wait_shutdown(&handle).await;
        let outcome = server.shutdown().await;
        let ok = outcome.is_ok();
        if let Err(error) = &outcome {
            log::error!("退出保存失败: {error}");
        }
        if let Some(report) = report {
            let value = serde_json::json!({"shutdownComplete":ok,"activeRecordings":server.state().engine.active_ids().await.len(),"pythonResolver":false,"pendingMediaJobs":server.state().scheduler.postprocess.pending(),"activePreviews":server.state().preview.active(),"verification":crate::kuaishou_verification::status(&handle)});
            if let Err(error) = std::fs::write(
                report,
                serde_json::to_vec_pretty(&value).unwrap_or_default(),
            ) {
                log::error!("写入验收结果失败: {error}");
            }
        }
        if let Some(state) = handle.try_state::<Lifecycle>() {
            state.finished.store(true, Ordering::SeqCst);
        }
        let shutdown_requested = handle
            .state::<Lifecycle>()
            .system_shutdown
            .load(Ordering::SeqCst);
        let failed_media = server
            .state()
            .scheduler
            .postprocess
            .jobs()
            .iter()
            .filter(|job| !known_terminal.contains(&job.id))
            .any(|job| {
                matches!(
                    job.state,
                    streamcap_core::model::MediaJobState::Failed
                        | streamcap_core::model::MediaJobState::Cancelled
                )
            });
        let commit_shutdown = {
            let state = handle.state::<Lifecycle>();
            let _gate = state.shutdown_at.lock().expect("shutdown timer");
            let commit = state.system_shutdown.load(Ordering::SeqCst)
                && ok
                && !failed_media
                && !state.is_smoke();
            if commit {
                state.power_committed.store(true, Ordering::SeqCst);
            }
            commit
        };
        if commit_shutdown {
            if let Err(error) = power_off().await {
                log::error!("{error}");
                handle.exit(1);
                return;
            }
        } else if shutdown_requested {
            log::warn!("未执行系统关机：处于验收模式或退出收尾未通过");
        }
        handle.exit(if ok { 0 } else { 1 });
    });
}

fn spawn_native_task(
    tracker: &TaskTracker,
    task: impl std::future::Future<Output = ()> + Send + 'static,
) {
    // Tauri setup runs on the main thread, outside a current Tokio runtime.
    tauri::async_runtime::spawn(tracker.track_future(task));
}

/// One owned event loop handles notifications and countdowns; it is joined on exit.
pub fn start_native_events(
    app: &AppHandle,
    mut events: tokio::sync::broadcast::Receiver<streamcap_core::store::GatewayEvent>,
) {
    use tauri_plugin_notification::NotificationExt;
    let state = app.state::<Lifecycle>();
    let stop = state.native_stop.clone();
    let handle = app.clone();
    spawn_native_task(&state.native_tasks, async move {
        let app = handle;
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                message = events.recv() => {
                    let message = match message {
                        Ok(value) => value,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    };
                    let state = app.state::<Lifecycle>();
                    if state.closing.load(Ordering::SeqCst) { continue; }
                    if message.topic=="kuaishouSessionChanged" { crate::kuaishou_verification::session_changed(&app).await;continue; }
                    if state.is_smoke() { continue; }
                    match message.topic.as_str() {
                        "kuaishouVerificationRequired" => { if let Some(id)=message.payload["recId"].as_str() { crate::kuaishou_verification::automatic(&app,id.to_owned()); } },
                        "nativeNotification" => {
                            let title = message.payload["title"].as_str().unwrap_or("StreamCap").chars().take(120).collect::<String>();
                            let body = message.payload["body"].as_str().unwrap_or("").chars().take(1000).collect::<String>();
                            if app.notification().builder().title(title).body(body).show().is_err() { state.server.state().store.snack("系统通知未送达，请检查 Windows 通知权限"); }
                        },
                        "shutdownSchedule" => {
                            state.system_shutdown.store(false, Ordering::SeqCst);
                            *state.shutdown_at.lock().expect("shutdown timer") = None;
                            if let Some(window)=app.get_webview_window("main"){emit_window_status(&window);}
                        },
                        "nativeShutdown" => {
                            let mut when = state.shutdown_at.lock().expect("shutdown timer");
                            if when.is_none() {
                                *when = Some(std::time::Instant::now() + std::time::Duration::from_secs(60));
                                state.system_shutdown.store(true, Ordering::SeqCst);
                                drop(when);
                                let _ = show_main(&app);
                            }
                        },
                        _ => {}
                    }
                },
                _ = timer.tick() => {
                    let state = app.state::<Lifecycle>();
                    let when = *state.shutdown_at.lock().expect("shutdown timer");
                    if when.is_some_and(|at| at <= std::time::Instant::now()) {
                        *state.shutdown_at.lock().expect("shutdown timer") = None;
                        request_exit(&app);
                        break;
                    }
                    if when.is_some() { if let Some(window) = app.get_webview_window("main") { emit_window_status(&window); } }
                }
            }
        }
    });
}
pub fn cancel_shutdown(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<Lifecycle>();
    {
        let mut timer = state.shutdown_at.lock().map_err(|_| "关机状态异常")?;
        if state.power_committed.load(Ordering::SeqCst) {
            return Err("关机请求已提交给 Windows，不能再通过应用取消".into());
        }
        state.system_shutdown.store(false, Ordering::SeqCst);
        *timer = None;
    }
    state
        .server
        .state()
        .scheduler
        .automation
        .quick_shutdown(None)?;
    if let Some(window) = app.get_webview_window("main") {
        emit_window_status(&window);
    }
    Ok(())
}
async fn power_off() -> Result<(), String> {
    #[cfg(windows)]
    {
        let executable =
            PathBuf::from(std::env::var_os("SystemRoot").ok_or("Windows 系统目录不可用")?)
                .join("System32/shutdown.exe");
        let mut command = tokio::process::Command::new(executable);
        command
            .args(["/s", "/t", "0"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(0x08000000)
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|_| "无法执行系统关机")?;
        match tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await {
            Ok(Ok(status)) if status.success() => Ok(()),
            Ok(_) => Err("系统拒绝关机，未强制关闭其他程序".into()),
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                Err("系统关机请求超时".into())
            }
        }
    }
    #[cfg(not(windows))]
    {
        Err("当前平台不支持自动关机".into())
    }
}

pub struct RunOptions {
    pub data_dir: Option<PathBuf>,
    pub smoke_seconds: Option<u64>,
    pub port: u16,
}
impl RunOptions {
    pub fn from_args() -> io::Result<Self> {
        let mut options = Self {
            data_dir: None,
            smoke_seconds: None,
            port: streamcap_core::GATEWAY_PORT,
        };
        let mut args = std::env::args().skip(1);
        let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "启动参数无效");
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--data-dir" => {
                    let path = PathBuf::from(args.next().ok_or_else(invalid)?);
                    if !path.is_absolute() {
                        return Err(invalid());
                    }
                    options.data_dir = Some(path);
                }
                "--smoke-seconds" => {
                    let seconds = args
                        .next()
                        .ok_or_else(invalid)?
                        .parse::<u64>()
                        .map_err(|_| invalid())?;
                    if !(2..=120).contains(&seconds) {
                        return Err(invalid());
                    }
                    options.smoke_seconds = Some(seconds);
                }
                "--api-port" => {
                    options.port = args
                        .next()
                        .ok_or_else(invalid)?
                        .parse()
                        .map_err(|_| invalid())?
                }
                _ => return Err(invalid()),
            }
        }
        if options.smoke_seconds.is_some() && options.data_dir.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "验收模式必须指定隔离数据目录",
            ));
        }
        if options.smoke_seconds.is_none() && options.port != streamcap_core::GATEWAY_PORT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "自定义端口仅用于隔离验收",
            ));
        }
        if options.smoke_seconds.is_some()
            && options
                .data_dir
                .as_ref()
                .is_some_and(|p| p.join("config").exists())
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "验收模式必须使用未初始化的新数据目录",
            ));
        }
        Ok(options)
    }
}

/// Test-only transport routing: the unchanged shipped UI must never reach the real profile's port.
/// Installed runs do not inject this script and continue to use the fixed local API address.
pub fn smoke_transport_script(address: std::net::SocketAddr) -> String {
    r#"(() => {
      if (!['http://tauri.localhost','https://tauri.localhost','tauri://localhost'].includes(location.origin)) return;
      const base='__ISOLATED_API__';
      const stats=window.__STREAMCAP_NATIVE_SMOKE__={requests:[],eventSources:[],errors:[]};
      const isApi=value=>{const url=new URL(String(value),location.href);return url.origin===base&&url.pathname.startsWith('/api/');};
      const nativeFetch=window.fetch.bind(window);
      window.fetch=async(input,init)=>{
        const target=input instanceof Request?input.url:String(input);
        if(!isApi(target))return nativeFetch(input,init);
        try {const response=await nativeFetch(input,init);stats.requests.push({path:new URL(target).pathname,status:response.status});return response;}
        catch(error){stats.errors.push(String(error));throw error;}
      };
      const NativeEventSource=window.EventSource;
      window.EventSource=class extends NativeEventSource {constructor(url,options){super(url,options);if(isApi(url))stats.eventSources.push(String(url));}};
      addEventListener('error',event=>stats.errors.push(event.message||'window error'));
      addEventListener('unhandledrejection',event=>stats.errors.push(String(event.reason)));
    })();"#.replace("__ISOLATED_API__",&format!("http://{address}"))
}

/// Only bundled assets, IPC, and this runtime's loopback server are reachable.
pub fn content_security_policy(address: std::net::SocketAddr, development: bool) -> String {
    let sockets = if development {
        " ws://localhost:1420 ws://127.0.0.1:1420"
    } else {
        ""
    };
    format!("default-src 'none'; base-uri 'none'; object-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self'; connect-src 'self' ipc: http://ipc.localhost http://{address}{sockets}; media-src http://{address} blob:; frame-src 'none'; form-action 'none'")
}
pub fn runtime_script(address: std::net::SocketAddr) -> String {
    let config = serde_json::json!({"apiOrigin":format!("http://{address}")});
    format!("Object.defineProperty(window,'__STREAMCAP_RUNTIME__',{{value:Object.freeze({config}),writable:false,configurable:false}});")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_event_receiver_keeps_detection_before_window_setup() {
        let store = streamcap_core::store::Store::new(streamcap_core::Workspace::from_repo_root(
            std::env::temp_dir(),
        ));
        let mut events = store.subscribe();
        store.emit(
            "kuaishouVerificationRequired",
            serde_json::json!({"recId":"fixture-startup"}),
        );
        let event = events
            .try_recv()
            .expect("startup verification must remain queued before the window event loop starts");
        assert_eq!(event.topic, "kuaishouVerificationRequired");
        assert_eq!(event.payload["recId"], "fixture-startup");
    }
    #[test]
    fn native_tasks_start_from_a_synchronous_thread_and_are_joined() {
        assert!(tokio::runtime::Handle::try_current().is_err());
        let tracker = TaskTracker::new();
        let stop = CancellationToken::new();
        let completed = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_completed = completed.clone();
        spawn_native_task(&tracker, async move {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            worker_stop.cancelled().await;
            worker_completed.store(true, Ordering::SeqCst);
        });
        tracker.close();
        stop.cancel();
        tauri::async_runtime::block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(3), tracker.wait())
                .await
                .unwrap();
        });
        assert!(completed.load(Ordering::SeqCst));
        assert!(tracker.is_empty());
    }

    #[test]
    fn runtime_policy_and_probe_use_the_bound_port() {
        let address = "127.0.0.1:54321".parse().unwrap();
        for script in [
            content_security_policy(address, false),
            runtime_script(address),
            smoke_transport_script(address),
        ] {
            assert!(script.contains("127.0.0.1:54321"));
            assert!(!script.contains("127.0.0.1:6059"));
        }
        assert!(!content_security_policy(address, false).contains("ws://"));
    }
    #[test]
    fn close_policy_is_explicit_and_never_hides_without_a_tray() {
        assert_eq!(ClosePolicy::parse("ask", true), ClosePolicy::Ask);
        assert_eq!(ClosePolicy::parse("exit", true), ClosePolicy::Exit);
        assert_eq!(ClosePolicy::parse("tray", true), ClosePolicy::Tray);
        assert_eq!(ClosePolicy::parse("tray", false), ClosePolicy::Ask);
        assert_eq!(ClosePolicy::parse("invalid", true), ClosePolicy::Ask);
    }
}
