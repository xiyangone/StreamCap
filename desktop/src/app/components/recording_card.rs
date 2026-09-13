use super::{ConfirmDialog, Icon};
use crate::{
    api::gateway::{self, Recording},
    app::labels::{format_label, quality_label},
};
use leptos::prelude::*;

#[derive(Clone, Copy)]
enum Action {
    Check,
    Record,
    Monitor,
}

#[component]
pub fn RecordingCard(
    recording: Recording,
    on_edit: Callback<Recording>,
    on_info: Callback<Recording>,
    on_preview: Callback<Recording>,
    #[prop(optional)] selectable: bool,
    #[prop(default = RwSignal::new(Vec::new()))] selected: RwSignal<Vec<String>>,
) -> impl IntoView {
    let state = gateway::app_state();
    let initial = StoredValue::new(recording);
    let record = Signal::derive(move || {
        state
            .recordings
            .with(|list| {
                list.iter()
                    .find(|r| r.rec_id == initial.with_value(|v| v.rec_id.clone()))
                    .cloned()
            })
            .unwrap_or_else(|| initial.get_value())
    });
    let busy = RwSignal::new(false);
    let deleting = RwSignal::new(false);
    let selected_now = move || selected.with(|ids| ids.contains(&record.get().rec_id));
    let perform = Callback::new(move |action: Action| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        let rec = record.get_untracked();
        leptos::task::spawn_local(async move {
            let (result, message) = match action {
                Action::Check => (
                    gateway::check_recording(&rec.rec_id).await,
                    "已更新直播状态",
                ),
                Action::Record if rec.is_recording => (
                    gateway::stop_recording(&rec.rec_id).await,
                    "已停止本次录制；监控开启时仍可能再次录制",
                ),
                Action::Record => (gateway::start_recording(&rec.rec_id).await, "已开始录制"),
                Action::Monitor => (
                    gateway::toggle_monitor(&rec.rec_id).await,
                    if rec.monitor_status {
                        "已暂停监控"
                    } else {
                        "已开启监控"
                    },
                ),
            };
            match result {
                Ok(()) => {
                    state.notify(message);
                    if let Err(error) = gateway::refresh_recordings(state).await {
                        state.fail(error);
                    }
                }
                Err(error) => state.fail(error),
            }
            let _ = busy.try_set(false);
        });
    });
    let delete = Callback::new(move |_| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        let id = record.get_untracked().rec_id;
        leptos::task::spawn_local(async move {
            match gateway::delete_recording(&id).await {
                Ok(()) => {
                    let _ = deleting.try_set(false);
                    state.notify("已移除直播间，录制文件保留");
                    if let Err(error) = gateway::refresh_recordings(state).await {
                        state.fail(error);
                    }
                }
                Err(error) => state.fail(error),
            }
            let _ = busy.try_set(false);
        });
    });
    view! {
        <article class="recording-card glass" class:selected=selected_now class:is-recording=move || record.get().is_recording data-rec-id=move || record.get().rec_id aria-label=move || record.get().name()>
            <div class="card-head">
                <div class="card-platform"><span class="platform-mark" data-platform=move || record.get().platform_key.unwrap_or_default()>{move || record.get().platform.unwrap_or_else(|| "直播".into()).chars().next().unwrap_or('播').to_string()}</span><span>{move || record.get().platform.unwrap_or_else(|| "自定义直播间".into())}</span></div>
                <div class="card-head-right"><span class=move || record.get().status().class()><i class="status-dot" />{move || record.get().status().label()}</span>
                    {selectable.then(|| view! { <input class="task-checkbox" type="checkbox" aria-label=move || format!("选择{}", record.get().name()) prop:checked=selected_now on:change=move |_| {
                        let id = record.get_untracked().rec_id;
                        selected.update(|ids| if ids.contains(&id) { ids.retain(|value| value != &id); } else { ids.push(id); });
                    } /> })}
                </div>
            </div>
            <div class="card-main">
                <button class="card-name" title="查看录制信息" on:click=move |_| on_info.run(record.get_untracked())>{move || record.get().name()}</button>
                <p class="card-subtitle" title=move || record.get().live_title.unwrap_or_default()>{move || record.get().live_title.filter(|s| !s.is_empty()).unwrap_or_else(|| if record.get().monitor_status { "监控中，开播后自动录制".into() } else { "监控已暂停".into() })}</p>
            </div>
            <div class="card-meta"><div class="card-tags"><span>{move || quality_label(record.get().quality.as_deref())}</span><span>{move || format_label(record.get().record_format.as_deref())}</span>{move || record.get().segment_record.unwrap_or(false).then(|| view! { <span>"分段"</span> })}</div>
                <span class="card-speed"><Show when=move || record.get().is_recording><Icon name="signal" size=13 /></Show>{move || record.get().speed.unwrap_or_else(|| "—".into())}</span>
            </div>
            <div class="card-actions">
                <button class="button card-record" class:recording=move || record.get().is_recording disabled=move || busy.get() || !state.status.get().ok || (!record.get().is_recording && !state.status.get().resolver_ready) on:click=move |_| perform.run(Action::Record)>
                    <Show when=move || record.get().is_recording fallback=|| view! { <Icon name="play" size=14 /> }><Icon name="stop" size=14 /></Show>{move || if record.get().is_recording { "停止录制" } else { "开始录制" }}
                </button>
                <div class="card-tools">
                    <button class="icon-button" aria-label="检测直播状态" title="检测状态（已开播时会启动录制）" disabled=move || busy.get() || !state.status.get().resolver_ready on:click=move |_| perform.run(Action::Check)><Icon name="refresh" size=16 /></button>
                    <button class="icon-button" aria-label="预览录制文件" title="预览录制文件" on:click=move |_| on_preview.run(record.get_untracked())><Icon name="eye" size=16 /></button>
                    <button class="icon-button" aria-label=move || if record.get().monitor_status { "暂停监控" } else { "开启监控" } title=move || if record.get().monitor_status { "暂停监控并停止录制" } else { "开启监控" } disabled=move || busy.get() || !state.status.get().ok on:click=move |_| perform.run(Action::Monitor)>
                        <Show when=move || record.get().monitor_status fallback=|| view! { <Icon name="play" size=16 /> }><Icon name="pause" size=16 /></Show>
                    </button>
                    <button class="icon-button" aria-label="编辑任务" title=move || if record.get().is_recording { "请先停止录制" } else { "编辑任务" } disabled=move || busy.get() || record.get().is_recording || !state.status.get().ok on:click=move |_| on_edit.run(record.get_untracked())><Icon name="edit" size=16 /></button>
                    <button class="icon-button danger-text" aria-label="删除任务" title="移除任务，保留录制文件" disabled=move || busy.get() || record.get().is_recording || !state.status.get().ok on:click=move |_| deleting.set(true)><Icon name="trash" size=16 /></button>
                </div>
            </div>
        </article>
        <ConfirmDialog open=deleting title="移除直播间" description=Signal::derive(move || format!("将从工作空间移除“{}”。已有录制文件不会被删除。", record.get().name())) busy=busy on_confirm=delete />
    }
}
