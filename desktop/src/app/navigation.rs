use super::components::Icon;
use crate::api::gateway;
use leptos::prelude::*;
use leptos_router::{components::A, hooks::use_location};

#[component]
pub fn Sidebar() -> impl IntoView {
    let location = use_location();
    let state = gateway::app_state();
    view! {
        <aside class="sidebar glass">
            <a class="sidebar-brand" href="/home" aria-label="StreamCap 总览">
                <span class="brand-mark"><img src="/logo-mark.png" alt="" /></span>
                <div><strong>"StreamCap"</strong><small>"LIVE RECORDING"</small></div>
            </a>
            <span class="nav-label">"工作空间"</span>
            <nav class="nav-primary" aria-label="主导航">
                {[("home", "总览", "/home"), ("video", "录制任务", "/recordings"), ("folder", "媒体库", "/storage"), ("settings", "偏好设置", "/settings")].into_iter().map(|(icon,label,href)| {
                    let active = move || { let path = location.pathname.get(); path == href || (href == "/home" && path == "/") };
                    view! {
                        <A href=href attr:aria-label=label attr:class=move || if active() { "nav-item active" } else { "nav-item" }>
                            <Icon name=icon /><span>{label}</span>
                            {(href == "/recordings").then(|| view! { <span class="nav-count">{move || state.recordings.get().len()}</span> })}
                        </A>
                    }
                }).collect_view()}
            </nav>
            <div class="sidebar-bottom">
                <div class="workspace-card"><span class="workspace-icon"><Icon name="shield" size=18 /></span><div><strong>"本地工作空间"</strong><p>"你的设备，你的内容"</p></div></div>
                <A href="/about" attr:aria-label="关于 StreamCap" attr:class=move || if location.pathname.get() == "/about" { "nav-item active" } else { "nav-item" }><Icon name="info" /><span>"关于 StreamCap"</span><span class="version-label">{env!("CARGO_PKG_VERSION")}</span></A>
            </div>
        </aside>
    }
}
