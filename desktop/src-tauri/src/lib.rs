//! Native desktop shell: in-process resolution, system WebView2 and owned-resource shutdown.
use std::sync::Arc;
use streamcap_core::{
    service::{Server, ServerOptions},
    Workspace,
};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, RunEvent, WindowEvent};
mod kuaishou;
mod lifecycle;
mod logging;
include!("../../build/ffmpeg-bundle/embedded.rs");

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    if let Err(error) = run_app() {
        log::error!("StreamCap 启动失败: {error}");
        eprintln!("StreamCap 启动失败: {error}");
        std::process::exit(1);
    }
}
fn run_app() -> Result<(), Box<dyn std::error::Error>> {
    let options = lifecycle::RunOptions::from_args()?;
    let exe_dir = std::env::current_exe()?
        .parent()
        .ok_or("程序目录不可用")?
        .to_path_buf();
    let workspace = if let Some(data) = options.data_dir.clone() {
        Workspace::from_repo_root(data)
    } else {
        Workspace::for_installed(&exe_dir)?
    };
    workspace.ensure_ready()?;
    let log_dir = workspace.user_data_dir.join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let log = logging::RollingLog::open(&log_dir)?;
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Pipe(Box::new(log)))
        .try_init();
    streamcap_core::tools::ensure_bundled_tools(&workspace, FFMPEG_BUNDLE)?;
    let state = tauri::async_runtime::block_on(streamcap_core::api::bootstrap(workspace.clone()))?;
    let browser_requests = state.resolver.attach_kuaishou_browser()?;
    let kuaishou_host = kuaishou::Host::new(
        workspace.user_data_dir.clone(),
        state.config.clone(),
        state.resolver.clone(),
        state.store.clone(),
        options.smoke_seconds.is_some(),
    );
    let theme = tauri::async_runtime::block_on(async {
        state.config.read().await.get_str("theme_mode", "light")
    });
    // Subscribe before monitoring can emit native notifications during window startup.
    let native_events = state.store.subscribe();
    let server = Arc::new(tauri::async_runtime::block_on(Server::start(
        state,
        ServerOptions {
            port: options.port,
            monitoring: options.smoke_seconds.is_none(),
        },
    ))?);
    let address = server.address();
    let smoke = options.smoke_seconds.is_some();
    let mut context = tauri::generate_context!();
    let csp = lifecycle::content_security_policy(address, cfg!(debug_assertions));
    context.config_mut().app.security.csp = Some(tauri::utils::config::Csp::Policy(csp.clone()));
    context.config_mut().app.security.dev_csp = Some(tauri::utils::config::Csp::Policy(csp));
    for window in &mut context.config_mut().app.windows {
        if options.smoke_seconds.is_some() {
            window.visible = false;
        }
    }
    let window_configs = std::mem::take(&mut context.config_mut().app.windows);
    let setup_server = server.clone();
    let setup_kuaishou = kuaishou_host.clone();
    let app=tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![desktop_ready,desktop_window_action,desktop_close_choice,desktop_theme,desktop_smoke_tray_quit,desktop_kuaishou_session])
        .setup(move |app|{
            setup_kuaishou.start(app.handle().clone(), browser_requests)?;
            app.manage(setup_kuaishou);
            log::info!("原生后端已启动：{}；无需 Python/Node",setup_server.address());
            app.manage(lifecycle::Lifecycle::new(setup_server,smoke.then(||workspace.user_data_dir.join("shutdown.json"))));
            for window_config in &window_configs{
                // Tauri's config conversion does not propagate data_directory to the webview.
                let mut window=tauri::WebviewWindowBuilder::from_config(app,window_config)?
                    .data_directory(workspace.user_data_dir.join("webview"))
                    .theme(Some(window_theme(&theme)));
                window=window.initialization_script(lifecycle::runtime_script(address));
                if smoke{
                    // Keep wry 0.57 defaults and opt into CDP through the API for this isolated profile.
                    #[cfg(windows)]
                    {window=window.additional_browser_args(concat!(
                        "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection ",
                        "--autoplay-policy=no-user-gesture-required ",
                        "--remote-debugging-port=0 --remote-debugging-address=127.0.0.1"
                    ));}
                    window=window.initialization_script(lifecycle::smoke_transport_script(address));
                }
                window.build()?;
            }
            lifecycle::start_native_events(app.handle(),native_events);
            match setup_tray(app){Ok(())=>app.state::<lifecycle::Lifecycle>().set_tray_available(true),Err(error)=>log::warn!("托盘不可用: {error}")}
            if smoke{
                std::fs::write(workspace.user_data_dir.join("ready.json"),serde_json::to_vec_pretty(&serde_json::json!({"pid":std::process::id(),"address":address.to_string(),"dataDirectory":workspace.user_data_dir,"resolver":"native"}))?)?;
            }
            if let Some(seconds)=options.smoke_seconds{
                let handle=app.handle().clone();
                tauri::async_runtime::spawn(async move{tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;log::warn!("隔离验收看门狗触发退出");lifecycle::request_exit(&handle);});
            }
            Ok(())
        })
        .on_window_event(|window,event|{
            if window.label()!="main" { return; }
            if let Some(webview)=window.app_handle().get_webview_window(window.label()){
                match event{
                    WindowEvent::CloseRequested{api,..}=>{api.prevent_close();lifecycle::on_close_requested(&webview);},
                    WindowEvent::Resized(_)|WindowEvent::Focused(_)=>lifecycle::emit_window_status(&webview),
                    _=>{},
                }
            }
        })
        .build(context);
    let app = match app {
        Ok(app) => app,
        Err(error) => {
            if let Err(cleanup) = tauri::async_runtime::block_on(server.shutdown()) {
                log::error!("窗口启动失败后的后端清理未完成: {cleanup}");
            }
            return Err(error.into());
        }
    };
    app.run(|handle, event| match event {
        RunEvent::ExitRequested { api, .. } => {
            if let Some(state) = handle.try_state::<lifecycle::Lifecycle>() {
                if !state.finished() {
                    api.prevent_exit();
                    lifecycle::request_exit(handle);
                }
            }
        }
        RunEvent::Exit => log::info!("StreamCap 已退出"),
        _ => {}
    });
    tauri::async_runtime::block_on(server.shutdown())?;
    tauri::async_runtime::block_on(kuaishou_host.wait_stopped());
    Ok(())
}
fn setup_tray(app: &mut tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 StreamCap", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let mut builder = TrayIconBuilder::with_id("main-tray")
        .tooltip("StreamCap 直播录制")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Err(error) = lifecycle::show_main(app) {
                    log::warn!("{error}");
                }
            }
            "quit" => lifecycle::request_exit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                if let Err(error) = lifecycle::show_main(tray.app_handle()) {
                    log::warn!("{error}");
                }
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

