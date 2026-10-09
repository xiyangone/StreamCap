use super::Icon;
use crate::app::i18n::t;
use leptos::prelude::*;

fn action_variant(variant: &str) -> &'static str {
    match variant {
        "primary" => "ui-btn-primary",
        "danger" => "ui-btn-error ui-btn-outline",
        "ghost-danger" => "ui-btn-error ui-btn-ghost",
        "ghost" | "subtle" => "ui-btn-ghost",
        _ => "",
    }
}

/// All imperative actions share the same states; configuration uses ToggleSwitch.
#[component]
pub fn ActionButton(
    #[prop(into)] label: Signal<&'static str>,
    on_click: Callback<()>,
    #[prop(default = Signal::stored(""), into)] icon: Signal<&'static str>,
    #[prop(default = Signal::stored("secondary"), into)] variant: Signal<&'static str>,
    #[prop(default = "")] class: &'static str,
    #[prop(default = Signal::stored(""), into)] title: Signal<&'static str>,
    #[prop(default = "button")] button_type: &'static str,
    #[prop(default = "")] busy_label: &'static str,
    #[prop(default = Signal::stored(false), into)] busy: Signal<bool>,
    #[prop(default = Signal::stored(false), into)] disabled: Signal<bool>,
    #[prop(default = Signal::stored(None), into)] pressed: Signal<Option<bool>>,
    #[prop(default = Signal::stored(None), into)] current: Signal<Option<&'static str>>,
    #[prop(default = Signal::stored(String::new()), into)] aria_label: Signal<String>,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    let custom_content = children.is_some();
    view! {
        <button
            class=move || format!("ui-btn ui-btn-sm button {} {}", action_variant(variant.get()), class)
            data-ui-component="button"
            type=button_type title=move || title.get()
            disabled=move || disabled.get() || busy.get()
            aria-busy=move || busy.get().to_string()
            aria-pressed=move || pressed.get().map(|value| value.to_string())
            aria-current=move || current.get()
            aria-label=move || {
                let text = aria_label.get();
                if text.is_empty() { label.get().to_string() } else { text }
            }
            on:click=move |_| {
                if !busy.get_untracked() && !disabled.get_untracked() {
                    on_click.run(());
                }
            }
        >
            <Show when=move || busy.get() fallback=move || view! {
                {move || (!icon.get().is_empty()).then(|| view! { <Icon name=icon.get() size=17 /> })}
            }>
                <span class="ui-loading ui-loading-spinner ui-loading-xs control-spinner" aria-hidden="true" />
            </Show>
            {children.map(|children| view! {
                <span class="button-content" hidden=move || busy.get() && !busy_label.is_empty()>{children()}</span>
            })}
            <Show when=move || !custom_content || (busy.get() && !busy_label.is_empty())>
                <span>{move || if busy.get() && !busy_label.is_empty() { busy_label } else { label.get() }}</span>
            </Show>
        </button>
    }
    .into_any()
}

#[component]
pub fn IconButton(
    label: &'static str,
    icon: &'static str,
    on_click: Callback<()>,
    #[prop(default = "")] class: &'static str,
    #[prop(default = false)] danger: bool,
    #[prop(default = Signal::stored(false), into)] busy: Signal<bool>,
    #[prop(default = Signal::stored(false), into)] disabled: Signal<bool>,
    #[prop(default = Signal::stored(None), into)] pressed: Signal<Option<bool>>,
    #[prop(default = Signal::stored(String::new()), into)] aria_label: Signal<String>,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    let custom_content = children.is_some();
    view! {
        <button
            class=format!("ui-btn ui-btn-ghost ui-btn-sm ui-btn-square icon-button {} {class}", if danger { "ui-btn-error danger-text" } else { "" })
            data-ui-component="icon-button"
            type="button" title=label
            aria-label=move || {
                let text = aria_label.get();
                if text.is_empty() { label.to_string() } else { text }
            }
            aria-pressed=move || pressed.get().map(|value| value.to_string())
            aria-busy=move || busy.get().to_string()
            disabled=move || disabled.get() || busy.get()
            on:click=move |_| {
                if !busy.get_untracked() && !disabled.get_untracked() {
                    on_click.run(());
                }
            }
        >
            {children.map(|children| view! {
                <span class="icon-button-content" hidden=move || busy.get()>{children()}</span>
            })}
            <Show when=move || !custom_content && !busy.get()><Icon name=icon size=18 /></Show>
            <Show when=move || busy.get()>
                <span class="ui-loading ui-loading-spinner ui-loading-xs control-spinner" aria-hidden="true" />
            </Show>
        </button>
    }
    .into_any()
}

