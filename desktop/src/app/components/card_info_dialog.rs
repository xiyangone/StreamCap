use super::{Dialog, Icon};
use crate::app::i18n::t;
use crate::{
    api::gateway::{self, Recording},
    app::labels::{format_label, quality_label},
};
use leptos::prelude::*;

#[component]
pub fn CardInfoDialog(target: RwSignal<Option<Recording>>) -> impl IntoView {
    let state = gateway::app_state();
    let record = move || {
        let initial = target.get().unwrap_or_default();
        state
            .recordings
            .with(|list| list.iter().find(|r| r.rec_id == initial.rec_id).cloned())
            .unwrap_or(initial)
    };
    let copy = move |_| {
        let url = record().url;
        leptos::task::spawn_local(async move {
            match wasm_bindgen_futures_copy(url).await {
                Ok(()) => state.notify(t("直播间地址已复制")),
                Err(()) => state.fail(t("无法访问剪贴板，请手动选择并复制地址")),
            }
        });
    };
    view! {
        <Dialog open=Signal::derive(move || target.get().is_some()) title=t("直播间信息") on_close=Callback::new(move |_| target.set(None))>
            <div class="info-heading"><span class="large-initial">{move || record().name().chars().next().unwrap_or('播').to_string()}</span><div><h3>{move || record().name()}</h3><span class=move || record().status().class()>{move || record().status().label()}</span></div></div>
            <dl class="info-list">
                <div><dt>{t("直播平台")}</dt><dd>{move || record().platform.unwrap_or_else(|| t("自定义直播间").into())}</dd></div>
                <div><dt>{t("清晰度")}</dt><dd>{move || quality_label(record().quality.as_deref())}{move || record().inherits("quality").then_some(t(" · 跟随全局"))}</dd></div>
                <div><dt>{t("录制格式")}</dt><dd>{move || format_label(record().record_format.as_deref())}{move || record().inherits("record_format").then_some(t(" · 跟随全局"))}</dd></div>
                <div><dt>{t("分段录制")}</dt><dd>{move || if record().segment_record.unwrap_or(false) { crate::tr_format!("每 {} 秒", record().segment_time.unwrap_or_else(|| "—".into())) } else { t("关闭").into() }}</dd></div>
                <div><dt>{t("码率")}</dt><dd>{move || record().video_bitrate.map(|v| format!("{v} kbps")).unwrap_or_else(|| t("复制源流").into())}</dd></div>
                <div><dt>{t("监控状态")}</dt><dd>{move || if record().monitor_status { t("已开启") } else { t("已暂停") }}</dd></div>
                <div><dt>{t("本次录制时长")}</dt><dd>{move||crate::app::labels::duration(record().recorded_seconds)}</dd></div><div><dt>{t("录制速率")}</dt><dd>{move || record().speed.unwrap_or_else(|| "—".into())}</dd></div>
                <div><dt>{t("保存目录")}</dt><dd class="break-anywhere">{move || record().recording_dir.unwrap_or_else(|| t("尚未生成录制文件").into())}</dd></div>
            </dl>
            <label class="field"><span>{t("直播间地址")}</span><input class="input" readonly prop:value=move || record().url /></label>
            <footer class="modal-actions"><button class="button secondary" on:click=copy><Icon name="copy" size=16 />{t("复制地址")}</button><button class="button primary" on:click=move |_| target.set(None)>{t("完成")}</button></footer>
        </Dialog>
    }
}

async fn wasm_bindgen_futures_copy(text: String) -> Result<(), ()> {
    let promise = window().navigator().clipboard().write_text(&text);
    js_sys::futures::JsFuture::from(promise)
        .await
        .map(|_| ())
        .map_err(|_| ())
}