fn require_main(window: &tauri::WebviewWindow) -> Result<(), String> {
    if window.label() == "main" {
        Ok(())
    } else {
        Err("仅主窗口可执行此操作".into())
    }
}
#[tauri::command]
async fn desktop_ready(window: tauri::WebviewWindow) -> Result<lifecycle::WindowStatus, String> {
    require_main(&window)?;
    window.state::<lifecycle::Lifecycle>().ready(&window).await;
    Ok(lifecycle::window_status(&window))
}
#[tauri::command]
async fn desktop_window_action(
    window: tauri::WebviewWindow,
    action: String,
) -> Result<serde_json::Value, String> {
    require_main(&window)?;
    if action == "cancel-shutdown" {
        lifecycle::cancel_shutdown(window.app_handle())?;
        return Ok(serde_json::Value::Null);
    }
    if action == "pick-directory" {
        use tauri_plugin_dialog::DialogExt;
        if window.state::<lifecycle::Lifecycle>().is_smoke() {
            return Err("隔离验收不打开系统目录选择器".into());
        }
        let (send, receive) = tokio::sync::oneshot::channel();
        window
            .dialog()
            .file()
            .set_parent(&window)
            .pick_folder(move |value| {
                let _ = send.send(value);
            });
        let path = receive
            .await
            .map_err(|_| "目录选择已取消")?
            .map(|file| {
                file.into_path()
                    .map(|p| p.to_string_lossy().into_owned())
                    .map_err(|_| "请选择本地目录")
            })
            .transpose()?;
        return Ok(serde_json::json!(path));
    }
    let result = match action.as_str() {
        "minimize" => window.minimize(),
        "maximize" => {
            if window.is_maximized().map_err(|_| "无法读取窗口状态")? {
                window.unmaximize()
            } else {
                window.maximize()
            }
        }
        "close" => window.close(),
        _ => return Err("窗口操作无效".into()),
    };
    result.map_err(|_| "窗口操作失败，请重试")?;
    lifecycle::emit_window_status(&window);
    serde_json::to_value(lifecycle::window_status(&window)).map_err(|_| "窗口状态无法读取".into())
}
#[tauri::command]
async fn desktop_close_choice(
    window: tauri::WebviewWindow,
    choice: String,
    remember: bool,
) -> Result<lifecycle::WindowStatus, String> {
    require_main(&window)?;
    lifecycle::resolve_close(&window, &choice, remember).await
}
fn window_theme(theme: &str) -> tauri::Theme {
    match theme {
        "dark" => tauri::Theme::Dark,
        _ => tauri::Theme::Light,
    }
}

#[tauri::command]
fn desktop_theme(window: tauri::WebviewWindow, theme: String) -> Result<(), String> {
    require_main(&window)?;
    window
        .set_theme(Some(window_theme(&theme)))
        .map_err(|_| "无法更新窗口主题".into())
}

#[tauri::command]
fn desktop_smoke_tray_quit(window: tauri::WebviewWindow) -> Result<(), String> {
    require_main(&window)?;
    if !window.state::<lifecycle::Lifecycle>().is_smoke() {
        return Err("仅隔离验收可调用".into());
    }
    // Same exit path as the tray menu. No global process or window operations.
    lifecycle::request_exit(window.app_handle());
    Ok(())
}

#[tauri::command]
async fn desktop_kuaishou_session(
    window: tauri::WebviewWindow,
    action: String,
    fixture_url: Option<String>,
) -> Result<serde_json::Value, String> {
    require_main(&window)?;
    let host = window.state::<Arc<kuaishou::Host>>().inner().clone();
    if action == "open" {
        return host.open().await;
    }
    if action == "retry" {
        return host.retry().await;
    }
    if !window.state::<lifecycle::Lifecycle>().is_smoke() {
        return Err("快手网页操作无效".into());
    }
    match action.as_str() {
        "smoke-session" => {
            host.smoke_shared_session(fixture_url.as_deref().ok_or("缺少隔离夹具地址")?)
                .await
        }
        "smoke-probe" => {
            host.smoke_probe(fixture_url.as_deref().ok_or("缺少隔离夹具地址")?)
                .await
        }
        "smoke-close" => {
            host.close().await;
            Ok(serde_json::json!({"closed":true}))
        }
        "smoke-status" => Ok(serde_json::json!({
            "workerExists":window.app_handle().get_webview_window("kuaishou-worker").is_some(),
            "portalExists":window.app_handle().get_webview_window("kuaishou-session").is_some(),
        })),
        _ => Err("快手网页操作无效".into()),
    }
}