/// A binary preference, never an alternate presentation of a one-shot action.
#[component]
pub fn ToggleSwitch(
    label: &'static str,
    #[prop(into)] checked: Signal<bool>,
    on_change: Callback<bool>,
    #[prop(default = "")] hint: &'static str,
    #[prop(default = false)] compact: bool,
    #[prop(default = Signal::stored(false), into)] busy: Signal<bool>,
    #[prop(default = Signal::stored(false), into)] disabled: Signal<bool>,
) -> impl IntoView {
    view! {
        <label class="toggle-field" class:compact=compact>
            <span class="toggle-copy"><strong>{label}</strong>
                {(!compact && !hint.is_empty()).then(|| view! { <small>{hint}</small> })}
            </span>
            <input
                class="ui-toggle ui-toggle-primary ui-toggle-sm toggle-switch" type="checkbox" role="switch"
                data-ui-component="toggle"
                aria-label=label title=hint aria-checked=move || checked.get().to_string()
                prop:checked=move || checked.get()
                aria-busy=move || busy.get().to_string()
                disabled=move || disabled.get() || busy.get()
                on:change=move |event| {
                    if !busy.get_untracked() && !disabled.get_untracked() {
                        on_change.run(event_target_checked(&event));
                    }
                }
            />
        </label>
    }
    .into_any()
}

#[component]
pub fn FormFeedback(
    #[prop(into)] error: Signal<Option<String>>,
    #[prop(default = t("请检查填写内容"))] title: &'static str,
) -> impl IntoView {
    view! {
        <Show when=move || error.get().is_some()>
            <div class="ui-alert ui-alert-error ui-alert-soft form-feedback" role="alert" tabindex="-1">
                <Icon name="alert" size=18 />
                <div><strong>{title}</strong><p>{move || error.get().unwrap_or_default()}</p></div>
            </div>
        </Show>
    }
    .into_any()
}

/// Shared page hierarchy. Actions stay separate from the title at narrow widths.
#[component]
pub fn PageHeader(
    title: &'static str,
    subtitle: &'static str,
    #[prop(optional)] children: Option<Children>,
) -> impl IntoView {
    view! {
        <header class="page-header">
            <div class="page-heading"><h1>{title}</h1><p>{subtitle}</p></div>
            {children.map(|children| view! { <div class="header-actions">{children()}</div> })}
        </header>
    }
}

#[component]
pub fn FieldSection(
    title: &'static str,
    #[prop(default = "")] description: &'static str,
    children: Children,
) -> impl IntoView {
    view! {
        <fieldset class="ui-fieldset field-section">
            <legend class="ui-fieldset-legend">{title}</legend>
            {(!description.is_empty()).then(|| view! { <p class="field-section-description">{description}</p> })}
            {children()}
        </fieldset>
    }
}

#[component]
pub fn InlineNotice(
    #[prop(default = "info")] icon: &'static str,
    #[prop(default = "neutral")] tone: &'static str,
    children: Children,
) -> impl IntoView {
    view! {
        <div class="ui-alert ui-alert-soft inline-notice" data-tone=tone>
            <Icon name=icon size=17 /><div>{children()}</div>
        </div>
    }
}
