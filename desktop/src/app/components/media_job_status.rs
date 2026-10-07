use super::Icon;
use crate::{
    api::gateway::{self, MediaJob},
    app::{
        i18n::{message, t},
        labels::{media_phase, media_progress},
    },
};
use leptos::prelude::*;

/// One stable presentation for live task progress and media-library history.
#[component]
pub fn MediaJobStatus(
    #[prop(into)] job: Signal<Option<MediaJob>>,
    #[prop(default = false)] compact: bool,
) -> impl IntoView {
    let state = gateway::app_state();
    let busy = RwSignal::new(false);
    let pending = move || job.with(|value| value.as_ref().is_some_and(MediaJob::pending));
    let attention = move || job.with(|value| value.as_ref().is_some_and(MediaJob::needs_attention));
    let phase = Memo::new(move |_| job.with(|value| value.as_ref().map(media_phase).unwrap_or("")));
    let source = Memo::new(move |_| {
        job.with(|value| {
            value
                .as_ref()
                .map(|job| job.source.clone())
                .unwrap_or_default()
        })
    });
    let blocked = move || {
        busy.get()
            || !state.status.get().ok
            || state.media_jobs.with(|jobs| {
                jobs.iter()
                    .any(|other| other.source == source.get() && other.pending())
            })
    };
    let retry = move |_| {
        if blocked() {
            return;
        }
        let path = source.get_untracked();
        busy.set(true);
        leptos::task::spawn_local(async move {
            match gateway::remux_file(&path).await {
                Ok(()) => state.notify(t("已加入转 MP4 队列，原 TS 保留")),
                Err(error) => state.fail(error),
            }
            let _ = busy.try_set(false);
        });
    };
    view! {
        <Show when=move || job.with(Option::is_some)>
            <div class=if compact { "media-job-status card-media-job" } else { "media-job-status media-job-row" }
                data-job-id=move || job.with(|value| value.as_ref().map(|job| job.id.clone()).unwrap_or_default())
                data-state=move || job.with(|value| value.as_ref().map(|job| job.state.clone()).unwrap_or_default())>
                <span class=if compact { "card-media-job-source" } else { "media-job-source" } title=move || source.get()>
                    {move || if compact { format!("{} · {}", if pending() { t("录后处理") } else { t("最近一次录后处理") }, source.get()) } else { source.get() }}
                </span>
                <div class="media-job-heading" role="status">
                    <span class="media-state-symbol" aria-hidden="true"><Icon name="file" size=15 /></span>
                    <strong>{move || phase.get()}</strong>
                </div>
                <span class="media-job-detail">
                    {move || job.with(|value| value.as_ref().map(|job| {
                        media_progress(job, if job.pending() { state.now.get() } else { 0 })
                    }).unwrap_or_default())}
                </span>
                <Show when=move || pending() && job.with(|value| value.as_ref().is_some_and(|job| job.state != "waiting"))>
                    <progress aria-label=t("本阶段进度") max="100"
                        value=move || job.with(|value| value.as_ref().and_then(|job| job.stage_percent).map(|value| value.to_string())) />
                </Show>
                <details class="media-job-diagnostics">
                    <summary>{move || if attention() { t("查看处理原因") } else { t("处理详情") }}</summary>
                    <p>{move || job.with(|value| value.as_ref().map(|job| message(job.message.clone())).unwrap_or_default())}</p>
                    <dl><div><dt>{t("源文件")}</dt><dd>{move || source.get()}</dd></div>
                        <Show when=move || job.with(|value| value.as_ref().is_some_and(MediaJob::output_ready))>
                            <div><dt>{t("输出文件")}</dt><dd>{move || job.with(|value| value.as_ref().map(|job| job.output.clone()).unwrap_or_default())}</dd></div>
                        </Show>
                    </dl>
                </details>
                <Show when=move || !compact && job.with(|value| value.as_ref().is_some_and(|job| matches!(job.state.as_str(), "failed" | "cancelled")))>
                    <button class="button secondary small media-retry" type="button"
                        title=t("重新转换，始终保留源 TS；已有 MP4 不覆盖")
                        disabled=blocked on:click=retry>
                        <Icon name="refresh" size=14 />
                        {move || if busy.get() { t("正在提交…") } else { t("保留 TS 重试") }}
                    </button>
                </Show>
            </div>
        </Show>
    }
}
