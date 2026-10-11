use crate::app::i18n::t;
use crate::{
    api::gateway::{self, Recording, TaskPhase},
    app::{
        components::{
            preview_state, ActionButton, AddRecordingDialog, CardInfoDialog, EmptyState, Icon,
            PageHeader,
        },
        labels::{duration, platform_label, queue_label, queue_time},
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
    let attention = move || {
        state.recording_list.with(|items| {
            items
                .iter()
                .filter(|r| r.phase == TaskPhase::Attention)
                .count()
        })
    };
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
    view! {
        <div class="page home-page">
            <PageHeader title=t("录制工作台") subtitle=t("当前任务与录制状态")>
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
                                    on_preview=Callback::new(move |record| preview.open_recording_mini(record)) />
                            })
                        } />
                    </div>
                </section>
                <Show when=move || media_counts.get() != (0, 0)>
                    <a class="overview-media-link" href="/storage"><Icon name="history" size=17 /><strong>{t("媒体处理")}</strong><span>{move || crate::tr_format!("{} 项处理中 · {} 项需查看", media_counts.get().0, media_counts.get().1)}</span><Icon name="arrow" size=15 /></a>
                </Show>
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
                <small class:danger-text=move || record.with(|record| !record.is_recording && record.needs_attention())
                    title=move || record.with(|record| if record.is_recording { String::new() } else { record.recording_error.as_ref().or(record.check_error.as_ref()).cloned().unwrap_or_default() })>
                    {move || record.with(|record| if record.is_recording { record.speed.clone().unwrap_or_default() }
                        else if let Some(error) = record.recording_error.as_ref().filter(|error| !error.is_empty()).or(record.check_error.as_ref().filter(|error| !error.is_empty())) { error.clone() }
                        else { queue_time(record, state.now.get()) })}
                </small>
            </div>
            <div class="overview-task-action"><ActionButton label=Signal::derive(move || if record.get().is_recording { t("小窗") } else { t("查看") })
                aria_label=Signal::derive(move || if record.get().is_recording { t("小窗预览") } else { t("查看") }.to_string())
                icon=Signal::derive(move || if record.get().is_recording { "picture-in-picture" } else { "arrow" }) variant="ghost" class="small"
                on_click=Callback::new(move |_| { let current = record.get_untracked(); if current.is_recording { on_preview.run(current); } else { on_info.run(current); } }) /></div>
        </article>
    }
}
