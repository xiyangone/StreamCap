use super::{ActionButton, Dialog, FormFeedback, InlineNotice, ToggleSwitch};
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
        if busy.get_untracked()
            || desktop.request.get_untracked().is_none()
            || (choice == "tray"
                && !desktop
                    .request
                    .get_untracked()
                    .is_some_and(|r| r.tray_available))
        {
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
            <InlineNotice icon="activity">{move || { let request = desktop.request.get().unwrap_or_default(); crate::tr_format!("正在录制 {} 项，待处理媒体 {} 项。退出将停止录制并等待转 MP4；超时会保留 TS。托盘模式继续后台处理。", request.active_recordings, request.pending_media_jobs) }}</InlineNotice>
            <div class="close-options">
                <div class="close-choice"><ActionButton label=t("最小化到托盘") icon="tray" class="close-choice-action" busy=busy
                    disabled=Signal::derive(move || !desktop.request.get().is_some_and(|r|r.tray_available))
                    on_click=Callback::new(move |_|choose.run("tray")) />
                    <p class="field-hint">{t("继续监控和录制，从托盘图标返回")}</p></div>
                <div class="close-choice"><ActionButton label=t("退出应用") icon="power" class="close-choice-action" variant="danger" busy=busy
                    on_click=Callback::new(move |_|choose.run("exit")) />
                    <p class="field-hint">{t("停止监控，完成录制收尾并关闭程序")}</p></div>
            </div>
            <Show when=move||desktop.request.get().is_some_and(|r|!r.tray_available)><p class="field-hint">{t("当前系统托盘不可用，请取消或退出应用。")}</p></Show>
            <ToggleSwitch label=t("记住我的选择") checked=remember disabled=busy
                hint=t("可以在偏好设置 → 外观与窗口中修改。") on_change=Callback::new(move |value|remember.set(value)) />
            <FormFeedback error=error title=t("操作未完成") />
            <footer class="modal-actions"><ActionButton label=t("取消") disabled=busy on_click=Callback::new(move |_|choose.run("cancel")) /></footer>
        </Dialog>
        <Show when=move||desktop.status.get().closing><div class="shutdown-overlay" role="status" aria-live="polite"><span class="ui-loading ui-loading-spinner ui-loading-md spinner" aria-hidden="true"/><strong>{t("正在安全退出")}</strong><p>{t("正在停止监控、保存录像并处理 MP4；校验失败或超时会保留 TS…")}</p></div></Show>
    }
}
