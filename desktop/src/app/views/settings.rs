use crate::{
    api::{desktop, gateway},
    app::{
        components::{Icon, QrLoginDialog},
        labels::{platform_label, FORMAT_OPTIONS, PLATFORMS, QUALITY_OPTIONS},
    },
};
use leptos::prelude::*;
use serde_json::{json, Map, Value};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Recording,
    Appearance,
    Network,
    Accounts,
    Automation,
}

fn merged(payload: gateway::SettingsPayload) -> Map<String, Value> {
    let mut values = payload.default_config;
    values.extend(payload.user_config);
    values
}

#[component]
pub fn SettingsView() -> impl IntoView {
    let state = gateway::app_state();
    let tab = RwSignal::new(Tab::Recording);
    let draft = RwSignal::new(Map::<String, Value>::new());
    let changes = RwSignal::new(Map::<String, Value>::new());
    let loading = RwSignal::new(true);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let reload = RwSignal::new(0_u32);
    let cookies = RwSignal::new(Map::<String, Value>::new());
    let cookie_changes = RwSignal::new(Map::<String, Value>::new());
    let cookies_loaded = RwSignal::new(false);
    let cookies_loading = RwSignal::new(false);
    let cookie_error = RwSignal::new(None::<String>);
    let cookie_search = RwSignal::new(String::new());
    let cookie_platform = RwSignal::new("douyin".to_string());
    let reveal = RwSignal::new(false);
    let qr_open = RwSignal::new(false);
    let appearance_busy = RwSignal::new(false);
    let window_busy = RwSignal::new(false);
    let qr_account = RwSignal::new(None::<String>);
    let desktop = desktop::state();

    Effect::new(move |_| {
        let _ = state.settings_version.get();
        let _ = reload.get();
        if !changes.get_untracked().is_empty() {
            return;
        }
        loading.set(true);
        error.set(None);
        leptos::task::spawn_local(async move {
            match gateway::fetch_settings().await {
                Ok(payload) => {
                    if changes
                        .try_get_untracked()
                        .is_some_and(|patch| patch.is_empty())
                    {
                        let _ = draft.try_set(merged(payload));
                    }
                }
                Err(message) => {
                    let _ = error.try_set(Some(message));
                }
            }
            let _ = loading.try_set(false);
        });
    });
    Effect::new(move |_| {
        if tab.get() != Tab::Accounts
            || cookies_loaded.get_untracked()
            || cookies_loading.get_untracked()
        {
            return;
        }
        cookies_loading.set(true);
        cookie_error.set(None);
        leptos::task::spawn_local(async move {
            match gateway::fetch_cookies().await {
                Ok(values) => {
                    let _ = cookies.try_set(values);
                    let _ = cookies_loaded.try_set(true);
                }
                Err(message) => {
                    let _ = cookie_error.try_set(Some(message));
                }
            }
            let _ = cookies_loading.try_set(false);
        });
    });
    let change = Callback::new(move |(key, value): (&'static str, Value)| {
        draft.update(|values| {
            values.insert(key.into(), value.clone());
        });
        changes.update(|values| {
            values.insert(key.into(), value);
        });
    });
    let save = move |_| {
        if busy.get_untracked() {
            return;
        }
        let patch = changes.get_untracked();
        if patch.is_empty() {
            return;
        }
        for (key, min, max, label) in [
            ("video_segment_time", 1_u64, 86400_u64, "分段时长"),
            ("loop_time_seconds", 30, 86400, "检测间隔"),
        ] {
            if let Some(value) = patch.get(key) {
                if value
                    .as_str()
                    .and_then(|s| s.parse::<u64>().ok())
                    .filter(|v| *v >= min && *v <= max)
                    .is_none()
                {
                    error.set(Some(format!("{label}应为 {min}–{max} 秒")));
                    return;
                }
            }
        }
        if let Some(value) = patch.get("recording_space_threshold") {
            if value
                .as_str()
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v >= 0.1)
                .is_none()
            {
                error.set(Some("空间阈值至少为 0.1 GB".into()));
                return;
            }
        }
        let interval_changed = patch.contains_key("loop_time_seconds");
        busy.set(true);
        error.set(None);
        leptos::task::spawn_local(async move {
            match gateway::save_settings(Value::Object(patch.clone())).await {
                Ok(()) => {
                    let _ = changes.try_update(|pending| {
                        for (key, value) in &patch {
                            if pending.get(key) == Some(value) {
                                pending.remove(key);
                            }
                        }
                    });
                    state.notify(if interval_changed {
                        "设置已保存；已按新的检测间隔重新计时"
                    } else {
                        "偏好设置已保存"
                    });
                    if let Ok(payload) = gateway::fetch_settings().await {
                        state.apply_settings(payload);
                    }
                }
                Err(message) => {
                    let _ = error.try_set(Some(message));
                }
            }
            let _ = busy.try_set(false);
        });
    };
    let save_cookies = move |_| {
        if busy.get_untracked() {
            return;
        }
        let patch = cookie_changes.get_untracked();
        if patch.is_empty() {
            return;
        }
        busy.set(true);
        cookie_error.set(None);
        leptos::task::spawn_local(async move {
            match gateway::save_cookies(Value::Object(patch.clone())).await {
                Ok(()) => {
                    let _ = cookie_changes.try_update(|pending| {
                        for (key, value) in &patch {
                            if pending.get(key) == Some(value) {
                                pending.remove(key);
                            }
                        }
                    });
                    state.notify("平台登录信息已保存到此设备");
                }
                Err(message) => {
                    let _ = cookie_error.try_set(Some(message));
                }
            }
            let _ = busy.try_set(false);
        });
    };
    let choose_close_action = Callback::new(move |choice: String| {
        if window_busy.get_untracked() {
            return;
        }
        window_busy.set(true);
        leptos::task::spawn_local(async move {
            match gateway::save_settings(json!({"close_action":choice})).await {
                Ok(()) => {
                    if let Ok(settings) = gateway::fetch_settings().await {
                        state.apply_settings(settings);
                    }
                    state.notify("关闭窗口行为已保存");
                }
                Err(error) => state.fail(error),
            }
            let _ = window_busy.try_set(false);
        });
    });
    let choose_theme = Callback::new(move |theme: &'static str| {
        state.theme.set(theme.into());
        gateway::save_appearance(state);
    });
    let choose_accent = Callback::new(move |accent: &'static str| {
        state.accent.set(accent.into());
        gateway::save_appearance(state);
    });
    let cookie_keys = move || {
        let search = cookie_search.get().to_lowercase();
        let mut keys: Vec<_> = PLATFORMS
            .iter()
            .filter(|(key, _)| *key != "custom")
            .map(|(key, _)| key.to_string())
            .collect();
        keys.extend(cookies.with(|map| map.keys().cloned().collect::<Vec<_>>()));
        keys.sort();
        keys.dedup();
        keys.into_iter()
            .filter(|key| {
                key.to_lowercase().contains(&search)
                    || platform_label(key).to_lowercase().contains(&search)
            })
            .collect::<Vec<_>>()
    };
    let select_cookie = Callback::new(move |key: String| {
        cookie_platform.set(key);
        reveal.set(false);
    });
    let set_cookie = Callback::new(move |value: String| {
        let key = cookie_platform.get_untracked();
        if key == "kuaishou" {
            qr_account.set(None);
        }
        cookies.update(|map| {
            map.insert(key.clone(), Value::String(value.clone()));
        });
        cookie_changes.update(|map| {
            map.insert(key, Value::String(value));
        });
    });
    let cookie_retry = move |_| {
        if cookies_loading.get_untracked() {
            return;
        }
        cookies_loading.set(true);
        leptos::task::spawn_local(async move {
            match gateway::fetch_cookies().await {
                Ok(values) => {
                    let _ = cookies.try_set(values);
                    let _ = cookies_loaded.try_set(true);
                    let _ = cookie_error.try_set(None);
                }
                Err(message) => {
                    let _ = cookie_error.try_set(Some(message));
                }
            }
            let _ = cookies_loading.try_set(false);
        });
    };
    view! {
        <div class="page settings-page">
            <header class="page-header"><div><span class="eyebrow">"MAKE IT YOURS"</span><h1>"偏好设置"</h1><p>"让每一次录制，更符合你的习惯。"</p></div>
                <Show when=move || !changes.get().is_empty()><button class="button primary" disabled=move || busy.get() on:click=save><Icon name="check" size=17 />{move || if busy.get() { "保存中…" } else { "保存修改" }}</button></Show>
            </header>
            <div class="settings-layout">
                <nav class="settings-nav" aria-label="设置分类">
                    {[(Tab::Recording,"video","录制与存储"),(Tab::Appearance,"sun","外观与窗口"),(Tab::Network,"signal","网络与检测"),(Tab::Accounts,"lock","平台登录"),(Tab::Automation,"clock","自动化与通知")].into_iter().map(|(mode,icon,label)| view! {
                        <button class="settings-nav-item" class:active=move || tab.get() == mode aria-pressed=move || (tab.get() == mode).to_string() on:click=move |_| tab.set(mode)><Icon name=icon size=18 /><span>{label}</span><Icon name="chevron" size=13 /></button>
                    }).collect_view()}
                    <div class="settings-local-note"><Icon name="shield" size=18 /><p>"设置和登录信息仅保存在此设备。"</p></div>
                </nav>
                <div class="settings-content">
                    <Show when=move || error.get().is_some()><div class="inline-error" role="alert"><Icon name="alert" size=18 /><p>{move || error.get().unwrap_or_default()}</p><Show when=move || changes.get().is_empty()><button class="button secondary small" on:click=move |_| reload.update(|v| *v = v.wrapping_add(1))>"重试"</button></Show></div></Show>
                    <Show when=move || tab.get() == Tab::Recording || tab.get() == Tab::Network>
                        <Show when=move || !loading.get() && !draft.get().is_empty() fallback=move || view! { <div class="loading-state glass"><span class="spinner" />"正在读取设置…"</div> }>
                            <fieldset disabled=move || busy.get()>
                                <Show when=move || tab.get() == Tab::Recording>
                                    <SettingsGroup title="录制偏好" subtitle="作为直播间的默认配置，可为单个任务单独覆盖。" icon="video">
                                        <div class="form-grid"><label class="field"><span>"默认清晰度"</span><select class="input" prop:value=move || draft.with(|m| m.get("record_quality").and_then(Value::as_str).unwrap_or("OD").to_string()) on:change=move |e| change.run(("record_quality", Value::String(event_target_value(&e))))>{QUALITY_OPTIONS.iter().map(|(code,label)| view! { <option value=*code>{*label}</option> }).collect_view()}</select></label>
                                        <label class="field"><span>"默认录制格式"</span><select class="input" prop:value=move || draft.with(|m| m.get("video_format").and_then(Value::as_str).unwrap_or("TS").to_string()) on:change=move |e| change.run(("video_format", Value::String(event_target_value(&e))))>{FORMAT_OPTIONS.iter().map(|format| view! { <option value=*format>{*format}</option> }).collect_view()}</select></label></div>
                                        <SettingToggle label="分段录制" description="按设定时长生成独立文件，便于保存和整理。" setting="segmented_recording_enabled" draft=draft on_change=change />
                                        <SettingText label="分段时长" hint="单位：秒，范围 1–86400。" setting="video_segment_time" draft=draft on_change=change numeric=true />
                                    </SettingsGroup>
                                    <SettingsGroup title="文件与目录" subtitle="文件保存在本机；改变路径不移动已有录制文件。" icon="folder">
                                        <SettingText label="录制保存位置" hint="留空使用应用默认录制目录。" setting="live_save_path" draft=draft on_change=change />
                                        <SettingToggle label="文件名包含直播标题" description="将直播标题添加到新录制文件的名称中。" setting="filename_includes_title" draft=draft on_change=change />
                                        <SettingToggle label="按平台建立文件夹" description="不同直播平台的内容分别存放。" setting="folder_name_platform" draft=draft on_change=change />
                                        <SettingToggle label="按主播建立文件夹" description="每个主播拥有独立的录制目录。" setting="folder_name_author" draft=draft on_change=change />
                                        <SettingToggle label="按日期建立文件夹" description="在目录中按录制日期进一步归档。" setting="folder_name_time" draft=draft on_change=change />
                                        <SettingText label="剩余空间保护阈值" hint="单位：GB。低于阈值时暂停录制。" setting="recording_space_threshold" draft=draft on_change=change numeric=true />
                                    </SettingsGroup>
                                </Show>
                                <Show when=move || tab.get() == Tab::Network>
                                    <SettingsGroup title="直播检测" subtitle="定期查询直播间状态，发现开播后启动录制。" icon="refresh">
                                        <SettingText label="检测间隔" hint="单位：秒，至少 30 秒；保存后立即重新计时。" setting="loop_time_seconds" draft=draft on_change=change numeric=true />
                                        <div class="capability-note"><Icon name="info" size=17 /><p>"平台独立限流和并发设置暂不可用。"</p></div>
                                    </SettingsGroup>
                                    <SettingsGroup title="解析代理" subtitle="仅用于查询直播信息，不代表录制流量已使用代理。" icon="signal">
                                        <SettingToggle label="使用代理解析" description="通过下面的代理地址查询平台直播信息。" setting="enable_proxy" draft=draft on_change=change />
                                        <SettingText label="代理地址" hint="例如 http://127.0.0.1:7890。请确认代理服务已启动。" setting="proxy_address" draft=draft on_change=change />
                                    </SettingsGroup>
                                </Show>
                            </fieldset>
                            <Show when=move || !changes.get().is_empty()><div class="settings-savebar glass"><span><i class="unsaved-dot" />"有未保存的修改"</span><button class="button secondary small" disabled=move || busy.get() on:click=move |_| { changes.set(Map::new()); reload.update(|v| *v = v.wrapping_add(1)); }>"放弃修改"</button><button class="button primary small" disabled=move || busy.get() on:click=save>"保存修改"</button></div></Show>
                        </Show>
                    </Show>
                    <Show when=move || tab.get() == Tab::Appearance>
                        <SettingsGroup title="外观" subtitle="窗口和内容使用同一套主题。" icon="sun">
                            <div class="theme-options" role="group" aria-label="主题模式">
                                {[("light","浅色","sun"),("dark","深色","moon"),("system","跟随系统","monitor")].into_iter().map(|(mode,label,icon)| view! {
                                    <button class="theme-option" class:active=move || state.theme.get() == mode aria-pressed=move || (state.theme.get() == mode).to_string() on:click=move |_| choose_theme.run(mode)><span class="theme-preview" data-preview=mode><i /><i /><i /></span><span><Icon name=icon size=17 />{label}<span class="theme-check"><Icon name="check" size=16 /></span></span></button>
                                }).collect_view()}
                            </div>
                            <div class="settings-row"><div><strong>"强调色"</strong><p>"用于主要按钮、选中状态和界面高亮。"</p></div></div>
                            <div class="accent-options" role="group" aria-label="强调色">
                                {[("blue","晴空蓝"),("teal","湖水青"),("purple","鸢尾紫"),("green","松林绿"),("orange","暖橙"),("red","珊瑚红"),("indigo","靛蓝")].into_iter().map(|(color,label)| view! {
                                    <button class="accent-swatch" data-color=color class:active=move || state.accent.get() == color aria-label=label title=label aria-pressed=move || (state.accent.get() == color).to_string() on:click=move |_| choose_accent.run(color)><Icon name="check" size=17 /></button>
                                }).collect_view()}
                            </div>
                            <div class="settings-row"><div><strong>"默认任务视图"</strong><p>"在网格卡片和紧凑列表之间切换。"</p></div><div class="view-switch"><button class:active=move || state.grid_view.get() aria-label="默认网格视图" disabled=move || appearance_busy.get() on:click=move |_| { state.grid_view.set(true); appearance_busy.set(true); leptos::task::spawn_local(async move { if let Err(error) = gateway::save_settings(json!({"is_grid_view":true})).await { state.fail(error); } let _ = appearance_busy.try_set(false); }); }><Icon name="grid" size=18 /></button><button class:active=move || !state.grid_view.get() aria-label="默认列表视图" disabled=move || appearance_busy.get() on:click=move |_| { state.grid_view.set(false); appearance_busy.set(true); leptos::task::spawn_local(async move { if let Err(error) = gateway::save_settings(json!({"is_grid_view":false})).await { state.fail(error); } let _ = appearance_busy.try_set(false); }); }><Icon name="list" size=18 /></button></div></div>
                        </SettingsGroup>
                    </Show>
                    <Show when=move || tab.get() == Tab::Appearance && desktop.available>
                        <SettingsGroup title="窗口行为" subtitle="选择点击窗口关闭按钮或按 Alt+F4 后的行为。" icon="monitor">
                            <div class="settings-row"><div><strong>"关闭窗口时"</strong><p>"退出会停止录制；托盘模式继续在后台运行。"</p></div>
                                <select class="input" aria-label="关闭窗口时" prop:value=move||state.setting("close_action","ask") disabled=move||window_busy.get() on:change=move|event|choose_close_action.run(event_target_value(&event))>
                                    <option value="ask">"每次询问"</option><option value="exit">"退出应用"</option><option value="tray">"最小化到托盘"</option>
                                </select>
                            </div>
                        </SettingsGroup>
                    </Show>
                    <Show when=move || tab.get() == Tab::Accounts>
                        <SettingsGroup title="平台登录" subtitle="部分平台需要 Cookie 才能获取直播信息。仅保存明确编辑的项目。" icon="lock">
                            <div class="scope-notice"><Icon name="shield" size=19 /><p>"Cookie 属于敏感登录信息，请勿分享或提交到代码仓库。"</p></div>
                            <Show when=move || cookies_loading.get()><div class="loading-state"><span class="spinner" />"正在读取平台信息…"</div></Show>
                            <Show when=move || cookie_error.get().is_some()><div class="inline-error" role="alert"><p>{move || cookie_error.get().unwrap_or_default()}</p><Show when=move || !cookies_loaded.get()><button class="button secondary small" on:click=cookie_retry>"重试"</button></Show></div></Show>
                            <Show when=move || cookies_loaded.get()>
                                <div class="cookie-workspace"><div class="cookie-sidebar"><label class="search-field compact"><Icon name="search" size=16 /><input aria-label="搜索平台" placeholder="搜索平台…" prop:value=move || cookie_search.get() on:input=move |e| cookie_search.set(event_target_value(&e)) /></label><div class="cookie-platform-list" role="group" aria-label="Cookie 平台列表">
                                    {move || cookie_keys().into_iter().map(|key| { let target = key.clone(); let active_key = key.clone(); let saved_key = key.clone(); view! { <button class="cookie-platform" class:active=move || cookie_platform.get() == active_key on:click=move |_| select_cookie.run(target.clone())><span>{platform_label(&key)}</span><span class="configured-dot" class:configured=move || cookies.with(|values| values.get(&saved_key).and_then(Value::as_str).is_some_and(|s| !s.is_empty())) /></button> } }).collect_view()}
                                </div></div><div class="cookie-editor"><div class="cookie-editor-heading"><div><h3>{move || platform_label(&cookie_platform.get())}</h3><span>"平台 Cookie"</span></div><Show when=move || cookie_platform.get() == "kuaishou"><button class="button secondary small" disabled=move || !state.status.get().resolver_ready on:click=move |_| qr_open.set(true)><Icon name="qr" size=16 />"扫码登录"</button></Show></div>
                                    <label class="field"><span>"登录信息"</span><input class="input cookie-input" type=move || if reveal.get() { "text" } else { "password" } autocomplete="off" spellcheck="false" aria-label="平台 Cookie" placeholder="粘贴该平台的 Cookie" disabled=move || busy.get()
                                        prop:value=move || cookies.with(|values| values.get(&cookie_platform.get()).and_then(Value::as_str).unwrap_or("").to_string()) on:input=move |e| set_cookie.run(event_target_value(&e)) /></label>
                                    <div class="cookie-tools"><button class="text-button" on:click=move |_| reveal.update(|v| *v = !*v)><Icon name="eye" size=15 />{move || if reveal.get() { "隐藏内容" } else { "显示内容" }}</button><button class="text-button danger-text" disabled=move || busy.get() on:click=move |_| set_cookie.run(String::new())>"清空此平台"</button></div>
                                    <Show when=move||cookie_platform.get()=="kuaishou"&&qr_account.get().is_some()><div class="account-status" role="status"><Icon name="check" size=17/><span>{move||qr_account.get().unwrap_or_default()}" · 已保存"</span></div></Show>
                                    <p class="field-hint">"清空后需要保存才会移除登录信息。其他平台不受影响。"</p>
                                    <div class="cookie-save"><span>{move || format!("{} 个平台待保存", cookie_changes.get().len())}</span><button class="button primary" disabled=move || busy.get() || cookie_changes.get().is_empty() on:click=save_cookies>{move || if busy.get() { "保存中…" } else { "保存登录信息" }}</button></div>
                                </div></div>
                            </Show>
                        </SettingsGroup>
                    </Show>
                    <Show when=move || tab.get() == Tab::Automation>
                        <SettingsGroup title="自动化与通知" subtitle="下列能力当前不可用。已有配置保留，但不会作为生效设置展示。" icon="clock">
                            {[("clock","定时录制与关机","指定时间段录制、到时停止和系统关机。"),("signal","开播与下播通知","系统通知，以及钉钉、微信、Telegram 等渠道推送。"),("video","录后转码","自动转换为 MP4，以及转码完成后清理源文件。"),("settings","自定义脚本与高级网络","录后执行脚本、FLV 直下、录制流代理和强制 HTTPS。")].into_iter().map(|(icon,title,description)| view! {
                                <div class="unavailable-row"><span class="unavailable-icon"><Icon name=icon size=19 /></span><div><strong>{title}</strong><p>{description}</p></div><span class="capability-badge">"暂不可用"</span></div>
                            }).collect_view()}
                        </SettingsGroup>
                    </Show>
                </div>
            </div>
        </div>
        <QrLoginDialog open=qr_open on_success=Callback::new(move |result: gateway::QrSnapshot| {
            if let Some(cookie)=result.cookies{cookies.update(|values|{values.insert("kuaishou".into(),Value::String(cookie));});}
            cookie_changes.update(|values|{values.remove("kuaishou");});
            qr_account.set(Some(result.message));cookie_platform.set("kuaishou".into());
            qr_open.set(false);state.notify("快手登录信息已保存，账号验证通过");
        }) />
    }
}

