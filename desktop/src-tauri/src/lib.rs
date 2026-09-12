//! Native desktop shell: in-process resolution, system WebView2 and owned-resource shutdown.
use streamcap_core::{
    service::{Server, ServerOptions},
    Workspace,
};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, RunEvent, WindowEvent};
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
    let mut context = tauri::generate_context!();
    for window in &mut context.config_mut().app.windows {
        window.data_directory = Some(workspace.user_data_dir.join("webview"));
        if options.smoke_seconds.is_some() {
            window.visible = false;
        }
    }
    let window_configs = std::mem::take(&mut context.config_mut().app.windows);
    let app=tauri::Builder::default()
        .setup(move |app|{
            let state=tauri::async_runtime::block_on(streamcap_core::api::bootstrap(workspace.clone()))?;
            let server=tauri::async_runtime::block_on(Server::start(state,ServerOptions{port:options.port,monitoring:options.smoke_seconds.is_none()}))?;
            log::info!("原生后端已启动：{}；无需 Python/Node",server.address());
            let address=server.address();
            let smoke=options.smoke_seconds.is_some();
            let report=smoke.then(||workspace.user_data_dir.join("shutdown.json"));
            app.manage(lifecycle::Lifecycle::new(server,report));
            for window_config in &window_configs {
                let mut window=tauri::WebviewWindowBuilder::from_config(app,window_config)?;
                if smoke {window=window.initialization_script(lifecycle::smoke_transport_script(address));}
                window.build()?;
            }
            if smoke {
                std::fs::write(workspace.user_data_dir.join("ready.json"),serde_json::to_vec_pretty(&serde_json::json!({"pid":std::process::id(),"address":address.to_string(),"dataDirectory":workspace.user_data_dir,"resolver":"native"}))?)?;
            }
            if let Err(error)=setup_tray(app){log::warn!("托盘不可用: {error}");}
            if let Some(seconds)=options.smoke_seconds {
                let handle=app.handle().clone();
                tauri::async_runtime::spawn(async move{tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;if let Some(window)=handle.get_webview_window("main"){let _=window.close();}else{lifecycle::request_exit(&handle);}});
            }
            Ok(())
        })
        .on_window_event(|window,event|{if let WindowEvent::CloseRequested{api,..}=event{api.prevent_close();lifecycle::request_exit(window.app_handle());}})
        .build(context)?;
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
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
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
                if let Some(window) = tray.app_handle().get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}
