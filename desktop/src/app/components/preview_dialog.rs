use super::{Dialog, EmptyState, Icon};
use crate::{
    api::gateway::{self, Recording, RecordingFile},
    app::labels::human_size,
};
use leptos::prelude::*;

fn extension(path: &str) -> String {
    path.rsplit('.').next().unwrap_or("").to_ascii_lowercase()
}

#[component]
pub fn MediaPlayer(#[prop(into)] path: Signal<String>) -> impl IntoView {
    let failed = RwSignal::new(false);
    let audio = move || {
        matches!(
            extension(&path.get()).as_str(),
            "mp3" | "m4a" | "aac" | "wav" | "ogg"
        )
    };
    let supported = move || {
        matches!(
            extension(&path.get()).as_str(),
            "mp4" | "m4v" | "webm" | "ogv" | "mp3" | "m4a" | "aac" | "wav" | "ogg"
        )
    };
    Effect::new(move |_| {
        let _ = path.get();
        failed.set(false);
    });
    view! {
        <div class="media-preview">
            <Show when=supported fallback=|| view! { <EmptyState icon="file" title="此格式需要本地播放器" description="内嵌播放器不支持该容器格式。请复制保存路径，在本地播放器中打开。" /> }>
                <Show when=audio fallback=move || view! {
                    <video class="video-player" controls playsinline preload="metadata" aria-label="录制视频预览" src=move || gateway::video_url(&path.get()) on:error=move |_| failed.set(true) />
                }>
                    <div class="audio-art"><Icon name="volume" size=46 /><span>"AUDIO RECORDING"</span></div>
                    <audio class="audio-player" controls preload="metadata" aria-label="录制音频预览" src=move || gateway::video_url(&path.get()) on:error=move |_| failed.set(true) />
                </Show>
                <Show when=move || failed.get()><div class="player-error" role="alert"><Icon name="alert" size=17 />"无法播放此文件，可能仍在录制或编码不受支持。原始文件不受影响。"</div></Show>
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
    Effect::new(move |_| {
        let current = target.get();
        let generation = revision.get_untracked().wrapping_add(1);
        revision.set(generation);
        files.set(Vec::new());
        playing.set(String::new());
        directory.set(String::new());
        error.set(None);
        let Some(rec) = current else {
            loading.set(false);
            return;
        };
        loading.set(true);
        leptos::task::spawn_local(async move {
            let result = gateway::fetch_recording_files(&rec.rec_id).await;
            if revision.try_get_untracked() != Some(generation) {
                return;
            }
            match result {
                Ok(result) => {
                    directory.set(result.dir.unwrap_or_default());
                    playing.set(
                        result
                            .files
                            .first()
                            .map(|f| f.path.clone())
                            .unwrap_or_default(),
                    );
                    files.set(result.files);
                }
                Err(message) => error.set(Some(message)),
            }
            loading.set(false);
        });
    });
    let copy_directory = move |_| {
        let text = directory.get_untracked();
        leptos::task::spawn_local(async move {
            match js_sys::futures::JsFuture::from(
                window().navigator().clipboard().write_text(&text),
            )
            .await
            {
                Ok(_) => state.notify("保存目录已复制"),
                Err(_) => state.fail("无法访问剪贴板，请手动复制保存目录"),
            }
        });
    };
    view! {
        <Dialog open=Signal::derive(move || target.get().is_some()) title="录制预览" wide=true on_close=Callback::new(move |_| target.set(None))>
            <p class="dialog-description">{move || target.get().map(|rec| rec.name()).unwrap_or_default()}</p>
            <Show when=move || loading.get()><div class="loading-state"><span class="spinner" />"正在读取录制文件…"</div></Show>
            <Show when=move || error.get().is_some()><p class="field-error" role="alert">{move || error.get().unwrap_or_default()}</p></Show>
            <Show when=move || !loading.get() && error.get().is_none() && files.get().is_empty()><EmptyState icon="video" title="还没有录制文件" description="开始录制后，文件会出现在这里。正在写入的文件可能暂时无法预览。" /></Show>
            <Show when=move || !playing.get().is_empty()><MediaPlayer path=Signal::derive(move || playing.get()) /></Show>
            <div class="preview-file-list" role="list" aria-label="录制文件">
                {move || files.get().into_iter().map(|file| {
                    let path = file.path.clone(); let active_path = path.clone();
                    view! { <button role="listitem" class="preview-file" class:active=move || playing.get() == active_path on:click=move |_| playing.set(path.clone())><Icon name="file" size=17 /><span>{file.name}</span><small>{human_size(file.size)}<br />{crate::app::labels::modified_time(Some(file.modified))}</small></button> }
                }).collect_view()}
            </div>
            <Show when=move || !directory.get().is_empty()><div class="directory-note"><Icon name="folder" size=16 /><span>{move || directory.get()}</span><button class="icon-button" aria-label="复制保存目录" on:click=copy_directory><Icon name="copy" size=15 /></button></div></Show>
            <footer class="modal-actions"><button class="button secondary" on:click=move |_| target.set(None)>"关闭预览"</button></footer>
        </Dialog>
    }
}
