//! Human-operated official-site verification. Remote pages never receive native IPC permissions.
use serde::Serialize;
use serde_json::Value;
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
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
#[derive(Clone)]
struct Session {
    id: u64,
    record: Recording,
    target: Url,
    original_cookie: Option<String>,
    loaded: Arc<AtomicU64>,
    stop: CancellationToken,
    busy: bool,
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
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    active: bool,
    busy: bool,
    rec_id: Option<String>,
    active_workers: usize,
}
impl Verification {
    pub fn new(core: ApiState, smoke: bool) -> Self {
        Self {
            core,
            state: Mutex::new(State::default()),
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
            smoke,
        }
    }
    pub fn status(&self) -> Status {
        let state = self.state.lock().expect("verification state");
        Status {
            active: state.active.is_some(),
            busy: state.active.as_ref().is_some_and(|s| s.busy),
            rec_id: state.active.as_ref().map(|s| s.record.rec_id.clone()),
            active_workers: self.tasks.len(),
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
            handle.state::<Verification>().core.store.snack(error);
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
    let session = {
        let mut state = owner.state.lock().map_err(|_| "验证状态异常")?;
        if state.active.is_some() {
            drop(state);
            if manual {
                if let Some(window) = app.get_webview_window(LABEL) {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            return Ok(());
        }
        if state.dismissed && !manual {
            return Ok(());
        }
        state.next_id = state.next_id.wrapping_add(1);
        let session = Session {
            id: state.next_id,
            record,
            target,
            original_cookie: cookie,
            loaded: Arc::new(AtomicU64::new(0)),
            stop: owner.stop.child_token(),
            busy: false,
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
    log::info!("快手手动验证窗口已打开");
    Ok(())
}
async fn create_window(app: &AppHandle, session: &Session, smoke: bool) -> Result<(), String> {
    let complete = MenuItem::with_id(
        app,
        "kuaishou-complete",
        "已完成验证，继续",
        true,
        None::<&str>,
    )
    .map_err(|_| "无法创建验证操作")?;
    let cancel = MenuItem::with_id(app, "kuaishou-cancel", "取消验证", true, None::<&str>)
        .map_err(|_| "无法创建验证操作")?;
    let menu = Menu::with_items(app, &[&complete, &cancel]).map_err(|_| "无法创建验证菜单")?;
    let smoke_origin = smoke.then(|| session.target.origin().ascii_serialization());
    let navigation_origin = smoke_origin.clone();
    let loaded = session.loaded.clone();
    let mut builder = WebviewWindowBuilder::new(
        app,
        LABEL,
        WebviewUrl::External(Url::parse("about:blank").expect("static URL")),
    )
    .title("快手验证 · 手动完成后点击顶部“已完成验证，继续”")
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
    .on_page_load(move |_, payload| {
        if matches!(payload.event(), PageLoadEvent::Finished) {
            loaded.fetch_add(1, Ordering::SeqCst);
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
    let cookie = session.original_cookie.clone().unwrap_or_default();
    let cookie_window = window.clone();
    let host = if smoke {
        session.target.host_str().unwrap_or_default().to_owned()
    } else {
        ".kuaishou.com".into()
    };
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        for pair in cookie.split(';').map(str::trim) {
            let Some((name, value)) = pair.split_once('=') else {
                continue;
            };
            if name.is_empty() || !seen.insert(name.to_owned()) {
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
    window.show().map_err(|_| "无法显示验证窗口")?;
    let _ = window.set_focus();
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
                let owner = handle.state::<Verification>();
                let mut state = owner.state.lock().expect("verification state");
                if let Some(current) = state
                    .active
                    .as_mut()
                    .filter(|current| current.id == session.id)
                {
                    current.busy = false;
                    drop(state);
                    owner.core.store.snack(error);
                    if let Some(window) = handle.get_webview_window(LABEL) {
                        let _ =
                            window.set_title("快手验证未完成 · 请验证后再点顶部“已完成验证，继续”");
                    }
                }
            }
        }
    });
    Ok(())
}
async fn snapshot(window: &WebviewWindow) -> Result<Value, String> {
    let (send, receive) = tokio::sync::oneshot::channel();
    let send = Mutex::new(Some(send));
    window
        .eval_with_callback(
            "JSON.stringify({href:location.href,html:document.documentElement.outerHTML})",
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
    let epoch = session.loaded.load(Ordering::SeqCst);
    window
        .set_title("快手验证 · 正在核实目标直播间")
        .map_err(|_| "验证窗口已关闭")?;
    // One same-session navigation per explicit user completion action. No request polling or CAPTCHA automation.
    window
        .navigate(session.target.clone())
        .map_err(|_| "无法重新核实快手房间")?;
    tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            if session.loaded.load(Ordering::SeqCst) > epoch
                && window.url().ok().as_ref() == Some(&session.target)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| "快手网页载入超时，未更改登录信息")?;
    let data = snapshot(&window).await?;
    let href = data["href"]
        .as_str()
        .and_then(|s| Url::parse(s).ok())
        .ok_or("无法确认验证页面来源")?;
    if href != session.target {
        return Err("当前页面不是目标直播间，未应用验证结果".into());
    }
    let html = data["html"]
        .as_str()
        .filter(|s| s.len() <= 4 * 1024 * 1024)
        .ok_or("验证页面数据无效或过大")?;
    let info = kuaishou::parse_page(html, session.record.quality.as_deref())?;
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
    if let Some(expected) = expected_id {
        if actual_id.as_deref() != Some(expected.as_str()) {
            return Err("验证账号与原快手账号不一致，登录信息未替换".into());
        }
        streamcap_core::platforms::kuaishou_login::verify_login_page(html, &expected)
            .map_err(|_| "房间已可读，但快手账号尚未验证，请在此窗口完成登录")?;
    }
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
    {
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
        config
            .update_cookies(serde_json::Map::from_iter([(
                "kuaishou".into(),
                Value::String(header),
            )]))
            .map_err(|_| "快手验证成功，但会话保存失败")?;
    }
    owner.core.resolver.kuaishou_session_changed().await;
    let result = owner
        .core
        .scheduler
        .accept_verified(&session.record, &info)
        .await;
    log::info!("快手目标房间验证完成");
    Ok(match result {
        Ok(outcome) => format!("快手验证完成；{}", outcome.message()),
        Err(_) => "快手验证完成；当前任务未恢复，请查看任务状态".into(),
    })
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
    cookies.sort_by_key(|cookie| std::cmp::Reverse(cookie.path().unwrap_or("/").len()));
    let header = cookies
        .iter()
        .map(|cookie| format!("{}={}", cookie.name(), cookie.value()))
        .collect::<Vec<_>>()
        .join("; ");
    if header.is_empty() || header.len() > 65536 || header.contains(['\r', '\n']) {
        return Err("验证未返回有效快手会话，登录信息未保存".into());
    }
    Ok(header)
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
    app.state::<Verification>().status()
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn cookie_export_preserves_same_name_scopes_without_exposing_other_data() {
        let cookies = vec![
            Cookie::build(("did", "parent")).path("/").build(),
            Cookie::build(("did", "room")).path("/u/").build(),
        ];
        assert_eq!(cookie_header(&cookies).unwrap(), "did=room; did=parent");
        assert_eq!(
            cookie_value("userId=fixture; did=other", "userId").as_deref(),
            Some("fixture")
        );
        assert!(cookie_header(&[]).is_err());
    }
}
