use super::{ActionButton, ConfirmDialog, Icon, MediaJobStatus, ToggleSwitch};
use crate::app::i18n::t;
use crate::{
    api::gateway::{self, Recording},
    app::labels::{duration, format_label, monitoring_label, quality_label, recording_media_job},
};
use leptos::{html, prelude::*};
use wasm_bindgen::JsCast;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Check,
    Start,
    Stop,
    Monitor,
}

/// Owned by the request, not the rendered card: removing a row must not release
/// the parent operation lock while its request is still in flight.
struct CardOperation {
    busy: RwSignal<bool>,
    active_action: RwSignal<Option<Action>>,
    on_busy_change: Callback<bool>,
}

impl CardOperation {
    fn begin(
        busy: RwSignal<bool>,
        active_action: RwSignal<Option<Action>>,
        on_busy_change: Callback<bool>,
    ) -> Self {
        busy.set(true);
        let _ = on_busy_change.try_run(true);
        Self {
            busy,
            active_action,
            on_busy_change,
        }
    }
}

impl Drop for CardOperation {
    fn drop(&mut self) {
        let _ = self.active_action.try_set(None);
        let _ = self.busy.try_set(false);
        let _ = self.on_busy_change.try_run(false);
    }
}

#[component]
pub fn RecordingCard(
    recording: Recording,
    on_edit: Callback<Recording>,
    on_info: Callback<Recording>,
    on_preview: Callback<Recording>,
    on_mini_preview: Callback<Recording>,
    #[prop(optional)] selectable: bool,
    #[prop(default = RwSignal::new(Vec::new()))] selected: RwSignal<Vec<String>>,
    #[prop(default = Signal::stored(false), into)] locked: Signal<bool>,
    #[prop(default = Signal::stored(false), into)] selection_locked: Signal<bool>,
    #[prop(default = Callback::new(|_| {}))] on_busy_change: Callback<bool>,
) -> impl IntoView {
    let state = gateway::app_state();
    let preview = super::preview_state();
    let initial = StoredValue::new(recording);
    let record = Memo::new(move |_| {
        initial
            .with_value(|value| state.recording(&value.rec_id))
            .unwrap_or_else(|| initial.get_value())
    });
    let busy = RwSignal::new(false);
    let active_action = RwSignal::new(None::<Action>);
    let deleting = RwSignal::new(false);
    let menu = NodeRef::<html::Details>::new();
    let menu_opens_up = RwSignal::new(false);
    let close_menu = Callback::new(move |_| {
        if let Some(menu) = menu.get() {
            // Dialogs must restore focus to the visible trigger, not a hidden menu item.
            if let Some(summary) = menu
                .query_selector("summary")
                .ok()
                .flatten()
                .and_then(|summary| summary.dyn_into::<web_sys::HtmlElement>().ok())
            {
                let _ = summary.focus();
            }
            menu.set_open(false);
        }
    });
    let media_job = Memo::new(move |_| {
        state
            .media_jobs
            .with(|jobs| record.with(|record| recording_media_job(record, jobs).cloned()))
    });
    let selected_now = move || selected.with(|ids| ids.contains(&record.get().rec_id));
    let checking = move || {
        record.with(|value| matches!(value.check_state.as_str(), "checking" | "rechecking"))
    };
    let perform = Callback::new(move |action: Action| {
        if busy.get_untracked() || locked.get_untracked() || !state.status.get_untracked().ok {
            return;
        }
        let rec = record.get_untracked();
        let unavailable = match action {
            Action::Check => {
                !rec.monitor_status
                    || rec.verification_required
                    || !state.status.get_untracked().resolver_ready
                    || matches!(rec.check_state.as_str(), "checking" | "rechecking")
            }
            Action::Start => {
                rec.is_recording
                    || rec.verification_required
                    || !state.status.get_untracked().resolver_ready
                    || matches!(rec.check_state.as_str(), "checking" | "rechecking")
            }
            Action::Stop => !rec.is_recording,
            Action::Monitor => false,
        };
        if unavailable {
            return;
        }
        active_action.set(Some(action));
        let operation = CardOperation::begin(busy, active_action, on_busy_change);
        leptos::task::spawn_local(async move {
            let _operation = operation;
            let (result, message) = match action {
                Action::Check => (
                    gateway::check_recording(&rec.rec_id).await,
                    t("已更新直播状态"),
                ),
                Action::Stop => (
                    gateway::stop_recording(&rec.rec_id).await,
                    t("已停止本次录制；监控开启时仍可能再次录制"),
                ),
                Action::Start => (gateway::start_recording(&rec.rec_id).await, t("已开始录制")),
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
        });
    });
    let delete = Callback::new(move |_| {
        if busy.get_untracked()
            || locked.get_untracked()
            || record.get_untracked().is_recording
            || !state.status.get_untracked().ok
        {
            return;
        }
        let operation = CardOperation::begin(busy, active_action, on_busy_change);
        let id = record.get_untracked().rec_id;
        leptos::task::spawn_local(async move {
            let _operation = operation;
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
        });
    });
    view! {
        <article class="recording-card" class:selectable=selectable class:selected=selected_now class:is-recording=move || record.get().is_recording class:needs-attention=move || record.get().needs_attention() data-rec-id=move || record.get().rec_id aria-label=move || record.get().name()>
            {selectable.then(|| view! { <label class="task-select"><input class="ui-checkbox ui-checkbox-xs task-checkbox" type="checkbox" aria-label=move || crate::tr_format!("选择{}", record.get().name()) prop:checked=selected_now disabled=move || locked.get() || selection_locked.get() || busy.get() on:change=move |_| {
                if locked.get_untracked() || selection_locked.get_untracked() || busy.get_untracked() { return; }
                let id = record.get_untracked().rec_id;
                selected.update(|ids| if ids.contains(&id) { ids.retain(|value| value != &id); } else { ids.push(id); });
            } /></label> })}
            <div class="card-identity">
                <span class="platform-mark" data-platform=move || record.get().platform_key.unwrap_or_default() aria-hidden="true">{move || record.get().platform.unwrap_or_else(|| t("直播").into()).chars().next().unwrap_or('播').to_string()}</span>
                <div class="card-main">
                    <button class="card-name" type="button" title=t("查看录制信息") on:click=move |_| on_info.run(record.get_untracked())>{move || record.get().name()}</button>
                    <div class="card-platform">{move || record.get().platform.unwrap_or_else(|| t("自定义直播间").into())}</div>
                    <Show when=move || record.with(|r| r.live_title.as_ref().is_some_and(|title| !title.trim().is_empty()) || r.check_state == "rechecking")>
                        <p class="card-subtitle" title=move || record.get().live_title.unwrap_or_default()>{move || crate::app::labels::recording_subtitle(&record.get())}</p>
                    </Show>
                <Show when=move || record.get().check_error.is_some() && record.get().check_state != "rechecking">
                    <div class="card-check-error" role="status" title=move || record.get().check_error.unwrap_or_default()>
                        <span><Icon name="alert" size=14 />{move || crate::tr_format!("检测失败：{}",record.get().check_error.unwrap_or_default())}</span>
                    </div>
                </Show>
                <Show when=move || record.get().recording_error.as_ref().is_some_and(|error| !error.is_empty())>
                    <p class="card-recording-error" role="status"><Icon name="alert" size=14 /><span>{move || crate::tr_format!("录制失败：{}", record.get().recording_error.unwrap_or_default())}</span></p>
                </Show>
                </div>
            </div>
            <div class="card-state">
                <div class="card-head-right"><span class=move || format!("ui-badge ui-badge-sm ui-badge-ghost {}",record.get().status().class()) class:card-recording-status=move || record.get().is_recording title=move || if record.get().is_live && record.get().recording_error.is_some() { t("最近一次平台检测为直播中") } else { "" }><i class="status-dot" />{move || record.get().status().label()}</span></div>
            <Show when=move || !record.get().is_recording && record.get().monitor_status>
                <div class="card-schedule" aria-label=t("检测安排") data-wait-reason=move || record.get().check_wait_reason>
                    <Show when=move || record.with(|r| !matches!(r.check_wait_reason.as_str(), "interval" | "") || r.next_check_at.is_none())>
                        <span class="card-queue-label">{move || crate::app::labels::queue_label(&record.get())}</span>
                    </Show>
                    <span class="card-queue-time">{move || if state.status.get().ok { crate::app::labels::queue_time(&record.get(), state.now.get()) } else { t("本地服务断开，时间待同步").into() }}</span>
                </div>
            </Show>
            </div>
            <div class="card-summary">
                <div class="card-tags"><span>{move || quality_label(record.get().quality.as_deref())}</span><span>{move || format_label(record.get().record_format.as_deref())}</span>{move || record.get().segment_record.unwrap_or(false).then(|| view! { <span>{t("分段")}</span> })}</div>
                <Show when=move || record.get().is_recording fallback=move || view! {
                    <div class="card-monitor-row"><span class:monitor-on=move || record.get().monitor_status><Icon name="eye" size=13 />{move || monitoring_label(&record.get())}</span></div>
                }><span class="card-speed"><Icon name="activity" size=13 /><span class="card-duration">{move || duration(record.get().recorded_seconds)}</span>{move || record.get().speed.unwrap_or_else(|| "—".into())}</span>
                    <Show when=move || !record.get().monitor_status><span class="card-monitor-row"><span>{t("监控已暂停")}</span></span></Show>
                </Show>
            </div>
            <div class="card-controls" role="group" aria-label=t("直播间操作")>
            <div class="card-monitor-setting">
                <ToggleSwitch label=t("自动检测") compact=true
                    hint=t("开启后自动检测开播并录制；关闭会停止监控和当前录制。")
                    checked=Signal::derive(move || record.get().monitor_status)
                    busy=Signal::derive(move || active_action.get() == Some(Action::Monitor))
                    disabled=Signal::derive(move || locked.get() || busy.get() || !state.status.get().ok)
                    on_change=Callback::new(move |enabled| {
                        if enabled != record.get_untracked().monitor_status { perform.run(Action::Monitor); }
                    }) />
            </div>
            <div class="card-preview-actions" role="group" aria-label=t("预览操作")>
                <ActionButton label=t("预览") icon="eye" class="card-preview"
                    aria_label=t("预览录制文件").to_string() title=t("预览录制文件")
                    pressed=Signal::derive(move || Some(preview.recording_is_open(&record.get().rec_id, false)))
                    on_click=Callback::new(move |_| on_preview.run(record.get_untracked())) />
                <ActionButton label=t("小窗") icon="picture-in-picture" class="card-preview"
                    aria_label=t("小窗预览").to_string() title=t("小窗预览")
                    pressed=Signal::derive(move || Some(preview.recording_is_open(&record.get().rec_id, true)))
                    on_click=Callback::new(move |_| on_mini_preview.run(record.get_untracked())) />
            </div>
            <div class="card-actions">
                    <Show when=move || active_action.get() != Some(Action::Start) && (record.get().is_recording || active_action.get() == Some(Action::Stop))
                        fallback=move || view! {
                            <ActionButton label=t("单次录制") icon="play" class="card-record"
                                title=t("立即尝试录制，不改变自动监控设置") busy_label=t("正在启动…")
                                busy=Signal::derive(move || active_action.get() == Some(Action::Start))
                                disabled=Signal::derive(move || locked.get() || busy.get() || checking() || !state.status.get().ok || !state.status.get().resolver_ready || record.get().verification_required)
                                on_click=Callback::new(move |_| perform.run(Action::Start)) />
                        }>
                        <ActionButton label=t("停止录制") icon="stop" variant="danger" class="card-record recording"
                            title=t("停止本次录制，不关闭自动监控") busy_label=t("正在停止…")
                            busy=Signal::derive(move || active_action.get() == Some(Action::Stop))
                            disabled=Signal::derive(move || locked.get() || busy.get() || !state.status.get().ok)
                            on_click=Callback::new(move |_| perform.run(Action::Stop)) />
                    </Show>
                <details node_ref=menu class="ui-dropdown ui-dropdown-end card-menu card-tools" name="streamcap-task-menu"
                    class:card-menu-above=move || menu_opens_up.get()
                    role="group" aria-label=t("任务操作")
                    on:toggle=move |_| {
                        let Some(menu) = menu.get() else { return; };
                        if !menu.open() {
                            menu_opens_up.set(false);
                            return;
                        }
                        let Some(panel) = menu.query_selector(".card-menu-panel").ok().flatten() else { return; };
                        let Some(content) = document().get_element_by_id("main-content") else { return; };
                        let anchor = menu.get_bounding_client_rect();
                        let viewport = content.get_bounding_client_rect();
                        let needed = panel.get_bounding_client_rect().height() + 9.0;
                        let below = viewport.bottom() - anchor.bottom();
                        let above = anchor.top() - viewport.top();
                        menu_opens_up.set(below < needed && above > below);
                    }
                    on:keydown=move |event: leptos::ev::KeyboardEvent| {
                        if event.key() == "Escape" {
                            event.prevent_default();
                            close_menu.run(());
                        }
                    }>
                    <summary class="ui-btn ui-btn-ghost ui-btn-sm ui-btn-square" role="button" aria-label=t("更多操作") title=t("更多操作")><Icon name="more" size=18 /></summary>
                    <ul class="ui-menu ui-dropdown-content card-menu-panel">
                    <CardTool label=t("检测直播状态") icon="refresh"
                        hint=Signal::derive(move || if locked.get() { t("正在执行批量操作") } else if checking() || active_action.get() == Some(Action::Check) { t("正在检测直播状态…") } else if busy.get() { t("操作正在进行，请稍候") } else if !state.status.get().ok { t("本地服务未连接") } else if record.get().verification_required { t("平台验证受限，自动检测已暂停") } else if !state.status.get().resolver_ready { t("直播解析服务尚未就绪") } else if !record.get().monitor_status { t("请先开启自动检测") } else { t("检测状态（已开播时会启动录制）") }.to_string())
                        busy=Signal::derive(move || checking() || active_action.get() == Some(Action::Check))
                        disabled=Signal::derive(move || locked.get() || busy.get() || checking() || !state.status.get().ok || !record.get().monitor_status || record.get().verification_required || !state.status.get().resolver_ready)
                        on_click=Callback::new(move |_| { close_menu.run(()); perform.run(Action::Check); }) />
                    <CardTool label=t("编辑任务") icon="edit"
                        hint=Signal::derive(move || if locked.get() { t("正在执行批量操作") } else if record.get().is_recording { t("请先停止录制") } else if busy.get() { t("操作正在进行，请稍候") } else if !state.status.get().ok { t("本地服务未连接") } else { t("编辑任务") }.to_string())
                        disabled=Signal::derive(move || locked.get() || busy.get() || record.get().is_recording || !state.status.get().ok)
                        on_click=Callback::new(move |_| { close_menu.run(()); on_edit.run(record.get_untracked()); }) />
                    <CardTool label=t("移除任务") icon="trash" danger=true
                        hint=Signal::derive(move || if locked.get() { t("正在执行批量操作") } else if record.get().is_recording { t("请先停止录制") } else if busy.get() { t("操作正在进行，请稍候") } else if !state.status.get().ok { t("本地服务未连接") } else { t("移除任务，保留录制文件") }.to_string())
                        disabled=Signal::derive(move || locked.get() || busy.get() || record.get().is_recording || !state.status.get().ok)
                        on_click=Callback::new(move |_| { close_menu.run(()); deleting.set(true); }) />
                    </ul>
                </details>
            </div>
            </div>
            <MediaJobStatus job=media_job />
        </article>
        <ConfirmDialog open=deleting title=t("移除直播间") description=Signal::derive(move || crate::tr_format!("将从工作空间移除“{}”。已有录制文件不会被删除。", record.get().name()))
            busy=busy disabled=Signal::derive(move || locked.get() || record.get().is_recording || !state.status.get().ok) on_confirm=delete />
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
    #[prop(default = Signal::stored(false), into)] busy: Signal<bool>,
) -> impl IntoView {
    view! {
        <li class="card-tool" data-tooltip=move || hint.get()
            tabindex=move || if disabled.get() || busy.get() { "0" } else { "-1" }
            role="group" aria-label=move || hint.get()>
            <ActionButton label=label icon=icon variant=if danger { "ghost-danger" } else { "ghost" }
                busy=busy disabled=disabled on_click=on_click />
        </li>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn pending_card_operation_outlives_its_row_and_notifies_once() {
        let parent = Owner::new();
        let notifications = Arc::new(Mutex::new(Vec::new()));
        let observed = notifications.clone();
        let (row, operation) = parent.with(|| {
            let callback = Callback::new(move |busy| observed.lock().unwrap().push(busy));
            let row = Owner::new();
            let operation = row.with(|| {
                CardOperation::begin(
                    RwSignal::new(false),
                    RwSignal::new(Some(Action::Start)),
                    callback,
                )
            });
            (row, operation)
        });
        assert_eq!(*notifications.lock().unwrap(), vec![true]);
        row.cleanup();
        assert_eq!(*notifications.lock().unwrap(), vec![true]);
        drop(operation);
        assert_eq!(*notifications.lock().unwrap(), vec![true, false]);
        row.cleanup();
        assert_eq!(*notifications.lock().unwrap(), vec![true, false]);
    }

    #[test]
    fn pending_card_operation_can_finish_after_parent_disposal() {
        let parent = Owner::new();
        let operation = parent.with(|| {
            CardOperation::begin(
                RwSignal::new(false),
                RwSignal::new(Some(Action::Stop)),
                Callback::new(|_| {}),
            )
        });
        parent.cleanup();
        drop(operation);
    }
}