#[component]
fn SettingsGroup(
    title: &'static str,
    subtitle: &'static str,
    icon: &'static str,
    children: Children,
) -> impl IntoView {
    view! { <section class="settings-group glass"><header class="settings-group-header"><span class="settings-group-icon"><Icon name=icon size=20 /></span><div><h2>{title}</h2><p>{subtitle}</p></div></header><div class="settings-group-body">{children()}</div></section> }
}
#[component]
fn SettingText(
    label: &'static str,
    hint: &'static str,
    setting: &'static str,
    draft: RwSignal<Map<String, Value>>,
    on_change: Callback<(&'static str, Value)>,
    #[prop(optional)] numeric: bool,
) -> impl IntoView {
    view! { <label class="field settings-field"><span>{label}</span><input class="input" inputmode=if numeric { "decimal" } else { "text" } prop:value=move || draft.with(|values| values.get(setting).map(|v| v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string())).unwrap_or_default()) on:input=move |e| on_change.run((setting,Value::String(event_target_value(&e)))) /><small class="field-hint">{hint}</small></label> }
}
#[component]
fn SettingToggle(
    label: &'static str,
    description: &'static str,
    setting: &'static str,
    draft: RwSignal<Map<String, Value>>,
    on_change: Callback<(&'static str, Value)>,
) -> impl IntoView {
    view! { <label class="settings-row"><span><strong>{label}</strong><small>{description}</small></span><input class="switch" type="checkbox" role="switch" aria-label=label prop:checked=move || draft.with(|values| values.get(setting).and_then(Value::as_bool).unwrap_or(false)) on:change=move |e| on_change.run((setting,Value::Bool(event_target_checked(&e)))) /></label> }
}
