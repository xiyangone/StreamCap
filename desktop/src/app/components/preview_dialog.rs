use super::{Dialog, EmptyState, Icon};
use crate::app::i18n::t;
use crate::{
    api::gateway::{self, Recording, RecordingFile},
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
    ) -> js_sys::Function;
}
struct PlayerSession {
    dispose: js_sys::Function,
    _state: Closure<dyn FnMut(String, String)>,
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
        let callback = Closure::<dyn FnMut(String, String)>::new(move |state, text| {
            let _ = phase.try_set(state);
            let _ = message.try_set(crate::app::i18n::message(text));
        });
        let dispose = attach_media(
            element.unchecked_ref(),
            &gateway::gateway_base(),
            &current,
            callback.as_ref().unchecked_ref(),
        );
        session.set_value(Some(PlayerSession {
            dispose,
            _state: callback,
        }));
    });
    on_cleanup(move || {
        session.update_value(|value| {
            value.take();
        });
    });
    view! {
        <div class="media-preview" data-player-state=move || phase.get() data-path=move||path.get()>
            <Show when=supported fallback=|| view! { <EmptyState icon="file" title=t("此格式需要本地播放器") description=t("当前预览支持 TS、MP4 和常见网页媒体格式。原文件不受影响。") /> }>
                <Show when=audio fallback=move || view! { <video node_ref=video class="video-player" controls playsinline preload="metadata" aria-label=t("录制视频预览") /> }>
                    <div class="audio-art"><Icon name="volume" size=46 /><span>"AUDIO RECORDING"</span></div>
                    <audio class="audio-player" controls preload="metadata" aria-label=t("录制音频预览") src=move || gateway::video_url(&path.get()) on:error=move |_| { phase.set("error".into()); message.set(t("无法播放此音频，原始文件未改动。").into()); } />
                </Show>
                <Show when=move||!audio()&&!path.get().starts_with("live:")><button class="button secondary small" type="button" on:click=move |_|{let current=path.get_untracked();match capture_frame(&current){Ok(image)=>leptos::task::spawn_local(async move{match gateway::save_screenshot(&current,&image).await{Ok(_)=>app.notify(t("截图已保存")),Err(error)=>app.fail(error)}}),Err(_)=>app.fail(t("请先播放视频再截图"))}} >{t("保存截图")}</button></Show>
            <Show when=move || !message.get().is_empty()><div class="player-message" class:player-error=move || phase.get() == "error" role="status"><Icon name="info" size=17 /><span>{move || message.get()}</span></div></Show>
            </Show>
        </div>
    }
}

