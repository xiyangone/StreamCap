use super::{Icon, IconButton};
use crate::api::gateway::{self, NoticeKind};
use crate::app::i18n::t;
use leptos::prelude::*;

#[component]
pub fn Toast() -> impl IntoView {
    let state = gateway::app_state();
    let timer = StoredValue::new_local(None::<gloo_timers::callback::Timeout>);
    let hovered = RwSignal::new(false);
    let focused = RwSignal::new(false);
    Effect::new(move |_| {
        let notice = state.notice.get();
        let paused = hovered.get() || focused.get();
        timer.update_value(|slot| {
            *slot = notice.filter(|_| !paused).map(|n| {
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
    on_cleanup(move || {
        timer.update_value(|slot| {
            slot.take();
        })
    });
    view! {
        <div class="toast-region" aria-live="polite" aria-atomic="true">
            <Show when=move || state.notice.get().is_some()>
                <div class="ui-alert toast" class:error=move || state.notice.get().is_some_and(|n| n.kind == NoticeKind::Error) role="status"
                    on:mouseenter=move |_| hovered.set(true) on:mouseleave=move |_| hovered.set(false)
                    on:focusin=move |_| focused.set(true) on:focusout=move |_| focused.set(false)>
                    <Show when=move || state.notice.get().is_some_and(|n| n.kind == NoticeKind::Error) fallback=|| view! { <Icon name="check" /> }><Icon name="alert" /></Show>
                    <div class="toast-content"><strong>{move || if state.notice.get().is_some_and(|n| n.kind == NoticeKind::Error) { t("操作未完成") } else { t("操作完成") }}</strong><span>{move || state.notice.get().map(|n| n.message).unwrap_or_default()}</span></div>
                    <IconButton label=t("关闭提示") icon="close" on_click=Callback::new(move |_| {
                        state.notice.set(None);
                        hovered.set(false);
                        focused.set(false);
                    }) />
                </div>
            </Show>
        </div>
    }
}
