use crate::app::i18n::t;
use crate::{
    api::gateway::{self, Recording, TaskPhase},
    app::{
        components::{
            preview_state, ActionButton, AddRecordingDialog, CardInfoDialog, EmptyState, Icon,
            PageHeader,
        },
        labels::{duration, format_label, platform_label, quality_label, queue_label, queue_time},
    },
};
use leptos::prelude::*;

#[component]
pub fn HomeView() -> impl IntoView {
    let state = gateway::app_state();
    let add = RwSignal::new(false);
    let info = RwSignal::new(None::<Recording>);
    let preview = preview_state();
    let media_counts = Memo::new(move |_| {
        state.media_jobs.with(|jobs| {
            (
                jobs.iter().filter(|job| job.pending()).count(),
                crate::app::labels::media_attention_count(jobs),
            )
        })
    });
    let monitoring_counts = Memo::new(move |_| {
        state.recordings.with(|records| {
            (
                records.len(),
                records
                    .iter()
                    .filter(|record| record.monitor_status)
                    .count(),
            )
        })
    });
    let active = move || {
        state.recording_list.with(|items| {
            items
                .iter()
                .filter(|r| r.phase == TaskPhase::Recording)
                .count()
        })
    };
    let waiting = move || {
        state.recording_list.with(|items| {
            items
                .iter()
                .filter(|r| r.phase == TaskPhase::Waiting)
                .count()
        })
    };
    let live_idle = move || {
        state.recording_list.with(|items| {
            items
                .iter()
                .filter(|r| r.phase == TaskPhase::LiveIdle)
                .count()
        })
    };
    let attention = move || {
        state.recording_list.with(|items| {
            items
                .iter()
                .filter(|r| r.phase == TaskPhase::Attention)
                .count()
        })
    };
    let queue_counts = Memo::new(move |_| {
        state.recordings.with(|records| {
            records
                .iter()
                .filter(|r| r.monitor_status && !r.is_recording)
                .fold([0usize; 4], |mut counts, r| {
                    if matches!(r.check_state.as_str(), "checking" | "rechecking") {
                        counts[0] += 1;
                    } else if r.check_wait_reason == "cooldown" {
                        counts[3] += 1;
                    } else if r.check_state == "recovering" {
                        counts[2] += 1;
                    } else if r.check_state == "queued" {
                        counts[1] += 1;
                    }
                    counts
                })
        })
    });
    let recent = Memo::new(move |_| {
        state.recording_list.with(|list| {
            let mut items: Vec<_> = list.iter().collect();
            items.sort_by_cached_key(|record| (record.priority, record.name.clone()));
            items
                .into_iter()
                .take(6)
                .map(|record| record.rec_id.clone())
                .collect::<Vec<_>>()
        })
    });
    let postprocess = move || {
        if !state.settings.with(|settings| {
            settings
                .get("convert_to_mp4")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        }) {
            t("保留录制原文件")
        } else if state.settings.with(|settings| {
            settings
                .get("delete_original")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        }) {
            t("转 MP4 · 校验后清理 TS")
        } else {
            t("转 MP4 · 保留 TS")
        }
    };
    let runtime_title = move || {
        if state.loading.get() {
            t("正在载入工作台").into()
        } else if !state.status.get().ok {
            t("本地服务未连接").into()
        } else if active() > 0 {
            crate::tr_format!("{} 路直播，正在记录", active())
        } else if attention() > 0 {
            t("有任务需要确认状态").into()
        } else if live_idle() > 0 {
            t("直播已开播，当前未录制").into()
        } else if monitoring_counts.get().0 == 0 {
            t("添加直播间，开始自动录制").into()
        } else if monitoring_counts.get().1 == 0 {
            t("全部任务已暂停监控").into()
        } else {
            t("正在等待直播开播").into()
        }
    };
    let runtime_description = move || {
        if state.loading.get() {
            t("正在读取任务与运行状态…")
        } else if !state.status.get().ok {
            t("连接恢复后将同步任务状态，不会因刷新重复检测。")
        } else if monitoring_counts.get().0 == 0 {
            t("尚未添加直播间，当前没有自动监控任务。")
        } else if attention() > 0 {
            t("有需要处理的任务，请查看下方提示。")
        } else if monitoring_counts.get().1 == 0 {
            t("自动监控已暂停；现有录制不受页面刷新影响。")
        } else if live_idle() > 0 {
            t("请查看任务的录制与通知设置。")
        } else {
            t("按各任务的监控、时间窗与通知设置自动检测。")
        }
    };
    view! {
        <div class="page home-page">
            <PageHeader title=t("录制工作台") subtitle=t("当前任务与本机运行状态")>
                <ActionButton label=t("添加直播间") icon="plus" variant="primary"
                    disabled=Signal::derive(move || !state.status.get().ok)
                    on_click=Callback::new(move |_| add.set(true)) />
            </PageHeader>
            <dl class="overview-summary" aria-label=t("录制概况")>
                <div><dt>{t("全部直播间")}</dt><dd>{move || state.recording_list.with(Vec::len)}</dd></div>
                <div><dt>{t("正在录制")}</dt><dd>{active}</dd></div>
                <div><dt>{t("等待开播")}</dt><dd>{waiting}</dd></div>
                <div class:has-attention=move || attention() != 0><dt>{t("需关注")}</dt><dd>{attention}</dd></div>
            </dl>
            <div class="overview-layout">
                <div class="overview-main">
                    <section class="overview-tasks">
                        <header class="overview-section-heading"><div><h2>{t("当前活动")}</h2><p>{t("失败与正在录制的任务优先显示")}</p></div><a class="text-link" href="/recordings">{t("查看全部")}<Icon name="arrow" size=15 /></a></header>
                        <Show when=move || state.loading.get()><div class="overview-loading" aria-label=t("加载任务")><div class="ui-skeleton" /><div class="ui-skeleton" /><div class="ui-skeleton" /></div></Show>
                        <Show when=move || !state.loading.get() && recent.get().is_empty()><EmptyState icon="video" title=t("还没有直播间") description=t("添加直播间后，开播时会自动开始录制。") /></Show>
                        <Show when=move || !recent.get().is_empty()><div class="overview-list-heading" aria-hidden="true"><span>{t("直播间")}</span><span>{t("状态")}</span><span>{t("检测与录制")}</span><span>{t("操作")}</span></div></Show>
                        <div class="overview-task-list">
                            <For each=move || recent.get() key=|id| id.clone() children=move |id| {
                                state.recording_untracked(&id).map(|recording| view! {
                                    <OverviewTaskRow recording=recording on_info=Callback::new(move |record| info.set(Some(record)))
                                        on_preview=Callback::new(move |record| preview.open_recording(record)) />
                                })
                            } />
                        </div>
                    </section>
                    <Show when=move || media_counts.get() != (0, 0)>
                        <a class="overview-media-link" href="/storage"><Icon name="history" size=17 /><strong>{t("媒体处理")}</strong><span>{move || crate::tr_format!("{} 项处理中 · {} 项需查看", media_counts.get().0, media_counts.get().1)}</span><Icon name="arrow" size=15 /></a>
                    </Show>
                </div>
                <aside class="overview-aside" aria-label=t("录制运行信息")>
                    <section class="overview-health">
                        <h2>{t("本机状态")}</h2>
                        <div class="overview-service" role="status"><span class:healthy=move || state.status.get().ok><i class="status-dot" />{move || if state.status.get().ok { t("本地服务已连接") } else { t("本地服务未连接") }}</span><span>{move || if state.status.get().resolver_ready { t("解析已就绪") } else { t("解析暂不可用") }}</span></div>
                        <strong class="overview-runtime-title">{runtime_title}</strong>
                        <p class="overview-runtime-description">{runtime_description}</p>
                        <dl class="overview-facts"><div><dt>{t("自动监控")}</dt><dd>{move || crate::tr_format!("{} / {} 项", monitoring_counts.get().1, monitoring_counts.get().0)}</dd></div></dl>
                    </section>
                    <section class="overview-queue-section" aria-label=t("检测队列")>
                        <h2>{t("检测队列")}</h2>
                        <dl class="overview-queue"><div><dt>{t("检测中")}</dt><dd>{move || queue_counts.get()[0]}</dd></div><div><dt>{t("排队中")}</dt><dd>{move || queue_counts.get()[1]}</dd></div>
                            <div><dt>{t("断流恢复")}</dt><dd>{move || queue_counts.get()[2]}</dd></div><div><dt>{t("平台冷却中")}</dt><dd>{move || queue_counts.get()[3]}</dd></div></dl>
                    </section>
                    <details class="overview-policy"><summary>{t("当前录制方案")}</summary><dl class="overview-facts">
                        <div><dt>{t("画质与格式")}</dt><dd>{move || format!("{} · {}", quality_label(Some(&state.setting("record_quality", "OD"))), format_label(Some(&state.setting("video_format", "TS"))))}</dd></div>
                        <div><dt>{t("检测间隔")}</dt><dd>{move || crate::app::labels::interval_label(&state.setting("loop_time_seconds","300"))}</dd></div>
                        <div><dt>{t("录制后处理")}</dt><dd>{postprocess}</dd></div></dl>
                        <a class="text-link" href="/settings">{t("调整设置")}<Icon name="arrow" size=14 /></a>
                    </details>
                </aside>
            </div>
            <AddRecordingDialog open=add /><CardInfoDialog target=info />
        </div>
    }
}

