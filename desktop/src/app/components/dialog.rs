use super::Icon;
use crate::app::i18n::t;
use leptos::{html, prelude::*};
use wasm_bindgen::JsCast;

/// Native dialogs provide focus containment, Escape handling and focus restoration.
#[component]
pub fn Dialog(
    #[prop(into)] open: Signal<bool>,
    title: &'static str,
    on_close: Callback<()>,
    #[prop(optional)] wide: bool,
    #[prop(optional)] busy: Option<RwSignal<bool>>,
    children: ChildrenFn,
) -> impl IntoView {
    let element = NodeRef::<html::Dialog>::new();
    let close = move || {
        if !busy.is_some_and(|b| b.get_untracked()) {
            on_close.run(());
        }
    };
    Effect::new(move |_| {
        let is_open = open.get();
        if let Some(dialog) = element.get() {
            if is_open && !dialog.open() {
                if let Err(error) = dialog.show_modal() {
                    log::error!("无法打开对话框: {error:?}");
                }
            } else if !is_open && dialog.open() {
                dialog.close();
            }
        }
    });
    view! {
        <dialog node_ref=element class=if wide { "modal modal-wide glass" } else { "modal glass" }
            aria-label=title aria-modal="true"
            on:cancel=move |event: leptos::ev::Event| { event.prevent_default(); close(); }
            on:click=move |event: leptos::ev::MouseEvent| {
                if let Some(dialog) = element.get() {
                    if event.target().as_ref() != Some(dialog.unchecked_ref::<web_sys::EventTarget>()) {
                        return;
                    }
                    let rect = dialog.get_bounding_client_rect();
                    let x = f64::from(event.client_x()); let y = f64::from(event.client_y());
                    if x < rect.left() || x > rect.right() || y < rect.top() || y > rect.bottom() { close(); }
                }
            }>
            <header class="modal-header"><div><span class="eyebrow">"STREAMCAP"</span><h2>{title}</h2></div>
                <button class="icon-button" type="button" aria-label=t("关闭对话框") title=t("关闭") disabled=move || busy.is_some_and(|b| b.get()) on:click=move |_| close()><Icon name="close" /></button>
            </header>
            <div class="modal-body">{children()}</div>
        </dialog>
    }
}

#[component]
pub fn ConfirmDialog(
    open: RwSignal<bool>,
    title: &'static str,
    #[prop(into)] description: Signal<String>,
    busy: RwSignal<bool>,
    on_confirm: Callback<()>,
    #[prop(default = "trash")] icon: &'static str,
    #[prop(default = true)] destructive: bool,
    #[prop(default = t("确认删除"))] confirm_text: &'static str,
    #[prop(default = t("正在删除…"))] busy_text: &'static str,
    #[prop(default = t("请确认操作对象和影响范围。"))] hint: &'static str,
) -> impl IntoView {
    view! {
        <Dialog open=Signal::derive(move || open.get()) title=title busy=busy on_close=Callback::new(move |_| open.set(false))>
            <div class=if destructive {"confirm-symbol"} else {"confirm-symbol confirm-neutral"}><Icon name=icon size=26 /></div>
            <p class="dialog-description">{move || description.get()}</p>
            <p class="field-hint">{hint}</p>
            <footer class="modal-actions"><button class="button secondary" disabled=move || busy.get() on:click=move |_| open.set(false)>{t("取消")}</button>
                <button class=if destructive {"button danger"} else {"button primary"} disabled=move || busy.get() on:click=move |_| on_confirm.run(())>{move || if busy.get() { busy_text } else { confirm_text }}</button>
            </footer>
        </Dialog>
    }
}

#[component]
pub fn EmptyState(
    icon: &'static str,
    title: &'static str,
    description: &'static str,
) -> impl IntoView {
    view! { <div class="empty-state"><span class="empty-icon"><Icon name=icon size=30 /></span><h3>{title}</h3><p>{description}</p></div> }
}
