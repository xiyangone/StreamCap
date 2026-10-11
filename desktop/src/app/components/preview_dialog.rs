use super::{ActionButton, Dialog, EmptyState, FormFeedback, Icon, InlineNotice};
use crate::app::i18n::t;
use crate::{
    api::gateway::{self, Recording, RecordingFile, StorageItem},
    app::labels::human_size,
};
use leptos::prelude::*;
use wasm_bindgen::{prelude::*, JsCast};

fn extension(path: &str) -> String {
    if path.starts_with("live:") {
        return "live".into();
    }
    path.rsplit('.').next().unwrap_or("").to_ascii_lowercase()
}
#[wasm_bindgen(inline_js = "export { attachMedia, captureFrame } from '/media-player.js';")]
extern "C" {
    #[wasm_bindgen(catch,js_name = captureFrame)]
    fn capture_frame(path: &str) -> Result<String, JsValue>;
    #[wasm_bindgen(js_name = attachMedia)]
    fn attach_media(
        video: &web_sys::HtmlVideoElement,
        origin: &str,
        path: &str,
        on_state: &js_sys::Function,
        on_capture: &js_sys::Function,
    ) -> js_sys::Function;
}
struct PlayerSession {
    dispose: js_sys::Function,
    _state: Closure<dyn FnMut(String, String)>,
    _capture: Closure<dyn FnMut()>,
}
impl Drop for PlayerSession {
    fn drop(&mut self) {
        let _ = self.dispose.call0(&JsValue::NULL);
    }
}
#[component]
pub fn MediaPlayer(#[prop(into)] path: Signal<String>) -> impl IntoView {
    let app = gateway::app_state();
    let message = RwSignal::new(String::new());
    let phase = RwSignal::new("loading".to_string());
    let capturing = RwSignal::new(false);
    let video = NodeRef::<leptos::html::Video>::new();
    let session = StoredValue::new_local(None::<PlayerSession>);
    let audio = move || {
        matches!(
            extension(&path.get()).as_str(),
            "mp3" | "m4a" | "aac" | "wav" | "ogg"
        )
    };
    let supported = move || {
        matches!(
            extension(&path.get()).as_str(),
            "live"
                | "ts"
                | "mp4"
                | "m4v"
                | "webm"
                | "ogv"
                | "mp3"
                | "m4a"
                | "aac"
                | "wav"
                | "ogg"
                | "flv"
                | "mkv"
                | "mov"
                | "nut"
                | "wma"
        )
    };
    Effect::new(move |_| {
        let current = path.get();
        let element = video.get();
        session.update_value(|value| {
            value.take();
        });
        message.set(String::new());
        phase.set("loading".into());
        if audio() || !supported() {
            return;
        }
        let Some(element) = element else {
            return;
        };
        let callback =
            Closure::<dyn FnMut(String, String)>::new(move |state: String, text: String| {
                if phase.try_get_untracked().as_deref() == Some("error")
                    && !matches!(state.as_str(), "loading" | "error")
                {
                    return;
                }
                let _ = phase.try_set(state);
                let _ = message.try_set(crate::app::i18n::message(text));
            });
        let capture_path = current.clone();
        let capture = Closure::<dyn FnMut()>::new(move || {
            if capturing.get_untracked() {
                return;
            }
            match capture_frame(&capture_path) {
                Ok(image) => {
                    capturing.set(true);
                    let current = capture_path.clone();
                    leptos::task::spawn_local(async move {
                        match gateway::save_screenshot(&current, &image).await {
                            Ok(_) => app.notify(t("截图已保存")),
                            Err(error) => app.fail(error),
                        }
                        let _ = capturing.try_set(false);
                    });
                }
                Err(_) => app.fail(t("请先播放视频再截图")),
            }
        });
        let dispose = attach_media(
            element.unchecked_ref(),
            &gateway::gateway_base(),
            &current,
            callback.as_ref().unchecked_ref(),
            capture.as_ref().unchecked_ref(),
        );
        session.set_value(Some(PlayerSession {
            dispose,
            _state: callback,
            _capture: capture,
        }));
    });
    on_cleanup(move || {
        session.update_value(|value| {
            value.take();
        });
    });
    view! {
        <div class="media-preview" data-player-state=move || phase.get() data-path=move||path.get()
            data-capturing=move || capturing.get().to_string()>
            <Show when=supported fallback=|| view! { <EmptyState icon="file" title=t("此格式需要本地播放器") description=t("当前预览支持 TS、MP4 和常见网页媒体格式。原文件不受影响。") /> }>
                <Show when=audio fallback=move || view! { <video node_ref=video class="video-player" controls playsinline preload="auto" aria-label=t("录制视频预览") /> }>
                    <div class="audio-art"><Icon name="volume" size=40 /><span>{t("音频录制")}</span></div>
                    <audio class="audio-player" controls preload="metadata" aria-label=t("录制音频预览") src=move || gateway::video_url(&path.get()) on:error=move |_| { phase.set("error".into()); message.set(t("无法播放此音频，原始文件未改动。").into()); } />
                </Show>
            <Show when=move || !message.get().is_empty()><div class="player-message" class:player-error=move || phase.get() == "error" role="status"><Icon name="info" size=17 /><span>{move || message.get()}</span></div></Show>
            </Show>
        </div>
    }
}

