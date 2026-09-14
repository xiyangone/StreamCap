use super::{Dialog, Icon};
use crate::app::i18n::t;
use crate::{
    api::gateway::{self, NewRecording},
    app::labels::parse_recordings,
};
use leptos::prelude::*;

#[component]
pub fn AddRecordingDialog(open: RwSignal<bool>) -> impl IntoView {
    let state = gateway::app_state();
    let urls = RwSignal::new(String::new());
    let name = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() {
            return;
        }
        let mut items: Vec<NewRecording> = match parse_recordings(&urls.get_untracked()) {
            Ok(items) => items,
            Err(message) => {
                error.set(Some(message));
                return;
            }
        };
        let existing = state.recordings.get_untracked();
        if items
            .iter()
            .any(|item| existing.iter().any(|rec| rec.url == item.url))
        {
            error.set(Some(t("其中有已添加的直播间，请移除重复地址后重试").into()));
            return;
        }
        let default_name = name.get_untracked().trim().to_string();
        for item in &mut items {
            if item.streamer_name.is_none() && !default_name.is_empty() {
                item.streamer_name = Some(default_name.clone());
            }
        }
        let count = items.len();
        busy.set(true);
        error.set(None);
        leptos::task::spawn_local(async move {
            match gateway::create_recordings(items).await {
                Ok(()) => {
                    let _ = open.try_set(false);
                    let _ = urls.try_set(String::new());
                    let _ = name.try_set(String::new());
                    state.notify(crate::tr_format!("已添加 {count} 个直播间"));
                    if let Err(message) = gateway::refresh_recordings(state).await {
                        state.fail(crate::tr_format!("任务已添加，列表同步失败：{message}"));
                    }
                }
                Err(message) => {
                    let _ = error.try_set(Some(message));
                }
            }
            let _ = busy.try_set(false);
        });
    };
    view! {
        <Dialog open=Signal::derive(move || open.get()) title=t("添加直播间") busy=busy on_close=Callback::new(move |_| { open.set(false); error.set(None); })>
            <p class="dialog-description">{t("把喜欢的直播间加入工作空间，开播后自动开始录制。")}</p>
            <form on:submit=submit>
                <label class="field"><span>{t("直播间地址")}<span class="required">" *"</span></span>
                    <textarea class="input url-input" rows="6" autofocus placeholder=t("每行一个 HTTP 或 HTTPS 地址") prop:value=move || urls.get()
                        on:input=move |event| { urls.set(event_target_value(&event)); error.set(None); } disabled=move || busy.get() />
                </label>
                <div class="input-caption"><span><Icon name="link" size=14 />{t("支持一次添加多个直播间")}</span><span>{move || crate::tr_format!("{} 行", urls.get().lines().filter(|l| !l.trim().is_empty()).count())}</span></div>
                <label class="field"><span>{t("默认主播名称")}<small>{t("可选")}</small></span><input class="input" placeholder=t("留空后在检测时识别") prop:value=move || name.get() on:input=move |event| name.set(event_target_value(&event)) disabled=move || busy.get() /></label>
                <details class="import-help"><summary>{t("批量导入格式")}</summary><p>{t("也支持“地址,主播名”或“清晰度,地址,主播名”。清晰度可填 0–4 或 OD / UHD / HD / SD / LD。每行独立解析。")}</p></details>
                <Show when=move || error.get().is_some()><p class="field-error" role="alert">{move || error.get().unwrap_or_default()}</p></Show>
                <footer class="modal-actions"><button type="button" class="button secondary" disabled=move || busy.get() on:click=move |_| open.set(false)>{t("取消")}</button>
                    <button type="submit" class="button primary" disabled=move || busy.get() || !state.status.get().ok><Icon name="plus" size=17 />{move || if busy.get() { t("正在添加…") } else { t("添加直播间") }}</button>
                </footer>
            </form>
        </Dialog>
    }
}
