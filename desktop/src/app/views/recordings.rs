use crate::app::i18n::t;
use crate::{
    api::gateway::{self, Recording, TaskPhase},
    app::{
        components::{
            preview_state, ActionButton, AddRecordingDialog, BatchEditDialog, CardInfoDialog,
            ConfirmDialog, EditRecordingDialog, EmptyState, Icon, IconButton, PageHeader,
            RecordingCard,
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
    Attention,
}
impl Filter {
    fn matches(self, phase: TaskPhase) -> bool {
        match self {
            Self::All => true,
            Self::Recording => phase == TaskPhase::Recording,
            Self::Live => phase == TaskPhase::LiveIdle,
            Self::Monitoring => phase == TaskPhase::Waiting,
            Self::Paused => phase == TaskPhase::Paused,
            Self::Attention => phase == TaskPhase::Attention,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn each_task_matches_one_and_only_one_status_filter() {
        for live in [false, true] {
            for recording in [false, true] {
                for monitor in [false, true] {
                    for error in [false, true] {
                        let r = Recording {
                            is_live: live,
                            is_recording: recording,
                            monitor_status: monitor,
                            check_error: error.then(|| "unreadable".into()),
                            ..Default::default()
                        };
                        let filters = [
                            Filter::Recording,
                            Filter::Live,
                            Filter::Monitoring,
                            Filter::Paused,
                            Filter::Attention,
                        ];
                        assert_eq!(
                            filters
                                .into_iter()
                                .filter(|f| f.matches(r.task_phase()))
                                .count(),
                            1,
                            "{r:?}"
                        );
                    }
                }
            }
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
    let active_card_operations = RwSignal::new(0usize);
    let selection_locked = Signal::derive(move || busy.get() || active_card_operations.get() > 0);
    let card_busy_changed = Callback::new(move |active: bool| {
        let _ = active_card_operations.try_update(|count| {
            *count = if active {
                count.saturating_add(1)
            } else {
                count.saturating_sub(1)
            };
        });
    });
    let batch_progress = RwSignal::new(None::<(usize, usize)>);
    let batch_status = RwSignal::new("");
    let add = RwSignal::new(false);
    let editing = RwSignal::new(None::<Recording>);
    let info = RwSignal::new(None::<Recording>);
    let preview = preview_state();
    let batch_open = RwSignal::new(false);
    let batch_ids = RwSignal::new(Vec::<String>::new());
    let delete_open = RwSignal::new(false);
    let delete_ids = RwSignal::new(Vec::<String>::new());
    let processing = Memo::new(move |_| {
        state
            .media_jobs
            .with(|jobs| jobs.iter().filter(|job| job.pending()).count())
    });
    let visible = Memo::new(move |_| {
        let query = keyword.get().trim().to_lowercase();
        let key = platform.get();
        let mode = filter.get();
        let by_name = sorting.get() == "name";
        state.recording_list.with(|list| {
            let mut items: Vec<_> = list
                .iter()
                .filter(|rec| mode.matches(rec.phase))
                .filter(|rec| key.is_empty() || rec.platform_key == key)
                .filter(|rec| query.is_empty() || rec.search_text.contains(&query))
                .collect();
            items.sort_by_cached_key(|rec| {
                (if by_name { 0 } else { rec.priority }, rec.name.clone())
            });
            items
                .into_iter()
                .map(|rec| rec.rec_id.clone())
                .collect::<Vec<_>>()
        })
    });
    let platforms = Memo::new(move |_| {
        let mut keys: Vec<String> = state
            .recording_list
            .with(|items| items.iter().map(|rec| rec.platform_key.clone()).collect());
        keys.sort();
        keys.dedup();
        keys
    });
    let all_ids = Memo::new(move |_| {
        state.recording_list.with(|items| {
            items
                .iter()
                .map(|rec| rec.rec_id.clone())
                .collect::<std::collections::HashSet<_>>()
        })
    });
    Effect::new(move |_| {
        all_ids.with(|items| {
            if selected.with_untracked(|ids| ids.iter().any(|id| !items.contains(id))) {
                selected.update(|ids| ids.retain(|id| items.contains(id)));
            }
        });
    });
    let all_visible_selected = move || {
        let items = visible.get();
        !items.is_empty() && selected.with(|ids| items.iter().all(|id| ids.contains(id)))
    };
    let visible_selected = Memo::new(move |_| {
        visible
            .with(|items| selected.with(|ids| items.iter().filter(|id| ids.contains(id)).count()))
    });
    let partially_selected =
        move || visible_selected.get() > 0 && visible_selected.get() < visible.with(Vec::len);
    let filtered = move || {
        !keyword.get().is_empty() || !platform.get().is_empty() || filter.get() != Filter::All
    };
    let clear_filters = Callback::new(move |_| {
        if selection_locked.get_untracked() {
            return;
        }
        keyword.set(String::new());
        platform.set(String::new());
        filter.set(Filter::All);
    });
    let select_visible = move |_| {
        if selection_locked.get_untracked() {
            return;
        }
        let ids = visible.get_untracked();
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
            state.recording_list.with(|items| {
                items
                    .iter()
                    .any(|r| ids.contains(&r.rec_id) && r.phase == TaskPhase::Recording)
            })
        })
    };
    let refresh = Callback::new(move |_| {
        if selection_locked.get_untracked() {
            return;
        }
        batch_status.set(t("正在同步任务列表…"));
        batch_progress.set(None);
        busy.set(true);
        leptos::task::spawn_local(async move {
            match gateway::refresh_recordings(state).await {
                Ok(()) => state.notify(t("任务列表已刷新")),
                Err(message) => state.fail(message),
            }
            let _ = busy.try_set(false);
        });
    });
    let monitor_selected = Callback::new(move |enabled: bool| {
        if selection_locked.get_untracked() || !state.status.get_untracked().ok {
            return;
        }
        let ids = selected.get_untracked();
        if ids.is_empty() {
            return;
        }
        batch_status.set(if enabled {
            t("正在开启所选任务的监控")
        } else {
            t("正在暂停所选任务的监控")
        });
        batch_progress.set(Some((0, ids.len())));
        busy.set(true);
        leptos::task::spawn_local(async move {
            match gateway::fetch_recordings().await {
                Ok(items) => {
                    let mut failures = Vec::new();
                    for (index, id) in ids.iter().enumerate() {
                        match items.iter().find(|r| &r.rec_id == id) {
                            Some(rec) if rec.monitor_status != enabled => {
                                if let Err(error) = gateway::toggle_monitor(id).await {
                                    failures.push(error);
                                }
                            }
                            None => failures.push(t("部分任务已不存在").into()),
                            _ => {}
                        }
                        let _ = batch_progress.try_set(Some((index + 1, ids.len())));
                    }
                    if failures.is_empty() {
                        state.notify(crate::tr_format!(
                            "已{}所选 {} 个任务的监控",
                            if enabled { t("开启") } else { t("暂停") },
                            ids.len()
                        ));
                    } else {
                        state.fail(crate::tr_format!(
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
            let _ = batch_progress.try_set(None);
            let _ = busy.try_set(false);
        });
    });
    let delete = Callback::new(move |_| {
        if selection_locked.get_untracked()
            || !state.status.get_untracked().ok
            || delete_ids.with_untracked(|ids| {
                ids.is_empty()
                    || state.recording_list.with_untracked(|items| {
                        items.iter().any(|record| {
                            ids.contains(&record.rec_id) && record.phase == TaskPhase::Recording
                        })
                    })
            })
        {
            return;
        }
        batch_status.set(t("正在移除所选任务…"));
        batch_progress.set(None);
        busy.set(true);
        let ids = delete_ids.get_untracked();
        leptos::task::spawn_local(async move {
            match gateway::delete_recordings(&ids).await {
                Ok(()) => {
                    let _ = delete_open.try_set(false);
                    let _ = selected.try_update(|values| values.retain(|id| !ids.contains(id)));
                    state.notify(t("所选任务已移除，录制文件保留"));
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
        if selection_locked.get_untracked() || !state.status.get_untracked().ok {
            return;
        }
        gateway::save_grid_view(state, grid);
    });
    view! {
        <div class="page recordings-page">
            <PageHeader title=t("录制任务") subtitle=t("管理直播间，跟踪录制与监控状态。")>
                <IconButton label=t("刷新列表") icon="refresh" disabled=selection_locked on_click=refresh />
                <ActionButton label=t("添加直播间") icon="plus" variant="primary"
                    disabled=Signal::derive(move || selection_locked.get() || !state.status.get().ok)
                    on_click=Callback::new(move |_| add.set(true)) />
            </PageHeader>
            <section class="task-workspace" aria-label=t("录制任务")>
            <div class="task-toolbar" aria-label=t("任务筛选")>
                <div class="filter-tabs task-status-filters" role="group" aria-label=t("状态筛选")>
                    {[(Filter::All,t("全部")),(Filter::Attention,t("需关注")),(Filter::Recording,t("录制中")),(Filter::Live,t("直播未录制")),(Filter::Monitoring,t("等待开播")),(Filter::Paused,t("已暂停"))].into_iter().map(|(mode,label)| view! {
                        <ActionButton label=label class="filter-tab" variant="ghost"
                            pressed=Signal::derive(move || Some(filter.get() == mode)) disabled=selection_locked
                            on_click=Callback::new(move |_| filter.set(mode))>
                            <span>{label}</span><span class="ui-badge ui-badge-sm filter-count">{move || state.recording_list.with(|items| items.iter().filter(|r| mode.matches(r.phase)).count())}</span>
                        </ActionButton>
                    }).collect_view()}
                </div>
                <div class="toolbar-controls">
                    <div class="search-field task-search"><Icon name="search" size=18 /><input class="ui-input ui-input-sm" aria-label=t("搜索直播间") placeholder=t("搜索主播、标题或地址…") prop:value=move || keyword.get() disabled=move || selection_locked.get() on:input=move |e| keyword.set(event_target_value(&e)) />
                        <Show when=move || !keyword.get().is_empty()><IconButton label=t("清除搜索") icon="close" disabled=selection_locked on_click=Callback::new(move |_| keyword.set(String::new())) /></Show>
                    </div>
                    <label class="toolbar-field"><span>{t("平台")}</span><select class="ui-select ui-select-sm input platform-filter" aria-label=t("筛选平台") prop:value=move || platform.get() disabled=move || selection_locked.get() on:change=move |e| platform.set(event_target_value(&e))><option value="">{t("全部平台")}</option>{move || platforms.get().into_iter().map(|key| { let label = platform_label(&key); view! { <option value=key>{label}</option> } }).collect_view()}</select></label>
                    <label class="toolbar-field"><span>{t("排序")}</span><select class="ui-select ui-select-sm input sort-select" aria-label=t("任务排序") prop:value=move || sorting.get() disabled=move || selection_locked.get() on:change=move |e| sorting.set(event_target_value(&e))><option value="status">{t("状态优先")}</option><option value="name">{t("名称排序")}</option></select></label>
                    <div class="view-switch" role="group" aria-label=t("视图模式") aria-busy=move || state.grid_view_saving.get().to_string()>
                        <IconButton label=t("网格视图") icon="grid" pressed=Signal::derive(move || Some(state.grid_view.get()))
                            disabled=Signal::derive(move || selection_locked.get() || !state.status.get().ok)
                            on_click=Callback::new(move |_| set_layout.run(true)) />
                        <IconButton label=t("列表视图") icon="list" pressed=Signal::derive(move || Some(!state.grid_view.get()))
                            disabled=Signal::derive(move || selection_locked.get() || !state.status.get().ok)
                            on_click=Callback::new(move |_| set_layout.run(false)) />
                    </div>
                </div>
            </div>
            <div class="results-line task-selection-toolbar"><label class="select-all"><input class="ui-checkbox ui-checkbox-sm" type="checkbox" prop:checked=all_visible_selected prop:indeterminate=partially_selected disabled=move || visible.get().is_empty() || selection_locked.get() on:change=select_visible /><span>{t("全选当前结果")}</span></label>
                <Show when=filtered><ActionButton label=t("清除筛选") class="small" variant="ghost" disabled=selection_locked on_click=clear_filters /></Show>
                <Show when=move || { processing.get() > 0 }><a class="text-link processing-link" href="/storage"><Icon name="file" size=14 />{move || crate::tr_format!("{} 项录后处理", processing.get())}</a></Show>
                <span>{move || crate::tr_format!("显示 {} 个直播间", visible.get().len())}</span></div>
            <Show when=move || !selected.get().is_empty()>
                <div class="batch-bar" role="region" aria-label=t("所选任务操作") aria-busy=move || busy.get().to_string()>
                    <div class="selection-summary"><strong>{move || crate::tr_format!("已选 {} 项", selected.get().len())}</strong><small>{move || crate::tr_format!("当前可见 {} 项 · 隐藏 {} 项", visible_selected.get(), selected.with(Vec::len).saturating_sub(visible_selected.get()))}</small></div>
                    <ActionButton label=t("批量编辑") icon="edit" class="small" variant="ghost" title=t("录制中的任务须先停止")
                        disabled=Signal::derive(move || selection_locked.get() || has_active_selection() || !state.status.get().ok)
                        on_click=Callback::new(move |_| {
                            if selection_locked.get_untracked() || has_active_selection() || !state.status.get_untracked().ok { return; }
                            batch_status.set(t("正在保存所选任务…")); batch_progress.set(None);
                            batch_ids.set(selected.get_untracked()); batch_open.set(true);
                        }) />
                    <ActionButton label=t("开启监控") class="small" variant="ghost" disabled=Signal::derive(move || selection_locked.get() || !state.status.get().ok)
                        on_click=Callback::new(move |_| monitor_selected.run(true)) />
                    <ActionButton label=t("暂停监控") class="small" variant="ghost" disabled=Signal::derive(move || selection_locked.get() || !state.status.get().ok)
                        on_click=Callback::new(move |_| monitor_selected.run(false)) />
                    <ActionButton label=t("移除所选") icon="trash" class="small" variant="ghost-danger"
                        disabled=Signal::derive(move || selection_locked.get() || has_active_selection() || !state.status.get().ok)
                        on_click=Callback::new(move |_| {
                            if selection_locked.get_untracked() || has_active_selection() || !state.status.get_untracked().ok { return; }
                            delete_ids.set(selected.get_untracked()); delete_open.set(true);
                        }) />
                    <IconButton label=t("取消选择") icon="close" disabled=selection_locked on_click=Callback::new(move |_| selected.set(Vec::new())) />
                    <Show when=move || { selected.with(Vec::len) > visible_selected.get() }><p class="selection-hint">{t("批量操作也会应用到当前筛选隐藏的已选任务。")}</p></Show>
                    <Show when=move || { active_card_operations.get() > 0 }><p class="selection-hint" role="status">{t("单项操作进行中，完成后可继续批量操作。")}</p></Show>
                    <Show when=move || busy.get()><div class="batch-progress" role="status" aria-live="polite"><span class="control-spinner" /><span>{move || batch_status.get()}</span>
                        <Show when=move || batch_progress.get().is_some()><span>{move || { let (done,total) = batch_progress.get().unwrap_or_default(); crate::tr_format!("已处理 {} / {} 项",done,total) }}</span><progress aria-label=t("批量操作进度") max=move || batch_progress.get().map(|(_,total)|total).unwrap_or(1) value=move || batch_progress.get().map(|(done,_)|done).unwrap_or_default() /></Show>
                    </div></Show>
                </div>
            </Show>
            <div class="task-collection" class:task-collection-list=move || !state.grid_view.get()>
            <Show when=move || !state.grid_view.get() && !visible.get().is_empty()>
                <div class="task-list-header" aria-hidden="true">
                    <span class="task-selection-gutter" /><span class="task-identity-column">{t("直播间")}</span><span class="task-status-column">{t("状态")}</span><span class="task-actions-column">{t("操作")}</span>
                </div>
            </Show>
            <Show when=move || state.loading.get()><div class="skeleton-grid" aria-label=t("加载任务")><div class="skeleton" /><div class="skeleton" /><div class="skeleton" /></div></Show>
            <Show when=move || !state.loading.get() && visible.get().is_empty()><div class="empty-panel">
                <Show when=move || !state.recording_list.with(Vec::is_empty) fallback=move || view! {
                    <EmptyState icon="video" title=t("还没有直播间") description=t("添加直播间后，可在这里管理监控与录制。") />
                    <ActionButton label=t("添加直播间") icon="plus" variant="primary" disabled=Signal::derive(move || selection_locked.get() || !state.status.get().ok) on_click=Callback::new(move |_| add.set(true)) />
                }>
                    <EmptyState icon="search" title=t("没有匹配的直播间") description=t("尝试调整搜索条件，或添加新的直播间。") />
                    <ActionButton label=t("清除筛选") disabled=selection_locked on_click=clear_filters />
                </Show>
            </div></Show>
            <div class="cards-grid" class:list-view=move || !state.grid_view.get()>
                <For each=move || visible.get() key=|id| id.clone() children=move |id| state.recording_untracked(&id).map(|rec| view! { <RecordingCard recording=rec selectable=true selected=selected locked=busy selection_locked=selection_locked on_busy_change=card_busy_changed on_edit=Callback::new(move |r| editing.set(Some(r))) on_info=Callback::new(move |r| info.set(Some(r))) on_preview=Callback::new(move |r| preview.open_recording(r)) on_mini_preview=Callback::new(move |r| preview.open_recording_mini(r)) /> }) />
            </div>
            </div>
            </section>
        </div>
        <AddRecordingDialog open=add /><EditRecordingDialog target=editing /><BatchEditDialog open=batch_open ids=batch_ids busy=busy locked=Signal::derive(move || active_card_operations.get() > 0) /><CardInfoDialog target=info />
        <ConfirmDialog open=delete_open title=t("移除所选直播间") description=Signal::derive(move || crate::tr_format!("将移除选中的 {} 个直播间。其他任务及已有录制文件保持不变。", delete_ids.get().len())) busy=busy disabled=Signal::derive(move || active_card_operations.get() > 0 || has_active_selection() || !state.status.get().ok) on_confirm=delete />
    }
}
