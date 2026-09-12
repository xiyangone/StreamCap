use super::{Dialog, Icon};
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
                Ok(()) => state.notify("直播间地址已复制"),
                Err(()) => state.fail("无法访问剪贴板，请手动选择并复制地址"),
            }
        });
    };
    view! {
        <Dialog open=Signal::derive(move || target.get().is_some()) title="直播间信息" on_close=Callback::new(move |_| target.set(None))>
            <div class="info-heading"><span class="large-initial">{move || record().name().chars().next().unwrap_or('播').to_string()}</span><div><h3>{move || record().name()}</h3><span class=move || record().status().class()>{move || record().status().label()}</span></div></div>
            <dl class="info-list">
                <div><dt>"直播平台"</dt><dd>{move || record().platform.unwrap_or_else(|| "自定义直播间".into())}</dd></div>
                <div><dt>"清晰度"</dt><dd>{move || quality_label(record().quality.as_deref())}{move || record().inherits("quality").then_some(" · 跟随全局")}</dd></div>
                <div><dt>"录制格式"</dt><dd>{move || format_label(record().record_format.as_deref())}{move || record().inherits("record_format").then_some(" · 跟随全局")}</dd></div>
                <div><dt>"分段录制"</dt><dd>{move || if record().segment_record.unwrap_or(false) { format!("每 {} 秒", record().segment_time.unwrap_or_else(|| "—".into())) } else { "关闭".into() }}</dd></div>
                <div><dt>"码率"</dt><dd>{move || record().video_bitrate.map(|v| format!("{v} kbps")).unwrap_or_else(|| "复制源流".into())}</dd></div>
                <div><dt>"监控状态"</dt><dd>{move || if record().monitor_status { "已开启" } else { "已暂停" }}</dd></div>
                <div><dt>"录制速率"</dt><dd>{move || record().speed.unwrap_or_else(|| "—".into())}</dd></div>
                <div><dt>"保存目录"</dt><dd class="break-anywhere">{move || record().recording_dir.unwrap_or_else(|| "尚未生成录制文件".into())}</dd></div>
            </dl>
            <label class="field"><span>"直播间地址"</span><input class="input" readonly prop:value=move || record().url /></label>
            <footer class="modal-actions"><button class="button secondary" on:click=copy><Icon name="copy" size=16 />"复制地址"</button><button class="button primary" on:click=move |_| target.set(None)>"完成"</button></footer>
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
