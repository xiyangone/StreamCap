//! Human-operated official-site verification. Remote pages never receive native IPC permissions.
use serde::Serialize;
use serde_json::Value;
use std::{
    sync::{atomic::Ordering, Arc, Mutex},
    time::Duration,
};
use streamcap_core::{api::ApiState, model::Recording, platforms::kuaishou};
use tauri::{
    menu::{Menu, MenuItem},
    webview::{Cookie, NewWindowResponse, PageLoadEvent},
    AppHandle, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

pub const LABEL: &str = "kuaishou-verification";

#[derive(Default)]
struct StableChallenge {
    first: Option<std::time::Instant>,
    last: Option<std::time::Instant>,
    samples: u32,
}
impl StableChallenge {
    fn observe(&mut self, now: std::time::Instant, visible: bool) -> bool {
        if !visible {
            *self = Self::default();
            return false;
        }
        if self
            .last
            .is_some_and(|last| now.duration_since(last) > Duration::from_millis(2500))
        {
            *self = Self::default();
        }
        let first = *self.first.get_or_insert(now);
        self.last = Some(now);
        self.samples = self.samples.saturating_add(1);
        self.samples >= 3 && now.duration_since(first) >= Duration::from_secs(3)
    }
}

#[derive(Clone)]
struct Session {
    id: u64,
    record: Recording,
    target: Url,
    original_cookie: Option<String>,
    auto_running: Arc<std::sync::atomic::AtomicBool>,
    surfaced: Arc<std::sync::atomic::AtomicBool>,
    stop: CancellationToken,
    busy: bool,
    _page_permit: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
}
#[derive(Default)]
struct State {
    active: Option<Session>,
    dismissed: bool,
    next_id: u64,
}
pub struct Verification {
    core: ApiState,
    state: Mutex<State>,
    stop: CancellationToken,
    tasks: TaskTracker,
    smoke: bool,
    opening: tokio::sync::Mutex<()>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    active: bool,
    busy: bool,
    rec_id: Option<String>,
    active_workers: usize,
    visible: bool,
}
impl Verification {
    pub fn new(core: ApiState, smoke: bool) -> Self {
        Self {
            core,
            state: Mutex::new(State::default()),
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
            smoke,
            opening: tokio::sync::Mutex::new(()),
        }
    }
    pub fn status(&self) -> Status {
        let state = self.state.lock().expect("verification state");
        Status {
            active: state.active.is_some(),
            busy: state.active.as_ref().is_some_and(|s| s.busy),
            rec_id: state.active.as_ref().map(|s| s.record.rec_id.clone()),
            active_workers: self.tasks.len(),
            visible: false,
        }
    }
}
fn allowed_navigation(url: &Url, smoke_origin: Option<&str>) -> bool {
    if url.as_str() == "about:blank" {
        return true;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    if let Some(origin) = smoke_origin {
        return url.origin().ascii_serialization() == origin;
    }
    url.scheme() == "https"
        && matches!(
            url.host_str(),
            Some(
                "live.kuaishou.com"
                    | "www.kuaishou.com"
                    | "id.kuaishou.com"
                    | "passport.kuaishou.com"
            )
        )
}
fn room_url(value: &str) -> Result<Url, String> {
    let mut url = Url::parse(value).map_err(|_| "快手直播间地址无效")?;
    let id = url.path().strip_prefix("/u/").unwrap_or_default();
    if url.scheme() != "https"
        || url.host_str() != Some("live.kuaishou.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || id.is_empty()
        || id.contains('/')
        || url.port().is_some()
    {
        return Err("验证窗口仅接受快手直播间 HTTPS 地址".into());
    }
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}
fn fixture_target(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| "隔离验证地址无效")?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none()
        || !url.path().starts_with("/kuaishou-fixture/")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("隔离验证只允许显式本机夹具地址".into());
    }
    Ok(url)
}
fn spawn(app: &AppHandle, work: impl std::future::Future<Output = ()> + Send + 'static) {
    let tasks = app.state::<Verification>().tasks.clone();
    tauri::async_runtime::spawn(tasks.track_future(work));
}
pub fn automatic(app: &AppHandle, rec_id: String) {
    let handle = app.clone();
    spawn(app, async move {
        if let Err(error) = open(&handle, &rec_id, false, None).await {
            log::debug!("快手自动页面检查未启动: {error}");
        }
    });
}
pub async fn open(
    app: &AppHandle,
    rec_id: &str,
    manual: bool,
    fixture: Option<&str>,
) -> Result<(), String> {
    let tasks = app.state::<Verification>().tasks.clone();
    tasks
        .track_future(open_inner(app, rec_id, manual, fixture))
        .await
}
async fn open_inner(
    app: &AppHandle,
    rec_id: &str,
    manual: bool,
    fixture: Option<&str>,
) -> Result<(), String> {
    let owner = app.state::<Verification>();
    let _opening = owner.opening.lock().await;
    if owner.stop.is_cancelled() {
        return Err("应用正在退出".into());
    }
    if fixture.is_some() && !owner.smoke {
        return Err("仅隔离验收可使用本机夹具".into());
    }
    if owner.smoke && fixture.is_none() {
        return Err("隔离验收不访问真实快手网站".into());
    }
    let record = owner.core.store.get(rec_id).await.ok_or("验证任务不存在")?;
    if record.platform_key.as_deref() != Some("kuaishou") {
        return Err("此任务不支持快手验证".into());
    }
    let official = room_url(&record.url)?;
    let target = fixture.map(fixture_target).transpose()?.unwrap_or(official);
    let cookie = owner
        .core
        .config
        .read()
        .await
        .cookies_for_resolver()
        .map_err(|_| "无法读取快手会话")?
        .get("kuaishou")
        .cloned();
    {
        let state = owner.state.lock().map_err(|_| "验证状态异常")?;
        if let Some(active) = &state.active {
            if active.record.rec_id != record.rec_id {
                return Err("另一个快手页面检查正在进行，未打开其他房间".into());
            }
            if manual {
                if let Some(session) = &state.active {
                    session.surfaced.store(true, Ordering::SeqCst);
                }
            }
            drop(state);
            if manual {
                if let Some(window) = app.get_webview_window(LABEL) {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            return Ok(());
        }
        if owner.smoke && state.dismissed && !manual {
            return Ok(());
        }
    }
    let permit = if owner.smoke {
        None
    } else {
        let interval = owner
            .core
            .config
            .read()
            .await
            .get_i64("loop_time_seconds", 4500)
            .max(1) as u64;
        match owner
            .core
            .resolver
            .kuaishou_begin_page_check(cookie.as_deref(), Duration::from_secs(interval))
            .await
        {
            Ok(permit) => Some(Arc::new(permit)),
            Err(error) => {
                owner
                    .core
                    .scheduler
                    .report_kuaishou_access(&record, &error)
                    .await;
                return Err(error);
            }
        }
    };
    let session = {
        let mut state = owner.state.lock().map_err(|_| "验证状态异常")?;
        state.next_id = state.next_id.wrapping_add(1);
        let session = Session {
            id: state.next_id,
            record,
            target,
            original_cookie: cookie,
            auto_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            surfaced: Arc::new(std::sync::atomic::AtomicBool::new(manual)),
            stop: owner.stop.child_token(),
            busy: false,
            _page_permit: permit,
        };
        state.active = Some(session.clone());
        state.dismissed = false;
        session
    };
    let result = create_window(app, &session, owner.smoke).await;
    if let Err(error) = result {
        dispose(app, session.id, true);
        return Err(error);
    }
    log::info!("快手页面检查已开始 visible={manual}");
    Ok(())
}
async fn create_window(app: &AppHandle, session: &Session, smoke: bool) -> Result<(), String> {
    let complete = MenuItem::with_id(app, "kuaishou-complete", "检查当前页面", true, None::<&str>)
        .map_err(|_| "无法创建验证操作")?;
    let cancel = MenuItem::with_id(app, "kuaishou-cancel", "取消验证", true, None::<&str>)
        .map_err(|_| "无法创建验证操作")?;
    let menu = Menu::with_items(app, &[&complete, &cancel]).map_err(|_| "无法创建验证菜单")?;
    let smoke_origin = smoke.then(|| session.target.origin().ascii_serialization());
    let navigation_origin = smoke_origin.clone();
    let automatic_target = session.target.clone();
    let mut builder = WebviewWindowBuilder::new(
        app,
        LABEL,
        WebviewUrl::External(Url::parse("about:blank").expect("static URL")),
    )
    .title("快手验证 · 页面正常后会自动恢复，也可点击顶部“检查当前页面”")
    .inner_size(1060.0, 760.0)
    .min_inner_size(720.0, 520.0)
    .visible(false)
    .data_directory(
        app.state::<Verification>()
            .core
            .workspace
            .user_data_dir
            .join("webview-kuaishou"),
    )
    .incognito(true)
    .menu(menu)
    .on_navigation(move |url| allowed_navigation(url, navigation_origin.as_deref()))
    .on_page_load(move |window, payload| {
        if matches!(payload.event(), PageLoadEvent::Finished)
            && window.url().ok().as_ref() == Some(&automatic_target)
        {
            schedule_automatic_check(window.app_handle().clone(), window.label().to_owned());
        }
    })
    .on_menu_event(|window, event| match event.id.as_ref() {
        "kuaishou-complete" => {
            if let Err(error) = begin_complete(window.app_handle()) {
                window
                    .app_handle()
                    .state::<Verification>()
                    .core
                    .store
                    .snack(error);
            }
        }
        "kuaishou-cancel" => cancel_window(window.app_handle()),
        _ => {}
    });
    let navigation_app = app.clone();
    builder = builder.on_new_window(move |url, _| {
        if allowed_navigation(&url, smoke_origin.as_deref()) {
            if let Some(window) = navigation_app.get_webview_window(LABEL) {
                let _ = window.navigate(url);
            }
        }
        NewWindowResponse::Deny
    });
    let proxy = {
        let owner = app.state::<Verification>();
        let config = owner.core.config.read().await;
        streamcap_core::scheduler::recording_proxy(&config, Some("kuaishou"))?
    };
    if let Some(proxy) = proxy {
        let proxy = Url::parse(&proxy).map_err(|_| "验证代理地址无效")?;
        if !proxy.username().is_empty() || proxy.password().is_some() {
            return Err("验证窗口不支持带账号密码的代理，请先配置无凭证的本机代理".into());
        }
        if !matches!(proxy.scheme(), "http" | "socks5") {
            return Err("系统验证窗口仅支持 HTTP 或 SOCKS5 代理，未改为直连".into());
        }
        builder = builder.proxy_url(proxy);
    } else if !smoke {
        builder = builder.additional_browser_args("--no-proxy-server");
    }
    if session.stop.is_cancelled() {
        return Err("验证已取消".into());
    }
    let window = builder.build().map_err(|_| "无法打开快手验证窗口")?;
    // Keep credentials out of page scripts and IPC. Reconstruct only this platform's cookies.
    let cookie =
        kuaishou::canonical_cookie_header(session.original_cookie.as_deref().unwrap_or_default())?;
    let cookie_window = window.clone();
    let host = if smoke {
        session.target.host_str().unwrap_or_default().to_owned()
    } else {
        ".kuaishou.com".into()
    };
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        for pair in cookie.split(';').map(str::trim) {
            let Some((name, value)) = pair.split_once('=') else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            cookie_window
                .set_cookie(
                    Cookie::build((name.to_owned(), value.to_owned()))
                        .domain(host.clone())
                        .path("/")
                        .secure(!smoke)
                        .build(),
                )
                .map_err(|_| "无法载入快手验证会话")?;
        }
        Ok(())
    })
    .await
    .map_err(|_| "验证会话载入已取消")??;
    if session.stop.is_cancelled() {
        let _ = window.destroy();
        return Err("验证已取消".into());
    }
    window
        .navigate(session.target.clone())
        .map_err(|_| "无法打开快手网页")?;
    if session.surfaced.load(Ordering::SeqCst) {
        window.show().map_err(|_| "无法显示验证窗口")?;
        let _ = window.set_focus();
    }
    schedule_automatic_check(app.clone(), LABEL.into());
    Ok(())
}
pub fn begin_complete(app: &AppHandle) -> Result<(), String> {
    let owner = app.state::<Verification>();
    if owner.stop.is_cancelled() {
        return Err("应用正在退出".into());
    }
    let session = {
        let mut state = owner.state.lock().map_err(|_| "验证状态异常")?;
        let session = state.active.as_mut().ok_or("没有待完成的验证")?;
        if session.busy {
            return Err("正在核实验证结果，请勿重复点击".into());
        }
        session.busy = true;
        session.clone()
    };
    let handle = app.clone();
    spawn(app, async move {
        let result = tokio::select! { biased; _=session.stop.cancelled()=>Err("验证已取消".into()), result=complete(&handle,&session)=>result };
        match result {
            Ok(message) => {
                handle.state::<Verification>().core.store.snack(message);
                dispose(&handle, session.id, false);
            }
            Err(error) => {
                if terminal_page_problem(&error) {
                    dispose(&handle, session.id, true);
                    return;
                }
                let owner = handle.state::<Verification>();
                let mut state = owner.state.lock().expect("verification state");
                if let Some(current) = state
                    .active
                    .as_mut()
                    .filter(|current| current.id == session.id)
                {
                    current.busy = false;
                    drop(state);
                    log::info!("快手页面手动检查未完成: {error}");
                    owner.core.store.snack(error.clone());
                    if let Some(window) = handle.get_webview_window(LABEL) {
                        let _ = window.set_title(&format!("快手页面检查 · {error}"));
                    }
                }
            }
        }
    });
    Ok(())
}
fn schedule_automatic_check(app: AppHandle, label: String) {
    if label != LABEL {
        return;
    }
    let session = {
        let owner = app.state::<Verification>();
        let state = owner.state.lock().expect("verification state");
        let Some(session) = state.active.as_ref() else {
            return;
        };
        if session.auto_running.swap(true, Ordering::SeqCst) {
            return;
        }
        session.clone()
    };
    let handle = app.clone();
    spawn(&app, async move {
        let mut last_error = None;
        let started = std::time::Instant::now();
        let mut challenge = StableChallenge::default();
        let mut challenge_reported = false;
        loop {
            tokio::select! {
                biased;
                _ = session.stop.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_millis(1000)) => {}
            }
            // Manual checks and this local DOM observer share one in-flight slot.
            // Login can complete without navigation, so keep observing until close.
            {
                let owner = handle.state::<Verification>();
                let mut state = owner.state.lock().expect("verification state");
                let Some(current) = state.active.as_mut().filter(|s| s.id == session.id) else {
                    break;
                };
                if current.busy {
                    challenge.observe(std::time::Instant::now(), false);
                    continue;
                }
                current.busy = true;
            }
            let result = tokio::select! {
                biased;
                _ = session.stop.cancelled() => break,
                result = evaluate_current_page(&handle, &session) => result
            };
            if !matches!(&result, Ok(Some(_))) {
                let owner = handle.state::<Verification>();
                let mut state = owner.state.lock().expect("verification state");
                if let Some(current) = state.active.as_mut().filter(|s| s.id == session.id) {
                    current.busy = false;
                }
            }
            match result {
                Ok(Some(message)) => {
                    handle.state::<Verification>().core.store.snack(message);
                    dispose(&handle, session.id, false);
                    return;
                }
                Ok(None) => {
                    if challenge.observe(std::time::Instant::now(), true)
                        && !session.stop.is_cancelled()
                    {
                        if let Some(window) = handle.get_webview_window(LABEL) {
                            let _ = window.set_title(
                                "快手验证 · 仅页面出现滑块时需要操作；完成后点“检查当前页面”",
                            );
                            if !challenge_reported {
                                handle
                                    .state::<Verification>()
                                    .core
                                    .scheduler
                                    .report_kuaishou_access(
                                        &session.record,
                                        kuaishou::VERIFICATION_REQUIRED,
                                    )
                                    .await;
                                challenge_reported = true;
                            }
                            if !session.stop.is_cancelled()
                                && !session.surfaced.swap(true, Ordering::SeqCst)
                            {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                    }
                    last_error = None;
                }
                Err(error) => {
                    challenge.observe(std::time::Instant::now(), false);
                    challenge_reported = false;
                    if terminal_page_problem(&error) {
                        log::info!("快手页面检查已停止: {error}");
                        dispose(&handle, session.id, true);
                        return;
                    }
                    if last_error.as_ref() != Some(&error) {
                        log::info!("快手页面自动检查未完成: {error}");
                        if let Some(window) = handle.get_webview_window(LABEL) {
                            let _ = window.set_title(&format!("快手页面检查 · {error}"));
                        }
                        last_error = Some(error);
                    }
                }
            }
            if !session.surfaced.load(Ordering::SeqCst)
                && started.elapsed() >= Duration::from_secs(30)
            {
                handle
                    .state::<Verification>()
                    .core
                    .store
                    .snack("快手页面暂不可读，自动检查已停止；可在任务中手动重试");
                handle
                    .state::<Verification>()
                    .core
                    .scheduler
                    .report_kuaishou_access(&session.record, kuaishou::PAGE_UNAVAILABLE)
                    .await;
                dispose(&handle, session.id, true);
                return;
            }
        }
        session.auto_running.store(false, Ordering::SeqCst);
    });
}
async fn snapshot(window: &WebviewWindow) -> Result<Value, String> {
    let (send, receive) = tokio::sync::oneshot::channel();
    let send = Mutex::new(Some(send));
    window
        .eval_with_callback(
            r#"(() => {
                const pinia=document.querySelector('#app')?.__vue_app__?.config?.globalProperties?.$pinia;
                const state=pinia?.state?.value || window.__INITIAL_STATE__;
                const index=state?.liveroom?.activeIndex ?? 0;
                const raw=Array.isArray(state?.liveroom?.playList) ? state.liveroom.playList[index] : state?.room;
                const stream=raw?.liveStream;
                // Project only target-room fields; never copy login or websocket tokens.
                const room=raw ? {
                    author:{id:raw.author?.id,name:raw.author?.name},
                    isLiving:raw.isLiving,
                    errorType:{type:raw.errorType?.type,title:raw.errorType?.title,content:raw.errorType?.content},
                    liveStream:stream ? {
                        caption:stream.caption,title:stream.title,living:stream.living,
                        user:{user_name:stream.user?.user_name},
                        playUrls:stream.playUrls,multiResolutionPlayUrls:stream.multiResolutionPlayUrls,
                        multiResolutionHlsPlayUrls:stream.multiResolutionHlsPlayUrls
                    } : null
                } : null;
                const visible=el=>{
                    const r=el.getBoundingClientRect();
                    if(r.width<=20||r.height<=20||r.bottom<=0||r.right<=0||r.top>=innerHeight||r.left>=innerWidth)return false;
                    for(let p=el;p;p=p.parentElement){const s=getComputedStyle(p);if(s.display==='none'||s.visibility==='hidden'||Number(s.opacity||1)===0)return false;}
                    return true;
                };
                const player=document.querySelector('.swiper-slide-active .player');
                return JSON.stringify({
                    href:location.href,state:{room},
                    text:(player?.innerText||'').slice(0,262144),
                    loginPromptVisible:Array.from(document.querySelectorAll('[role="dialog"],[class*="login" i]')).some(el=>visible(el)&&/快手APP登录/.test(el.innerText||'')&&/手机号登录/.test(el.innerText||'')),
                    challengeVisible:Array.from(document.querySelectorAll('iframe[src*="captcha" i],iframe[src*="verify" i],iframe[title*="验证"],[class*="captcha" i],[id*="captcha" i],[class*="geetest" i],[id*="geetest" i],[class*="slider-verify" i],[id*="slider-verify" i]')).some(visible)
                });
            })()"#,
            move |value| {
                if let Some(send) = send.lock().ok().and_then(|mut slot| slot.take()) {
                    let _ = send.send(value);
                }
            },
        )
        .map_err(|_| "无法读取验证页面")?;
    let result = tokio::time::timeout(Duration::from_secs(5), receive)
        .await
        .map_err(|_| "读取验证页面超时")?
        .map_err(|_| "验证窗口已关闭")?;
    if result.len() > 12 * 1024 * 1024 {
        return Err("验证页面超过大小限制".into());
    }
    let inner: String = serde_json::from_str(&result).map_err(|_| "验证页面尚未就绪")?;
    serde_json::from_str(&inner).map_err(|_| "验证页面数据无效".into())
}

