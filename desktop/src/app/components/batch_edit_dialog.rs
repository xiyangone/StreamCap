use super::{
    edit_dialog::{RecordingDraft, RecordingFields},
    Dialog, Icon,
};
use crate::api::gateway;
use leptos::prelude::*;

#[component]
pub fn BatchEditDialog(open: RwSignal<bool>, ids: RwSignal<Vec<String>>) -> impl IntoView {
    let state = gateway::app_state();
    let draft = RecordingDraft::new(true);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    Effect::new(move |_| {
        if open.get() {
            draft.reset();
            error.set(None);
        }
    });
    let submit = move |event: leptos::ev::SubmitEvent| {
        event.prevent_default();
        if busy.get_untracked() {
            return;
        }
        let selected = ids.get_untracked();
        if selected.is_empty() {
            error.set(Some("请先选择任务".into()));
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
            error.set(Some("请至少选择一项需要修改的设置".into()));
            return;
        }
        let count = selected.len();
        busy.set(true);
        error.set(None);
        leptos::task::spawn_local(async move {
            match gateway::batch_edit(selected, changes, follow).await {
                Ok(()) => {
                    let _ = open.try_set(false);
                    state.notify(format!("已更新选中的 {count} 个任务"));
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
        <Dialog open=Signal::derive(move || open.get()) title="批量编辑任务" busy=busy on_close=Callback::new(move |_| open.set(false))>
            <div class="scope-notice"><Icon name="shield" /><p>{move || format!("仅修改已选择的 {} 个任务，其他任务不受影响。", ids.get().len())}</p></div>
            <form on:submit=submit><fieldset disabled=move || busy.get()><RecordingFields draft=draft batch=true /></fieldset>
                <Show when=move || error.get().is_some()><p class="field-error" role="alert">{move || error.get().unwrap_or_default()}</p></Show>
                <footer class="modal-actions"><button class="button secondary" type="button" disabled=move || busy.get() on:click=move |_| open.set(false)>"取消"</button><button class="button primary" type="submit" disabled=move || busy.get()>{move || if busy.get() { "保存中…" } else { "应用到所选任务" }}</button></footer>
            </form>
        </Dialog>
    }
}
