use super::{Dialog, Icon};
use crate::api::gateway::{self, QrSnapshot};
use crate::app::i18n::t;
use leptos::prelude::*;
use serde_json::json;

#[component]
pub fn QrLoginDialog(open: RwSignal<bool>, on_success: Callback<QrSnapshot>) -> impl IntoView {
    let session = RwSignal::new(None::<String>);
    let snapshot = RwSignal::new(None::<QrSnapshot>);
    let error = RwSignal::new(None::<String>);
    let save_error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
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
        save_error.set(None);
        if !shown {
            return;
        }
        leptos::task::spawn_local(async move {
            let first = match gateway::qr_start().await {
                Ok(snapshot) if !snapshot.session_id.is_empty() => snapshot,
                Ok(_) => {
                    if revision.try_get_untracked() == Some(generation) {
                        let _ = error.try_set(Some(t("二维码会话未创建，请重试").into()));
                    }
                    return;
                }
                Err(message) => {
                    if revision.try_get_untracked() == Some(generation) {
                        let _ = error.try_set(Some(message));
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
            for attempt in 0_u32..300 {
                if revision.try_get_untracked() != Some(generation) {
                    return;
                }
                let terminal = current.is_terminal();
                let pause = match current.state.as_str() {
                    "waiting" => 2000 + (attempt / 5).min(3) * 500,
                    _ => 1000,
                };
                snapshot.set(Some(current));
                if terminal {
                    let _ = gateway::qr_cancel(&id).await;
                    if revision.try_get_untracked() == Some(generation) {
                        let _ = session.try_set(None);
                    }
                    return;
                }
                gateway::sleep_ms(pause).await;
                if revision.try_get_untracked() != Some(generation) {
                    return;
                }
                current = match gateway::qr_status(&id).await {
                    Ok(value) => value,
                    Err(message) => {
                        if revision.try_get_untracked() == Some(generation) {
                            let _ = error.try_set(Some(message));
                        }
                        let _ = gateway::qr_cancel(&id).await;
                        return;
                    }
                };
            }
            if revision.try_get_untracked() == Some(generation) {
                let _ = error.try_set(Some(t("等待扫码超时，请重新获取二维码").into()));
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
    let phase = move || {
        if error.get().is_some() {
            "error".to_string()
        } else {
            snapshot
                .get()
                .map(|s| s.state)
                .unwrap_or_else(|| "loading".into())
        }
    };
    let failed = move || matches!(phase().as_str(), "error" | "expired" | "cancelled");
    let message = move || {
        error.get().unwrap_or_else(|| {
            snapshot
                .get()
                .map(|s| s.message)
                .unwrap_or_else(|| t("正在获取二维码").into())
        })
    };
    let save = move |_| {
        if busy.get_untracked() {
            return;
        }
        let Some(result) = snapshot.get_untracked().filter(|s| s.state == "success") else {
            return;
        };
        let Some(cookie) = result
            .cookies
            .as_ref()
            .filter(|cookie| !cookie.trim().is_empty())
            .cloned()
        else {
            save_error.set(Some(t("未获得可保存的登录信息，请重新登录").into()));
            return;
        };
        busy.set(true);
        save_error.set(None);
        leptos::task::spawn_local(async move {
            match gateway::save_cookies(json!({"kuaishou":cookie})).await {
                Ok(()) => on_success.run(result),
                Err(message) => {
                    let _ = save_error.try_set(Some(message));
                }
            }
            let _ = busy.try_set(false);
        });
    };
    view! {
        <Dialog open=Signal::derive(move||open.get()) title=t("快手扫码登录") on_close=Callback::new(move |_|open.set(false)) busy=busy>
            <p class="dialog-description">{t("在快手 App 扫码并确认。账号验证完成后，点击保存登录信息。")}</p>
            <div class="qr-panel" class:qr-code-container=move||phase()=="waiting">
                <Show when=move||phase()=="waiting"&&snapshot.get().is_some_and(|s|!s.image_base64.is_empty()) fallback=move||view!{
                    <div class="qr-stage" class:error=failed class:success=move||phase()=="success" role="status" aria-live="polite">
                        <Show when=failed fallback=move||view!{<Show when=move||phase()=="success" fallback=||view!{<span class="spinner"/>}><Icon name="check" size=34/></Show>}><Icon name="alert" size=34/></Show>
                        <strong>{move||match phase().as_str(){"success"=>t("账号验证通过"),"error"=>t("登录未完成"),"expired"=>t("二维码已过期"),"cancelled"=>t("登录已取消"),"scanned"=>t("等待手机确认"),"verifying"=>t("手机已确认"),"loading"=>t("正在获取二维码"),_=>t("正在处理登录")}}</strong>
                        <p>{message}</p>
                        <Show when=move||phase()=="success"><p class="qr-save-hint">{t("登录信息尚未保存，点击下方按钮完成登录。")}</p></Show>
                    </div>
                }>
                    <img class="qr-image" src=move||format!("data:image/png;base64,{}",snapshot.get().map(|s|s.image_base64).unwrap_or_default()) alt=t("快手登录二维码")/>
                </Show>
            </div>
            <Show when=move||phase()=="waiting"><p class="qr-status">{t("使用快手 App 扫描二维码")}</p><p class="qr-status qr-countdown">{move||snapshot.get().map(|s|crate::tr_format!("二维码有效期：{} 秒",s.seconds_left)).unwrap_or_default()}</p></Show>
            <Show when=move||save_error.get().is_some()><p class="field-error" role="alert">{move||save_error.get().unwrap_or_default()}</p></Show>
            <footer class="modal-actions">
                <button class="button secondary" disabled=move||busy.get() on:click=move |_|open.set(false)>{t("关闭")}</button>
                <Show when=failed><button class="button primary" on:click=move |_|retry.update(|v|*v=v.wrapping_add(1))><Icon name="refresh" size=16/>{t("重新获取")}</button></Show>
                <Show when=move||phase()=="success"><button class="button primary" disabled=move||busy.get() on:click=save>{move||if busy.get(){t("保存中…")}else{t("保存登录信息")}}</button></Show>
            </footer>
        </Dialog>
    }
}
