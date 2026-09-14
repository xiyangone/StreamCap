use crate::app::i18n::t;
use crate::{
    api::{desktop, gateway},
    app::{
        components::{ConfirmDialog, Icon, QrLoginDialog},
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
            ("video_segment_time", 1_u64, 86400_u64, t("分段时长")),
            ("loop_time_seconds", 30, 86400, t("检测间隔")),
        ] {
            if let Some(value) = patch.get(key) {
                if value
                    .as_str()
                    .and_then(|s| s.parse::<u64>().ok())
                    .filter(|v| *v >= min && *v <= max)
                    .is_none()
                {
                    error.set(Some(crate::tr_format!("{label}应为 {min}–{max} 秒")));
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
                error.set(Some(t("空间阈值至少为 0.1 GB").into()));
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
                        t("设置已保存；已按新的检测间隔重新计时")
                    } else {
                        t("偏好设置已保存")
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
                    state.notify(t("平台登录信息已保存到此设备"));
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
                    state.notify(t("关闭窗口行为已保存"));
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
            <header class="page-header"><div><span class="eyebrow">"MAKE IT YOURS"</span><h1>{t("偏好设置")}</h1><p>{t("让每一次录制，更符合你的习惯。")}</p></div>
                <Show when=move || !changes.get().is_empty()><button class="button primary" disabled=move || busy.get() on:click=save><Icon name="check" size=17 />{move || if busy.get() { t("保存中…") } else { t("保存修改") }}</button></Show>
            </header>
            <div class="settings-layout">
                <nav class="settings-nav" aria-label=t("设置分类")>
                    {[(Tab::Recording,"video",t("录制与存储")),(Tab::Appearance,"sun",t("外观与窗口")),(Tab::Network,"signal",t("网络与检测")),(Tab::Accounts,"lock",t("平台登录")),(Tab::Automation,"clock",t("自动化与通知"))].into_iter().map(|(mode,icon,label)| view! {
                        <button class="settings-nav-item" class:active=move || tab.get() == mode aria-pressed=move || (tab.get() == mode).to_string() on:click=move |_| tab.set(mode)><Icon name=icon size=18 /><span>{label}</span><Icon name="chevron" size=13 /></button>
                    }).collect_view()}
                    <div class="settings-local-note"><Icon name="shield" size=18 /><p>{t("设置和登录信息仅保存在此设备。")}</p></div>
                </nav>
                <div class="settings-content">
                    <Show when=move || error.get().is_some()><div class="inline-error" role="alert"><Icon name="alert" size=18 /><p>{move || error.get().unwrap_or_default()}</p><Show when=move || changes.get().is_empty()><button class="button secondary small" on:click=move |_| reload.update(|v| *v = v.wrapping_add(1))>{t("重试")}</button></Show></div></Show>
                    <Show when=move || tab.get() == Tab::Recording || tab.get() == Tab::Network || tab.get() == Tab::Automation>
                        <Show when=move || !loading.get() && !draft.get().is_empty() fallback=move || view! { <div class="loading-state glass"><span class="spinner" />{t("正在读取设置…")}</div> }>
                            <fieldset disabled=move || busy.get()>
                                <Show when=move || tab.get() == Tab::Recording>
                                    <SettingsGroup title=t("录制偏好") subtitle=t("作为直播间的默认配置，可为单个任务单独覆盖。") icon="video">
                                        <div class="form-grid"><label class="field"><span>{t("默认清晰度")}</span><select class="input" prop:value=move || draft.with(|m| m.get("record_quality").and_then(Value::as_str).unwrap_or("OD").to_string()) on:change=move |e| change.run(("record_quality", Value::String(event_target_value(&e))))>{QUALITY_OPTIONS.iter().map(|(code,label)| view! { <option value=*code>{*label}</option> }).collect_view()}</select></label>
                                        <label class="field"><span>{t("默认录制格式")}</span><select class="input" prop:value=move || draft.with(|m| m.get("video_format").and_then(Value::as_str).unwrap_or("TS").to_string()) on:change=move |e| change.run(("video_format", Value::String(event_target_value(&e))))>{FORMAT_OPTIONS.iter().map(|format| view! { <option value=*format>{*format}</option> }).collect_view()}</select></label></div>
                                        <SettingToggle label=t("录制结束转 MP4") description=t("TS 停止录制后无损转封装；仅影响之后启动的录制。需要 FFmpeg 和同目录 ffprobe。") setting="convert_to_mp4" draft=draft on_change=change />
                                        <SettingToggle label=t("转换成功后清理源 TS") description=t("仅在 MP4 完整校验成功后删除对应源 TS；失败或取消时保留。") setting="delete_original" draft=draft on_change=change />
                                        <SettingToggle label=t("分段录制") description=t("按设定时长生成独立文件，便于保存和整理。") setting="segmented_recording_enabled" draft=draft on_change=change />
                                        <SettingText label=t("分段时长") hint=t("单位：秒，范围 1–86400。") setting="video_segment_time" draft=draft on_change=change numeric=true />
                                    </SettingsGroup>
                                    <SettingsGroup title=t("文件与目录") subtitle=t("文件保存在本机；改变路径不移动已有录制文件。") icon="folder">
                                        <div class="directory-picker"><SettingText label=t("录制保存位置") hint=t("留空使用应用默认录制目录。") setting="live_save_path" draft=draft on_change=change /><button class="button secondary small" type="button" disabled=move || !desktop.available on:click=move |_| {leptos::task::spawn_local(async move {match desktop::pick_directory().await{Ok(Some(path))=>change.run(("live_save_path",Value::String(path))),Ok(None)=>{},Err(error)=>state.fail(error)}});} >{t("选择文件夹")}</button></div>
                                        <SettingToggle label=t("生成时间字幕") description=t("录制结束后生成同名 SRT，按真实分段时长写入录制时间；需要 ffprobe。") setting="generate_time_subtitle_file" draft=draft on_change=change />
                                        <SettingToggle label=t("文件名包含直播标题") description=t("将直播标题添加到新录制文件的名称中。") setting="filename_includes_title" draft=draft on_change=change />
                                        <SettingToggle label=t("按平台建立文件夹") description=t("不同直播平台的内容分别存放。") setting="folder_name_platform" draft=draft on_change=change />
                                        <SettingToggle label=t("按主播建立文件夹") description=t("每个主播拥有独立的录制目录。") setting="folder_name_author" draft=draft on_change=change />
                                        <SettingToggle label=t("按日期建立文件夹") description=t("在目录中按录制日期进一步归档。") setting="folder_name_time" draft=draft on_change=change />
                                        <SettingToggle label=t("按直播标题建立文件夹") description=t("将新录像按直播标题归档。") setting="folder_name_title" draft=draft on_change=change /><SettingToggle label=t("文件名移除 Emoji") description=t("只影响之后生成的录像文件名。") setting="remove_emojis" draft=draft on_change=change /><SettingText label=t("自定义文件名模板") hint=t("支持 {anchor_name}、{title}、{time}、{platform}；留空使用默认命名。") setting="custom_filename_template" draft=draft on_change=change /><SettingText label=t("剩余空间保护阈值") hint=t("单位：GB。低于阈值时暂停录制。") setting="recording_space_threshold" draft=draft on_change=change numeric=true />
                                    </SettingsGroup>
                                </Show>
                                <Show when=move || tab.get() == Tab::Network>
                                    <SettingsGroup title=t("直播检测") subtitle=t("定期查询直播间状态，发现开播后启动录制。") icon="refresh">
                                        <SettingText label=t("检测间隔") hint=t("单位：秒，至少 30 秒；保存后立即重新计时。") setting="loop_time_seconds" draft=draft on_change=change numeric=true />
                                        <SettingText label=t("平台最大并发") hint=t("每个平台同时解析的请求数，范围 1–16。") setting="platform_max_concurrent_requests" draft=draft on_change=change numeric=true /><SettingText label=t("同平台请求间隔") hint=t("单位：秒，范围 0–300；手动检测同样遵守。") setting="platform_request_interval" draft=draft on_change=change numeric=true />
                                    </SettingsGroup>
                                    <SettingsGroup title=t("代理与直播源") subtitle=t("应用于直播解析和新启动的录制。") icon="signal">
                                        <SettingToggle label=t("使用代理") description=t("通过指定代理查询并拉取直播流。") setting="enable_proxy" draft=draft on_change=change />
                                        <SettingText label=t("代理地址") hint=t("FFmpeg 录制使用 http:// 代理；SOCKS 仅支持解析和 FLV 直下。") setting="proxy_address" draft=draft on_change=change /><SettingText label=t("使用代理的平台") hint=t("平台标识用逗号分隔；留空对全部平台生效。") setting="default_platform_with_proxy" draft=draft on_change=change /><label class="field"><span>{t("优先直播源")}</span><select class="input" prop:value=move || draft.with(|m|m.get("default_live_source").and_then(Value::as_str).unwrap_or("FLV").to_string()) on:change=move |e|change.run(("default_live_source",Value::String(event_target_value(&e))))><option value="FLV">"FLV"</option><option value="HLS">"HLS"</option></select></label><SettingToggle label=t("强制 HTTPS 拉流") description=t("将 HTTP 播放地址升级为 HTTPS；平台必须支持 HTTPS。") setting="force_https_recording" draft=draft on_change=change /><SettingToggle label=t("FLV 直接下载") description=t("使用 Rust 直接下载；需要 FLV 格式并关闭分段。") setting="flv_use_direct_download" draft=draft on_change=change />
                                    </SettingsGroup>
                                </Show>

                                <Show when=move || tab.get()==Tab::Automation>
                                  <SettingsGroup title=t("自动化") subtitle=t("任务的录制时间段在任务编辑中设置。") icon="clock">
                                    <SettingToggle label=t("定时关机") description=t("每天到指定时间提醒，倒计时结束前可取消。") setting="scheduled_shutdown_enabled" draft=draft on_change=change />
                                    <SettingText label=t("关机时间") hint=t("本地时间，格式 HH:MM。") setting="scheduled_shutdown_time" draft=draft on_change=change />
                                    <ShutdownTimer />
                                    <SettingToggle label=t("录制后执行脚本") description=t("只执行显式配置的程序参数，不隐式调用 shell。") setting="execute_custom_script" draft=draft on_change=change />
                                    <SettingText label=t("脚本命令") hint=t("JSON 数组，首项为程序绝对路径；参数支持 {file} 与 {room}。") setting="custom_script_command" draft=draft on_change=change />
                                  </SettingsGroup>
                                  <SettingsGroup title=t("开播与下播通知") subtitle=t("渠道默认关闭，启用前填写对应接收配置。") icon="signal">
                                    <SettingToggle label=t("系统通知") description=t("在桌面显示直播状态通知。") setting="system_notification_enabled" draft=draft on_change=change />
                                    <SettingToggle label=t("推送开播通知") description=t("主播开播时发送到启用的渠道。") setting="stream_start_notification_enabled" draft=draft on_change=change />
                                    <SettingToggle label=t("推送下播通知") description=t("平台确认下播时发送通知。") setting="stream_end_notification_enabled" draft=draft on_change=change />
                                    <SettingToggle label=t("仅通知，不自动录制") description=t("保留直播监控，不自动开始录制。") setting="only_notify_no_record" draft=draft on_change=change />
                                    <SettingText label=t("通知标题") hint=t("留空使用 StreamCap。") setting="custom_notification_title" draft=draft on_change=change />
                                    <SettingText label=t("开播通知内容") hint=t("支持 [room_name]、[title]、[time]。") setting="custom_stream_start_content" draft=draft on_change=change />
                                    <SettingText label=t("下播通知内容") hint=t("支持 [room_name]、[title]、[time]。") setting="custom_stream_end_content" draft=draft on_change=change />
                                    {[("dingtalk_enabled",t("钉钉")),("wechat_enabled",t("企业微信")),("feishu_enabled",t("飞书")),("bark_enabled","Bark"),("ntfy_enabled","ntfy"),("serverchan_enabled",t("Server酱")),("telegram_enabled","Telegram"),("email_enabled",t("邮件"))].into_iter().map(|(key,label)|view!{<NotificationChannel setting=key label=label draft=draft on_change=change />}).collect_view()}
                                  </SettingsGroup>
                                </Show>
                            </fieldset>
                            <Show when=move || !changes.get().is_empty()><div class="settings-savebar glass"><span><i class="unsaved-dot" />{t("有未保存的修改")}</span><button class="button secondary small" disabled=move || busy.get() on:click=move |_| { changes.set(Map::new()); reload.update(|v| *v = v.wrapping_add(1)); }>{t("放弃修改")}</button><button class="button primary small" disabled=move || busy.get() on:click=save>{t("保存修改")}</button></div></Show>
                        </Show>
                    </Show>
                    <Show when=move || tab.get() == Tab::Appearance>
                        <LanguagePicker /><SettingsGroup title=t("外观") subtitle=t("窗口和内容使用同一套主题。") icon="sun">
                            <div class="theme-options" role="group" aria-label=t("主题模式")>
                                {[("light",t("浅色"),"sun"),("dark",t("深色"),"moon"),("system",t("跟随系统"),"monitor")].into_iter().map(|(mode,label,icon)| view! {
                                    <button class="theme-option" class:active=move || state.theme.get() == mode aria-pressed=move || (state.theme.get() == mode).to_string() on:click=move |_| choose_theme.run(mode)><span class="theme-preview" data-preview=mode><i /><i /><i /></span><span><Icon name=icon size=17 />{label}<span class="theme-check"><Icon name="check" size=16 /></span></span></button>
                                }).collect_view()}
                            </div>
                            <div class="settings-row"><div><strong>{t("强调色")}</strong><p>{t("用于主要按钮、选中状态和界面高亮。")}</p></div></div>
                            <div class="accent-options" role="group" aria-label=t("强调色")>
                                {[("blue",t("晴空蓝")),("teal",t("湖水青")),("purple",t("鸢尾紫")),("green",t("松林绿")),("orange",t("暖橙")),("red",t("珊瑚红")),("indigo",t("靛蓝"))].into_iter().map(|(color,label)| view! {
                                    <button class="accent-swatch" data-color=color class:active=move || state.accent.get() == color aria-label=label title=label aria-pressed=move || (state.accent.get() == color).to_string() on:click=move |_| choose_accent.run(color)><Icon name="check" size=17 /></button>
                                }).collect_view()}
                            </div>
                            <div class="settings-row"><div><strong>{t("默认任务视图")}</strong><p>{t("在网格卡片和紧凑列表之间切换。")}</p></div><div class="view-switch"><button class:active=move || state.grid_view.get() aria-label=t("默认网格视图") disabled=move || appearance_busy.get() on:click=move |_| { state.grid_view.set(true); appearance_busy.set(true); leptos::task::spawn_local(async move { if let Err(error) = gateway::save_settings(json!({"is_grid_view":true})).await { state.fail(error); } let _ = appearance_busy.try_set(false); }); }><Icon name="grid" size=18 /></button><button class:active=move || !state.grid_view.get() aria-label=t("默认列表视图") disabled=move || appearance_busy.get() on:click=move |_| { state.grid_view.set(false); appearance_busy.set(true); leptos::task::spawn_local(async move { if let Err(error) = gateway::save_settings(json!({"is_grid_view":false})).await { state.fail(error); } let _ = appearance_busy.try_set(false); }); }><Icon name="list" size=18 /></button></div></div>
                        </SettingsGroup>
                    </Show>
                    <Show when=move || tab.get() == Tab::Appearance && desktop.available>
                        <SettingsGroup title=t("窗口行为") subtitle=t("选择点击窗口关闭按钮或按 Alt+F4 后的行为。") icon="monitor">
                            <div class="settings-row"><div><strong>{t("关闭窗口时")}</strong><p>{t("退出会停止录制；托盘模式继续在后台运行。")}</p></div>
                                <select class="input" aria-label=t("关闭窗口时") prop:value=move||state.setting("close_action","ask") disabled=move||window_busy.get() on:change=move|event|choose_close_action.run(event_target_value(&event))>
                                    <option value="ask">{t("每次询问")}</option><option value="exit">{t("退出应用")}</option><option value="tray">{t("最小化到托盘")}</option>
                                </select>
                            </div>
                        </SettingsGroup>
                    </Show>
                    <Show when=move || tab.get() == Tab::Accounts><AccountEditor platform=cookie_platform />
                        <SettingsGroup title=t("平台登录") subtitle=t("部分平台需要 Cookie 才能获取直播信息。仅保存明确编辑的项目。") icon="lock">
                            <div class="scope-notice"><Icon name="shield" size=19 /><p>{t("Cookie 属于敏感登录信息，请勿分享或提交到代码仓库。")}</p></div>
                            <Show when=move || cookies_loading.get()><div class="loading-state"><span class="spinner" />{t("正在读取平台信息…")}</div></Show>
                            <Show when=move || cookie_error.get().is_some()><div class="inline-error" role="alert"><p>{move || cookie_error.get().unwrap_or_default()}</p><Show when=move || !cookies_loaded.get()><button class="button secondary small" on:click=cookie_retry>{t("重试")}</button></Show></div></Show>
                            <Show when=move || cookies_loaded.get()>
                                <div class="cookie-workspace"><div class="cookie-sidebar"><label class="search-field compact"><Icon name="search" size=16 /><input aria-label=t("搜索平台") placeholder=t("搜索平台…") prop:value=move || cookie_search.get() on:input=move |e| cookie_search.set(event_target_value(&e)) /></label><div class="cookie-platform-list" role="group" aria-label=t("Cookie 平台列表")>
                                    {move || cookie_keys().into_iter().map(|key| { let target = key.clone(); let active_key = key.clone(); let saved_key = key.clone(); view! { <button class="cookie-platform" class:active=move || cookie_platform.get() == active_key on:click=move |_| select_cookie.run(target.clone())><span>{platform_label(&key)}</span><span class="configured-dot" class:configured=move || cookies.with(|values| values.get(&saved_key).and_then(Value::as_str).is_some_and(|s| !s.is_empty())) /></button> } }).collect_view()}
                                </div></div><div class="cookie-editor"><div class="cookie-editor-heading"><div><h3>{move || platform_label(&cookie_platform.get())}</h3><span>{t("平台 Cookie")}</span></div><Show when=move || cookie_platform.get() == "kuaishou"><button class="button secondary small" disabled=move || !state.status.get().resolver_ready on:click=move |_| qr_open.set(true)><Icon name="qr" size=16 />{t("扫码登录")}</button></Show></div>
                                    <label class="field"><span>{t("登录信息")}</span><input class="input cookie-input" type=move || if reveal.get() { "text" } else { "password" } autocomplete="off" spellcheck="false" aria-label=t("平台 Cookie") placeholder=t("粘贴该平台的 Cookie") disabled=move || busy.get()
                                        prop:value=move || cookies.with(|values| values.get(&cookie_platform.get()).and_then(Value::as_str).unwrap_or("").to_string()) on:input=move |e| set_cookie.run(event_target_value(&e)) /></label>
                                    <div class="cookie-tools"><button class="text-button" on:click=move |_| reveal.update(|v| *v = !*v)><Icon name="eye" size=15 />{move || if reveal.get() { t("隐藏内容") } else { t("显示内容") }}</button><button class="text-button danger-text" disabled=move || busy.get() on:click=move |_| set_cookie.run(String::new())>{t("清空此平台")}</button></div>
                                    <Show when=move||cookie_platform.get()=="kuaishou"&&qr_account.get().is_some()><div class="account-status" role="status"><Icon name="check" size=17/><span>{move||qr_account.get().unwrap_or_default()}{t(" · 已保存")}</span></div></Show>
                                    <p class="field-hint">{t("清空后需要保存才会移除登录信息。其他平台不受影响。")}</p>
                                    <div class="cookie-save"><span>{move || crate::tr_format!("{} 个平台待保存", cookie_changes.get().len())}</span><button class="button primary" disabled=move || busy.get() || cookie_changes.get().is_empty() on:click=save_cookies>{move || if busy.get() { t("保存中…") } else { t("保存登录信息") }}</button></div>
                                </div></div>
                            </Show>
                        </SettingsGroup>
                    </Show>

                    <Show when=move || tab.get()==Tab::Recording><ToolSettings /></Show>
                </div>
            </div>
        </div>
        <QrLoginDialog open=qr_open on_success=Callback::new(move |result: gateway::QrSnapshot| {
            if let Some(cookie)=result.cookies{cookies.update(|values|{values.insert("kuaishou".into(),Value::String(cookie));});}
            cookie_changes.update(|values|{values.remove("kuaishou");});
            qr_account.set(Some(result.message));cookie_platform.set("kuaishou".into());
            qr_open.set(false);state.notify(t("快手登录信息已保存，账号验证通过"));
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
    #[prop(optional)] secret: bool,
) -> impl IntoView {
    view! { <label class="field settings-field"><span>{label}</span><input class="input" type=if secret {"password"}else{"text"} autocomplete="off" inputmode=if numeric { "decimal" } else { "text" } prop:value=move || draft.with(|values| values.get(setting).map(|v| v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string())).unwrap_or_default()) on:input=move |e| on_change.run((setting,Value::String(event_target_value(&e)))) /><small class="field-hint">{hint}</small></label> }
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

#[component]
fn LanguagePicker() -> impl IntoView {
    let state = gateway::app_state();
    let busy = RwSignal::new(false);
    view! {<SettingsGroup title=t("界面语言") subtitle=t("保存后重新加载界面，不停止后台录制。") icon="settings"><label class="field"><span>{t("语言")}</span><select class="input" aria-label=t("界面语言") prop:value=move||state.setting("language","zh_CN") disabled=move||busy.get() on:change=move|e|{let value=event_target_value(&e);busy.set(true);leptos::task::spawn_local(async move{match gateway::save_settings(json!({"language":value})).await{Ok(())=>crate::app::i18n::set_language(&value),Err(error)=>state.fail(error)}let _=busy.try_set(false);});}><option value="zh_CN">{t("简体中文")}</option><option value="en">"English"</option></select></label></SettingsGroup>}
}
#[component]
fn ShutdownTimer() -> impl IntoView {
    let state = gateway::app_state();
    let native = desktop::state();
    let hours = RwSignal::new(state.setting("quick_shutdown_hours", "3"));
    let confirm = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let start = Callback::new(move |_| {
        let Ok(value) = hours.get_untracked().parse::<f64>() else {
            state.fail(t("请输入有效小时数"));
            return;
        };
        busy.set(true);
        leptos::task::spawn_local(async move {
            match gateway::shutdown_timer(Some(value)).await {
                Ok(()) => {
                    state.notify(t("关机倒计时已启动"));
                    let _ = confirm.try_set(false);
                }
                Err(error) => state.fail(error),
            }
            let _ = busy.try_set(false);
        });
    });
    view! {<div class="form-grid"><label class="field"><span>{t("关机倒计时（小时）")}</span><input class="input" type="number" min="0.01" max="168" prop:value=move||hours.get() on:input=move|e|hours.set(event_target_value(&e))/></label><div class="timer-actions"><button class="button secondary small" type="button" on:click=move |_|confirm.set(true)>{t("启动倒计时")}</button><button class="button secondary small" type="button" on:click=move |_|leptos::task::spawn_local(async move{let result=if native.available {desktop::cancel_shutdown().await} else {gateway::shutdown_timer(None).await};match result{Ok(())=>state.notify(t("关机倒计时已取消")),Err(error)=>state.fail(error)}})>{t("取消倒计时")}</button></div></div><ConfirmDialog open=confirm title=t("启动关机倒计时") description=Signal::derive(move||crate::tr_format!("将在 {} 小时后提示关机。请先保存其他应用中的工作。",hours.get())) busy=busy on_confirm=start icon="clock" destructive=false confirm_text=t("启动倒计时") busy_text=t("正在设置…") />}
}
#[component]
fn AccountEditor(platform: RwSignal<String>) -> impl IntoView {
    let state = gateway::app_state();
    let draft = RwSignal::new(Map::<String, Value>::new());
    let dirty = RwSignal::new(Map::<String, Value>::new());
    let busy = RwSignal::new(false);
    let revision = RwSignal::new(0_u32);
    let has_password = RwSignal::new(false);
    let has_token = RwSignal::new(false);
    Effect::new(move |_| {
        let key = platform.get();
        let generation = revision.get_untracked().wrapping_add(1);
        revision.set(generation);
        draft.set(Map::new());
        dirty.set(Map::new());
        has_password.set(false);
        has_token.set(false);
        leptos::task::spawn_local(async move {
            match gateway::accounts().await {
                Ok(values) => {
                    if revision.try_get_untracked() != Some(generation) {
                        return;
                    }
                    let value = &values[&key];
                    let mut fields = Map::new();
                    fields.insert("username".into(), value["username"].clone());
                    fields.insert("accountType".into(), value["accountType"].clone());
                    draft.set(fields);
                    has_password.set(value["hasPassword"].as_bool().unwrap_or(false));
                    has_token.set(value["hasAccessToken"].as_bool().unwrap_or(false));
                }
                Err(error) => state.fail(error),
            }
        });
    });
    let save = move |_| {
        let key = platform.get_untracked();
        let generation = revision.get_untracked();
        let patch = dirty.get_untracked();
        if patch.is_empty() {
            return;
        }
        busy.set(true);
        leptos::task::spawn_local(async move {
            match gateway::save_account(&key, Value::Object(patch)).await {
                Ok(()) => {
                    if revision.try_get_untracked() == Some(generation) {
                        let _ = dirty.try_set(Map::new());
                    }
                    state.notify(t("平台账号配置已保存"));
                }
                Err(error) => state.fail(error),
            }
            let _ = busy.try_set(false);
        });
    };
    view! {<details class="settings-group glass"><summary>{t("平台账号与访问令牌")}</summary><div class="settings-group-body"><p>{t("使用下方所选平台。已有密码和令牌不会回显，未修改的字段不会被覆盖。")}</p>{[("username",t("账号"),false),("password",t("密码"),true),("accessToken",t("访问令牌"),true),("accountType",t("账号类型"),false)].into_iter().map(|(key,label,secret)|view!{<label class="field"><span>{label}</span><input class="input" type=if secret{"password"}else{"text"} autocomplete="off" prop:value=move||draft.with(|m|m.get(key).and_then(Value::as_str).unwrap_or("").to_string()) placeholder=move||if (key=="password"&&has_password.get())||(key=="accessToken"&&has_token.get()){t("已保存；输入可替换")}else{""} on:input=move|e|{let value=Value::String(event_target_value(&e));draft.update(|m|{m.insert(key.into(),value.clone());});dirty.update(|m|{m.insert(key.into(),value);});}/></label>}).collect_view()}<button class="button secondary" type="button" disabled=move||busy.get()||dirty.get().is_empty() on:click=save>{t("保存平台账号")}</button></div></details>}
}
#[component]
fn ToolSettings() -> impl IntoView {
    let state = gateway::app_state();
    let result = RwSignal::new(Value::Null);
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let confirm = RwSignal::new(false);
    let reload = RwSignal::new(0_u32);
    Effect::new(move |_| {
        let _ = reload.get();
        leptos::task::spawn_local(async move {
            match gateway::tool_status().await {
                Ok(value) => {
                    let _ = result.try_set(value);
                    let _ = error.try_set(None);
                }
                Err(message) => {
                    let _ = error.try_set(Some(message));
                }
            }
        });
    });
    let timer = StoredValue::new_local(Some(gloo_timers::callback::Interval::new(
        1500,
        move || {
            if result
                .try_get_untracked()
                .is_some_and(|r| r["installation"]["state"] == "downloading")
            {
                let _ = reload.try_update(|v| *v = v.wrapping_add(1));
            }
        },
    )));
    on_cleanup(move || {
        timer.update_value(|v| {
            v.take();
        })
    });
    let install = Callback::new(move |_| {
        busy.set(true);
        leptos::task::spawn_local(async move {
            match gateway::install_tools().await {
                Ok(()) => {
                    let _ = confirm.try_set(false);
                    state.notify(t("开始下载并校验 FFmpeg"));
                    let _ = reload.try_update(|v| *v = v.wrapping_add(1));
                }
                Err(error) => state.fail(error),
            }
            let _ = busy.try_set(false);
        });
    });
    view! {<SettingsGroup title=t("录制工具") subtitle=t("noFF 不捆绑 FFmpeg。安装仅写入应用数据目录，不修改 PATH。") icon="settings"><p role="status">{move||if result.get()["ffmpegReady"]==true&&result.get()["ffprobeReady"]==true{t("FFmpeg 与 ffprobe 已就绪")}else{t("需要 FFmpeg 与 ffprobe")}}</p><p role="status">{move||result.get()["installation"]["message"].as_str().unwrap_or("").to_string()}</p><Show when=move||error.get().is_some()><p class="field-error">{move||error.get().unwrap_or_default()}</p></Show><button class="button secondary" type="button" disabled=move||busy.get()||result.get()["installation"]["state"]=="downloading" on:click=move |_|confirm.set(true)>{t("安装 FFmpeg")}</button></SettingsGroup><ConfirmDialog open=confirm title=t("安装录制工具") description=t("将从 Gyan 下载并验证 FFmpeg，保存到应用数据目录；不会覆盖已有工具。") busy=busy on_confirm=install icon="download" destructive=false confirm_text=t("确认安装") busy_text=t("正在安装…") />}
}

#[component]
fn NotificationChannel(
    setting: &'static str,
    label: &'static str,
    draft: RwSignal<Map<String, Value>>,
    on_change: Callback<(&'static str, Value)>,
) -> impl IntoView {
    let fields: &[(&str, &str)] = match setting {
        "dingtalk_enabled" => &[
            ("dingtalk_webhook_url", "钉钉 Webhook"),
            ("dingtalk_at_objects", "钉钉提醒手机号"),
        ],
        "wechat_enabled" => &[("wechat_webhook_url", "企业微信 Webhook")],
        "feishu_enabled" => &[("feishu_webhook_url", "飞书 Webhook")],
        "bark_enabled" => &[
            ("bark_webhook_url", "Bark 设备地址"),
            ("bark_interrupt_level", "Bark 提醒级别"),
            ("bark_sound", "Bark 提示音"),
        ],
        "ntfy_enabled" => &[
            ("ntfy_server_url", "ntfy topic 地址"),
            ("ntfy_tags", "ntfy 标签"),
            ("ntfy_email", "ntfy 转发邮箱"),
            ("ntfy_action_url", "ntfy 操作链接"),
        ],
        "serverchan_enabled" => &[
            ("serverchan_sendkey", "Server酱 SendKey"),
            ("serverchan_channel", "Server酱 渠道编号"),
            ("serverchan_tags", "Server酱 标签"),
        ],
        "telegram_enabled" => &[
            ("telegram_api_token", "Telegram Token"),
            ("telegram_chat_id", "Telegram Chat ID"),
        ],
        "email_enabled" => &[
            ("smtp_server", "SMTP 服务器"),
            ("smtp_port", "SMTP 端口"),
            ("email_username", "邮箱用户名"),
            ("email_password", "邮箱授权密码"),
            ("sender_email", "发件邮箱"),
            ("sender_name", "发件人名称"),
            ("recipient_email", "收件邮箱"),
        ],
        _ => &[],
    };
    view! {<div class="notification-channel"><SettingToggle label=label description=t("启用后填写接收配置并保存，不会自动发送测试消息。") setting=setting draft=draft on_change=on_change />
        <Show when=move||draft.with(|values|values.get(setting).and_then(Value::as_bool).unwrap_or(false))><div class="notification-fields">
            {fields.iter().map(|(key,label)|view!{<SettingText label=t(label) hint="" setting=*key draft=draft on_change=on_change secret=key.ends_with("webhook_url")||matches!(*key,"email_password"|"telegram_api_token"|"serverchan_sendkey"|"ntfy_server_url")/>}).collect_view()}
            {(setting=="dingtalk_enabled").then(||view!{<SettingToggle label=t("钉钉提醒所有人") description=t("仅影响钉钉渠道，发送前请确认群通知范围。") setting="dingtalk_at_all" draft=draft on_change=on_change/>})}
        </div></Show>
    </div>}
}
