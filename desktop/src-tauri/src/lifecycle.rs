//! All close/quit paths share one ordered, idempotent shutdown. Never kill unrelated browser processes.
use std::{
    io,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use streamcap_core::service::Server;
use tauri::{AppHandle, Manager};

pub struct Lifecycle {
    server: Arc<Server>,
    closing: AtomicBool,
    finished: AtomicBool,
    report: Option<PathBuf>,
}
impl Lifecycle {
    pub fn new(server: Server, report: Option<PathBuf>) -> Self {
        Self {
            server: Arc::new(server),
            closing: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            report,
        }
    }
    pub fn finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }
}
pub fn request_exit(app: &AppHandle) {
    let Some(state) = app.try_state::<Lifecycle>() else {
        app.exit(1);
        return;
    };
    if state.closing.swap(true, Ordering::SeqCst) {
        return;
    }
    state.server.request_shutdown();
    let server = state.server.clone();
    let report = state.report.clone();
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let outcome = server.shutdown().await;
        let ok = outcome.is_ok();
        if let Err(error) = &outcome {
            log::error!("退出保存失败: {error}");
        }
        if let Some(report) = report {
            let value = serde_json::json!({"shutdownComplete":ok,"activeRecordings":server.state().engine.active_ids().await.len(),"pythonResolver":false});
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
        // Tauri owns WebView2 controllers and releases them on normal exit.
        handle.exit(if ok { 0 } else { 1 });
    });
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
      const route=value=>{const url=new URL(String(value),location.href);return url.origin==='http://127.0.0.1:6059'?base+url.pathname+url.search:String(value);};
      const nativeFetch=window.fetch.bind(window);
      window.fetch=async(input,init)=>{
        const original=input instanceof Request?input.url:String(input),target=route(original);
        if(target===original)return nativeFetch(input,init);
        try {const response=await nativeFetch(input instanceof Request?new Request(target,input):target,init);stats.requests.push({path:new URL(target).pathname,status:response.status});return response;}
        catch(error){stats.errors.push(String(error));throw error;}
      };
      const NativeEventSource=window.EventSource;
      window.EventSource=class extends NativeEventSource {constructor(url,options){const target=route(url);super(target,options);stats.eventSources.push(target);}};
      addEventListener('error',event=>stats.errors.push(event.message||'window error'));
      addEventListener('unhandledrejection',event=>stats.errors.push(String(event.reason)));
    })();"#.replace("__ISOLATED_API__",&format!("http://{address}"))
}
