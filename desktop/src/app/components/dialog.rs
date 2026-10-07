use super::Icon;
use crate::app::i18n::t;
use leptos::{html, prelude::*};
use wasm_bindgen::JsCast;

/// Native dialogs provide focus containment, Escape handling and focus restoration.
#[component]
pub fn Dialog(
    #[prop(into)] open: Signal<bool>,
    #[prop(into)] title: Signal<&'static str>,
    on_close: Callback<()>,
    #[prop(optional)] wide: bool,
    #[prop(optional)] busy: Option<RwSignal<bool>>,
    #[prop(optional)] minimized: Option<RwSignal<bool>>,
    children: ChildrenFn,
) -> impl IntoView {
    let element = NodeRef::<html::Dialog>::new();
    let is_minimized = move || minimized.is_some_and(|value| value.get());
    let shown_minimized = StoredValue::new(false);
    let close = move || {
        if !busy.is_some_and(|b| b.get_untracked()) {
            on_close.run(());
        }
    };
    Effect::new(move |_| {
        let is_open = open.get();
        let mini = is_minimized();
        if let Some(dialog) = element.get() {
            if dialog.open() && mini != shown_minimized.get_value() {
                // Change modality on the same DOM node; media remains mounted.
                dialog.close();
            }
            if is_open && !dialog.open() {
                let result = if mini {
                    dialog.show();
                    Ok(())
                } else {
                    dialog.show_modal()
                };
                if let Err(error) = result {
                    log::error!("无法打开对话框: {error:?}");
                }
                if mini {
                    if let Some(main) = document()
                        .get_element_by_id("main-content")
                        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
                    {
                        let _ = main.focus();
                    }
                }
            } else if !is_open && dialog.open() {
                dialog.close();
            }
            shown_minimized.set_value(mini);
        }
    });
    view! {
        <dialog node_ref=element class=if wide { "modal modal-wide" } else { "modal" }
            class:preview-shell=minimized.is_some() class:modal-minimized=is_minimized
            aria-label=move || title.get() aria-modal=move || (!is_minimized()).to_string()
            on:keydown=move |event: leptos::ev::KeyboardEvent| {
                if is_minimized() && event.key() == "Escape" {
                    event.prevent_default(); event.stop_propagation(); close();
                }
            }
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
            <header class="modal-header"><div class="modal-title"><h2>{move || title.get()}</h2></div>
                <div class="modal-header-actions">
                {minimized.map(|value| view! {
                    <button class="icon-button" type="button" aria-label=move || if value.get() { t("恢复预览") } else { t("最小化预览") } title=move || if value.get() { t("恢复预览") } else { t("最小化预览") } on:click=move |_| value.update(|mini| *mini = !*mini)>
                        <Show when=move || value.get() fallback=|| view! { <Icon name="minimize" /> }><Icon name="restore" /></Show>
                    </button>
                })}
                <button class="icon-button" type="button" aria-label=t("关闭对话框") title=t("关闭") disabled=move || busy.is_some_and(|b| b.get()) on:click=move |_| close()><Icon name="close" /></button>
                </div>
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
            <div class="confirm-layout">
                <div class=if destructive {"confirm-symbol"} else {"confirm-symbol confirm-neutral"}><Icon name=icon size=24 /></div>
                <div><p class="dialog-description">{move || description.get()}</p><p class="field-hint">{hint}</p></div>
            </div>
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
    view! { <div class="empty-state"><span class="empty-icon"><Icon name=icon size=24 /></span><div><h3>{title}</h3><p>{description}</p></div></div> }
}
