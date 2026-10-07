use super::Icon;
use leptos::prelude::*;

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
        <fieldset class="field-section">
            <legend>{title}</legend>
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
        <div class="inline-notice" data-tone=tone>
            <Icon name=icon size=17 /><div>{children()}</div>
        </div>
    }
}

#[component]
pub fn Metric(
    label: &'static str,
    icon: &'static str,
    #[prop(into)] value: Signal<String>,
    #[prop(default = "neutral")] tone: &'static str,
) -> impl IntoView {
    view! {
        <div class="metric" data-tone=tone>
            <span class="metric-label"><Icon name=icon size=17 />{label}</span>
            <strong>{move || value.get()}</strong>
        </div>
    }
}