#[derive(Clone)]
enum PreviewTarget {
    Recording(String),
    File(StorageItem),
}

#[derive(Clone, Copy)]
pub struct PreviewState {
    target: RwSignal<Option<PreviewTarget>>,
    minimized: RwSignal<bool>,
}
impl PreviewState {
    pub fn open_recording(self, recording: Recording) {
        self.open_recording_window(recording, false);
    }
    pub fn open_recording_mini(self, recording: Recording) {
        self.open_recording_window(recording, true);
    }
    fn open_recording_window(self, recording: Recording, minimized: bool) {
        batch(move || {
            if !matches!(self.target.get_untracked(), Some(PreviewTarget::Recording(id)) if id == recording.rec_id)
            {
                self.target
                    .set(Some(PreviewTarget::Recording(recording.rec_id)));
            }
            self.minimized.set(minimized);
        });
    }
    pub fn recording_is_open(self, rec_id: &str, minimized: bool) -> bool {
        matches!(self.target.get(), Some(PreviewTarget::Recording(id)) if id == rec_id)
            && self.minimized.get() == minimized
    }
    pub fn open_file(self, file: StorageItem) {
        self.minimized.set(false);
        self.target.set(Some(PreviewTarget::File(file)));
    }
    fn close(self) {
        self.target.set(None);
        self.minimized.set(false);
    }
}
pub fn provide_preview() {
    provide_context(PreviewState {
        target: RwSignal::new(None),
        minimized: RwSignal::new(false),
    });
}
pub fn preview_state() -> PreviewState {
    expect_context::<PreviewState>()
}

