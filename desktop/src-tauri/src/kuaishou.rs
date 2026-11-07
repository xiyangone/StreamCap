//! Dedicated website session. Remote pages receive no main-window IPC capability.
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
    time::Duration,
};
use streamcap_core::{
    config::ConfigStore,
    platforms::kuaishou::{self, BrowserRequest, BrowserRequests, BrowserResponse},
    resolver::Resolver,
};
use tauri::{
    webview::{Cookie, NewWindowResponse},
    AppHandle, Manager, Url, WebviewUrl, WebviewWindow,
};
use tokio::sync::{oneshot, Mutex, Notify, RwLock};
use tokio_util::sync::CancellationToken;

const WORKER: &str = "kuaishou-worker";
const PORTAL: &str = "kuaishou-session";
const LIVE: &str = "https://live.kuaishou.com/";
const DETAIL_PATH: &str = "/live_api/liveroom/livedetail";
const FIXTURE_PATH: &str = "/kuaishou-fixture";
const MAX_REPLY: usize = 4 * 1024 * 1024;

fn set_browser_cookie_domain(cookie: &mut Cookie<'_>, domain: &str) {
    // Wry 0.57 passes Cookie::domain() to WebView2, where a leading dot means
    // subdomains. cookie 0.18 strips one dot, so preserve it at this API boundary.
    #[cfg(windows)]
    cookie.set_domain(if domain.starts_with('.') {
        format!(".{domain}")
    } else {
        domain.to_owned()
    });
    #[cfg(not(windows))]
    cookie.set_domain(domain.to_owned());
}

fn is_site_cookie(cookie: &Cookie<'_>) -> bool {
    cookie.domain().is_some_and(|domain| {
        let domain = domain.trim_start_matches('.');
        domain == "kuaishou.com" || domain.ends_with(".kuaishou.com")
    })
}

struct Session {
    proxy: Option<Url>,
    directory: PathBuf,
    worker: WebviewWindow,
    last_room: Option<Url>,
    recovery_room: Option<Url>,
}

pub struct Host {
    directory: PathBuf,
    config: Arc<RwLock<ConfigStore>>,
    resolver: Resolver,
    store: streamcap_core::store::Store,
    app: OnceLock<AppHandle>,
    session: Mutex<Option<Session>>,
    stop: CancellationToken,
    finished: AtomicBool,
    done: Notify,
    smoke: bool,
}

fn proxy_url(value: Option<&str>) -> Result<Option<Url>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.trim().is_empty() {
        return Err("快手网页代理地址为空，未直接联网".into());
    }
    let mut url = Url::parse(value).map_err(|_| "快手网页代理地址无效")?;
    if url.scheme() == "socks5h" {
        url.set_scheme("socks5")
            .map_err(|_| "快手网页代理协议无效")?;
    }
    if !matches!(url.scheme(), "http" | "socks5")
        || url.host_str().is_none()
        || url.port_or_known_default().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/" && !url.path().is_empty()
    {
        return Err("快手网页仅支持不带账号密码的 HTTP 或 SOCKS5 代理，未直接联网".into());
    }
    Ok(Some(url))
}

fn official(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
        && url
            .host_str()
            .is_some_and(|host| host == "kuaishou.com" || host.ends_with(".kuaishou.com"))
}

fn fixture(url: &Url) -> bool {
    url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.port().is_some_and(|port| port > 1024)
        && url.path() == FIXTURE_PATH
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

fn navigation_allowed(url: &Url, worker: bool, smoke: bool) -> bool {
    if url.as_str() == "about:blank" {
        return true;
    }
    if smoke {
        return fixture(url);
    }
    official(url)
        && (!worker || url.host_str() == Some("live.kuaishou.com") && url.path() == DETAIL_PATH)
}

fn read_script(url: &Url) -> String {
    // The callback reads one navigation only. It neither executes site-provided source nor exports cookies.
    format!(
        r#"(()=>{{
      if(window.__streamcapOldDocument||location.href!=={expected}||document.readyState==='loading')return null;
      const body=document.body?.innerText??'';
      if(body.length>{limit})return {{error:'oversize'}};
      try{{const payload=JSON.parse(body);const navigation=performance.getEntriesByType('navigation')[0];
        return {{url:location.href,httpStatus:navigation?.responseStatus??0,payload}};
      }}catch{{return {{error:'unreadable'}};}}
    }})()"#,
        expected = serde_json::to_string(url.as_str()).expect("URL serialization"),
        limit = MAX_REPLY
    )
}

