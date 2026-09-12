use super::Icon;
use leptos::{html, prelude::*};

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
                    let rect = dialog.get_bounding_client_rect();
                    let x = f64::from(event.client_x()); let y = f64::from(event.client_y());
                    if x < rect.left() || x > rect.right() || y < rect.top() || y > rect.bottom() { close(); }
                }
            }>
            <header class="modal-header"><div><span class="eyebrow">"STREAMCAP"</span><h2>{title}</h2></div>
                <button class="icon-button" type="button" aria-label="关闭对话框" title="关闭" disabled=move || busy.is_some_and(|b| b.get()) on:click=move |_| close()><Icon name="close" /></button>
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
) -> impl IntoView {
    view! {
        <Dialog open=Signal::derive(move || open.get()) title=title busy=busy on_close=Callback::new(move |_| open.set(false))>
            <div class="confirm-symbol"><Icon name="trash" size=26 /></div>
            <p class="dialog-description">{move || description.get()}</p>
            <p class="field-hint">"此操作无法撤销，请确认选择范围。"</p>
            <footer class="modal-actions"><button class="button secondary" disabled=move || busy.get() on:click=move |_| open.set(false)>"取消"</button>
                <button class="button danger" disabled=move || busy.get() on:click=move |_| on_confirm.run(())>{move || if busy.get() { "正在删除…" } else { "确认删除" }}</button>
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
