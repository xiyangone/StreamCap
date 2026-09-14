use crate::app::i18n::t;
pub mod components;
pub mod i18n;
pub mod labels;
pub mod navigation;
pub mod views;

use crate::api::{desktop, gateway};
use components::Icon;
use leptos::prelude::*;
use leptos_router::hooks::use_location;
use wasm_bindgen::{closure::Closure, JsCast};

#[component]
pub fn AppShell() -> impl IntoView {
    view! { <leptos_router::components::Router><Shell /></leptos_router::components::Router> }
}

#[component]
fn Shell() -> impl IntoView {
    let state = gateway::provide_app_state();
    let desktop = desktop::provide_desktop();
    install_navigation();
    if let Some(root) = document().document_element() {
        let _ = root.set_attribute(
            "lang",
            if i18n::language() == "en" {
                "en"
            } else {
                "zh-CN"
            },
        );
    }
    let media_listener = window()
        .match_media("(prefers-color-scheme: dark)")
        .ok()
        .flatten()
        .map(|query| {
            let observed = query.clone();
            let handler = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
                state.system_dark.set(observed.matches())
            });
            let _ =
                query.add_event_listener_with_callback("change", handler.as_ref().unchecked_ref());
            (query, handler)
        });
    let media_listener = StoredValue::new_local(media_listener);
    on_cleanup(move || {
        media_listener.update_value(|slot| {
            if let Some((query, handler)) = slot.take() {
                let _ = query.remove_event_listener_with_callback(
                    "change",
                    handler.as_ref().unchecked_ref(),
                );
            }
        })
    });
    let events = StoredValue::new_local(None::<gateway::EventConnection>);
    Effect::new(move |_| {
        let _ = state.events_revision.get();
        events.update_value(|connection| {
            *connection = None;
            *connection = gateway::subscribe_events(state);
        });
    });
    on_cleanup(move || events.update_value(|connection| *connection = None));
    leptos::task::spawn_local_scoped_with_cancellation(gateway::poll_connection(state));
    Effect::new(move |_| {
        let theme = state.theme.get();
        let accent = state.accent.get();
        let resolved = if state.is_dark() { "dark" } else { "light" };
        desktop::apply_theme(&theme);
        if let Some(root) = document().document_element() {
            let _ = root.set_attribute("data-theme", resolved);
            let _ = root.set_attribute("data-accent", &accent);
        }
        if let Ok(Some(storage)) = window().local_storage() {
            let _ = storage.set_item(
                "streamcap.appearance",
                &serde_json::json!({"theme":theme,"accent":accent}).to_string(),
            );
        }
    });
    view! {
        <div class="app-shell" class:native-desktop=desktop.available>
            <a class="skip-link" href="#main-content">{t("跳到主要内容")}</a>
            <navigation::Sidebar />
            <div class="main-area">
                <Topbar />
                <ShutdownBanner />
                <main id="main-content" class="content-area" tabindex="-1">
                    <Show when=move || state.error.get().is_some()>
                        <div class="connection-banner" role="alert"><Icon name="alert" /><div><strong>{t("暂时无法连接本地服务")}</strong><p>{t("正在自动重试。已有任务仍保留，恢复连接后将刷新任务状态。")}</p></div></div>
                    </Show>
                    <AppRoutes />
                </main>
            </div>
            <components::Toast />
            <components::CloseDialog />
        </div>
    }
}

#[component]
fn Topbar() -> impl IntoView {
    let state = gateway::app_state();
    let desktop = desktop::state();
    let location = use_location();
    let title = move || match location.pathname.get().as_str() {
        "/recordings" => t("录制任务"),
        "/storage" => t("媒体库"),
        "/settings" => t("偏好设置"),
        "/about" => t("关于 StreamCap"),
        _ => t("总览"),
    };
    view! {
        <header class="topbar">
            <div class="titlebar-drag-zone" data-tauri-drag-region="deep"><div class="breadcrumbs"><strong>{title}</strong></div></div>
            <div class="topbar-actions">
                <span class="sync-status" class:online=move||state.status.get().ok&&state.events_connected.get() title=t("任务状态由本机自动更新，不涉及云同步")><i class="status-dot" />{move||if !state.status.get().ok{t("本地服务连接中")}else if state.events_connected.get(){t("本地实时更新")}else{t("正在重连")}}</span>
                <button class="icon-button theme-toggle" title=t("切换明暗主题") aria-label=move||if state.is_dark(){t("切换到浅色")}else{t("切换到深色")} on:click=move |_|{state.theme.set(if state.is_dark(){"light".into()}else{"dark".into()});gateway::save_appearance(state);}>
                    <Show when=move||state.is_dark() fallback=||view!{<Icon name="moon"/>}><Icon name="sun"/></Show>
                </button>
                <Show when=move||desktop.available><div class="window-controls" role="group" aria-label=t("窗口控制")>
                    <button class="window-button" title=t("最小化") aria-label=t("最小化窗口") on:click=move |_|desktop::action(desktop,state,"minimize")><Icon name="minimize" size=16/></button>
                    <button class="window-button" title=move||if desktop.status.get().maximized{t("还原")}else{t("最大化")} aria-label=move||if desktop.status.get().maximized{t("还原窗口")}else{t("最大化窗口")} on:click=move |_|desktop::action(desktop,state,"maximize")><Show when=move||desktop.status.get().maximized fallback=||view!{<Icon name="maximize" size=14/>}><Icon name="restore" size=14/></Show></button>
                    <button class="window-button window-close" title=t("关闭") aria-label=t("关闭窗口") on:click=move |_|desktop::action(desktop,state,"close")><Icon name="close" size=18/></button>
                </div></Show>
            </div>
        </header>
    }
}

