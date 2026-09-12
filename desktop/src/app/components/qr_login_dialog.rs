use super::{Dialog, Icon};
use crate::api::gateway::{self, QrSnapshot};
use leptos::prelude::*;

#[component]
pub fn QrLoginDialog(open: RwSignal<bool>, on_success: Callback<String>) -> impl IntoView {
    let session = RwSignal::new(None::<String>);
    let snapshot = RwSignal::new(None::<QrSnapshot>);
    let error = RwSignal::new(None::<String>);
    let revision = RwSignal::new(0_u32);
    let retry = RwSignal::new(0_u32);
    Effect::new(move |_| {
        let shown = open.get();
        let _ = retry.get();
        let generation = revision.get_untracked().wrapping_add(1);
        revision.set(generation);
        if let Some(id) = session.get_untracked() {
            leptos::task::spawn_local(async move {
                let _ = gateway::qr_cancel(&id).await;
            });
        }
        session.set(None);
        snapshot.set(None);
        error.set(None);
        if !shown {
            return;
        }
        leptos::task::spawn_local(async move {
            let first = match gateway::qr_start().await {
                Ok(snapshot) if !snapshot.session_id.is_empty() => snapshot,
                Ok(_) => {
                    if revision.try_get_untracked() == Some(generation) {
                        error.set(Some("二维码会话未创建，请重试".into()));
                    }
                    return;
                }
                Err(message) => {
                    if revision.try_get_untracked() == Some(generation) {
                        error.set(Some(message));
                    }
                    return;
                }
            };
            let id = first.session_id.clone();
            if revision.try_get_untracked() != Some(generation) {
                let _ = gateway::qr_cancel(&id).await;
                return;
            }
            session.set(Some(id.clone()));
            let mut current = first;
            for _ in 0..300 {
                if revision.try_get_untracked() != Some(generation) {
                    return;
                }
                let terminal = current.is_terminal();
                let cookie = current.cookies.clone();
                let succeeded = current.state == "success";
                snapshot.set(Some(current));
                if terminal {
                    if succeeded {
                        if let Some(cookie) = cookie.filter(|c| !c.trim().is_empty()) {
                            on_success.run(cookie);
                        }
                    }
                    return;
                }
                gateway::sleep_ms(1500).await;
                if revision.try_get_untracked() != Some(generation) {
                    return;
                }
                current = match gateway::qr_status(&id).await {
                    Ok(value) => value,
                    Err(message) => {
                        if revision.try_get_untracked() == Some(generation) {
                            error.set(Some(message));
                        }
                        return;
                    }
                };
            }
            if revision.try_get_untracked() == Some(generation) {
                error.set(Some("等待扫码超时，请重新获取二维码".into()));
            }
            let _ = gateway::qr_cancel(&id).await;
        });
    });
    on_cleanup(move || {
        if let Some(Some(id)) = session.try_get_untracked() {
            leptos::task::spawn_local(async move {
                let _ = gateway::qr_cancel(&id).await;
            });
        }
    });
    view! {
        <Dialog open=Signal::derive(move || open.get()) title="快手扫码登录" on_close=Callback::new(move |_| open.set(false))>
            <p class="dialog-description">"打开快手 App 扫描二维码并确认登录。登录信息仅保存在此设备。"</p>
            <div class="qr-panel">
                <Show when=move || snapshot.get().is_some_and(|s| !s.image_base64.is_empty()) fallback=|| view! { <div class="qr-loading"><span class="spinner" /><span>"正在获取二维码"</span></div> }>
                    <img class="qr-image" src=move || format!("data:image/png;base64,{}", snapshot.get().map(|s| s.image_base64).unwrap_or_default()) alt="快手登录二维码" />
                </Show>
            </div>
            <div class="qr-status"><Icon name="shield" size=17 /><span>{move || snapshot.get().map(|s| s.message).unwrap_or_else(|| "请稍候…".into())}</span></div>
            <Show when=move || snapshot.get().is_some_and(|s| !s.is_terminal() && s.seconds_left > 0)><p class="qr-status qr-countdown">{move || snapshot.get().map(|s| format!("二维码有效期：{} 秒", s.seconds_left)).unwrap_or_default()}</p></Show>
            <Show when=move || error.get().is_some()><p class="field-error" role="alert">{move || error.get().unwrap_or_default()}</p></Show>
            <footer class="modal-actions">
                <Show when=move || error.get().is_some() || snapshot.get().is_some_and(|s| matches!(s.state.as_str(), "error" | "expired" | "cancelled"))><button class="button secondary" on:click=move |_| retry.update(|v| *v = v.wrapping_add(1))><Icon name="refresh" size=16 />"重新获取"</button></Show>
                <button class="button primary" on:click=move |_| open.set(false)>"关闭"</button>
            </footer>
        </Dialog>
    }
}
