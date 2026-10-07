use crate::app::i18n::t;
use crate::{
    api::gateway::{self, StorageItem, StorageListing},
    app::{
        components::{preview_state, ConfirmDialog, EmptyState, Icon, MediaJobStatus},
        labels::{human_size, modified_time},
    },
};
use futures::StreamExt;
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

fn completed_output_keys(jobs: &[gateway::MediaJob]) -> Vec<(String, String, bool)> {
    jobs.iter()
        .filter(|job| job.output_ready())
        .map(|job| (job.id.clone(), job.output.clone(), job.source_removed))
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SortKey {
    Name,
    Modified,
    Size,
}
impl SortKey {
    fn parse(value: &str) -> Self {
        match value {
            "modified" => Self::Modified,
            "size" => Self::Size,
            _ => Self::Name,
        }
    }
    fn value(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Modified => "modified",
            Self::Size => "size",
        }
    }
}
fn sort_preference(value: &str) -> (SortKey, bool) {
    match value.split_once(':') {
        Some((key @ ("name" | "modified" | "size"), direction @ ("asc" | "desc"))) => {
            (SortKey::parse(key), direction == "desc")
        }
        _ => (SortKey::Name, false),
    }
}
fn stored_sort() -> (SortKey, bool) {
    #[cfg(target_arch = "wasm32")]
    if let Some(value) = web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|storage| storage.get_item("streamcap.mediaSort").ok().flatten())
    {
        return sort_preference(&value);
    }
    sort_preference("")
}
fn sort_items(items: &mut [StorageItem], key: SortKey, descending: bool) {
    items.sort_by(|a, b| {
        let dirs = b.is_dir.cmp(&a.is_dir);
        if dirs != std::cmp::Ordering::Equal {
            return dirs;
        }
        let known = |item: &StorageItem| match key {
            SortKey::Name => true,
            SortKey::Size => item.size_known,
            SortKey::Modified => item.modified.is_some_and(f64::is_finite),
        };
        let availability = known(b).cmp(&known(a));
        if availability != std::cmp::Ordering::Equal {
            return availability;
        }
        let order = match key {
            SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortKey::Size => a.size.cmp(&b.size),
            SortKey::Modified => a
                .modified
                .unwrap_or_default()
                .total_cmp(&b.modified.unwrap_or_default()),
        };
        let order = if descending { order.reverse() } else { order };
        order
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.path.cmp(&b.path))
    });
}

