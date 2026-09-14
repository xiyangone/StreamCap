use super::{Dialog, Icon};
use crate::api::desktop;
use crate::app::i18n::t;
use leptos::prelude::*;

#[component]
pub fn CloseDialog() -> impl IntoView {
    let desktop = desktop::state();
    let remember = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    Effect::new(move |_| {
        let _ = desktop.request.get();
        remember.set(false);
        error.set(None);
    });
    let choose = Callback::new(move |choice: &'static str| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        error.set(None);
        let save = remember.get_untracked();
        leptos::task::spawn_local(async move {
            if let Err(message) = desktop::choose_close(desktop, choice, save).await {
                let _ = error.try_set(Some(message));
            }
            let _ = busy.try_set(false);
        });
    });
    view! {
        <Dialog open=Signal::derive(move||desktop.request.get().is_some()) title=t("关闭 StreamCap") on_close=Callback::new(move |_|choose.run("cancel")) busy=busy>
            <p class="dialog-description">{move || { let request = desktop.request.get().unwrap_or_default(); crate::tr_format!("正在录制 {} 项，待处理媒体 {} 项。退出将停止录制并等待转 MP4；超时会保留 TS。托盘模式继续后台处理。", request.active_recordings, request.pending_media_jobs) }}</p>
            <div class="close-options">
                <button class="close-option" disabled=move||busy.get()||!desktop.request.get().is_some_and(|r|r.tray_available) on:click=move |_|choose.run("tray")><Icon name="tray" size=24 /><span><strong>{t("最小化到托盘")}</strong><small>{t("继续监控和录制，从托盘图标返回")}</small></span><Icon name="chevron" size=17 /></button>
                <button class="close-option" disabled=move||busy.get() on:click=move |_|choose.run("exit")><Icon name="power" size=24 /><span><strong>{t("退出应用")}</strong><small>{t("停止监控，完成录制收尾并关闭程序")}</small></span><Icon name="chevron" size=17 /></button>
            </div>
            <Show when=move||desktop.request.get().is_some_and(|r|!r.tray_available)><p class="field-hint">{t("当前系统托盘不可用，请取消或退出应用。")}</p></Show>
            <label class="close-remember"><input type="checkbox" prop:checked=move||remember.get() disabled=move||busy.get() on:change=move|event|remember.set(event_target_checked(&event)) /><span>{t("记住我的选择")}</span></label>
            <p class="field-hint">{t("可以在偏好设置 → 外观与窗口中修改。")}</p>
            <Show when=move||error.get().is_some()><p class="field-error" role="alert">{move||error.get().unwrap_or_default()}</p></Show>
            <footer class="modal-actions"><button class="button secondary" disabled=move||busy.get() on:click=move |_|choose.run("cancel")>{t("取消")}</button></footer>
        </Dialog>
        <Show when=move||desktop.status.get().closing><div class="shutdown-overlay" role="status" aria-live="polite"><span class="spinner"/><strong>{t("正在安全退出")}</strong><p>{t("正在停止监控、保存录像并处理 MP4；校验失败或超时会保留 TS…")}</p></div></Show>
    }
}
