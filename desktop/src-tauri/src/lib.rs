//! Native desktop shell: in-process resolution, system WebView2 and owned-resource shutdown.
use std::sync::Arc;
use streamcap_core::{
    service::{Server, ServerOptions},
    Workspace,
};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, RunEvent, WindowEvent};
mod kuaishou_verification;
mod lifecycle;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    if let Err(error) = run_app() {
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
        Workspace {
            resource_dir: exe_dir.clone(),
            user_data_dir: data,
        }
    } else {
        Workspace::for_installed(&exe_dir)?
    };
    workspace.ensure_ready()?;
    let log_dir = workspace.user_data_dir.join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("native.log"))?;
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .target(env_logger::Target::Pipe(Box::new(log)))
        .try_init();
    let state = tauri::async_runtime::block_on(streamcap_core::api::bootstrap(workspace.clone()))?;
    let theme = tauri::async_runtime::block_on(async {
        state.config.read().await.get_str("theme_mode", "system")
    });
    // Subscribe before monitoring can emit its first verification request during window startup.
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
        window.data_directory = Some(workspace.user_data_dir.join("webview"));
        if options.smoke_seconds.is_some() {
            window.visible = false;
        }
    }
    let window_configs = std::mem::take(&mut context.config_mut().app.windows);
    let setup_server = server.clone();
    let app=tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![desktop_ready,desktop_window_action,desktop_close_choice,desktop_theme,desktop_smoke_tray_quit,desktop_kuaishou_verification])
        .setup(move |app|{
            log::info!("原生后端已启动：{}；无需 Python/Node",setup_server.address());
            app.manage(kuaishou_verification::Verification::new(setup_server.state().clone(),smoke));
            app.manage(lifecycle::Lifecycle::new(setup_server,smoke.then(||workspace.user_data_dir.join("shutdown.json"))));
            for window_config in &window_configs{
                let mut window=tauri::WebviewWindowBuilder::from_config(app,window_config)?.theme(match theme.as_str(){"light"=>Some(tauri::Theme::Light),"dark"=>Some(tauri::Theme::Dark),_=>None});
                window=window.initialization_script(lifecycle::runtime_script(address));
                if smoke{window=window.initialization_script(lifecycle::smoke_transport_script(address));}
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
            if window.label()==kuaishou_verification::LABEL {
                if let WindowEvent::CloseRequested{api,..}=event { api.prevent_close();kuaishou_verification::cancel_window(window.app_handle()); }
                return;
            }
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
#[tauri::command]
fn desktop_theme(window: tauri::WebviewWindow, theme: String) -> Result<(), String> {
    require_main(&window)?;
    let theme = match theme.as_str() {
        "light" => Some(tauri::Theme::Light),
        "dark" => Some(tauri::Theme::Dark),
        "system" => None,
        _ => return Err("窗口主题无效".into()),
    };
    window
        .set_theme(theme)
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
async fn desktop_kuaishou_verification(
    window: tauri::WebviewWindow,
    action: String,
    rec_id: Option<String>,
    fixture_url: Option<String>,
) -> Result<kuaishou_verification::Status, String> {
    require_main(&window)?;
    let app = window.app_handle();
    match action.as_str() {
        "open" => {
            if fixture_url.is_some() {
                return Err("普通验证不接受替换地址".into());
            }
            kuaishou_verification::open(
                app,
                rec_id.as_deref().ok_or("请选择验证任务")?,
                true,
                None,
            )
            .await?;
        }
        "smoke-open" => {
            if !window.state::<lifecycle::Lifecycle>().is_smoke() {
                return Err("仅隔离验收可调用".into());
            }
            kuaishou_verification::open(
                app,
                rec_id.as_deref().ok_or("请选择验证任务")?,
                true,
                Some(fixture_url.as_deref().ok_or("缺少隔离夹具地址")?),
            )
            .await?;
        }
        "smoke-close" => {
            if !window.state::<lifecycle::Lifecycle>().is_smoke() {
                return Err("仅隔离验收可调用".into());
            }
            if let Some(verification) = app.get_webview_window(kuaishou_verification::LABEL) {
                verification.close().map_err(|_| "无法关闭隔离验证窗口")?;
            }
        }
        "complete" => kuaishou_verification::begin_complete(app)?,
        "cancel" => kuaishou_verification::cancel_window(app),
        "status" => {}
        _ => return Err("验证操作无效".into()),
    }
    Ok(kuaishou_verification::status(app))
}
