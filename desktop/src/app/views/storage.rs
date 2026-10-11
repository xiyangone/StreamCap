use crate::app::i18n::t;
use crate::{
    api::gateway::{self, StorageItem, StorageListing},
    app::{
        components::{
            preview_state, ActionButton, ConfirmDialog, EmptyState, FormFeedback, Icon, IconButton,
            PageHeader,
        },
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
    let listed_folder = RwSignal::new(None::<String>);
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
    let copying = RwSignal::new(false);
    let conversion_submissions = RwSignal::new(std::collections::HashSet::<String>::new());
    let conversion_errors = RwSignal::new(std::collections::HashMap::<String, String>::new());
    let completed_jobs =
        Memo::new(move |_| state.media_jobs.with(|jobs| completed_output_keys(jobs)));
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
            let _ = loading.try_set(false);
            error.set(Some(t("无法创建请求").into()));
            return;
        };
        requests.set_value(Some(abort.clone()));
        leptos::task::spawn_local(async move {
            let requested_folder = folder.clone();
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
                    listed_folder.set(Some(requested_folder));
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
            let _ = loading.try_set(false);
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
        if loading.get_untracked() || busy.get_untracked() {
            return;
        }
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
        if busy.get_untracked()
            || loading.get_untracked()
            || !state.status.get_untracked().ok
            || conversion_submissions.with_untracked(|paths| !paths.is_empty())
        {
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
        if copying.get_untracked() || path.is_empty() {
            return;
        }
        copying.set(true);
        leptos::task::spawn_local(async move {
            match js_sys::futures::JsFuture::from(
                window().navigator().clipboard().write_text(&path),
            )
            .await
            {
                Ok(_) => state.notify(t("路径已复制")),
                Err(_) => state.fail(t("无法访问剪贴板，请手动复制路径")),
            }
            let _ = copying.try_set(false);
        });
    });
    let convert = Callback::new(move |path: String| {
        let output = format!(
            "{}.mp4",
            path.rsplit_once('.').map(|(base, _)| base).unwrap_or(&path)
        );
        if busy.get_untracked()
            || loading.get_untracked()
            || !state.status.get_untracked().ok
            || conversion_submissions.with_untracked(|paths| paths.contains(&path))
            || state
                .media_jobs
                .with_untracked(|jobs| jobs.iter().any(|job| job.source == path && job.pending()))
            || listing.with_untracked(|list| list.items.iter().any(|item| item.path == output))
        {
            return;
        }
        if !listing.with_untracked(|list| {
            list.items.iter().any(|item| {
                item.path == path && !item.is_dir && item.name.to_ascii_lowercase().ends_with(".ts")
            })
        }) {
            return;
        }
        conversion_submissions.update(|paths| {
            paths.insert(path.clone());
        });
        conversion_errors.update(|errors| {
            errors.remove(&path);
        });
        leptos::task::spawn_local(async move {
            match gateway::remux_file(&path).await {
                Ok(()) => {
                    state.notify(t("已加入转 MP4 队列，原 TS 保留"));
                    if let Err(error) = gateway::refresh_recordings(state).await {
                        let _ = conversion_errors.try_update(|errors| {
                            errors.insert(
                                path.clone(),
                                crate::tr_format!("任务已提交，状态同步失败：{error}"),
                            );
                        });
                    }
                }
                Err(error) => {
                    let _ = conversion_errors.try_update(|errors| {
                        errors.insert(path.clone(), error);
                    });
                }
            }
            let _ = conversion_submissions.try_update(|paths| {
                paths.remove(&path);
            });
        });
    });
    view! {
        <div class="page storage-page">
            <PageHeader title=t("媒体库") subtitle=t("浏览、预览和整理录制文件。") />
            <section class="file-browser file-manager">
                <header class="file-manager-toolbar">
                <div class="file-browser-toolbar"><div class="file-location">
                <nav class="file-breadcrumb" aria-label=t("媒体库路径")>
                    <ActionButton label=t("媒体库") icon="folder" variant="ghost" class="breadcrumb-action" aria_label=t("返回媒体库根目录").to_string()
                        disabled=busy on_click=Callback::new(move |_| { subfolder.set(String::new()); keyword.set(String::new()); }) />
                    {move || { let path = subfolder.get(); let mut prefix = String::new(); path.split('/').filter(|p| !p.is_empty()).map(|part| { if !prefix.is_empty() { prefix.push('/'); } prefix.push_str(part); let target = StoredValue::new(prefix.clone()); let label = part.to_string(); view! {
                        <Icon name="chevron" size=13 /><ActionButton label=t("打开目录") variant="ghost" class="breadcrumb-action" aria_label=label.clone()
                            current=Signal::derive(move || (subfolder.get() == target.get_value()).then_some("page")) disabled=busy
                            on_click=Callback::new(move |_| { subfolder.set(target.get_value()); keyword.set(String::new()); })><span>{label}</span></ActionButton>
                    } }).collect_view() }}
                </nav>
                <div class="file-root-line"><span class="sr-only">{t("录制文件位置")}</span>
                    <code class="file-root-value" title=move || listing.with(|list| list.root.clone())>{move || listing.with(|list| if list.root.is_empty() { t("正在读取…").to_string() } else { list.root.clone() })}</code>
                </div></div>
                <div class="file-directory-actions" role="group" aria-label=t("目录操作")>
                    <IconButton label=t("复制录制根目录") icon="copy" busy=copying disabled=Signal::derive(move || listing.get().root.is_empty())
                        on_click=Callback::new(move |_| copy_path.run(listing.get_untracked().root)) />
                    <IconButton label=t("刷新文件") icon="refresh" busy=loading
                        disabled=busy on_click=Callback::new(move |_| reload.update(|n| *n = n.wrapping_add(1))) />
                </div></div>
                <div class="file-filter-row"><div class="mini-tabs" role="group" aria-label=t("文件类型")>{[("all",t("全部文件")),("video",t("视频")),("audio",t("音频")),("folder",t("文件夹"))].into_iter().map(|(value,label)| view! {
                    <ActionButton label=label variant="ghost" pressed=Signal::derive(move || Some(filter.get() == value)) on_click=Callback::new(move |_| filter.set(value.into())) />
                }).collect_view()}</div>
                <div class="search-field file-search"><Icon name="search" size=17 /><input class="ui-input ui-input-sm" aria-label=t("搜索文件") placeholder=t("搜索当前目录…") prop:value=move || keyword.get() on:input=move |e| keyword.set(event_target_value(&e)) />
                    <Show when=move || !keyword.get().is_empty()><IconButton label=t("清除搜索") icon="close" on_click=Callback::new(move |_| keyword.set(String::new())) /></Show>
                </div>
                <div class="file-sort-controls"><label class="toolbar-field"><span class="sr-only">{t("排序")}</span><select class="ui-select ui-select-sm input" aria-label=t("媒体排序") prop:value=move || sort.get().value() on:change=move |e| sort.set(SortKey::parse(&event_target_value(&e)))><option value="name">{t("名称")}</option><option value="modified">{t("修改时间")}</option><option value="size">{t("大小")}</option></select></label>
                    <ActionButton label=Signal::derive(move || if descending.get() { t("降序") } else { t("升序") }) class="small" variant="ghost"
                        aria_label=t("切换排序方向").to_string() pressed=Signal::derive(move || Some(descending.get())) on_click=Callback::new(move |_| descending.update(|value| *value = !*value)) />
                </div></div>
                </header>
                <div class="file-manager-content">
                <Show when=move || loading.get()><div class="loading-state"><span class="ui-loading ui-loading-spinner ui-loading-sm spinner" aria-hidden="true" />{t("正在读取文件…")}</div></Show>
                <Show when=move || error.get().is_some()><div class="inline-error"><FormFeedback error=error title=t("操作未完成") /><ActionButton label=t("重试") class="small" disabled=loading on_click=Callback::new(move |_| reload.update(|n| *n = n.wrapping_add(1))) /></div></Show>
                <Show when=move || error.get().is_none() && listed_folder.get().as_deref() == Some(subfolder.get().as_str())>
                    <Show when=move || !visible().is_empty() fallback=move || view! {
                        <Show when=move || !loading.get()>
                            <Show when=move || listing.with(|list| list.items.is_empty()) fallback=move || view! {
                                <div class="empty-panel"><EmptyState icon="search" title=t("没有匹配的文件") description=t("当前目录中有文件，请调整文件类型或搜索条件。") />
                                    <ActionButton label=t("清除筛选") on_click=Callback::new(move |_| { keyword.set(String::new()); filter.set("all".into()); }) />
                                </div>
                            }><EmptyState icon="folder" title=t("这里还没有文件") description=t("录制完成后可在这里浏览、预览和整理文件。") /></Show>
                        </Show>
                    }>
                        <div class="table-container"><table class="ui-table ui-table-sm file-table"><thead><tr>
                            <th scope="col" aria-sort=move || if sort.get() == SortKey::Name { if descending.get() { "descending" } else { "ascending" } } else { "none" }>{t("名称")}</th>
                            <th scope="col" class="file-size" aria-sort=move || if sort.get() == SortKey::Size { if descending.get() { "descending" } else { "ascending" } } else { "none" }>{t("大小")}</th>
                            <th scope="col" class="file-date" aria-sort=move || if sort.get() == SortKey::Modified { if descending.get() { "descending" } else { "ascending" } } else { "none" }>{t("修改时间")}</th>
                            <th scope="col" class="file-actions">{t("操作")}</th></tr></thead><tbody>
                            <For each=visible key=|item| item.path.clone() children=move |initial| {
                                let icon = match kind(&initial) { "folder" => "folder", "audio" => "volume", "video" => "video", _ => "file" };
                                let initial = StoredValue::new(initial);
                                let item = Memo::new(move |_| initial.with_value(|initial| listing.with(|list| list.items.iter().find(|entry| entry.path == initial.path).cloned().unwrap_or_else(|| initial.clone()))));
                                let exists = Memo::new(move |_| item.with(|item| {
                                    let output = format!("{}.mp4", item.path.rsplit_once('.').map(|(base,_)|base).unwrap_or(&item.path));
                                    listing.with(|list| list.items.iter().any(|entry| entry.path == output))
                                }));
                                let submitted = Signal::derive(move || item.with(|item| conversion_submissions.with(|paths| paths.contains(&item.path))));
                                let processing = Memo::new(move |_| item.with(|item| state.media_jobs.with(|jobs| jobs.iter().any(|job| job.source == item.path && job.pending()))));
                                let conversion_hint = Signal::derive(move || if exists.get() { t("同名 MP4 已存在") } else if processing.get() { t("处理中…") } else if !state.status.get().ok { t("本地服务未连接") } else { t("手动转换，保留源 TS") });
                                let row_error = Signal::derive(move || item.with(|item| conversion_errors.with(|errors| errors.get(&item.path).cloned())));
                                view! {
                                    <tr data-path=move || item.get().path>
                                        <td><ActionButton label=t("打开文件") variant="ghost" class="file-name"
                                            aria_label=Signal::derive(move || item.get().name)
                                            disabled=Signal::derive(move || loading.get() || busy.get())
                                            on_click=Callback::new(move |_| enter.run(item.get_untracked()))>
                                            <span class="file-symbol" data-kind=move || item.with(kind)><Icon name=icon size=21 /></span>
                                            <span class="file-name-copy"><strong title=move || item.get().name>{move || item.get().name}</strong><small>{move || item.with(|item| if item.is_dir { t("文件夹").into() } else { item.name.rsplit('.').next().unwrap_or(t("文件")).to_ascii_uppercase() })}</small></span>
                                        </ActionButton></td>
                                        <td class="file-size">{move || item.with(|item| if item.size_known { human_size(item.size) } else if item.size_error.is_some() { t("体积不可用").into() } else { t("计算中…").into() })}</td>
                                        <td class="file-date">{move || modified_time(item.get().modified)}</td>
                                        <td class="file-actions"><div class="file-row-actions">
                                            <Show when=move || item.with(|item| !item.is_dir && item.name.to_ascii_lowercase().ends_with(".ts"))>
                                                <span class="file-convert-control" role="group" aria-label=move || conversion_hint.get()
                                                    tabindex=move || if exists.get() || processing.get() || !state.status.get().ok { "0" } else { "-1" }>
                                                <ActionButton label=Signal::derive(move || if processing.get() { t("处理中…") } else { t("转 MP4") }) icon="file" class="small convert-action" variant="ghost"
                                                    aria_label=Signal::derive(move || crate::tr_format!("转为MP4：{}", item.get().name))
                                                    title=conversion_hint
                                                    busy=submitted busy_label=t("正在提交…")
                                                    disabled=Signal::derive(move || exists.get() || processing.get() || loading.get() || busy.get() || !state.status.get().ok)
                                                    on_click=Callback::new(move |_| convert.run(item.get_untracked().path)) />
                                                <span class="file-convert-hint" role="tooltip">{move || conversion_hint.get()}</span>
                                                </span>
                                            </Show>
                                            <IconButton label=t("移至回收站") icon="trash" danger=true
                                                aria_label=Signal::derive(move || crate::tr_format!("回收{}", item.get().name))
                                                disabled=Signal::derive(move || busy.get() || loading.get() || submitted.get() || processing.get() || !state.status.get().ok)
                                                on_click=Callback::new(move |_| { deleting.set(Some(item.get_untracked())); confirm.set(true); }) />
                                        </div>
                                            <FormFeedback error=row_error title=t("操作未完成") />
                                        </td>
                                    </tr>
                                }
                            } />
                        </tbody></table></div>
                    </Show>
                </Show>
                </div>
                <footer class="file-manager-status" aria-label=t("目录摘要")>
                    <span>{move || crate::tr_format!("显示 {} / {} 项", visible().len(), listing.with(|list| list.items.len()))}</span>
                    <span class="file-manager-total"><span>{t("当前目录大小")}</span><strong>{move || listing.with(|list| if list.total_size_known { human_size(list.total_size) } else if list.items.iter().any(|item| item.size_error.is_some()) { t("部分体积不可用").into() } else { t("体积计算中…").into() })}</strong></span>
                </footer>
            </section>
        </div>
        <ConfirmDialog open=confirm title=t("移至回收站") confirm_text=t("移到回收站") busy_text=t("正在移入回收站…") hint=t("可在系统回收站恢复。无法回收时，原文件会保留。") description=Signal::derive(move || deleting.get().map(|item| if item.is_dir { crate::tr_format!("将移至回收站：“{}”及其目录内容。",item.name) } else { crate::tr_format!("将移至回收站：“{}”（{}）。",item.name,human_size(item.size)) }).unwrap_or_default()) busy=busy disabled=Signal::derive(move || loading.get() || !state.status.get().ok || conversion_submissions.with(|paths| !paths.is_empty())) on_confirm=remove />
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
