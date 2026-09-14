use crate::app::i18n::t;
use crate::{
    api::gateway::{self, StorageItem, StorageListing},
    app::{
        components::{ConfirmDialog, Dialog, EmptyState, Icon, MediaPlayer},
        labels::{human_size, modified_time},
    },
};
use leptos::prelude::*;

fn kind(item: &StorageItem) -> &'static str {
    if item.is_dir {
        return "folder";
    }
    match item
        .name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "mp3" | "m4a" | "aac" | "wav" | "wma" | "ogg" => "audio",
        "mp4" | "ts" | "flv" | "mkv" | "mov" | "webm" | "nut" => "video",
        _ => "file",
    }
}

#[component]
pub fn StorageView() -> impl IntoView {
    let state = gateway::app_state();
    let subfolder = RwSignal::new(String::new());
    let listing = RwSignal::new(StorageListing::default());
    let reload = RwSignal::new(0_u32);
    let generation = RwSignal::new(0_u32);
    let loading = RwSignal::new(true);
    let error = RwSignal::new(None::<String>);
    let keyword = RwSignal::new(String::new());
    let filter = RwSignal::new("all".to_string());
    let playing = RwSignal::new(None::<StorageItem>);
    Effect::new(move |_| {
        let current = playing.get();
        let jobs = state.media_jobs.get();
        if let Some(mut item) = current {
            if let Some(job) = jobs
                .iter()
                .rev()
                .find(|job| job.source == item.path && job.source_removed)
            {
                item.path = job.output.clone();
                item.name = job
                    .output
                    .rsplit('/')
                    .next()
                    .unwrap_or(&job.output)
                    .to_string();
                playing.set(Some(item));
            }
        }
    });
    let deleting = RwSignal::new(None::<StorageItem>);
    let confirm = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let completed_jobs = Memo::new(move |_| {
        state.media_jobs.with(|jobs| {
            jobs.iter()
                .filter(|j| matches!(j.state.as_str(), "complete" | "cleanupFailed"))
                .count()
        })
    });
    Effect::new(move |_| {
        let folder = subfolder.get();
        let _ = reload.get();
        let _ = state.settings_version.get();
        let _ = completed_jobs.get();
        let current = generation.get_untracked().wrapping_add(1);
        generation.set(current);
        loading.set(true);
        error.set(None);
        leptos::task::spawn_local(async move {
            let result = gateway::fetch_storage(if folder.is_empty() {
                None
            } else {
                Some(folder)
            })
            .await;
            if generation.try_get_untracked() != Some(current) {
                return;
            }
            match result {
                Ok(result) => listing.set(result),
                Err(message) => error.set(Some(message)),
            }
            loading.set(false);
        });
    });
    let visible = move || {
        let text = keyword.get().trim().to_lowercase();
        let mode = filter.get();
        listing
            .get()
            .items
            .into_iter()
            .filter(|item| {
                (mode == "all" || kind(item) == mode) && item.name.to_lowercase().contains(&text)
            })
            .collect::<Vec<_>>()
    };
    let enter = Callback::new(move |item: StorageItem| {
        if item.is_dir {
            subfolder.set(item.path);
            keyword.set(String::new());
        } else {
            playing.set(Some(item));
        }
    });
    let remove = Callback::new(move |_| {
        let Some(item) = deleting.get_untracked() else {
            return;
        };
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        leptos::task::spawn_local(async move {
            match gateway::delete_storage(&item.path).await {
                Ok(()) => {
                    let _ = confirm.try_set(false);
                    let _ = deleting.try_set(None);
                    state.notify(t("文件已移至回收站"));
                    let _ = reload.try_update(|n| *n = n.wrapping_add(1));
                }
                Err(message) => state.fail(message),
            }
            let _ = busy.try_set(false);
        });
    });
    let copy_path = Callback::new(move |path: String| {
        leptos::task::spawn_local(async move {
            match js_sys::futures::JsFuture::from(
                window().navigator().clipboard().write_text(&path),
            )
            .await
            {
                Ok(_) => state.notify(t("路径已复制")),
                Err(_) => state.fail(t("无法访问剪贴板，请手动复制路径")),
            }
        });
    });
    view! {
        <div class="page storage-page">
            <header class="page-header"><div><span class="eyebrow">"YOUR LOCAL ARCHIVE"</span><h1>{t("媒体库")}</h1><p>{t("每一次录制，都值得好好收藏。")}</p></div><button class="button secondary" disabled=move || loading.get() on:click=move |_| reload.update(|n| *n = n.wrapping_add(1))><Icon name="refresh" size=17 />{t("刷新文件")}</button></header>
            <section class="library-summary glass"><span class="library-symbol"><Icon name="folder" size=30 /></span><div class="library-location"><small>{t("录制文件位置")}</small><strong class="break-anywhere">{move || if listing.get().root.is_empty() { t("正在读取…").into() } else { listing.get().root }}</strong></div><button class="icon-button" aria-label=t("复制录制根目录") disabled=move || listing.get().root.is_empty() on:click=move |_| copy_path.run(listing.get_untracked().root)><Icon name="copy" size=17 /></button><div class="library-size"><small>{t("当前目录大小")}</small><strong>{move || human_size(listing.get().total_size)}</strong></div></section>
            <section class="file-browser glass">
                <div class="file-browser-toolbar"><nav class="file-breadcrumb" aria-label=t("媒体库路径")><button aria-label=t("返回媒体库根目录") on:click=move |_| subfolder.set(String::new())><Icon name="folder" size=17 />{t("媒体库")}</button>
                    {move || { let path = subfolder.get(); let mut prefix = String::new(); path.split('/').filter(|p| !p.is_empty()).map(|part| { if !prefix.is_empty() { prefix.push('/'); } prefix.push_str(part); let target = prefix.clone(); view! { <Icon name="chevron" size=13 /><button on:click=move |_| subfolder.set(target.clone())>{part.to_string()}</button> } }).collect_view() }}
                </nav><label class="search-field compact"><Icon name="search" size=17 /><input aria-label=t("搜索文件") placeholder=t("搜索当前目录…") prop:value=move || keyword.get() on:input=move |e| keyword.set(event_target_value(&e)) /></label></div>
                <div class="file-filter-row"><div class="mini-tabs" role="group" aria-label=t("文件类型")>{[("all",t("全部文件")),("video",t("视频")),("audio",t("音频")),("folder",t("文件夹"))].into_iter().map(|(value,label)| view! { <button class:active=move || filter.get() == value aria-pressed=move || (filter.get() == value).to_string() on:click=move |_| filter.set(value.into())>{label}</button> }).collect_view()}</div><span>{move || crate::tr_format!("{} 项", visible().len())}</span></div>
                <Show when=move || loading.get()><div class="loading-state"><span class="spinner" />{t("正在读取文件…")}</div></Show>
                <Show when=move || error.get().is_some()><div class="inline-error" role="alert"><Icon name="alert" /><p>{move || error.get().unwrap_or_default()}</p><button class="button secondary small" on:click=move |_| reload.update(|n| *n = n.wrapping_add(1))>{t("重试")}</button></div></Show>
                <Show when=move || !loading.get() && error.get().is_none()>
                    <Show when=move || !visible().is_empty() fallback=|| view! { <EmptyState icon="folder" title=t("这里还没有文件") description=t("录制完成后可在这里浏览、预览和整理文件。") /> }>
                        <div class="table-container"><table class="file-table"><thead><tr><th scope="col">{t("名称")}</th><th scope="col" class="file-size">{t("大小")}</th><th scope="col" class="file-date">{t("修改时间")}</th><th scope="col" class="file-actions">{t("操作")}</th></tr></thead><tbody>
                            {move || visible().into_iter().map(|item| {
                                let open_item = item.clone(); let delete_item = item.clone(); let row_name = item.name.clone();
                                let convert_path = item.path.clone(); let processing_path = item.path.clone(); let label_path = item.path.clone();
                                let output = format!("{}.mp4", item.path.rsplit_once('.').map(|(base,_)|base).unwrap_or(&item.path));
                                let exists = listing.with(|list| list.items.iter().any(|entry| entry.path == output));
                                let can_convert = !item.is_dir && item.name.to_ascii_lowercase().ends_with(".ts");
                                let icon = match kind(&item) { "folder" => "folder", "audio" => "volume", "video" => "video", _ => "file" };
                                view! { <tr><td><button class="file-name" on:click=move |_| enter.run(open_item.clone())><span class="file-symbol" data-kind=kind(&item)><Icon name=icon size=21 /></span><span><strong>{item.name.clone()}</strong><small>{if item.is_dir { t("文件夹").into() } else { item.name.rsplit('.').next().unwrap_or(t("文件")).to_ascii_uppercase() }}</small></span></button></td><td class="file-size">{human_size(item.size)}</td><td class="file-date">{modified_time(item.modified)}</td><td class="file-actions">{can_convert.then(|| view! { <button class="button secondary small convert-action" aria-label=crate::tr_format!("转为MP4：{}",item.name) title=if exists {t("同名 MP4 已存在")} else {t("手动转换，保留源 TS")} disabled=move || exists || state.media_jobs.with(|jobs| jobs.iter().any(|j| j.source == processing_path && j.pending())) on:click=move |_| { let path = convert_path.clone(); leptos::task::spawn_local(async move { match gateway::remux_file(&path).await { Ok(()) => state.notify(t("已加入转 MP4 队列，原 TS 保留")), Err(error) => state.fail(error) } }); }><Icon name="file" size=15 />{move || if state.media_jobs.with(|jobs|jobs.iter().any(|job|job.source == label_path && job.pending())) {t("处理中…")} else {t("转 MP4")}}</button> })}<button class="icon-button danger-text" aria-label=crate::tr_format!("回收{}",row_name) title=t("移至回收站") disabled=move || busy.get() on:click=move |_| { deleting.set(Some(delete_item.clone())); confirm.set(true); }><Icon name="trash" size=17 /></button></td></tr> }
                            }).collect_view()}
                        </tbody></table></div>
                    </Show>
                </Show>
            </section>
            <Show when=move || !state.media_jobs.get().is_empty()><section class="media-jobs glass" aria-label=t("媒体处理任务")><h3>{t("媒体处理")}</h3>{move || state.media_jobs.get().into_iter().rev().take(8).map(|job| view! { <div class="media-job-row"><span>{job.source}</span><strong>{crate::app::i18n::message(job.message)}</strong></div> }).collect_view()}</section></Show>
        </div>
        <Dialog open=Signal::derive(move || playing.get().is_some()) title=t("媒体预览") wide=true on_close=Callback::new(move |_| playing.set(None))>
            <p class="dialog-description break-anywhere">{move || playing.get().map(|item| item.name).unwrap_or_default()}</p>
            <Show when=move || playing.get().is_some()><MediaPlayer path=Signal::derive(move || playing.get().map(|item| item.path).unwrap_or_default()) /></Show>
        </Dialog>
        <ConfirmDialog open=confirm title=t("移至回收站") confirm_text=t("移至回收站") busy_text=t("正在回收…") hint=t("可在系统回收站恢复。无法回收时，原文件会保留。") description=Signal::derive(move || deleting.get().map(|item| if item.is_dir { crate::tr_format!("将移至回收站：“{}”及其目录内容。",item.name) } else { crate::tr_format!("将移至回收站：“{}”（{}）。",item.name,human_size(item.size)) }).unwrap_or_default()) busy=busy on_confirm=remove />
    }
}
