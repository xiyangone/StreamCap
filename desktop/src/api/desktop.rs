//! Typed bridge to the native window. Browser previews have no native controls.
use leptos::prelude::*;
use serde::Deserialize;
use serde_json::json;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use wasm_bindgen::{prelude::*, JsCast};

#[wasm_bindgen(inline_js = r#"
import { invoke as nativeInvoke, isTauri } from '/tauri-api/core.js';
import { listen as nativeListen } from '/tauri-api/event.js';
export function streamcapDesktopAvailable() { return isTauri(); }
export function streamcapDesktopInvoke(command, args) {
  return nativeInvoke(command, JSON.parse(args)).then(value=>JSON.stringify(value??null));
}
export async function streamcapDesktopListen(onClose,onState) {
  const unlisteners=[];
  try {
    unlisteners.push(await nativeListen('streamcap:close-requested',event=>onClose(JSON.stringify(event.payload))));
    unlisteners.push(await nativeListen('streamcap:window-state',event=>onState(JSON.stringify(event.payload))));
    onState(JSON.stringify(await nativeInvoke('desktop_ready')));
  } catch(error) { for(const unlisten of unlisteners) unlisten(); throw error; }
  return ()=>{for(const unlisten of unlisteners) unlisten();};
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name=streamcapDesktopAvailable)]
    fn available() -> bool;
    #[wasm_bindgen(js_name=streamcapDesktopInvoke)]
    fn invoke(command: &str, args: &str) -> js_sys::Promise;
    #[wasm_bindgen(js_name=streamcapDesktopListen)]
    fn listen(close: &js_sys::Function, state: &js_sys::Function) -> js_sys::Promise;
}
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WindowStatus {
    pub maximized: bool,
    pub visible: bool,
    pub closing: bool,
    pub tray_available: bool,
}
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CloseRequest {
    pub active_recordings: usize,
    pub tray_available: bool,
}
#[derive(Clone, Copy)]
pub struct DesktopState {
    pub available: bool,
    pub status: RwSignal<WindowStatus>,
    pub request: RwSignal<Option<CloseRequest>>,
}
struct Listener {
    dispose: js_sys::Function,
    _close: Closure<dyn FnMut(String)>,
    _state: Closure<dyn FnMut(String)>,
}
impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self.dispose.call0(&JsValue::NULL);
    }
}
pub fn provide_desktop() -> DesktopState {
    let state = DesktopState {
        available: available(),
        status: RwSignal::new(WindowStatus::default()),
        request: RwSignal::new(None),
    };
    provide_context(state);
    if state.available {
        let slot = Rc::new(RefCell::new(None::<Listener>));
        let disposed = Rc::new(Cell::new(false));
        let cleanup = StoredValue::new_local((slot.clone(), disposed.clone()));
        on_cleanup(move || {
            cleanup.with_value(|(slot, disposed)| {
                disposed.set(true);
                slot.borrow_mut().take();
            });
        });
        leptos::task::spawn_local(async move {
            let on_close = Closure::<dyn FnMut(String)>::new(move |text: String| {
                if let Ok(request) = serde_json::from_str::<CloseRequest>(&text) {
                    let _ = state.request.try_set(Some(request));
                }
            });
            let on_state = Closure::<dyn FnMut(String)>::new(move |text: String| {
                if let Ok(status) = serde_json::from_str::<WindowStatus>(&text) {
                    let _ = state.status.try_set(status);
                }
            });
            match js_sys::futures::JsFuture::from(listen(
                on_close.as_ref().unchecked_ref(),
                on_state.as_ref().unchecked_ref(),
            ))
            .await
            {
                Ok(dispose) => {
                    let listener = Listener {
                        dispose: dispose.unchecked_into(),
                        _close: on_close,
                        _state: on_state,
                    };
                    if !disposed.get() {
                        *slot.borrow_mut() = Some(listener);
                    }
                }
                Err(error) => log::error!("窗口事件连接失败: {}", error_message(error)),
            }
        });
    }
    state
}
pub fn state() -> DesktopState {
    expect_context::<DesktopState>()
}
fn error_message(value: JsValue) -> String {
    value
        .as_string()
        .or_else(|| {
            js_sys::Reflect::get(&value, &JsValue::from_str("message"))
                .ok()?
                .as_string()
        })
        .unwrap_or_else(|| "原生窗口操作失败".into())
}
async fn call<T: serde::de::DeserializeOwned>(
    command: &str,
    args: serde_json::Value,
) -> Result<T, String> {
    if !available() {
        return Err("此操作仅在桌面应用中可用".into());
    }
    let value = js_sys::futures::JsFuture::from(invoke(command, &args.to_string()))
        .await
        .map_err(error_message)?;
    serde_json::from_str(&value.as_string().ok_or("窗口返回内容无效")?)
        .map_err(|_| "窗口返回内容无效".into())
}
pub fn action(desktop: DesktopState, app: super::gateway::AppState, action: &'static str) {
    if !desktop.available {
        return;
    }
    leptos::task::spawn_local(async move {
        match call::<WindowStatus>("desktop_window_action", json!({"action":action})).await {
            Ok(status) => {
                let _ = desktop.status.try_set(status);
            }
            Err(error) => app.fail(error),
        }
    });
}
pub async fn choose_close(
    desktop: DesktopState,
    choice: &str,
    remember: bool,
) -> Result<(), String> {
    let status = call::<WindowStatus>(
        "desktop_close_choice",
        json!({"choice":choice,"remember":remember}),
    )
    .await?;
    let _ = desktop.status.try_set(status);
    let _ = desktop.request.try_set(None);
    Ok(())
}
pub fn apply_theme(theme: &str) {
    if !available() {
        return;
    }
    let theme = theme.to_owned();
    leptos::task::spawn_local(async move {
        if let Err(error) = call::<serde_json::Value>("desktop_theme", json!({"theme":theme})).await
        {
            log::warn!("{error}");
        }
    });
}
