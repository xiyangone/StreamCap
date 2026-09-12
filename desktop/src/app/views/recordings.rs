use crate::{
    api::gateway::{self, Recording},
    app::{
        components::{
            AddRecordingDialog, BatchEditDialog, CardInfoDialog, ConfirmDialog,
            EditRecordingDialog, EmptyState, Icon, PreviewDialog, RecordingCard,
        },
        labels::platform_label,
    },
};
use leptos::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Recording,
    Live,
    Monitoring,
    Paused,
}
impl Filter {
    fn matches(self, rec: &Recording) -> bool {
        match self {
            Self::All => true,
            Self::Recording => rec.is_recording,
            Self::Live => rec.is_live,
            Self::Monitoring => rec.monitor_status && !rec.is_recording && !rec.is_live,
            Self::Paused => !rec.monitor_status,
        }
    }
}

#[component]
pub fn RecordingsView() -> impl IntoView {
    let state = gateway::app_state();
    let keyword = RwSignal::new(String::new());
    let platform = RwSignal::new(String::new());
    let filter = RwSignal::new(Filter::All);
    let sorting = RwSignal::new("status".to_string());
    let selected = RwSignal::new(Vec::<String>::new());
    let busy = RwSignal::new(false);
    let add = RwSignal::new(false);
    let editing = RwSignal::new(None::<Recording>);
    let info = RwSignal::new(None::<Recording>);
    let preview = RwSignal::new(None::<Recording>);
    let batch_open = RwSignal::new(false);
    let batch_ids = RwSignal::new(Vec::<String>::new());
    let delete_open = RwSignal::new(false);
    let delete_ids = RwSignal::new(Vec::<String>::new());
    let visible = Memo::new(move |_| {
        let query = keyword.get().trim().to_lowercase();
        let key = platform.get();
        let mode = filter.get();
        let mut items: Vec<_> = state
            .recordings
            .get()
            .into_iter()
            .filter(|rec| mode.matches(rec))
            .filter(|rec| key.is_empty() || rec.platform_key.as_deref().unwrap_or("custom") == key)
            .filter(|rec| {
                query.is_empty()
                    || rec.name().to_lowercase().contains(&query)
                    || rec.url.to_lowercase().contains(&query)
                    || rec
                        .live_title
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&query)
            })
            .collect();
        if sorting.get() == "name" {
            items.sort_by_key(|r| r.name().to_lowercase());
        } else {
            items.sort_by_key(|r| {
                (
                    !r.is_recording,
                    !r.is_live,
                    !r.monitor_status,
                    r.name().to_lowercase(),
                )
            });
        }
        items
    });
    let platforms = Memo::new(move |_| {
        let mut keys: Vec<String> = state
            .recordings
            .get()
            .into_iter()
            .map(|r| r.platform_key.unwrap_or_else(|| "custom".into()))
            .collect();
        keys.sort();
        keys.dedup();
        keys
    });
    Effect::new(move |_| {
        let items = state.recordings.get();
        selected.update(|ids| ids.retain(|id| items.iter().any(|rec| &rec.rec_id == id)));
    });
    let all_visible_selected = move || {
        let items = visible.get();
        !items.is_empty() && selected.with(|ids| items.iter().all(|rec| ids.contains(&rec.rec_id)))
    };
    let select_visible = move |_| {
        let ids: Vec<_> = visible
            .get_untracked()
            .into_iter()
            .map(|rec| rec.rec_id)
            .collect();
        let all = all_visible_selected();
        selected.update(|values| {
            if all {
                values.retain(|id| !ids.contains(id));
            } else {
                for id in ids {
                    if !values.contains(&id) {
                        values.push(id);
                    }
                }
            }
        });
    };
    let has_active_selection = move || {
        selected.with(|ids| {
            state.recordings.with(|items| {
                items
                    .iter()
                    .any(|r| ids.contains(&r.rec_id) && r.is_recording)
            })
        })
    };
    let refresh = move |_| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        leptos::task::spawn_local(async move {
            match gateway::refresh_recordings(state).await {
                Ok(()) => state.notify("任务列表已同步"),
                Err(message) => state.fail(message),
            }
            let _ = busy.try_set(false);
        });
    };
    let monitor_selected = Callback::new(move |enabled: bool| {
        if busy.get_untracked() {
            return;
        }
        let ids = selected.get_untracked();
        if ids.is_empty() {
            return;
        }
        busy.set(true);
        leptos::task::spawn_local(async move {
            match gateway::fetch_recordings().await {
                Ok(items) => {
                    let mut failures = Vec::new();
                    for id in &ids {
                        match items.iter().find(|r| &r.rec_id == id) {
                            Some(rec) if rec.monitor_status != enabled => {
                                if let Err(error) = gateway::toggle_monitor(id).await {
                                    failures.push(error);
                                }
                            }
                            None => failures.push("部分任务已不存在".into()),
                            _ => {}
                        }
                    }
                    if failures.is_empty() {
                        state.notify(format!(
                            "已{}所选 {} 个任务的监控",
                            if enabled { "开启" } else { "暂停" },
                            ids.len()
                        ));
                    } else {
                        state.fail(format!(
                            "{} 个任务操作失败：{}",
                            failures.len(),
                            failures[0]
                        ));
                    }
                    if let Err(error) = gateway::refresh_recordings(state).await {
                        state.fail(error);
                    }
                }
                Err(error) => state.fail(error),
            }
            let _ = busy.try_set(false);
        });
    });
    let delete = Callback::new(move |_| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        let ids = delete_ids.get_untracked();
        leptos::task::spawn_local(async move {
            match gateway::delete_recordings(&ids).await {
                Ok(()) => {
                    let _ = delete_open.try_set(false);
                    let _ = selected.try_update(|values| values.retain(|id| !ids.contains(id)));
                    state.notify("所选任务已移除，录制文件保留");
                    if let Err(error) = gateway::refresh_recordings(state).await {
                        state.fail(error);
                    }
                }
                Err(error) => state.fail(error),
            }
            let _ = busy.try_set(false);
        });
    });
    let set_layout = Callback::new(move |grid: bool| {
        state.grid_view.set(grid);
        leptos::task::spawn_local(async move {
            if let Err(error) =
                gateway::save_settings(serde_json::json!({"is_grid_view":grid})).await
            {
                state.fail(format!("视图已切换，偏好保存失败：{error}"));
            }
        });
    });
    view! {
        <div class="page recordings-page">
            <header class="page-header"><div><span class="eyebrow">"LIVE COLLECTION"</span><h1>"录制任务"<span class="heading-count">{move || state.recordings.get().len()}</span></h1><p>"管理你的直播间，让录制井然有序。"</p></div><div class="header-actions"><button class="button secondary" disabled=move || busy.get() on:click=refresh><Icon name="refresh" size=17 />"同步"</button><button class="button primary" on:click=move |_| add.set(true)><Icon name="plus" size=18 />"添加直播间"</button></div></header>
            <section class="task-toolbar glass" aria-label="任务筛选">
                <div class="filter-tabs" role="group" aria-label="状态筛选">
                    {[(Filter::All,"全部"),(Filter::Recording,"录制中"),(Filter::Live,"直播中"),(Filter::Monitoring,"监控中"),(Filter::Paused,"已暂停")].into_iter().map(|(mode,label)| view! {
                        <button class="filter-tab" class:active=move || filter.get() == mode aria-pressed=move || (filter.get() == mode).to_string() on:click=move |_| filter.set(mode)>{label}<span>{move || state.recordings.with(|items| items.iter().filter(|r| mode.matches(r)).count())}</span></button>
                    }).collect_view()}
                </div>
                <div class="toolbar-controls">
                    <label class="search-field"><Icon name="search" size=18 /><input aria-label="搜索直播间" placeholder="搜索主播、标题或地址…" prop:value=move || keyword.get() on:input=move |e| keyword.set(event_target_value(&e)) /></label>
                    <select class="input platform-filter" aria-label="筛选平台" prop:value=move || platform.get() on:change=move |e| platform.set(event_target_value(&e))><option value="">"全部平台"</option>{move || platforms.get().into_iter().map(|key| { let label = platform_label(&key); view! { <option value=key>{label}</option> } }).collect_view()}</select>
                    <select class="input sort-select" aria-label="任务排序" prop:value=move || sorting.get() on:change=move |e| sorting.set(event_target_value(&e))><option value="status">"状态优先"</option><option value="name">"名称排序"</option></select>
                    <div class="view-switch" role="group" aria-label="视图模式"><button class:active=move || state.grid_view.get() aria-label="网格视图" title="网格视图" aria-pressed=move || state.grid_view.get().to_string() on:click=move |_| set_layout.run(true)><Icon name="grid" size=18 /></button><button class:active=move || !state.grid_view.get() aria-label="列表视图" title="列表视图" aria-pressed=move || (!state.grid_view.get()).to_string() on:click=move |_| set_layout.run(false)><Icon name="list" size=18 /></button></div>
                </div>
            </section>
            <div class="results-line"><label class="select-all"><input type="checkbox" prop:checked=all_visible_selected disabled=move || visible.get().is_empty() on:change=select_visible /><span>"全选当前结果"</span></label><span>{move || format!("显示 {} 个直播间", visible.get().len())}</span></div>
            <Show when=move || !selected.get().is_empty()><div class="batch-bar glass" role="region" aria-label="所选任务操作"><strong>{move || format!("已选 {} 项", selected.get().len())}</strong><button class="button secondary small" disabled=move || busy.get() || has_active_selection() || !state.status.get().ok title="录制中的任务须先停止" on:click=move |_| { batch_ids.set(selected.get_untracked()); batch_open.set(true); }><Icon name="edit" size=15 />"批量编辑"</button><button class="button secondary small" disabled=move || busy.get() || !state.status.get().ok on:click=move |_| monitor_selected.run(true)>"开启监控"</button><button class="button secondary small" disabled=move || busy.get() || !state.status.get().ok on:click=move |_| monitor_selected.run(false)>"暂停监控"</button><button class="button ghost-danger small" disabled=move || busy.get() || has_active_selection() || !state.status.get().ok on:click=move |_| { delete_ids.set(selected.get_untracked()); delete_open.set(true); }><Icon name="trash" size=15 />"移除所选"</button><button class="icon-button" aria-label="取消选择" on:click=move |_| selected.set(Vec::new())><Icon name="close" size=17 /></button></div></Show>
            <Show when=move || state.loading.get()><div class="skeleton-grid" aria-label="加载任务"><div class="skeleton" /><div class="skeleton" /><div class="skeleton" /></div></Show>
            <Show when=move || !state.loading.get() && visible.get().is_empty()><div class="glass empty-panel"><EmptyState icon="search" title="没有匹配的直播间" description="尝试调整搜索条件，或添加新的直播间。" /><button class="button secondary" on:click=move |_| { keyword.set(String::new()); platform.set(String::new()); filter.set(Filter::All); }>"清除筛选"</button></div></Show>
            <div class="cards-grid" class:list-view=move || !state.grid_view.get()>
                <For each=move || visible.get() key=|rec| rec.rec_id.clone() children=move |rec| view! { <RecordingCard recording=rec selectable=true selected=selected on_edit=Callback::new(move |r| editing.set(Some(r))) on_info=Callback::new(move |r| info.set(Some(r))) on_preview=Callback::new(move |r| preview.set(Some(r))) /> } />
            </div>
        </div>
        <AddRecordingDialog open=add /><EditRecordingDialog target=editing /><BatchEditDialog open=batch_open ids=batch_ids /><CardInfoDialog target=info /><PreviewDialog target=preview />
        <ConfirmDialog open=delete_open title="移除所选直播间" description=Signal::derive(move || format!("将移除选中的 {} 个直播间。其他任务及已有录制文件保持不变。", delete_ids.get().len())) busy=busy on_confirm=delete />
    }
}
