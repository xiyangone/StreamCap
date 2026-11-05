use super::components::Icon;
use crate::api::gateway;
use crate::app::i18n::t;
use leptos::prelude::*;
use leptos_router::{components::A, hooks::use_location};

#[component]
pub fn Sidebar() -> impl IntoView {
    let location = use_location();
    let state = gateway::app_state();
    let active_recordings = Memo::new(move |_| {
        state
            .recordings
            .with(|items| items.iter().filter(|item| item.is_recording).count())
    });
    view! {
        <aside class="sidebar">
            <A attr:class="sidebar-brand" href="/home" attr:aria-label=t("StreamCap 总览")>
                <span class="brand-mark" aria-hidden="true"><img src="/logo-mark.png" width="30" height="32" alt="" /></span>
                <div><strong>"StreamCap"</strong><small>{t("本地录制")}</small></div>
            </A>
            <span class="nav-label">{t("工作空间")}</span>
            <nav class="nav-primary" aria-label=t("主导航")>
                {[("home", t("总览"), "/home"), ("video", t("录制任务"), "/recordings"), ("folder", t("媒体库"), "/storage"), ("settings", t("偏好设置"), "/settings")].into_iter().map(|(icon,label,href)| {
                    let active = move || { let path = location.pathname.get(); path == href || (href == "/home" && path == "/") };
                    view! {
                        <A href=href attr:aria-label=label attr:title=label
                            attr:aria-current=move || active().then_some("page")
                            attr:data-section=if href == "/settings" { "preferences" } else { "workspace" }
                            attr:class=move || if active() { "nav-item active" } else { "nav-item" }>
                            <Icon name=icon /><span>{label}</span>
                            {(href == "/recordings").then(|| view! { <span class="nav-count">{move || state.recordings.get().len()}</span> })}
                        </A>
                    }
                }).collect_view()}
            </nav>
            <div class="sidebar-bottom">
                <div class="workspace-card"><span class="workspace-icon"><Icon name="hard-drive" size=18 /></span><div><strong>{t("本地工作空间")}</strong><p>{move || {
                    if !state.status.get().ok { t("本地服务未连接").to_string() }
                    else if active_recordings.get() > 0 { crate::tr_format!("{} 路正在录制", active_recordings.get()) }
                    else { t("本地服务已连接").to_string() }
                }}</p></div><i class="workspace-status" class:online=move || state.status.get().ok /></div>
                <A href="/about" attr:aria-label=t("关于 StreamCap") attr:title=t("关于 StreamCap") attr:aria-current=move || (location.pathname.get() == "/about").then_some("page") attr:class=move || if location.pathname.get() == "/about" { "nav-item active" } else { "nav-item" }><Icon name="info" /><span>{t("关于 StreamCap")}</span><span class="version-label">{env!("CARGO_PKG_VERSION")}</span></A>
            </div>
        </aside>
    }
}
