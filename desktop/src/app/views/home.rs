use crate::app::i18n::t;
use crate::{
    api::gateway::{self, Recording, TaskPhase},
    app::{
        components::{
            preview_state, AddRecordingDialog, CardInfoDialog, EditRecordingDialog, EmptyState,
            Icon, Metric, PageHeader, RecordingCard,
        },
        labels::{format_label, quality_label},
    },
};
use leptos::prelude::*;

#[component]
pub fn HomeView() -> impl IntoView {
    let state = gateway::app_state();
    let add = RwSignal::new(false);
    let editing = RwSignal::new(None::<Recording>);
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
    view! {
        <div class="page home-page">
            <PageHeader title=t("录制工作台") subtitle=t("录制、监控与媒体处理，一目了然。")>
                <button class="button primary" on:click=move |_| add.set(true)><Icon name="plus" size=17 />{t("添加直播间")}</button>
            </PageHeader>
            <div class="stats-strip" aria-label=t("录制概况")>
                <Metric label=t("全部直播间") icon="video" value=Signal::derive(move || state.recording_list.with(Vec::len).to_string()) />
                <Metric label=t("正在录制") icon="activity" value=Signal::derive(move || active().to_string()) tone="success" />
                <Metric label=t("等待开播") icon="clock" value=Signal::derive(move || waiting().to_string()) tone="accent" />
                <Metric label=t("需关注") icon="alert" value=Signal::derive(move || attention().to_string()) tone="danger" />
            </div>
            <div class="dashboard-layout">
                <div class="dashboard-primary">
                    <section class="recent-section">
                        <div class="section-heading"><div><h2>{t("优先关注")}</h2><p>{t("失败与正在录制的任务优先显示")}</p></div><a class="text-link" href="/recordings">{t("查看全部")}<Icon name="arrow" size=15 /></a></div>
                        <Show when=move || state.loading.get()><div class="skeleton-grid" aria-label=t("加载任务")><div class="skeleton" /><div class="skeleton" /></div></Show>
                        <Show when=move || !state.loading.get() && recent.get().is_empty()><EmptyState icon="video" title=t("还没有直播间") description=t("添加直播间后，开播时会自动开始录制。") /></Show>
                        <div class="cards-grid list-view home-tasks">
                            <For each=move || recent.get() key=|id| id.clone() children=move |id| {
                                let recording = state.recording(&id).unwrap_or_default();
                                view! { <RecordingCard recording=recording on_edit=Callback::new(move |rec| editing.set(Some(rec))) on_info=Callback::new(move |rec| info.set(Some(rec))) on_preview=Callback::new(move |rec| preview.open_recording(rec)) /> }
                            } />
                        </div>
                    </section>
                    <Show when=move || media_counts.get() != (0, 0)>
                        <a class="processing-summary" href="/storage"><span class="summary-icon"><Icon name="history" size=21 /></span><div><strong>{t("媒体处理")}</strong><p>{move || crate::tr_format!("{} 项处理中 · {} 项需查看", media_counts.get().0, media_counts.get().1)}</p></div><Icon name="arrow" size=17 /></a>
                    </Show>
                </div>
                <aside class="dashboard-secondary" aria-label=t("录制运行信息")>
                    <section class="runtime-panel panel">
                        <div class="section-heading compact"><h2><Icon name="activity" size=16 />{t("自动录制")}</h2></div>
                        <strong class="runtime-title">{move || if active() > 0 { crate::tr_format!("{} 路直播，正在记录", active()) } else if live_idle() > 0 { t("直播已开播，当前未录制").into() } else if attention() > 0 { t("有任务需要确认状态").into() } else if waiting() > 0 { t("正在等待直播开播").into() } else { t("添加直播间，开始自动录制").into() }}</strong>
                        <p class="runtime-description">{move || if attention() > 0 { t("有需要处理的任务，请查看下方提示。") } else if live_idle() > 0 { t("请查看任务的录制与通知设置。") } else { t("监控已开启，无需逐个点击录制。") }}</p>
                        <div class="workbench-service" role="status"><span class:healthy=move || state.status.get().ok><i class="status-dot" />{move || if state.status.get().ok { t("本地服务已连接") } else { t("本地服务未连接") }}</span><span class:unhealthy=move || !state.status.get().resolver_ready>{move || if state.status.get().resolver_ready { t("解析已就绪") } else { t("解析暂不可用") }}</span></div>
                    </section>
                    <section class="queue-overview panel" aria-label=t("检测队列")>
                        <div class="section-heading compact"><h2><Icon name="refresh" size=16 />{t("检测队列")}</h2></div>
                        <div class="queue-stat"><span>{t("检测中")}</span><b>{move || queue_counts.get()[0]}</b></div><div class="queue-stat"><span>{t("排队中")}</span><b>{move || queue_counts.get()[1]}</b></div>
                        <div class="queue-stat"><span>{t("断流恢复")}</span><b>{move || queue_counts.get()[2]}</b></div><div class="queue-stat"><span>{t("平台冷却中")}</span><b>{move || queue_counts.get()[3]}</b></div>
                    </section>
                    <section class="recording-plan panel"><div class="section-heading compact"><h2>{t("当前录制方案")}</h2><a class="text-link" href="/settings" aria-label=t("调整设置")><Icon name="sliders" size=16 /></a></div>
                        <div class="health-row"><span>{t("画质与格式")}</span><strong>{move || format!("{} · {}", quality_label(Some(&state.setting("record_quality", "OD"))), format_label(Some(&state.setting("video_format", "TS"))))}</strong></div>
                        <div class="health-row"><span>{t("检测间隔")}</span><strong>{move || crate::app::labels::interval_label(&state.setting("loop_time_seconds","300"))}</strong></div>
                        <div class="health-row"><span>{t("录制后处理")}</span><strong>{postprocess}</strong></div>
                    </section>
                </aside>
            </div>
            <AddRecordingDialog open=add /><EditRecordingDialog target=editing /><CardInfoDialog target=info />
        </div>
    }
}