#[component]
pub fn PreviewHost() -> impl IntoView {
    let preview = preview_state();
    let state = gateway::app_state();
    let target = Signal::derive(move || match preview.target.get() {
        Some(PreviewTarget::Recording(id)) => state.recording(&id),
        _ => None,
    });
    Effect::new(move |_| {
        if let Some(PreviewTarget::Recording(id)) = preview.target.get() {
            if state.recording(&id).is_none() && !state.loading.get() {
                preview.close();
            }
        }
    });
    let files = RwSignal::new(Vec::<RecordingFile>::new());
    let playing = RwSignal::new(String::new());
    let live_source = RwSignal::new(false);
    let last_file = RwSignal::new(String::new());
    let select_path = Callback::new(move |path: String| {
        if playing.get_untracked() != path {
            playing.set(path);
        }
    });
    Effect::new(move |_| {
        let path = playing.get();
        if !path.is_empty() && !path.starts_with("live:") {
            last_file.set(path);
        }
    });
    Effect::new(move |_| {
        if target.get().is_some_and(|rec| !rec.is_live) && playing.get().starts_with("live:") {
            batch(move || {
                live_source.set(false);
                playing.set(String::new());
            });
        }
    });
    let loading = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let revision = RwSignal::new(0_u32);
    let request = StoredValue::new_local(None::<web_sys::AbortController>);
    on_cleanup(move || {
        request.with_value(|controller| {
            if let Some(controller) = controller {
                controller.abort();
            }
        })
    });
    let target_key = Memo::new(move |_| match preview.target.get() {
        Some(PreviewTarget::Recording(id)) => format!("recording:{id}"),
        Some(PreviewTarget::File(file)) => format!("file:{}", file.path),
        None => String::new(),
    });
    Effect::new(move |_| {
        let current = playing.get();
        let jobs = state.media_jobs.get();
        if let Some(job) = jobs
            .iter()
            .rev()
            .find(|job| job.source == current && job.source_removed)
        {
            playing.set(job.output.clone());
        }
    });
    Effect::new(move |_| {
        let _ = target_key.get();
        request.update_value(|controller| {
            if let Some(controller) = controller.take() {
                controller.abort();
            }
        });
        let current_id = target.get_untracked().map(|recording| recording.rec_id);
        let generation = revision.get_untracked().wrapping_add(1);
        revision.set(generation);
        files.set(Vec::new());
        playing.set(String::new());
        last_file.set(String::new());
        live_source.set(false);
        error.set(None);
        let Some(rec_id) = current_id else {
            if let Some(PreviewTarget::File(file)) = preview.target.get_untracked() {
                playing.set(file.path);
            }
            loading.set(false);
            return;
        };
        loading.set(true);
        leptos::task::spawn_local(async move {
            // One in-flight request per target. A request timeout must not cancel future polls.
            loop {
                if revision.try_get_untracked() != Some(generation)
                    || state.recording_untracked(&rec_id).is_none()
                {
                    return;
                }
                let Ok(abort) = web_sys::AbortController::new() else {
                    loading.set(false);
                    error.set(Some(t("无法创建请求").into()));
                    return;
                };
                request.set_value(Some(abort.clone()));
                let result = gateway::fetch_recording_files(&rec_id, abort).await;
                if revision.try_get_untracked() != Some(generation)
                    || state.recording_untracked(&rec_id).is_none()
                {
                    return;
                }
                request.set_value(None);
                match result {
                    Ok(result) => {
                        error.set(None);
                        let current = playing.get_untracked();
                        if !current.is_empty()
                            && !current.starts_with("live:")
                            && !result.files.iter().any(|f| f.path == current)
                        {
                            if let Some(job) = state
                                .media_jobs
                                .get_untracked()
                                .iter()
                                .rev()
                                .find(|j| j.source == current && j.source_removed)
                            {
                                if result.files.iter().any(|f| f.path == job.output) {
                                    playing.set(job.output.clone());
                                } else {
                                    playing.set(String::new());
                                }
                            } else {
                                playing.set(String::new());
                            }
                        }
                        if playing.get_untracked().is_empty() && !live_source.get_untracked() {
                            playing.set(
                                result
                                    .files
                                    .first()
                                    .map(|f| f.path.clone())
                                    .unwrap_or_default(),
                            );
                        }
                        files.set(result.files);
                    }
                    Err(message) => error.set(Some(message)),
                }
                loading.set(false);
                gateway::sleep_ms(2000).await;
                if revision.try_get_untracked() != Some(generation) {
                    return;
                }
            }
        });
    });
    let media_message = move || {
        state.media_jobs.with(|jobs| {
            jobs.iter()
                .rev()
                .find(|j| j.source == playing.get())
                .map(|j| j.message.clone())
                .unwrap_or_default()
        })
    };
    view! {
        <Dialog open=Signal::derive(move || preview.target.get().is_some()) title=Signal::derive(move || if target.get().is_some() { t("录制预览") } else { t("媒体预览") }) wide=true minimized=preview.minimized on_close=Callback::new(move |_| preview.close())>
            <Show when=move || target.get().is_some()>
            <div class="preview-heading">
                <div class="preview-context"><Icon name="video" size=21 /><div><span class="preview-eyebrow">{move || if live_source.get() { t("直播源") } else { t("录制文件") }}</span><p class="preview-title">{move || target.get().map(|rec| rec.name()).unwrap_or_default()}</p></div></div>
                <Show when=move || target.get().is_some_and(|rec| rec.is_recording)>
                    <span class="ui-badge ui-badge-sm badge recording"><i class="status-dot" />{t("录制中")}</span>
                </Show>
            </div>
            <div class="preview-sources" role="group" aria-label=t("预览来源")>
                <ActionButton label=t("录制文件") icon="file" class="small preview-source"
                    aria_label=t("切换到录制文件").to_string()
                    pressed=Signal::derive(move || Some(!live_source.get()))
                    on_click=Callback::new(move |_| {
                        let remembered = last_file.get_untracked();
                        let path = files.with_untracked(|files| {
                            files.iter().find(|file| file.path == remembered).or_else(|| files.first())
                                .map(|file| file.path.clone()).unwrap_or_default()
                        });
                        batch(move || {
                            live_source.set(false);
                            select_path.run(path);
                        });
                    }) />
                <Show when=move || target.get().is_some_and(|rec| rec.is_live)>
                    <ActionButton label=t("直播源") icon="video" class="small preview-source"
                        aria_label=t("预览直播源").to_string()
                        title=t("直播源缓存有效期 5 分钟，过期后请先手动检测。预览不会新增录制文件。")
                        pressed=Signal::derive(move || Some(live_source.get()))
                        disabled=Signal::derive(move || !state.status.get().ok)
                        on_click=Callback::new(move |_| {
                            if !state.status.get_untracked().ok { return; }
                            if let Some(rec)=target.get_untracked().filter(|rec| rec.is_live) {
                                batch(move || {
                                    live_source.set(true);
                                    select_path.run(format!("live:{}",rec.rec_id));
                                });
                            }
                        }) />
                </Show>
            </div>
            </Show>
            <Show when=move || target.get().is_none()><p class="dialog-description break-anywhere preview-media-title">{move || playing.get().rsplit(['/', '\\']).next().unwrap_or_default().to_string()}</p></Show>
            <Show when=move || loading.get()><div class="loading-state"><span class="ui-loading ui-loading-spinner ui-loading-sm spinner" aria-hidden="true" />{t("正在读取录制文件…")}</div></Show>
            <FormFeedback error=error title=t("无法读取录制文件") />
            <Show when=move || !loading.get() && error.get().is_none() && files.get().is_empty() && playing.get().is_empty()><EmptyState icon="video" title=t("还没有录制文件") description=t("TS 开始写入后可在这里预览，文件大小会自动更新。") /></Show>
            <Show when=move || !playing.get().is_empty()><MediaPlayer path=Signal::derive(move || playing.get()) /></Show>
            <Show when=move || target.get().is_some()>
            <div class="preview-files-heading"><span>{t("录制文件")}<small>{move || files.get().len()}</small></span><span class="preview-current-file" title=move || playing.get()>{move || playing.get().rsplit(['/', '\\']).next().unwrap_or_default().to_string()}</span></div>
            <ul class="preview-file-list" aria-label=t("录制文件")>
                <For each=move || files.with(|items| items.iter().map(|file| file.path.clone()).collect::<Vec<_>>())
                    key=|path| path.clone() children=move |path| {
                        let path = StoredValue::new(path);
                        let file = Signal::derive(move || files.with(|items| items.iter().find(|file| file.path == path.get_value()).cloned()));
                        view! {
                            <li><ActionButton label=t("播放录制文件") icon="file" class="preview-file"
                                aria_label=Signal::derive(move || crate::tr_format!("播放{}", file.get().map(|file| file.name).unwrap_or_default()))
                                pressed=Signal::derive(move || Some(playing.get() == path.get_value()))
                                on_click=Callback::new(move |_| {
                                    batch(move || {
                                        live_source.set(false);
                                        select_path.run(path.get_value());
                                    });
                                })>
                                <span class="preview-file-name">{move || file.get().map(|file| file.name).unwrap_or_default()}</span>
                                <small>{move || file.get().map(|file| human_size(file.size)).unwrap_or_default()}<br />
                                    {move || file.get().map(|file| crate::app::labels::modified_time(Some(file.modified))).unwrap_or_default()}</small>
                            </ActionButton></li>
                        }
                    } />
            </ul>
            </Show>
            <Show when=move || !media_message().is_empty()><InlineNotice icon="history"><span role="status">{media_message}</span></InlineNotice></Show>
        </Dialog>
    }
}
