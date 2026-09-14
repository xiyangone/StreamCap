use crate::app::i18n::t;
use crate::{
    api::gateway::{self, Recording},
    app::{
        components::{
            AddRecordingDialog, CardInfoDialog, EditRecordingDialog, EmptyState, Icon,
            PreviewDialog, RecordingCard,
        },
        labels::{format_label, quality_label, recording_priority},
    },
};
use leptos::prelude::*;

#[component]
pub fn HomeView() -> impl IntoView {
    let state = gateway::app_state();
    let add = RwSignal::new(false);
    let editing = RwSignal::new(None::<Recording>);
    let info = RwSignal::new(None::<Recording>);
    let preview = RwSignal::new(None::<Recording>);
    let active = move || {
        state
            .recordings
            .with(|items| items.iter().filter(|r| r.is_recording).count())
    };
    let waiting = move || {
        state.recordings.with(|items| {
            items
                .iter()
                .filter(|r| r.monitor_status && !r.is_recording)
                .count()
        })
    };
    let attention = move || {
        state.recordings.with(|items| {
            items
                .iter()
                .filter(|r| {
                    !r.is_recording && r.recording_error.as_ref().is_some_and(|e| !e.is_empty())
                })
                .count()
        })
    };
    let recent = move || {
        let mut items = state.recordings.get();
        items.sort_by_key(|record| (recording_priority(record), record.name().to_lowercase()));
        items.into_iter().take(6).collect::<Vec<_>>()
    };
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
            <header class="page-header"><div><h1>{t("录制工作台")}</h1><p>{t("添加即监控，开播自动录制；在这里查看进度和异常。")}</p></div><button class="button primary" on:click=move |_| add.set(true)><Icon name="plus" size=18 />{t("添加直播间")}</button></header>
            <div class="stats-strip glass">
                <div class="stat-item"><span class="stat-icon blue"><Icon name="video" /></span><div><span class="stat-label">{t("全部直播间")}</span><strong>{move || state.recordings.get().len()}</strong></div></div>
                <div class="stat-item"><span class="stat-icon green"><Icon name="signal" /></span><div><span class="stat-label">{t("正在录制")}</span><strong>{active}<small>{t("路")}</small></strong></div></div>
                <div class="stat-item"><span class="stat-icon cyan"><Icon name="eye" /></span><div><span class="stat-label">{t("监控中")}</span><strong>{waiting}</strong></div></div>
                <div class="stat-item"><span class="stat-icon attention"><Icon name="alert" /></span><div><span class="stat-label">{t("需关注")}</span><strong>{attention}</strong></div></div>
            </div>
            <div class="overview-grid workbench-overview">
                <section class="capture-panel glass">
                    <div class="capture-copy"><span class="live-label"><i class="status-dot" />{t("自动录制")}</span>
                        <h2>{move || if active() > 0 { crate::tr_format!("{} 路直播，正在记录", active()) } else if waiting() > 0 { t("正在等待直播开播").into() } else { t("添加直播间，开始自动录制").into() }}</h2>
                        <p>{move || if attention() > 0 { t("有录制未成功的任务，请查看下方提示。") } else if waiting() > 0 || active() > 0 { t("监控已开启，无需逐个点击录制。") } else { t("新增直播间会自动开启监控。") }}</p>
                        <div class="workbench-actions"><a class="button secondary small" href="/recordings">{t("管理直播间")}<Icon name="arrow" size=15 /></a><a class="text-link" href="/storage">{t("打开媒体库")}<Icon name="folder" size=15 /></a></div>
                        <div class="workbench-service" role="status"><span class:healthy=move || state.status.get().ok><i class="status-dot" />{move || if state.status.get().ok { t("本地服务已连接") } else {t("本地服务未连接")}}</span><span class:unhealthy=move || !state.status.get().resolver_ready>{move || if state.status.get().resolver_ready {t("解析已就绪")} else {t("解析暂不可用")}}</span></div>
                    </div>
                </section>
                <section class="health-panel glass recording-plan"><div class="section-heading compact"><h2>{t("当前录制方案")}</h2><a class="text-link" href="/settings">{t("调整设置")}<Icon name="chevron" size=14 /></a></div>
                    <div class="health-row"><span>{t("画质与格式")}</span><strong>{move || format!("{} · {}", quality_label(Some(&state.setting("record_quality", "OD"))), format_label(Some(&state.setting("video_format", "TS"))))}</strong></div>
                    <div class="health-row"><span>{t("检测间隔")}</span><strong>{move || crate::tr_format!("每 {} 秒",state.setting("loop_time_seconds","300"))}</strong></div>
                    <div class="health-row"><span>{t("录制后处理")}</span><strong>{postprocess}</strong></div>
                </section>
            </div>
            <section class="recent-section"><div class="section-heading"><div><h2>{t("优先关注")}<span class="count-pill">{move || state.recordings.get().len()}</span></h2><p>{t("失败与正在录制的任务优先显示")}</p></div><a class="text-link" href="/recordings">{t("查看全部")}<Icon name="arrow" size=15 /></a></div>
                <Show when=move || state.loading.get()><div class="skeleton-grid" aria-label=t("正在加载任务")><div class="skeleton" /><div class="skeleton" /><div class="skeleton" /></div></Show>
                <Show when=move || !state.loading.get() && state.recordings.get().is_empty()><div class="glass empty-panel"><EmptyState icon="video" title=t("你的直播间，从这里开始") description=t("添加第一个直播间，开启属于你的录制工作空间。") /><button class="button primary" on:click=move |_| add.set(true)><Icon name="plus" size=17 />{t("添加第一个直播间")}</button></div></Show>
                <div class="cards-grid">
                    <For each=recent key=|rec| rec.rec_id.clone() children=move |rec| view! {
                        <RecordingCard recording=rec on_edit=Callback::new(move |r| editing.set(Some(r))) on_info=Callback::new(move |r| info.set(Some(r))) on_preview=Callback::new(move |r| preview.set(Some(r))) />
                    } />
                </div>
            </section>
            <footer class="page-footer"><span><Icon name="shield" size=14 />{t("录制文件保存在本地")}</span><span>{t("专注此刻，留存精彩")}</span></footer>
        </div>
        <AddRecordingDialog open=add /><EditRecordingDialog target=editing /><CardInfoDialog target=info /><PreviewDialog target=preview />
    }
}
