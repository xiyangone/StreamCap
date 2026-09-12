use super::Icon;
use crate::api::gateway::{self, NoticeKind};
use leptos::prelude::*;

#[component]
pub fn Toast() -> impl IntoView {
    let state = gateway::app_state();
    let timer = StoredValue::new_local(None::<gloo_timers::callback::Timeout>);
    Effect::new(move |_| {
        let notice = state.notice.get();
        timer.update_value(|slot| {
            *slot = notice.map(|n| {
                gloo_timers::callback::Timeout::new(
                    if n.kind == NoticeKind::Error {
                        9000
                    } else {
                        4500
                    },
                    move || state.notice.set(None),
                )
            });
        });
    });
    view! {
        <div class="toast-region" aria-live="polite" aria-atomic="true">
            <Show when=move || state.notice.get().is_some()>
                <div class="toast glass" class:error=move || state.notice.get().is_some_and(|n| n.kind == NoticeKind::Error) role="status">
                    <Show when=move || state.notice.get().is_some_and(|n| n.kind == NoticeKind::Error) fallback=|| view! { <Icon name="check" /> }><Icon name="alert" /></Show>
                    <span>{move || state.notice.get().map(|n| n.message).unwrap_or_default()}</span>
                    <button class="icon-button" aria-label="关闭提示" on:click=move |_| state.notice.set(None)><Icon name="close" size=16 /></button>
                </div>
            </Show>
        </div>
    }
}