#[component]
fn OverviewTaskRow(
    recording: Recording,
    on_info: Callback<Recording>,
    on_preview: Callback<Recording>,
) -> impl IntoView {
    let state = gateway::app_state();
    let initial = StoredValue::new(recording);
    let record = Memo::new(move |_| {
        initial.with_value(|initial| {
            state
                .recording(&initial.rec_id)
                .unwrap_or_else(|| initial.clone())
        })
    });
    view! {
        <article class="overview-task-row" data-rec-id=move || record.get().rec_id aria-label=move || record.get().name()>
            <div class="overview-task-identity"><span class="overview-task-initial" aria-hidden="true">{move || record.get().name().chars().next().unwrap_or('播').to_string()}</span>
                <div><strong>{move || record.get().name()}</strong><small>{move || record.with(|record| platform_label(record.platform_key.as_deref().unwrap_or("custom")))}</small></div>
            </div>
            <div class="overview-task-status"><span class=move || format!("ui-badge ui-badge-sm {}", record.get().status().class())>{move || record.get().status().label()}</span></div>
            <div class="overview-task-detail">
                <strong>{move || record.with(|record| if record.is_recording { duration(record.recorded_seconds) } else { queue_label(record) })}</strong>
                <small>{move || record.with(|record| if record.is_recording { record.speed.clone().unwrap_or_default() } else if let Some(error) = &record.check_error { error.clone() } else { queue_time(record, state.now.get()) })}</small>
            </div>
            <div class="overview-task-action"><ActionButton label=Signal::derive(move || if record.get().is_recording { t("预览") } else { t("查看") })
                icon=Signal::derive(move || if record.get().is_recording { "play" } else { "arrow" }) variant="ghost" class="small"
                on_click=Callback::new(move |_| { let current = record.get_untracked(); if current.is_recording { on_preview.run(current); } else { on_info.run(current); } }) /></div>
        </article>
    }
}
