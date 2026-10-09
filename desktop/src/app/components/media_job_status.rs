use super::{ActionButton, FormFeedback, Icon};
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
    let error = RwSignal::new(None::<String>);
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
    let retry = Callback::new(move |_| {
        if blocked() {
            return;
        }
        let path = source.get_untracked();
        if path.is_empty() {
            return;
        }
        error.set(None);
        busy.set(true);
        leptos::task::spawn_local(async move {
            match gateway::remux_file(&path).await {
                Ok(()) => state.notify(t("已加入转 MP4 队列，原 TS 保留")),
                Err(message) => {
                    let _ = error.try_set(Some(message));
                }
            }
            let _ = busy.try_set(false);
        });
    });
    view! {
        <Show when=move || job.with(Option::is_some)>
            <div class=if compact { "media-job-status card-media-job" } else { "media-job-status media-job-row" }
                data-job-id=move || job.with(|value| value.as_ref().map(|job| job.id.clone()).unwrap_or_default())
                data-state=move || job.with(|value| value.as_ref().map(|job| job.state.clone()).unwrap_or_default())>
                <Show when=move || !compact>
                    <span class="media-job-source" title=move || source.get()>{move || source.get().rsplit('/').next().unwrap_or_default().to_string()}</span>
                </Show>
                <div class="media-job-heading" role="status">
                    <span class="media-state-symbol" aria-hidden="true">
                        <Show when=move || attention() fallback=move || view! {
                            <Show when=pending fallback=|| view! { <Icon name="check" size=14 /> }>
                                <Icon name="refresh" size=14 />
                            </Show>
                        }><Icon name="alert" size=14 /></Show>
                    </span>
                    <strong>{move || phase.get()}</strong>
                </div>
                <span class="media-job-detail">
                    {move || job.with(|value| value.as_ref().map(|job| {
                        media_progress(job, if job.pending() { state.now.get() } else { 0 })
                    }).unwrap_or_default())}
                </span>
                <Show when=move || pending() && job.with(|value| value.as_ref().is_some_and(|job| job.state != "waiting"))>
                    <progress class="ui-progress ui-progress-primary" aria-label=t("本阶段进度") max="100"
                        value=move || job.with(|value| value.as_ref().and_then(|job| job.stage_percent).map(|value| value.to_string())) />
                </Show>
                <details class="media-job-diagnostics">
                    <summary>{move || if attention() { t("查看处理原因") } else { t("处理详情") }}</summary>
                    <div class="media-job-diagnostic-body">
                    <p>{move || job.with(|value| value.as_ref().map(|job| message(job.message.clone())).unwrap_or_default())}</p>
                    <dl><div><dt>{t("源文件")}</dt><dd class=if compact { "card-media-job-source" } else { "" } title=move || source.get()>{move || source.get()}</dd></div>
                        <Show when=move || job.with(|value| value.as_ref().is_some_and(MediaJob::output_ready))>
                            <div><dt>{t("输出文件")}</dt><dd>{move || job.with(|value| value.as_ref().map(|job| job.output.clone()).unwrap_or_default())}</dd></div>
                        </Show>
                    </dl>
                    <Show when=move || !pending()><p class="history-note">{t("此处为当次处理记录，不代表文件当前仍在原位置。")}</p></Show>
                    <Show when=move || compact><a class="text-link" href="/storage">{t("查看处理记录")}<Icon name="arrow" size=14 /></a></Show>
                    </div>
                </details>
                <Show when=move || !compact && job.with(|value| value.as_ref().is_some_and(|job| matches!(job.state.as_str(), "failed" | "cancelled")))>
                    <ActionButton label=t("保留 TS 重试") icon="refresh" class="small media-retry"
                        title=t("重新转换，始终保留源 TS；已有 MP4 不覆盖")
                        busy=busy busy_label=t("正在提交…")
                        disabled=Signal::derive(blocked) on_click=retry />
                </Show>
                <Show when=move || error.get().is_some()>
                    <div class="media-job-feedback"><FormFeedback error=error title=t("操作未完成") /></div>
                </Show>
            </div>
        </Show>
    }
}