async fn complete(app: &AppHandle, session: &Session) -> Result<String, String> {
    let window = app.get_webview_window(LABEL).ok_or("验证窗口已关闭")?;
    window
        .set_title("快手验证 · 正在检查当前页面")
        .map_err(|_| "验证窗口已关闭")?;
    evaluate_current_page(app, session)
        .await?
        .ok_or_else(|| "页面仍有可见验证码，请在该页面手动完成".into())
}

async fn evaluate_current_page(
    app: &AppHandle,
    session: &Session,
) -> Result<Option<String>, String> {
    let window = app.get_webview_window(LABEL).ok_or("验证窗口已关闭")?;
    let data = snapshot(&window).await?;
    let href = data["href"]
        .as_str()
        .and_then(|s| Url::parse(s).ok())
        .ok_or("无法确认验证页面来源")?;
    if href != session.target {
        return Err("当前页面不是目标直播间，未应用验证结果".into());
    }
    let official = room_url(&session.record.url)?;
    let expected_id = official
        .path()
        .strip_prefix("/u/")
        .ok_or("目标主播地址无效")?;
    let visible_text = data["text"].as_str().unwrap_or_default();
    let challenge_visible = data["challengeVisible"].as_bool().unwrap_or(false);
    let info = match kuaishou::evaluate_verification_page(
        &data["state"],
        expected_id,
        visible_text,
        challenge_visible,
        session.record.quality.as_deref(),
    ) {
        kuaishou::VerificationPage::Challenge => return Ok(None),
        kuaishou::VerificationPage::RateLimited => {
            report_page_problem(app, session, kuaishou::RATE_LIMITED).await?;
            return Err(kuaishou::RATE_LIMITED.into());
        }
        kuaishou::VerificationPage::LoginRequired => {
            report_page_problem(app, session, kuaishou::LOGIN_REQUIRED).await?;
            return Err(kuaishou::LOGIN_REQUIRED.into());
        }
        kuaishou::VerificationPage::Room(info) => *info,
        kuaishou::VerificationPage::Unknown(_)
            if data["loginPromptVisible"].as_bool() == Some(true) =>
        {
            report_page_problem(app, session, kuaishou::LOGIN_PROMPT).await?;
            return Err(kuaishou::LOGIN_PROMPT.into());
        }
        kuaishou::VerificationPage::Unknown(error) => return Err(error),
    };
    let read_window = window.clone();
    let url = session.target.clone();
    let cookies = tauri::async_runtime::spawn_blocking(move || read_window.cookies_for_url(url))
        .await
        .map_err(|_| "读取快手会话已取消")?
        .map_err(|_| "无法读取快手会话")?;
    let header = cookie_header(&cookies)?;
    let expected_id = cookie_value(
        session.original_cookie.as_deref().unwrap_or_default(),
        "userId",
    );
    let actual_id = cookie_value(&header, "userId");
    let persist_session = can_save_page_session(expected_id.as_deref(), actual_id.as_deref())?;
    if session.stop.is_cancelled() {
        return Err("验证已取消".into());
    }
    let owner = app.state::<Verification>();
    let current = owner
        .core
        .store
        .get(&session.record.rec_id)
        .await
        .ok_or("验证任务已移除")?;
    if current.url != session.record.url
        || current.quality != session.record.quality
        || current.platform_key.as_deref() != Some("kuaishou")
    {
        return Err("任务已更新，验证结果未应用".into());
    }
    let saved_session = {
        let mut config = owner.core.config.write().await;
        let saved = config
            .cookies_for_resolver()
            .map_err(|_| "无法核对已保存的快手会话")?
            .get("kuaishou")
            .cloned();
        if saved != session.original_cookie {
            return Err("登录信息已在其他位置更新，旧验证结果未覆盖它".into());
        }
        if session.stop.is_cancelled() {
            return Err("验证已取消".into());
        }
        if persist_session && saved.as_deref() != Some(header.as_str()) {
            config
                .update_cookies(serde_json::Map::from_iter([(
                    "kuaishou".into(),
                    Value::String(header.clone()),
                )]))
                .map_err(|_| "快手验证成功，但会话保存失败")?;
        }
        if persist_session {
            Some(header.clone())
        } else {
            saved
        }
    };
    owner
        .core
        .resolver
        .kuaishou_page_session(saved_session.as_deref(), &header)
        .await?;
    let result = owner
        .core
        .scheduler
        .accept_verified(&session.record, &info)
        .await;
    if result.is_ok() {
        let pending = owner
            .core
            .store
            .all()
            .await
            .into_iter()
            .filter(|record| {
                record.rec_id != session.record.rec_id
                    && record.platform_key.as_deref() == Some("kuaishou")
                    && (record.verification_required || record.access_state == "pageCheck")
                    && record.monitor_status
                    && !record.is_recording
            })
            .map(|record| record.rec_id);
        owner.core.scheduler.request_monitoring(pending);
    }
    log::info!("快手目标房间验证完成");
    Ok(Some(match result {
        Ok(outcome) => format!("快手页面已恢复；{}", outcome.message()),
        Err(_) => "快手页面已恢复；当前任务未恢复，请查看任务状态".into(),
    }))
}
fn terminal_page_problem(error: &str) -> bool {
    matches!(
        error,
        kuaishou::RATE_LIMITED | kuaishou::LOGIN_REQUIRED | kuaishou::LOGIN_PROMPT
    )
}
async fn report_page_problem(
    app: &AppHandle,
    session: &Session,
    error: &str,
) -> Result<(), String> {
    let owner = app.state::<Verification>();
    let config = owner.core.config.read().await;
    let saved = config
        .cookies_for_resolver()
        .map_err(|_| "无法核对快手会话")?
        .get("kuaishou")
        .cloned();
    if session.stop.is_cancelled() || saved != session.original_cookie {
        return Err("旧页面检查已失效，未应用结果".into());
    }
    owner.core.resolver.kuaishou_page_problem(error).await;
    owner
        .core
        .scheduler
        .report_kuaishou_access(&session.record, error)
        .await;
    Ok(())
}
fn can_save_page_session(expected: Option<&str>, actual: Option<&str>) -> Result<bool, String> {
    match (expected, actual) {
        (Some(expected), Some(actual)) if expected != actual => {
            Err("验证账号与原快手账号不一致，登录信息未替换".into())
        }
        (Some(_), None) => Ok(false),
        _ => Ok(true),
    }
}
fn cookie_value(header: &str, key: &str) -> Option<String> {
    header
        .split(';')
        .map(str::trim)
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| *name == key)
        .map(|(_, value)| value.to_owned())
}
fn cookie_header(cookies: &[Cookie<'static>]) -> Result<String, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i128;
    let mut cookies = cookies
        .iter()
        .filter(|cookie| {
            cookie
                .expires_datetime()
                .is_none_or(|expires| i128::from(expires.unix_timestamp()) > now)
        })
        .collect::<Vec<_>>();
    cookies.sort_by_key(|cookie| {
        std::cmp::Reverse((
            cookie.path().unwrap_or("/").len(),
            cookie
                .domain()
                .unwrap_or_default()
                .trim_start_matches('.')
                .len(),
        ))
    });
    let header = cookies
        .iter()
        .map(|cookie| format!("{}={}", cookie.name(), cookie.value()))
        .collect::<Vec<_>>()
        .join("; ");
    if header.is_empty() || header.len() > 65536 || header.contains(['\r', '\n']) {
        return Err("验证未返回有效快手会话，登录信息未保存".into());
    }
    kuaishou::canonical_cookie_header(&header)
}
fn dispose(app: &AppHandle, id: u64, dismissed: bool) {
    let owner = app.state::<Verification>();
    let session = {
        let mut state = owner.state.lock().expect("verification state");
        if state.active.as_ref().is_none_or(|s| s.id != id) {
            return;
        }
        state.dismissed = dismissed;
        state.active.take()
    };
    if let Some(session) = session {
        session.stop.cancel();
    }
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.destroy();
    }
}
pub fn cancel_window(app: &AppHandle) {
    let id = app
        .state::<Verification>()
        .state
        .lock()
        .expect("verification state")
        .active
        .as_ref()
        .map(|s| s.id);
    if let Some(id) = id {
        dispose(app, id, true);
    }
}
pub async fn session_changed(app: &AppHandle) {
    let owner = app.state::<Verification>();
    let previous = owner
        .state
        .lock()
        .expect("verification state")
        .active
        .clone();
    let saved = owner
        .core
        .config
        .read()
        .await
        .cookies_for_resolver()
        .ok()
        .and_then(|cookies| cookies.get("kuaishou").cloned());
    if let Some(session) = previous {
        if saved != session.original_cookie {
            dispose(app, session.id, true);
        }
    }
    owner.state.lock().expect("verification state").dismissed = false;
}
pub fn begin_shutdown(app: &AppHandle) {
    let owner = app.state::<Verification>();
    owner.stop.cancel();
    owner.tasks.close();
    cancel_window(app);
}
pub async fn wait_shutdown(app: &AppHandle) {
    app.state::<Verification>().tasks.wait().await;
}
pub fn status(app: &AppHandle) -> Status {
    let mut status = app.state::<Verification>().status();
    status.visible = app
        .get_webview_window(LABEL)
        .is_some_and(|window| window.is_visible().unwrap_or(false));
    status
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn automatic_challenge_requires_stable_consecutive_observations() {
        let now = std::time::Instant::now();
        let mut challenge = StableChallenge::default();
        assert!(!challenge.observe(now, true));
        assert!(!challenge.observe(now + Duration::from_secs(1), true));
        assert!(!challenge.observe(now + Duration::from_secs(2), false));
        // Disappearance or an unreadable snapshot breaks the sequence.
        assert!(!challenge.observe(now + Duration::from_secs(3), true));
        assert!(!challenge.observe(now + Duration::from_secs(4), true));
        assert!(!challenge.observe(now + Duration::from_secs(5), true));
        assert!(challenge.observe(now + Duration::from_secs(6), true));
        // A stalled observer is not proof that the challenge stayed visible.
        assert!(!challenge.observe(now + Duration::from_secs(10), true));
        assert!(!challenge.observe(now + Duration::from_secs(11), true));
    }
    #[test]
    fn public_room_readability_never_overwrites_saved_login_or_requires_an_unneeded_login() {
        assert!(!can_save_page_session(Some("original"), None).unwrap());
        assert!(can_save_page_session(Some("original"), Some("original")).unwrap());
        assert!(can_save_page_session(Some("original"), Some("other")).is_err());
        assert!(can_save_page_session(None, None).unwrap());
    }
    #[test]
    fn navigation_never_grants_external_or_insecure_destinations() {
        for url in [
            "http://live.kuaishou.com/u/a",
            "https://kuaishou.com.evil.invalid/",
            "https://evil.invalid/",
            "https://user:secret@live.kuaishou.com/u/a",
            "file:///C:/private",
            "tauri://localhost/",
        ] {
            assert!(!allowed_navigation(&Url::parse(url).unwrap(), None));
        }
        assert!(allowed_navigation(
            &Url::parse("https://id.kuaishou.com/").unwrap(),
            None
        ));
        assert!(room_url("https://live.kuaishou.com/u/fixture").is_ok());
        assert!(room_url("https://live.kuaishou.com/").is_err());
        assert!(fixture_target("http://127.0.0.1:9876/kuaishou-fixture/room").is_ok());
        assert!(fixture_target("http://127.0.0.1:9876/api/cookies").is_err());
    }
    #[test]
    fn cookie_export_uses_the_same_specific_scope_as_the_http_client() {
        let cookies = vec![
            Cookie::build(("did", "parent")).path("/").build(),
            Cookie::build(("did", "room")).path("/u/").build(),
        ];
        assert_eq!(cookie_header(&cookies).unwrap(), "did=room");
        let domains = vec![
            Cookie::build(("did", "parent"))
                .domain(".kuaishou.com")
                .path("/")
                .build(),
            Cookie::build(("did", "host"))
                .domain("live.kuaishou.com")
                .path("/")
                .build(),
        ];
        assert_eq!(cookie_header(&domains).unwrap(), "did=host");
        assert_eq!(
            cookie_value("userId=fixture; did=other", "userId").as_deref(),
            Some("fixture")
        );
        assert!(cookie_header(&[]).is_err());
    }
}
