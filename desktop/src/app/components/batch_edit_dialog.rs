use super::{
    edit_dialog::{RecordingDraft, RecordingFields},
    ActionButton, Dialog, FormFeedback, InlineNotice,
};
use crate::api::gateway;
use crate::app::i18n::t;
use leptos::prelude::*;

#[component]
pub fn BatchEditDialog(
    open: RwSignal<bool>,
    ids: RwSignal<Vec<String>>,
    #[prop(default = RwSignal::new(false))] busy: RwSignal<bool>,
    #[prop(default = Signal::stored(false), into)] locked: Signal<bool>,
) -> impl IntoView {
    let state = gateway::app_state();
    let draft = RecordingDraft::new(true);
    let error = RwSignal::new(None::<String>);
    let unavailable = Signal::derive(move || {
        locked.get()
            || !state.status.get().ok
            || ids.with(|ids| {
                state.recording_list.with(|items| {
                    items.iter().any(|record| {
                        ids.contains(&record.rec_id)
                            && record.phase == gateway::TaskPhase::Recording
                    })
                })
            })
    });
    Effect::new(move |_| {
        if open.get() {
            draft.reset();
            error.set(None);
        }
    });
    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() || unavailable.get_untracked() {
            return;
        }
        let selected = ids.get_untracked();
        if selected.is_empty() {
            error.set(Some(t("请先选择任务").into()));
            return;
        }
        let (changes, follow) = match draft.collect() {
            Ok(data) => data,
            Err(message) => {
                error.set(Some(message));
                return;
            }
        };
        if changes.is_empty() && follow.is_empty() {
            error.set(Some(t("请至少选择一项需要修改的设置").into()));
            return;
        }
        let count = selected.len();
        busy.set(true);
        error.set(None);
        leptos::task::spawn_local(async move {
            match gateway::batch_edit(selected, changes, follow).await {
                Ok(()) => {
                    let _ = open.try_set(false);
                    state.notify(crate::tr_format!("已更新选中的 {count} 个任务"));
                    if let Err(message) = gateway::refresh_recordings(state).await {
                        state.fail(message);
                    }
                }
                Err(message) => {
                    let _ = error.try_set(Some(message));
                }
            }
            let _ = busy.try_set(false);
        });
    };
    view! {
        <Dialog open=Signal::derive(move || open.get()) title=t("批量编辑任务") busy=busy on_close=Callback::new(move |_| open.set(false))>
            <InlineNotice icon="shield">{move || crate::tr_format!("仅修改已选择的 {} 个任务，其他任务不受影响。", ids.get().len())}</InlineNotice>
            <p class="field-hint">{t("保持原值不会修改该项；跟随全局会恢复使用全局设置。")}</p>
            <Show when=move || unavailable.get()><p class="field-error" role="status">{t("请等待单项操作完成，停止所选录制，并确认本地服务已连接。")}</p></Show>
            <form on:submit=submit><fieldset disabled=move || busy.get() || unavailable.get()><RecordingFields draft=draft batch=true /></fieldset>
                <FormFeedback error=error />
                <footer class="modal-actions">
                    <ActionButton label=t("取消") disabled=busy on_click=Callback::new(move |_| open.set(false)) />
                    <ActionButton label=t("应用到所选任务") icon="check" variant="primary" button_type="submit"
                        busy=busy busy_label=t("保存中…") disabled=unavailable on_click=Callback::new(|_| {}) />
                </footer>
            </form>
        </Dialog>
    }
}
