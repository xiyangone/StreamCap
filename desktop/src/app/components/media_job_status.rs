use super::Icon;
use crate::{
    api::gateway::{self, MediaJob},
    app::{
        i18n::{message, t},
        labels::{media_phase, media_progress},
    },
};
use leptos::prelude::*;

fn visible_status(job: &MediaJob) -> bool {
    job.pending() || job.needs_attention()
}

/// Compact task progress; the media library is reserved for files.
#[component]
pub fn MediaJobStatus(#[prop(into)] job: Signal<Option<MediaJob>>) -> impl IntoView {
    let state = gateway::app_state();
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
    view! {
        <Show when=move || job.with(|value| value.as_ref().is_some_and(visible_status))>
            <div class="media-job-status card-media-job"
                data-job-id=move || job.with(|value| value.as_ref().map(|job| job.id.clone()).unwrap_or_default())
                data-state=move || job.with(|value| value.as_ref().map(|job| job.state.clone()).unwrap_or_default())>
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
                    <dl><div><dt>{t("源文件")}</dt><dd class="card-media-job-source" title=move || source.get()>{move || source.get()}</dd></div>
                        <Show when=move || job.with(|value| value.as_ref().is_some_and(MediaJob::output_ready))>
                            <div><dt>{t("输出文件")}</dt><dd>{move || job.with(|value| value.as_ref().map(|job| job.output.clone()).unwrap_or_default())}</dd></div>
                        </Show>
                    </dl>
                    <Show when=move || !pending()><p class="history-note">{t("此处为当次处理记录，不代表文件当前仍在原位置。")}</p></Show>
                    <a class="text-link" href="/storage">{t("在媒体库查看文件")}<Icon name="arrow" size=14 /></a>
                    </div>
                </details>
            </div>
        </Show>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn completed_jobs_do_not_occupy_task_rows_but_attention_is_preserved() {
        for (state, visible) in [
            ("waiting", true),
            ("running", true),
            ("verifying", true),
            ("cleaning", true),
            ("failed", true),
            ("cleanupFailed", true),
            ("cancelled", true),
            ("complete", false),
        ] {
            let job: MediaJob = serde_json::from_value(json!({
                "id": "job", "taskId": "room", "state": state,
                "source": "recording.ts", "output": "recording.mp4", "message": ""
            }))
            .unwrap();
            assert_eq!(visible_status(&job), visible, "{state}");
        }
    }
}