#[component]
pub fn AppRoutes() -> impl IntoView {
    use leptos_router::components::{Route, Routes};
    use leptos_router::path;
    use views::{AboutView, HomeView, RecordingsView, SettingsView, StorageView};
    view! {
        <Routes fallback=|| view! { <div class="empty-state"><h1>{t("页面不存在")}</h1><a class="button primary" href="/home">{t("返回总览")}</a></div> }>
            <Route path=path!("/") view=HomeView />
            <Route path=path!("/home") view=HomeView />
            <Route path=path!("/recordings") view=RecordingsView />
            <Route path=path!("/storage") view=StorageView />
            <Route path=path!("/settings") view=SettingsView />
            <Route path=path!("/about") view=AboutView />
        </Routes>
    }
}

fn install_navigation() {
    use leptos_router::hooks::use_navigate;
    let location = use_location();
    let navigate = use_navigate();
    let remembered = window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|s| s.get_item("streamcap.lastPage").ok().flatten());
    if location.pathname.get_untracked() == "/" {
        if let Some(path) = remembered.filter(|path| {
            matches!(
                path.as_str(),
                "/home" | "/recordings" | "/storage" | "/settings" | "/about"
            )
        }) {
            navigate(
                &path,
                leptos_router::NavigateOptions {
                    replace: true,
                    ..Default::default()
                },
            );
        }
    }
    Effect::new(move |_| {
        let path = location.pathname.get();
        if matches!(
            path.as_str(),
            "/home" | "/recordings" | "/storage" | "/settings" | "/about"
        ) {
            if let Ok(Some(storage)) = window().local_storage() {
                let _ = storage.set_item("streamcap.lastPage", &path);
            }
        }
    });
    let keyboard =
        Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(move |event: web_sys::KeyboardEvent| {
            if event.repeat()
                || event.alt_key()
                || !(event.ctrl_key() || event.meta_key())
                || document()
                    .query_selector("dialog[open]")
                    .ok()
                    .flatten()
                    .is_some()
            {
                return;
            }
            let target = match event.key().as_str() {
                "1" => "/home",
                "2" => "/recordings",
                "3" => "/storage",
                "4" | "," => "/settings",
                "5" => "/about",
                _ => return,
            };
            event.prevent_default();
            navigate(target, Default::default());
        });
    let _ =
        document().add_event_listener_with_callback("keydown", keyboard.as_ref().unchecked_ref());
    let listener = StoredValue::new_local(Some(keyboard));
    on_cleanup(move || {
        listener.update_value(|slot| {
            if let Some(callback) = slot.take() {
                let _ = document().remove_event_listener_with_callback(
                    "keydown",
                    callback.as_ref().unchecked_ref(),
                );
            }
        })
    });
}
#[component]
fn ShutdownBanner() -> impl IntoView {
    let desktop = desktop::state();
    let state = gateway::app_state();
    view! { <Show when=move || desktop.status.get().system_shutdown>
        <div class="shutdown-banner" role="alert"><Icon name="alert"/><span>{move || match desktop.status.get().shutdown_seconds { Some(seconds) => crate::tr_format!("将在 {seconds} 秒后停止录制、完成文件处理并关机。请保存其他应用的工作。"), None => t("正在收尾录制与文件处理，完成后关机。").into() }}</span>
        <button class="button secondary small" type="button" on:click=move |_| leptos::task::spawn_local(async move { match desktop::cancel_shutdown().await { Ok(())=>state.notify(t("已取消系统关机")), Err(error)=>state.fail(error) } })>{t("取消关机")}</button></div>
    </Show> }
}