fn decode_reply(reply: &str, expected: &Url) -> Result<Option<BrowserResponse>, String> {
    if reply.len() > MAX_REPLY + 65536 {
        return Err("快手网页返回内容过大".into());
    }
    let value: Value = serde_json::from_str(reply).map_err(|_| kuaishou::PAGE_UNAVAILABLE)?;
    if value.is_null() {
        return Ok(None);
    }
    if value.get("error").is_some() {
        return Err(kuaishou::PAGE_UNAVAILABLE.into());
    }
    let response: BrowserResponse =
        serde_json::from_value(value).map_err(|_| kuaishou::PAGE_UNAVAILABLE)?;
    if response.url != expected.as_str() {
        return Err("快手网页导航已变化，本次请求已取消".into());
    }
    Ok(Some(response))
}

impl Host {
    pub fn new(
        directory: PathBuf,
        config: Arc<RwLock<ConfigStore>>,
        resolver: Resolver,
        store: streamcap_core::store::Store,
        smoke: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            directory: directory.join("webview-kuaishou"),
            config,
            stop: resolver.cancellation(),
            resolver,
            store,
            app: OnceLock::new(),
            session: Mutex::new(None),
            finished: AtomicBool::new(false),
            done: Notify::new(),
            smoke,
        })
    }

    pub fn start(
        self: &Arc<Self>,
        app: AppHandle,
        requests: BrowserRequests,
    ) -> Result<(), String> {
        self.app.set(app).map_err(|_| "快手浏览器主机已启动")?;
        let host = self.clone();
        tauri::async_runtime::spawn(async move {
            host.run(requests).await;
        });
        Ok(())
    }

    fn app(&self) -> Result<&AppHandle, String> {
        self.app
            .get()
            .ok_or_else(|| "快手浏览器主机尚未准备完成".into())
    }

    async fn run(&self, mut requests: BrowserRequests) {
        loop {
            let request = tokio::select! { biased;
                _ = self.stop.cancelled() => break,
                request = requests.recv() => match request { Some(request) => request, None => break },
            };
            let cancellation = request.cancellation();
            if cancellation.is_cancelled() {
                continue;
            }
            let result = tokio::select! { biased;
                _ = self.stop.cancelled() => Err(kuaishou::BROWSER_CANCELLED.into()),
                _ = cancellation.cancelled() => Err(kuaishou::BROWSER_CANCELLED.into()),
                result = self.resolve(&request) => result,
            };
            if result.is_err() && !self.stop.is_cancelled() {
                if let Some(session) = self.session.lock().await.as_ref() {
                    let _ = session
                        .worker
                        .navigate(Url::parse("about:blank").expect("blank URL"));
                }
            }
            request.respond(result);
        }
        self.close().await;
        self.finished.store(true, Ordering::Release);
        self.done.notify_waiters();
    }

    pub async fn wait_stopped(&self) {
        let notified = self.done.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if !self.finished.load(Ordering::Acquire) {
            let _ = tokio::time::timeout(Duration::from_secs(3), notified).await;
        }
    }

    async fn create_window(
        &self,
        worker: bool,
        directory: PathBuf,
        proxy: Option<Url>,
        url: Url,
    ) -> Result<WebviewWindow, String> {
        let app = self.app()?.clone();
        let create_app = app.clone();
        let smoke = self.smoke;
        let (send, receive) = oneshot::channel();
        app.run_on_main_thread(move || {
            if send.is_closed() {
                return;
            }
            let result = {
                let mut builder = tauri::WebviewWindowBuilder::new(
                    &create_app,
                    if worker { WORKER } else { PORTAL },
                    WebviewUrl::External(url),
                )
                .title("快手网页 — StreamCap")
                .data_directory(directory)
                .visible(!worker)
                .inner_size(1000.0, 760.0)
                .min_inner_size(760.0, 560.0)
                .disable_drag_drop_handler()
                .on_navigation(move |url| navigation_allowed(url, worker, smoke))
                .on_new_window(|_, _| NewWindowResponse::Deny);
                if let Some(proxy) = proxy {
                    builder = builder.proxy_url(proxy);
                } else {
                    #[cfg(windows)]
                    {
                        builder = builder.additional_browser_args("--no-proxy-server");
                    }
                }
                builder
                    .build()
                    .map_err(|_| "无法创建快手网页，请检查 WebView2 运行环境".to_owned())
            };
            if let Err(Ok(window)) = send.send(result) {
                let _ = window.destroy();
            }
        })
        .map_err(|_| "无法调度快手网页窗口")?;
        tokio::time::timeout(Duration::from_secs(10), receive)
            .await
            .map_err(|_| "快手网页窗口创建超时")?
            .map_err(|_| "快手网页窗口创建已取消")?
    }

    async fn ensure_session<'a>(
        &self,
        slot: &'a mut Option<Session>,
        proxy: Option<Url>,
    ) -> Result<&'a mut Session, String> {
        if slot.as_ref().is_some_and(|session| session.proxy != proxy) {
            if self.app()?.get_webview_window(PORTAL).is_some() {
                return Err("代理设置已变化，请先关闭快手网页再重新打开".into());
            }
            if let Some(previous) = slot.take() {
                previous
                    .worker
                    .destroy()
                    .map_err(|_| "无法关闭旧快手取流窗口")?;
                tokio::time::timeout(Duration::from_secs(3), async {
                    while self.app()?.get_webview_window(WORKER).is_some() {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    Ok::<(), String>(())
                })
                .await
                .map_err(|_| "旧快手取流窗口尚未关闭，请稍后重试")??;
            }
        }
        if slot.is_none() {
            let key = kuaishou::session_key(&format!(
                "proxy:{}",
                proxy.as_ref().map(Url::as_str).unwrap_or("direct")
            ));
            let directory = self.directory.join(key);
            std::fs::create_dir_all(&directory).map_err(|_| "无法准备快手浏览器数据目录")?;
            let worker = self
                .create_window(
                    true,
                    directory.clone(),
                    proxy.clone(),
                    Url::parse("about:blank").expect("blank URL"),
                )
                .await?;
            *slot = Some(Session {
                proxy,
                directory,
                worker,
                last_room: None,
                recovery_room: None,
            });
        }
        slot.as_mut()
            .ok_or_else(|| kuaishou::BROWSER_UNAVAILABLE.into())
    }

    async fn clear_site_cookies(window: &WebviewWindow) -> Result<(), String> {
        for mut cookie in window
            .cookies()
            .map_err(|_| "无法读取快手网页登录状态")?
            .into_iter()
            .filter(is_site_cookie)
        {
            let domain = cookie
                .domain()
                .expect("site cookie domain")
                .trim_start_matches('.')
                .to_owned();
            // Cookie::domain() normalizes the readback too. Clear both native
            // scopes for this name/path instead of leaving a stale domain cookie.
            set_browser_cookie_domain(&mut cookie, &domain);
            window
                .delete_cookie(cookie.clone())
                .map_err(|_| "无法更新快手网页登录状态")?;
            set_browser_cookie_domain(&mut cookie, &format!(".{domain}"));
            window
                .delete_cookie(cookie)
                .map_err(|_| "无法更新快手网页登录状态")?;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if !window
                .cookies()
                .map_err(|_| "无法核对快手网页登录状态")?
                .iter()
                .any(is_site_cookie)
            {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("快手网页旧登录信息尚未清除，请关闭快手网页后重试".into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn synchronize_cookie(session: &Session, cookie: Option<&str>) -> Result<(), String> {
        let cookie = kuaishou::canonical_cookie_header(cookie.unwrap_or_default())?;
        let key = kuaishou::session_key(&format!("cookie:{cookie}"));
        let marker = session.directory.join("source-cookie.sha256");
        let previous = match std::fs::read_to_string(&marker) {
            Ok(value) => Some(value),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err("无法读取快手网页会话标识".into()),
        };
        // An unchanged saved input must never undo a website login or sign-out.
        if previous.as_deref() == Some(key.as_str()) {
            return Ok(());
        }
        let pairs = cookie
            .split(';')
            .map(str::trim)
            .filter(|pair| !pair.is_empty())
            .map(|pair| pair.split_once('=').ok_or("快手 Cookie 格式无效"))
            .collect::<Result<Vec<_>, _>>()?;
        Self::clear_site_cookies(&session.worker).await?;
        for &(name, value) in &pairs {
            let mut item = Cookie::new(name.to_owned(), value.to_owned());
            set_browser_cookie_domain(&mut item, ".kuaishou.com");
            item.set_path("/");
            item.set_secure(true);
            session
                .worker
                .set_cookie(item)
                .map_err(|_| "无法应用快手网页登录信息")?;
        }
        // Tauri's setter only acknowledges queueing. Read from the actual API
        // host before persisting the input marker or attempting a room request.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let saved = session
                .worker
                .cookies_for_url(Url::parse(LIVE).expect("live URL"))
                .map_err(|_| "无法核对快手网页登录信息")?;
            if pairs.iter().all(|&(name, value)| {
                saved.iter().any(|cookie| {
                    cookie.name() == name
                        && cookie.value() == value
                        && cookie.path() == Some("/")
                        && cookie
                            .domain()
                            .is_some_and(|domain| domain.trim_start_matches('.') == "kuaishou.com")
                })
            }) {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("快手网页登录信息未能生效，请关闭快手网页后重试".into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        std::fs::write(marker, key).map_err(|_| "无法保存快手网页会话标识")?;
        Ok(())
    }

    async fn evaluate(&self, window: &WebviewWindow, script: String) -> Result<String, String> {
        let (send, receive) = oneshot::channel();
        let send = std::sync::Mutex::new(Some(send));
        window
            .eval_with_callback(script, move |value| {
                if let Ok(mut send) = send.lock() {
                    if let Some(send) = send.take() {
                        let _ = send.send(value);
                    }
                }
            })
            .map_err(|_| "无法读取快手播放详情")?;
        tokio::select! { biased;
            _ = self.stop.cancelled() => Err(kuaishou::BROWSER_CANCELLED.into()),
            reply = tokio::time::timeout(Duration::from_secs(2), receive) =>
                reply.map_err(|_| kuaishou::BROWSER_TIMEOUT.to_owned())?
                    .map_err(|_| kuaishou::BROWSER_CANCELLED.into()),
        }
    }

    async fn read_json(&self, window: &WebviewWindow, url: Url) -> Result<BrowserResponse, String> {
        // Await the marker before navigating, including repeated requests to the same URL.
        // Only the new document may return data; the previous complete document is stale.
        let armed = self
            .evaluate(
                window,
                "(()=>{window.__streamcapOldDocument=true;return true})()".into(),
            )
            .await?;
        if armed.trim() != "true" {
            return Err(kuaishou::PAGE_UNAVAILABLE.into());
        }
        window
            .navigate(url.clone())
            .map_err(|_| "无法打开快手播放详情")?;
        let script = read_script(&url);
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let reply = self.evaluate(window, script.clone()).await?;
                if let Some(response) = decode_reply(&reply, &url)? {
                    return Ok(response);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .map_err(|_| kuaishou::BROWSER_TIMEOUT.to_owned())?
    }

    async fn resolve(&self, request: &BrowserRequest) -> Result<BrowserResponse, String> {
        if self.smoke {
            return Err("隔离验收不连接真实快手接口".into());
        }
        let id = kuaishou::principal_id(&request.room_url)?;
        let mut slot = self.session.lock().await;
        let session = self
            .ensure_session(&mut slot, proxy_url(request.proxy.as_deref())?)
            .await?;
        Self::synchronize_cookie(session, request.cookie.as_deref()).await?;
        session.last_room = Some(Url::parse(&request.room_url).map_err(|_| "快手房间地址无效")?);
        self.read_json(&session.worker, kuaishou::detail_url(&id)?)
            .await
    }

    pub async fn open(&self) -> Result<Value, String> {
        if self.smoke {
            return Err("隔离验收不打开真实快手网页".into());
        }
        if self.stop.is_cancelled() {
            return Err("应用正在退出".into());
        }
        let (proxy, cookie) = {
            let config = self.config.read().await;
            (
                streamcap_core::scheduler::recording_proxy(&config, Some("kuaishou"))?,
                config
                    .cookies_for_resolver()
                    .map_err(|_| "无法读取平台登录信息")?
                    .remove("kuaishou"),
            )
        };
        let waiting_room = self
            .store
            .all()
            .await
            .into_iter()
            .find(|record| {
                record.verification_required && kuaishou::principal_id(&record.url).is_ok()
            })
            .and_then(|record| Url::parse(&record.url).ok());
        let mut slot = self.session.lock().await;
        let session = self
            .ensure_session(&mut slot, proxy_url(proxy.as_deref())?)
            .await?;
        Self::synchronize_cookie(session, cookie.as_deref()).await?;
        let recovery_room = waiting_room
            .or_else(|| session.recovery_room.clone())
            .or_else(|| session.last_room.clone());
        let destination = recovery_room
            .clone()
            .unwrap_or_else(|| Url::parse(LIVE).expect("live URL"));
        let portal = if let Some(portal) = self.app()?.get_webview_window(PORTAL) {
            if session.recovery_room != recovery_room {
                portal
                    .navigate(destination.clone())
                    .map_err(|_| "无法打开待验证的快手直播间")?;
            }
            portal
        } else {
            self.create_window(
                false,
                session.directory.clone(),
                session.proxy.clone(),
                destination,
            )
            .await?
        };
        session.recovery_room = recovery_room;
        portal.show().map_err(|_| "无法显示快手网页")?;
        portal.set_focus().map_err(|_| "无法切换到快手网页")?;
        drop(slot);
        self.resolver.kuaishou_session_changed().await;
        Ok(json!({"opened":true}))
    }

    pub async fn retry(&self) -> Result<Value, String> {
        if self.smoke {
            return Err("隔离验收不恢复真实快手任务".into());
        }
        let room = self
            .session
            .lock()
            .await
            .as_ref()
            .and_then(|session| session.recovery_room.clone())
            .ok_or("请先打开需要验证的快手直播间")?;
        let restored = self
            .resolver
            .restore_kuaishou_access(&self.store, &self.config, room.as_str())
            .await?;
        Ok(json!({"restored":restored}))
    }

    pub async fn smoke_probe(&self, raw: &str) -> Result<Value, String> {
        if !self.smoke {
            return Err("仅隔离验收可调用".into());
        }
        let url = Url::parse(raw).map_err(|_| "隔离夹具地址无效")?;
        if !fixture(&url) {
            return Err("仅允许指定的本机快手夹具路径".into());
        }
        let mut slot = self.session.lock().await;
        let session = self.ensure_session(&mut slot, None).await?;
        let mut response = self.read_json(&session.worker, url).await?;
        // The loopback fixture is explicitly mapped to its synthetic room only in smoke mode.
        response.url = kuaishou::detail_url("fixture")?.to_string();
        match kuaishou::parse_browser_response(&response, "fixture", None) {
            Ok(info) => Ok(
                json!({"isLive":info.is_live,"anchorMatches":info.anchor_name=="Fixture","hasSource":!info.record_url.is_empty(),"accessState":""}),
            ),
            Err(error) => Ok(
                json!({"isLive":false,"hasSource":false,"accessState":kuaishou::access_state(&error),"error":error}),
            ),
        }
    }

    async fn fixture_cookie_matches(
        &self,
        window: &WebviewWindow,
        value: Option<&str>,
    ) -> Result<bool, String> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let cookies = window
                .cookies_for_url(Url::parse(LIVE).expect("live URL"))
                .map_err(|_| "隔离会话 Cookie 读取失败")?;
            let matches = match value {
                Some(value) => cookies
                    .iter()
                    .any(|cookie| cookie.name() == "streamcap_fixture" && cookie.value() == value),
                None => cookies
                    .iter()
                    .all(|cookie| cookie.name() != "streamcap_fixture"),
            };
            if matches || tokio::time::Instant::now() >= deadline {
                return Ok(matches);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub async fn smoke_shared_session(&self, raw: &str) -> Result<Value, String> {
        if !self.smoke {
            return Err("仅隔离验收可调用".into());
        }
        let url = Url::parse(raw).map_err(|_| "隔离夹具地址无效")?;
        if !fixture(&url) {
            return Err("仅允许指定的本机快手夹具路径".into());
        }
        let mut slot = self.session.lock().await;
        let session = self.ensure_session(&mut slot, None).await?;
        Self::synchronize_cookie(session, Some("streamcap_fixture=original")).await?;
        let portal = self
            .create_window(false, session.directory.clone(), None, url.clone())
            .await?;
        self.read_json(&portal, url).await?;
        let shared = self
            .fixture_cookie_matches(&portal, Some("original"))
            .await?;
        let mut updated = Cookie::new("streamcap_fixture", "updated-by-portal");
        set_browser_cookie_domain(&mut updated, ".kuaishou.com");
        updated.set_path("/");
        updated.set_secure(true);
        portal
            .set_cookie(updated)
            .map_err(|_| "隔离会话 Cookie 更新失败")?;
        Self::synchronize_cookie(session, Some("streamcap_fixture=original")).await?;
        let preserved = self
            .fixture_cookie_matches(&session.worker, Some("updated-by-portal"))
            .await?;
        Self::synchronize_cookie(session, Some("streamcap_fixture=replaced")).await?;
        let replaced = self
            .fixture_cookie_matches(&portal, Some("replaced"))
            .await?;
        Self::synchronize_cookie(session, None).await?;
        let cleared = self.fixture_cookie_matches(&portal, None).await?
            && self.fixture_cookie_matches(&session.worker, None).await?;
        self.evaluate(&portal, r#"(()=>{
          window.__streamcapIpcProbe='pending';
          const bridge=window.__TAURI_INTERNALS__;
          if(!bridge||typeof bridge.invoke!=='function'){window.__streamcapIpcProbe='unavailable';return true;}
          try{Promise.resolve(bridge.invoke('desktop_ready')).then(
            ()=>window.__streamcapIpcProbe='allowed',()=>window.__streamcapIpcProbe='blocked');}
          catch{window.__streamcapIpcProbe='blocked';}return true;
        })()"#.into()).await?;
        let mut ipc = String::new();
        for _ in 0..20 {
            let reply = self
                .evaluate(&portal, "window.__streamcapIpcProbe".into())
                .await?;
            ipc = serde_json::from_str(&reply).map_err(|_| "隔离 IPC 验证响应无效")?;
            if ipc != "pending" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(
            json!({"sharedCookie":shared,"websiteChangePreserved":preserved,
            "sourceCookieReplaced":replaced,"sourceCookieCleared":cleared,
            "remoteIpcBlocked":matches!(ipc.as_str(),"blocked"|"unavailable")}),
        )
    }

    pub async fn close(&self) {
        if let Ok(app) = self.app() {
            if let Some(portal) = app.get_webview_window(PORTAL) {
                let _ = portal.destroy();
            }
        }
        if let Some(session) = self.session.lock().await.take() {
            let _ = session.worker.destroy();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn webview2_cookie_domains_preserve_host_and_subdomain_scopes() {
        let mut cookie = Cookie::new("fixture", "synthetic");
        set_browser_cookie_domain(&mut cookie, ".kuaishou.com");
        assert_eq!(cookie.domain(), Some(".kuaishou.com"));
        assert!(is_site_cookie(&cookie));
        set_browser_cookie_domain(&mut cookie, "live.kuaishou.com");
        assert_eq!(cookie.domain(), Some("live.kuaishou.com"));
        assert!(is_site_cookie(&cookie));
        set_browser_cookie_domain(&mut cookie, ".kuaishou.com.invalid");
        assert!(!is_site_cookie(&cookie));
    }

    #[test]
    fn portal_and_worker_navigation_have_distinct_boundaries() {
        let room = Url::parse("https://live.kuaishou.com/u/fixture").unwrap();
        let login = Url::parse("https://id.kuaishou.com/pass/login").unwrap();
        let detail = kuaishou::detail_url("fixture").unwrap();
        assert!(navigation_allowed(&room, false, false));
        assert!(navigation_allowed(&login, false, false));
        assert!(!navigation_allowed(&room, true, false));
        assert!(!navigation_allowed(&login, true, false));
        assert!(navigation_allowed(&detail, true, false));
        for raw in [
            "http://live.kuaishou.com/",
            "https://kuaishou.com.evil.invalid/",
            "https://user:pass@live.kuaishou.com/",
            "file:///C:/config",
            "https://live.kuaishou.com:444/",
        ] {
            let url = Url::parse(raw).unwrap();
            assert!(!navigation_allowed(&url, false, false));
            assert!(!navigation_allowed(&url, true, false));
        }
    }

    #[test]
    fn smoke_navigation_is_confined_to_its_loopback_fixture() {
        let fixture = Url::parse("http://127.0.0.1:32123/kuaishou-fixture?case=live").unwrap();
        assert!(navigation_allowed(&fixture, true, true));
        assert!(!navigation_allowed(&fixture, true, false));
        assert!(!navigation_allowed(
            &kuaishou::detail_url("fixture").unwrap(),
            true,
            true
        ));
        for raw in [
            "http://localhost:32123/kuaishou-fixture",
            "http://127.0.0.1/kuaishou-fixture",
            "http://127.0.0.1:32123/private",
            "http://127.0.0.1:32123/kuaishou-fixture#other",
        ] {
            assert!(!navigation_allowed(&Url::parse(raw).unwrap(), false, true));
        }
    }

    #[test]
    fn unsupported_browser_proxies_do_not_fall_back_to_direct_access() {
        assert!(proxy_url(None).unwrap().is_none());
        assert_eq!(
            proxy_url(Some("socks5h://127.0.0.1:1080"))
                .unwrap()
                .unwrap()
                .scheme(),
            "socks5"
        );
        assert!(proxy_url(Some("http://127.0.0.1:1080")).unwrap().is_some());
        for value in [
            "",
            "   ",
            "https://127.0.0.1:1080",
            "http://user:secret@127.0.0.1:1080",
            "socks5://127.0.0.1",
            "http://127.0.0.1:1080/private",
            "http://127.0.0.1:1080/?key=secret",
            "invalid",
        ] {
            let error = proxy_url(Some(value)).unwrap_err();
            assert!(!error.contains("secret"));
        }
    }

    #[test]
    fn callback_decoder_rejects_stale_malformed_and_oversized_replies() {
        let url = kuaishou::detail_url("fixture").unwrap();
        assert!(decode_reply("null", &url).unwrap().is_none());
        let valid = json!({"url":url.as_str(),"httpStatus":200,"payload":{"data":{"result":671}}});
        assert!(decode_reply(&valid.to_string(), &url).unwrap().is_some());
        let mut stale = valid;
        stale["url"] = json!(kuaishou::detail_url("old").unwrap().as_str());
        assert!(decode_reply(&stale.to_string(), &url).is_err());
        for value in ["undefined", "{}", "{\"error\":\"oversize\"}"] {
            assert!(decode_reply(value, &url).is_err());
        }
        assert!(decode_reply(&" ".repeat(MAX_REPLY + 65537), &url).is_err());
        let script = read_script(&url);
        assert!(script.contains("location.href!=="));
        assert!(!script.contains("document.cookie"));
    }
}