#[component]
pub fn StorageView() -> impl IntoView {
    let state = gateway::app_state();
    let subfolder = RwSignal::new(String::new());
    let listing = RwSignal::new(StorageListing::default());
    let reload = RwSignal::new(0_u32);
    let generation = RwSignal::new(0_u32);
    let requests = StoredValue::new_local(None::<web_sys::AbortController>);
    on_cleanup(move || {
        requests.with_value(|request| {
            if let Some(request) = request {
                request.abort();
            }
        })
    });
    let loading = RwSignal::new(true);
    let error = RwSignal::new(None::<String>);
    let keyword = RwSignal::new(String::new());
    let filter = RwSignal::new("all".to_string());
    let (initial_sort, initial_descending) = stored_sort();
    let sort = RwSignal::new(initial_sort);
    let descending = RwSignal::new(initial_descending);
    Effect::new(move |_| {
        let preference = format!(
            "{}:{}",
            sort.get().value(),
            if descending.get() { "desc" } else { "asc" }
        );
        if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
            let _ = storage.set_item("streamcap.mediaSort", &preference);
        }
    });
    let preview = preview_state();
    let deleting = RwSignal::new(None::<StorageItem>);
    let confirm = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let completed_jobs =
        Memo::new(move |_| state.media_jobs.with(|jobs| completed_output_keys(jobs)));
    let show_all_history = RwSignal::new(false);
    let active_jobs = Memo::new(move |_| {
        state.media_jobs.with(|jobs| {
            jobs.iter()
                .filter(|job| job.pending() && job.state != "waiting")
                .chain(jobs.iter().filter(|job| job.state == "waiting"))
                .map(|job| job.id.clone())
                .collect::<Vec<_>>()
        })
    });
    let history_jobs = Memo::new(move |_| {
        state.media_jobs.with(|jobs| {
            jobs.iter()
                .rev()
                .filter(|job| !job.pending())
                .map(|job| job.id.clone())
                .collect::<Vec<_>>()
        })
    });
    let visible_history = Memo::new(move |_| {
        history_jobs.with(|ids| {
            ids.iter()
                .take(if show_all_history.get() {
                    usize::MAX
                } else {
                    8
                })
                .cloned()
                .collect::<Vec<_>>()
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
        requests.with_value(|request| {
            if let Some(request) = request {
                request.abort();
            }
        });
        let Ok(abort) = web_sys::AbortController::new() else {
            loading.set(false);
            error.set(Some(t("无法创建请求").into()));
            return;
        };
        requests.set_value(Some(abort.clone()));
        leptos::task::spawn_local(async move {
            let result = gateway::fetch_storage(
                if folder.is_empty() {
                    None
                } else {
                    Some(folder)
                },
                abort.clone(),
            )
            .await;
            if generation.try_get_untracked() != Some(current) {
                return;
            }
            match result {
                Ok(result) => {
                    let paths = result
                        .items
                        .iter()
                        .filter(|item| !item.size_known)
                        .map(|item| item.path.clone())
                        .collect::<Vec<_>>();
                    listing.set(result);
                    loading.set(false);
                    let mut sizes = futures::stream::iter(paths.into_iter().map(|path| {
                        let abort = abort.clone();
                        async move {
                            let size = gateway::fetch_storage_size(&path, abort).await;
                            (path, size)
                        }
                    }))
                    .buffer_unordered(2);
                    while let Some((path, size)) = sizes.next().await {
                        if generation.try_get_untracked() != Some(current) {
                            return;
                        }
                        listing.update(|list| {
                            if let Some(item) = list.items.iter_mut().find(|item| item.path == path)
                            {
                                match size {
                                    Ok(size) => {
                                        item.size = size;
                                        item.size_known = true;
                                    }
                                    Err(error) => item.size_error = Some(error),
                                }
                            }
                            list.total_size = list
                                .items
                                .iter()
                                .map(|item| item.size)
                                .fold(0u64, u64::saturating_add);
                            list.total_size_known = list.items.iter().all(|item| item.size_known);
                        });
                    }
                }
                Err(message) => error.set(Some(message)),
            }
            loading.set(false);
        });
    });
    let visible = move || {
        let text = keyword.get().trim().to_lowercase();
        let mode = filter.get();
        let mut items = listing
            .get()
            .items
            .into_iter()
            .filter(|item| {
                (mode == "all" || kind(item) == mode) && item.name.to_lowercase().contains(&text)
            })
            .collect::<Vec<_>>();
        sort_items(&mut items, sort.get(), descending.get());
        items
    };
    let enter = Callback::new(move |item: StorageItem| {
        if item.is_dir {
            subfolder.set(item.path);
            keyword.set(String::new());
        } else {
            preview.open_file(item);
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
            <header class="page-header"><div><h1>{t("媒体库")}</h1><p>{t("浏览本地录像，预览或转换为 MP4。")}</p></div><button class="button secondary" disabled=move || loading.get() on:click=move |_| reload.update(|n| *n = n.wrapping_add(1))><Icon name="refresh" size=17 />{t("刷新文件")}</button></header>
            <section class="library-summary glass"><span class="library-symbol"><Icon name="folder" size=30 /></span><div class="library-location"><small>{t("录制文件位置")}</small><strong class="break-anywhere">{move || if listing.get().root.is_empty() { t("正在读取…").into() } else { listing.get().root }}</strong></div><button class="icon-button" aria-label=t("复制录制根目录") disabled=move || listing.get().root.is_empty() on:click=move |_| copy_path.run(listing.get_untracked().root)><Icon name="copy" size=17 /></button><div class="library-size"><small>{t("当前目录大小")}</small><strong>{move || if listing.get().total_size_known { human_size(listing.get().total_size) } else if listing.with(|list|list.items.iter().any(|item|item.size_error.is_some())) { t("部分体积不可用").into() } else { t("体积计算中…").into() }}</strong></div></section>
            <section class="file-browser glass">
                <div class="file-browser-toolbar"><nav class="file-breadcrumb" aria-label=t("媒体库路径")><button aria-label=t("返回媒体库根目录") on:click=move |_| subfolder.set(String::new())><Icon name="folder" size=17 />{t("媒体库")}</button>
                    {move || { let path = subfolder.get(); let mut prefix = String::new(); path.split('/').filter(|p| !p.is_empty()).map(|part| { if !prefix.is_empty() { prefix.push('/'); } prefix.push_str(part); let target = prefix.clone(); view! { <Icon name="chevron" size=13 /><button on:click=move |_| subfolder.set(target.clone())>{part.to_string()}</button> } }).collect_view() }}
                </nav><label class="search-field compact"><Icon name="search" size=17 /><input aria-label=t("搜索文件") placeholder=t("搜索当前目录…") prop:value=move || keyword.get() on:input=move |e| keyword.set(event_target_value(&e)) /></label></div>
                <div class="file-filter-row"><div class="mini-tabs" role="group" aria-label=t("文件类型")>{[("all",t("全部文件")),("video",t("视频")),("audio",t("音频")),("folder",t("文件夹"))].into_iter().map(|(value,label)| view! { <button class:active=move || filter.get() == value aria-pressed=move || (filter.get() == value).to_string() on:click=move |_| filter.set(value.into())>{label}</button> }).collect_view()}</div><div class="file-sort-controls"><select class="input" aria-label=t("媒体排序") prop:value=move || sort.get().value() on:change=move |e| sort.set(SortKey::parse(&event_target_value(&e)))><option value="name">{t("名称")}</option><option value="modified">{t("修改时间")}</option><option value="size">{t("大小")}</option></select><button class="button secondary small" type="button" aria-label=t("切换排序方向") aria-pressed=move || descending.get().to_string() on:click=move |_| descending.update(|value| *value = !*value)>{move || if descending.get() { t("降序") } else { t("升序") }}</button><span>{move || crate::tr_format!("{} 项", visible().len())}</span></div></div>
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
                                view! { <tr><td><button class="file-name" on:click=move |_| enter.run(open_item.clone())><span class="file-symbol" data-kind=kind(&item)><Icon name=icon size=21 /></span><span><strong>{item.name.clone()}</strong><small>{if item.is_dir { t("文件夹").into() } else { item.name.rsplit('.').next().unwrap_or(t("文件")).to_ascii_uppercase() }}</small></span></button></td><td class="file-size">{if item.size_known { human_size(item.size) } else if item.size_error.is_some() { t("体积不可用").into() } else { t("计算中…").into() }}</td><td class="file-date">{modified_time(item.modified)}</td><td class="file-actions">{can_convert.then(|| view! { <button class="button secondary small convert-action" aria-label=crate::tr_format!("转为MP4：{}",item.name) title=if exists {t("同名 MP4 已存在")} else {t("手动转换，保留源 TS")} disabled=move || exists || state.media_jobs.with(|jobs| jobs.iter().any(|j| j.source == processing_path && j.pending())) on:click=move |_| { let path = convert_path.clone(); leptos::task::spawn_local(async move { match gateway::remux_file(&path).await { Ok(()) => state.notify(t("已加入转 MP4 队列，原 TS 保留")), Err(error) => state.fail(error) } }); }><Icon name="file" size=15 />{move || if state.media_jobs.with(|jobs|jobs.iter().any(|job|job.source == label_path && job.pending())) {t("处理中…")} else {t("转 MP4")}}</button> })}<button class="icon-button danger-text" aria-label=crate::tr_format!("回收{}",row_name) title=t("移至回收站") disabled=move || busy.get() on:click=move |_| { deleting.set(Some(delete_item.clone())); confirm.set(true); }><Icon name="trash" size=17 /></button></td></tr> }
                            }).collect_view()}
                        </tbody></table></div>
                    </Show>
                </Show>
            </section>
            <Show when=move || !active_jobs.get().is_empty() || !history_jobs.get().is_empty()>
                <section class="media-jobs glass" aria-label=t("媒体处理任务")>
                    <div class="section-heading compact"><div><h2>{t("媒体处理")}</h2><p>{t("正在处理的文件优先显示，原文件按转换设置保留。")}</p></div>
                        <span class="count-pill">{move || crate::tr_format!("{} 项处理中", active_jobs.get().len())}</span>
                    </div>
                    <div class="media-active-jobs">
                        <For each=move || active_jobs.get() key=|id| id.clone() children=move |id| {
                            let job = Memo::new(move |_| state.media_jobs.with(|jobs| jobs.iter().find(|job| job.id == id).cloned()));
                            view! { <MediaJobStatus job=job /> }
                        } />
                    </div>
                    <Show when=move || !history_jobs.get().is_empty()>
                        <details class="media-history" open>
                            <summary>{t("处理记录")}<span class="count-pill">{move || history_jobs.get().len()}</span></summary>
                            <For each=move || visible_history.get() key=|id| id.clone() children=move |id| {
                                let job = Memo::new(move |_| state.media_jobs.with(|jobs| jobs.iter().find(|job| job.id == id).cloned()));
                                view! { <MediaJobStatus job=job /> }
                            } />
                            <Show when=move || { history_jobs.get().len() > 8 }>
                                <button class="button secondary small history-toggle" type="button"
                                    aria-expanded=move || show_all_history.get().to_string()
                                    on:click=move |_| show_all_history.update(|show| *show = !*show)>
                                    {move || if show_all_history.get() { t("收起较早记录") } else { t("展开全部记录") }}
                                </button>
                            </Show>
                        </details>
                    </Show>
                </section>
            </Show>
        </div>
        <ConfirmDialog open=confirm title=t("移至回收站") confirm_text=t("移至回收站") busy_text=t("正在回收…") hint=t("可在系统回收站恢复。无法回收时，原文件会保留。") description=Signal::derive(move || deleting.get().map(|item| if item.is_dir { crate::tr_format!("将移至回收站：“{}”及其目录内容。",item.name) } else { crate::tr_format!("将移至回收站：“{}”（{}）。",item.name,human_size(item.size)) }).unwrap_or_default()) busy=busy on_confirm=remove />
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_new_output_refreshes_the_library_even_when_history_count_is_unchanged() {
        let job = |id: &str| {
            serde_json::from_value(serde_json::json!({
                "id":id,"taskId":null,"source":"source.ts","output":"output.mp4",
                "state":"complete","message":"","sourceRemoved":false
            }))
            .unwrap()
        };
        let before = vec![job("previous")];
        let after = vec![job("latest")];
        assert_ne!(
            completed_output_keys(&before),
            completed_output_keys(&after)
        );
        let mut cleaning = after.clone();
        cleaning[0].source_removed = true;
        assert_ne!(
            completed_output_keys(&after),
            completed_output_keys(&cleaning)
        );
    }
    fn item(name: &str, dir: bool, size: u64, known: bool, modified: Option<f64>) -> StorageItem {
        StorageItem {
            name: name.into(),
            path: name.into(),
            is_dir: dir,
            size,
            size_known: known,
            size_error: None,
            modified,
        }
    }
    #[test]
    fn sort_preferences_reject_dirty_storage() {
        assert_eq!(sort_preference("size:desc"), (SortKey::Size, true));
        for value in ["", "other:desc", "size:broken", "name", "name:desc:extra"] {
            assert_eq!(sort_preference(value), (SortKey::Name, false));
        }
    }
    #[test]
    fn sorting_keeps_directories_first_and_unknown_values_last() {
        let mut items = vec![
            item("unknown", false, 0, false, None),
            item("large", false, 30, true, Some(5.0)),
            item("small", false, 10, true, Some(9.0)),
            item("folder", true, 1, true, None),
        ];
        for desc in [false, true] {
            sort_items(&mut items, SortKey::Size, desc);
            assert_eq!(items[0].name, "folder");
            assert_eq!(items[3].name, "unknown");
            assert_eq!(items[1].name, if desc { "large" } else { "small" });
        }
        sort_items(&mut items, SortKey::Modified, true);
        assert_eq!(items[1].name, "small");
        assert_eq!(items[3].name, "unknown");
        items[3].size = 50;
        items[3].size_known = true;
        sort_items(&mut items, SortKey::Size, true);
        assert_eq!(items[1].name, "unknown");
    }
}