#[component]
pub fn PreviewDialog(target: RwSignal<Option<Recording>>) -> impl IntoView {
    let state = gateway::app_state();
    let files = RwSignal::new(Vec::<RecordingFile>::new());
    let directory = RwSignal::new(String::new());
    let playing = RwSignal::new(String::new());
    let loading = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let revision = RwSignal::new(0_u32);
    let refresh = RwSignal::new(0_u32);
    let previous = StoredValue::new(None::<String>);
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
    let timer = StoredValue::new_local(Some(gloo_timers::callback::Interval::new(
        2000,
        move || {
            if target.try_get_untracked().flatten().is_some() {
                let _ = refresh.try_update(|v| *v = v.wrapping_add(1));
            }
        },
    )));
    on_cleanup(move || {
        timer.update_value(|value| {
            value.take();
        });
    });
    Effect::new(move |_| {
        let current = target.get();
        let _ = refresh.get();
        let _ = state.media_jobs.get();
        let generation = revision.get_untracked().wrapping_add(1);
        revision.set(generation);
        let current_id = current.as_ref().map(|r| r.rec_id.clone());
        if previous.get_value() != current_id {
            files.set(Vec::new());
            playing.set(String::new());
            directory.set(String::new());
            previous.set_value(current_id);
        }
        error.set(None);
        let Some(rec) = current else {
            loading.set(false);
            return;
        };
        loading.set(files.get_untracked().is_empty());
        leptos::task::spawn_local(async move {
            let result = gateway::fetch_recording_files(&rec.rec_id).await;
            if revision.try_get_untracked() != Some(generation) {
                return;
            }
            match result {
                Ok(result) => {
                    directory.set(result.dir.unwrap_or_default());
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
                    if playing.get_untracked().is_empty() {
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
        });
    });
    let processing = move || {
        state.media_jobs.with(|jobs| {
            jobs.iter()
                .any(|j| j.source == playing.get() && j.pending())
        })
    };
    let media_message = move || {
        state.media_jobs.with(|jobs| {
            jobs.iter()
                .rev()
                .find(|j| j.source == playing.get())
                .map(|j| j.message.clone())
                .unwrap_or_default()
        })
    };
    let copy_directory = move |_| {
        let text = directory.get_untracked();
        leptos::task::spawn_local(async move {
            match js_sys::futures::JsFuture::from(
                window().navigator().clipboard().write_text(&text),
            )
            .await
            {
                Ok(_) => state.notify(t("保存目录已复制")),
                Err(_) => state.fail(t("无法访问剪贴板，请手动复制保存目录")),
            }
        });
    };
    let convert = move |_| {
        let current = playing.get_untracked();
        leptos::task::spawn_local(async move {
            match gateway::remux_file(&current).await {
                Ok(()) => state.notify(t("已加入转 MP4 队列，原 TS 保留")),
                Err(error) => state.fail(error),
            }
        });
    };
    view! {
        <Dialog open=Signal::derive(move || target.get().is_some()) title=t("录制预览") wide=true on_close=Callback::new(move |_| target.set(None))>
            <p class="dialog-description">{move || target.get().map(|rec| rec.name()).unwrap_or_default()}</p>
            <Show when=move || target.get().is_some_and(|rec|rec.is_live)><button class="button secondary small" type="button" on:click=move |_|{if let Some(rec)=target.get_untracked(){playing.set(format!("live:{}",rec.rec_id));}}>{t("预览直播源")}</button><p class="field-hint">{t("直播源缓存有效期 5 分钟，过期后请先手动检测。预览不会新增录制文件。")}</p></Show>
            <Show when=move || loading.get()><div class="loading-state"><span class="spinner" />{t("正在读取录制文件…")}</div></Show>
            <Show when=move || error.get().is_some()><p class="field-error" role="alert">{move || error.get().unwrap_or_default()}</p></Show>
            <Show when=move || !loading.get() && error.get().is_none() && files.get().is_empty() && playing.get().is_empty()><EmptyState icon="video" title=t("还没有录制文件") description=t("TS 开始写入后可在这里预览，文件大小会自动更新。") /></Show>
            <Show when=move || !playing.get().is_empty()><MediaPlayer path=Signal::derive(move || playing.get()) /></Show>
            <div class="preview-file-list" role="list" aria-label=t("录制文件")>{move || files.get().into_iter().map(|file| {
                let path = file.path.clone(); let active_path = path.clone();
                view! { <button role="listitem" class="preview-file" class:active=move || playing.get() == active_path on:click=move |_| playing.set(path.clone())><Icon name="file" size=17 /><span>{file.name}</span><small>{human_size(file.size)}<br />{crate::app::labels::modified_time(Some(file.modified))}</small></button> }
            }).collect_view()}</div>
            <Show when=move || !media_message().is_empty()><p class="media-job-status" role="status">{media_message}</p></Show>
            <Show when=move || !directory.get().is_empty()><div class="directory-note"><Icon name="folder" size=16 /><span>{move || directory.get()}</span><button class="icon-button" aria-label=t("复制保存目录") on:click=copy_directory><Icon name="copy" size=15 /></button></div></Show>
            <footer class="modal-actions"><Show when=move || extension(&playing.get()) == "ts"><button class="button secondary" disabled=processing on:click=convert><Icon name="refresh" size=16 />{t("转为 MP4")}</button></Show><button class="button secondary" on:click=move |_| target.set(None)>{t("关闭预览")}</button></footer>
        </Dialog>
    }
}
