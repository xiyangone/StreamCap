use crate::{
    api::gateway::{self, Recording},
    app::{
        components::{
            AddRecordingDialog, CardInfoDialog, EditRecordingDialog, EmptyState, Icon,
            PreviewDialog, RecordingCard,
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
    let paused = move || {
        state
            .recordings
            .with(|items| items.iter().filter(|r| !r.monitor_status).count())
    };
    view! {
        <div class="page home-page">
            <header class="page-header"><div><span class="eyebrow">"WORKSPACE OVERVIEW"</span><h1>"录制工作台"</h1><p>"从直播到珍藏，把每一个精彩瞬间留下。"</p></div><button class="button primary" on:click=move |_| add.set(true)><Icon name="plus" size=18 />"添加直播间"</button></header>
            <div class="stats-strip glass">
                <div class="stat-item"><span class="stat-icon blue"><Icon name="video" /></span><div><span class="stat-label">"全部直播间"</span><strong>{move || state.recordings.get().len()}</strong></div></div>
                <div class="stat-item"><span class="stat-icon green"><Icon name="signal" /></span><div><span class="stat-label">"正在录制"</span><strong>{active}<small>"路"</small></strong></div></div>
                <div class="stat-item"><span class="stat-icon cyan"><Icon name="eye" /></span><div><span class="stat-label">"等待开播"</span><strong>{waiting}</strong></div></div>
                <div class="stat-item"><span class="stat-icon muted"><Icon name="pause" /></span><div><span class="stat-label">"暂停监控"</span><strong>{paused}</strong></div></div>
            </div>
            <div class="overview-grid">
                <section class="capture-panel glass">
                    <div class="capture-copy"><span class="live-label"><i class="status-dot" />"CAPTURE YOUR MOMENTS"</span>
                        <h2>{move || if active() > 0 { format!("{} 路直播，正在记录", active()) } else { "下一份精彩，随时就绪".into() }}</h2>
                        <p>{move || if active() > 0 { "录制在本地持续进行，你可以随时查看状态和文件。" } else { "添加直播间并开启监控，开播时自动开始录制。" }}</p>
                        <a class="button subtle" href="/recordings">"查看录制任务"<Icon name="arrow" size=16 /></a>
                    </div>
                    <div class="capture-art" aria-hidden="true" class:active=move || { active() > 0 }><div class="orbit orbit-outer" /><div class="orbit orbit-inner" /><div class="record-disc"><span /></div><span class="art-chip chip-video"><Icon name="video" size=19 /></span><span class="art-chip chip-wave"><i /><i /><i /><i /><i /></span></div>
                </section>
                <section class="health-panel glass"><div class="section-heading compact"><h2>"工作空间状态"</h2><span class="live-mini">"LIVE"</span></div>
                    <div class="health-row"><span><Icon name="monitor" size=17 />"本地服务"</span><strong class:healthy=move || state.status.get().ok>{move || if state.status.get().ok { "已连接" } else { "未连接" }}</strong></div>
                    <div class="health-row"><span><Icon name="link" size=17 />"平台解析"</span><strong class:healthy=move || state.status.get().resolver_ready class:unhealthy=move || !state.status.get().resolver_ready>{move || if state.status.get().resolver_ready { "已就绪" } else { "不可用" }}</strong></div>
                    <div class="health-divider" />
                    <a class="preference-summary" href="/settings"><span class="preference-icon"><Icon name="settings" size=20 /></span><div><small>"默认录制偏好"</small><strong>{move || format!("{} · {}", quality_label(Some(&state.setting("record_quality", "OD"))), format_label(Some(&state.setting("video_format", "TS"))))}</strong></div><Icon name="chevron" size=15 /></a>
                </section>
            </div>
            <section class="recent-section"><div class="section-heading"><div><h2>"直播间动态"<span class="count-pill">{move || state.recordings.get().len()}</span></h2><p>"录制状态实时更新"</p></div><a class="text-link" href="/recordings">"查看全部"<Icon name="arrow" size=15 /></a></div>
                <Show when=move || state.loading.get()><div class="skeleton-grid" aria-label="正在加载任务"><div class="skeleton" /><div class="skeleton" /><div class="skeleton" /></div></Show>
                <Show when=move || !state.loading.get() && state.recordings.get().is_empty()><div class="glass empty-panel"><EmptyState icon="video" title="你的直播间，从这里开始" description="添加第一个直播间，开启属于你的录制工作空间。" /><button class="button primary" on:click=move |_| add.set(true)><Icon name="plus" size=17 />"添加第一个直播间"</button></div></Show>
                <div class="cards-grid">
                    <For each=move || { state.recordings.get().into_iter().take(6).collect::<Vec<_>>() } key=|rec| rec.rec_id.clone() children=move |rec| view! {
                        <RecordingCard recording=rec on_edit=Callback::new(move |r| editing.set(Some(r))) on_info=Callback::new(move |r| info.set(Some(r))) on_preview=Callback::new(move |r| preview.set(Some(r))) />
                    } />
                </div>
            </section>
            <footer class="page-footer"><span><Icon name="shield" size=14 />"录制文件保存在本地"</span><span>"专注此刻，留存精彩"</span></footer>
        </div>
        <AddRecordingDialog open=add /><EditRecordingDialog target=editing /><CardInfoDialog target=info /><PreviewDialog target=preview />
    }
}
