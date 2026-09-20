use super::{ConfirmDialog, Icon};
use crate::app::i18n::t;
use crate::{
    api::gateway::{self, Recording},
    app::labels::{duration, format_label, monitoring_label, quality_label},
};
use leptos::prelude::*;

#[derive(Clone, Copy)]
enum Action {
    Check,
    Record,
    Monitor,
    Verify,
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
                Action::Verify => (
                    crate::api::desktop::verify_kuaishou(&rec.rec_id).await,
                    t("已打开快手验证窗口"),
                ),
                Action::Check => (
                    gateway::check_recording(&rec.rec_id).await,
                    t("已更新直播状态"),
                ),
                Action::Record if rec.is_recording => (
                    gateway::stop_recording(&rec.rec_id).await,
                    t("已停止本次录制；监控开启时仍可能再次录制"),
                ),
                Action::Record => (gateway::start_recording(&rec.rec_id).await, t("已开始录制")),
                Action::Monitor => (
                    gateway::toggle_monitor(&rec.rec_id).await,
                    if rec.monitor_status {
                        t("已暂停监控")
                    } else {
                        t("已开启监控")
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
                    state.notify(t("已移除直播间，录制文件保留"));
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
        <article class="recording-card glass" class:selected=selected_now class:is-recording=move || record.get().is_recording class:needs-attention=move || record.get().needs_attention() data-rec-id=move || record.get().rec_id aria-label=move || record.get().name()>
            <div class="card-head">
                <div class="card-platform"><span class="platform-mark" data-platform=move || record.get().platform_key.unwrap_or_default()>{move || record.get().platform.unwrap_or_else(|| t("直播").into()).chars().next().unwrap_or('播').to_string()}</span><span>{move || record.get().platform.unwrap_or_else(|| t("自定义直播间").into())}</span></div>
                <div class="card-head-right"><span class=move || record.get().status().class() title=move || if record.get().is_live && record.get().recording_error.is_some() { t("最近一次平台检测为直播中") } else { "" }><i class="status-dot" />{move || record.get().status().label()}</span>
                    {selectable.then(|| view! { <input class="task-checkbox" type="checkbox" aria-label=move || crate::tr_format!("选择{}", record.get().name()) prop:checked=selected_now on:change=move |_| {
                        let id = record.get_untracked().rec_id;
                        selected.update(|ids| if ids.contains(&id) { ids.retain(|value| value != &id); } else { ids.push(id); });
                    } /> })}
                </div>
            </div>
            <div class="card-main">
                <button class="card-name" title=t("查看录制信息") on:click=move |_| on_info.run(record.get_untracked())>{move || record.get().name()}</button>
                <p class="card-subtitle" title=move || record.get().live_title.unwrap_or_default()>{move || record.get().live_title.filter(|s| !s.is_empty()).unwrap_or_else(|| if record.get().is_live { t("直播中").into() } else { t("等待开播").into() })}</p>
                <Show when=move || record.get().check_error.is_some()>
                    <div class="card-check-error" role="status" title=move || record.get().check_error.unwrap_or_default()>
                        <span><Icon name="alert" size=14 />{move || if record.get().verification_required { t("请在快手窗口完成验证").to_string() } else { crate::tr_format!("检测失败：{}",record.get().check_error.unwrap_or_default()) }}</span>
                        <Show when=move || record.get().verification_required || matches!(record.get().access_state.as_str(), "pageCheck" | "unavailable" | "loginPrompt")>
                            <button class="button secondary small card-verify" type="button" disabled=move || busy.get() || !state.status.get().ok || !crate::api::desktop::state().available on:click=move |_|perform.run(Action::Verify)>{move || if record.get().verification_required { t("重新验证") } else { t("检查页面") }}</button>
                        </Show>
                    </div>
                </Show>
                <Show when=move || record.get().recording_error.as_ref().is_some_and(|error| !error.is_empty())>
                    <p class="card-recording-error" role="status"><Icon name="alert" size=14 /><span>{move || crate::tr_format!("录制失败：{}", record.get().recording_error.unwrap_or_default())}</span></p>
                </Show>
                {move || state.media_jobs.with(|jobs| jobs.iter().rev().find(|j| j.task_id.as_deref() == Some(record.get().rec_id.as_str())).map(|j| view! { <p class="media-job-status" role="status">{j.message.clone()}</p> }))}
            </div>
            <div class="card-monitor-row"><span class:monitor-on=move || record.get().monitor_status><Icon name="eye" size=14 />{move || monitoring_label(&record.get())}</span></div>
            <div class="card-meta"><div class="card-tags"><span>{move || quality_label(record.get().quality.as_deref())}</span><span>{move || format_label(record.get().record_format.as_deref())}</span>{move || record.get().segment_record.unwrap_or(false).then(|| view! { <span>{t("分段")}</span> })}</div>
                <span class="card-speed"><Show when=move || record.get().is_recording><span class="card-duration">{move || duration(record.get().recorded_seconds)}</span></Show><Show when=move || record.get().is_recording><Icon name="signal" size=13 /></Show>{move || record.get().speed.unwrap_or_else(|| "—".into())}</span>
            </div>
            <div class="card-actions">
                <div class="card-primary-actions">
                    <button class="button secondary small card-monitor" type="button" aria-label=move || if record.get().monitor_status { t("暂停监控") } else { t("开启监控") } title=move || if record.get().monitor_status {t("暂停监控并停止录制")} else {t("开启监控并安排首次检测")} disabled=move || busy.get() || !state.status.get().ok on:click=move |_| perform.run(Action::Monitor)>{move || if record.get().monitor_status {t("暂停监控")} else {t("开启监控")}}</button>
                <button class="button card-record" aria-label=move || if record.get().is_recording {t("停止录制")} else {t("单次录制")} title=move || if record.get().is_recording {t("停止本次录制，不关闭自动监控")} else {t("立即尝试录制，不改变自动监控设置")} class:recording=move || record.get().is_recording disabled=move || busy.get() || !state.status.get().ok || (!record.get().is_recording && (!state.status.get().resolver_ready || record.get().verification_required)) on:click=move |_| perform.run(Action::Record)>
                    <Show when=move || record.get().is_recording fallback=|| view! { <Icon name="play" size=14 /> }><Icon name="stop" size=14 /></Show>{move || if record.get().is_recording { t("停止录制") } else { t("单次录制") }}
                </button>
                </div>
                <div class="card-tools" role="group" aria-label=t("任务操作")>
                    <CardTool label=t("检测直播状态") icon="refresh"
                        hint=Signal::derive(move || if busy.get() { t("操作正在进行，请稍候") } else if !state.status.get().ok { t("本地服务未连接") } else if record.get().verification_required { t("请先完成快手验证") } else if !state.status.get().resolver_ready { t("直播解析服务尚未就绪") } else if !record.get().monitor_status { t("请先开启监控") } else { t("检测状态（已开播时会启动录制）") }.to_string())
                        disabled=Signal::derive(move || busy.get() || !state.status.get().ok || !record.get().monitor_status || record.get().verification_required || !state.status.get().resolver_ready)
                        on_click=Callback::new(move |_| perform.run(Action::Check)) />
                    <CardTool label=t("预览录制文件") icon="eye"
                        hint=Signal::derive(move || t("预览录制文件").to_string()) disabled=Signal::derive(|| false)
                        on_click=Callback::new(move |_| on_preview.run(record.get_untracked())) />
                    <CardTool label=t("编辑任务") icon="edit"
                        hint=Signal::derive(move || if record.get().is_recording { t("请先停止录制") } else if busy.get() { t("操作正在进行，请稍候") } else if !state.status.get().ok { t("本地服务未连接") } else { t("编辑任务") }.to_string())
                        disabled=Signal::derive(move || busy.get() || record.get().is_recording || !state.status.get().ok)
                        on_click=Callback::new(move |_| on_edit.run(record.get_untracked())) />
                    <CardTool label=t("删除任务") icon="trash" danger=true
                        hint=Signal::derive(move || if record.get().is_recording { t("请先停止录制") } else if busy.get() { t("操作正在进行，请稍候") } else if !state.status.get().ok { t("本地服务未连接") } else { t("移除任务，保留录制文件") }.to_string())
                        disabled=Signal::derive(move || busy.get() || record.get().is_recording || !state.status.get().ok)
                        on_click=Callback::new(move |_| deleting.set(true)) />
                </div>
            </div>
        </article>
        <ConfirmDialog open=deleting title=t("移除直播间") description=Signal::derive(move || crate::tr_format!("将从工作空间移除“{}”。已有录制文件不会被删除。", record.get().name())) busy=busy on_confirm=delete />
    }
}

#[component]
fn CardTool(
    label: &'static str,
    icon: &'static str,
    hint: Signal<String>,
    disabled: Signal<bool>,
    on_click: Callback<()>,
    #[prop(default = false)] danger: bool,
) -> impl IntoView {
    view! {
        <span class="card-tool" data-tooltip=move || hint.get()
            tabindex=move || if disabled.get() { "0" } else { "-1" }
            role="group" aria-label=move || hint.get()>
            <button class=if danger { "icon-button danger-text" } else { "icon-button" }
                type="button" aria-label=label aria-description=move || hint.get()
                disabled=move || disabled.get() on:click=move |_| on_click.run(())>
                <Icon name=icon size=18 />
            </button>
        </span>
    }
}
