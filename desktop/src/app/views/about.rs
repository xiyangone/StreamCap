use crate::api::gateway;
use crate::app::components::{ActionButton, Icon, PageHeader};
use crate::app::i18n::t;
use leptos::prelude::*;

#[component]
pub fn AboutView() -> impl IntoView {
    let state = gateway::app_state();
    let update = RwSignal::new(None::<gateway::UpdateCheck>);
    let update_message = RwSignal::new(String::new());
    let update_error = RwSignal::new(None::<String>);
    let checking = RwSignal::new(false);
    let check = Callback::new(move |_| {
        if checking.get_untracked() || !state.status.get_untracked().ok {
            return;
        }
        checking.set(true);
        update.set(None);
        update_error.set(None);
        update_message.set(t("正在检查项目发行版…").into());
        leptos::task::spawn_local(async move {
            match gateway::check_update().await {
                Ok(value) => {
                    let _ = update.try_set(Some(value));
                    let _ = update_message.try_set(t("已读取最新发行版").into());
                }
                Err(error) => {
                    let _ = update.try_set(None);
                    let _ = update_message.try_set(String::new());
                    let _ = update_error.try_set(Some(error));
                }
            }
            let _ = checking.try_set(false);
        });
    });
    view! {
        <div class="page about-page">
            <PageHeader title=t("关于 StreamCap") subtitle=t("版本信息与项目资源") />
            <div class="about-content">
                <section class="about-app">
                    <div class="about-app-line"><img src="/logo-mark.png" width="36" height="36" alt=t("StreamCap 熊猫标识") /><h2>"StreamCap"</h2><span class="ui-badge ui-badge-sm">{t("桌面版 ")}{env!("CARGO_PKG_VERSION")}</span></div>
                    <p>{t("多平台直播与媒体直链录制，文件保存在本地。")}</p>
                    <dl class="about-runtime"><div><dt>{t("运行平台")}</dt><dd>{t("Windows 桌面版")}</dd></div><div><dt>{t("本地服务版本")}</dt><dd>{move || if state.status.get().version.is_empty() { t("未连接").into() } else { state.status.get().version }}</dd></div><div><dt>{t("构建标识")}</dt><dd class="break-anywhere">{move || if state.status.get().build_id.is_empty() { t("未连接").into() } else { state.status.get().build_id }}</dd></div><div><dt>{t("本地解析组件")}</dt><dd>{move || if state.status.get().resolver_ready { t("已就绪") } else { t("未就绪") }}</dd></div><div><dt>{t("许可协议")}</dt><dd>"Apache License 2.0"</dd></div></dl>
                </section>
                <section class="about-updates">
                    <header class="about-section-heading"><div><h2>{t("版本检查")}</h2><p>{t("仅检查当前项目发行版，不自动下载或替换程序。")}</p></div>
                        <ActionButton label=t("检查更新") icon="refresh" variant="ghost" class="small" busy=checking busy_label=t("检查中…")
                            disabled=Signal::derive(move || !state.status.get().ok) on_click=check />
                    </header>
                    <p class="about-update-status" role="status" aria-live="polite">{move || if update_message.get().is_empty() && update_error.get().is_none() { t("尚未检查更新").into() } else { update_message.get() }}</p>
                    <Show when=move || update_error.get().is_some()><p class="field-error" role="alert">{move || update_error.get().unwrap_or_default()}</p></Show>
                    <Show when=move || !checking.get() && update_error.get().is_none() && update.get().is_some()>
                        <a class="resource-link about-release-link" href=move || update.get().map(|value| value.release.html_url).unwrap_or_default() target="_blank" rel="noopener noreferrer"><strong>{move || update.get().map(|value| value.release.tag_name).unwrap_or_default()}</strong><span>{t("查看发行说明")}</span><Icon name="external" size=15 /></a>
                    </Show>
                </section>
                <section class="about-resources"><h2>{t("项目与支持")}</h2>
                    <ul class="ui-menu about-resource-list">
                        <li><a class="resource-link" href="https://github.com/xiyangone/StreamCap" target="_blank" rel="noopener noreferrer"><span>{t("项目主页")}</span><small>{t("源代码与版本信息")}</small><Icon name="external" size=15 /></a></li>
                        <li><a class="resource-link" href="https://github.com/ihmily/StreamCap/wiki" target="_blank" rel="noopener noreferrer"><span>{t("使用文档")}</span><small>{t("平台支持与操作指南")}</small><Icon name="external" size=15 /></a></li>
                        <li><a class="resource-link" href="https://github.com/xiyangone/StreamCap/issues" target="_blank" rel="noopener noreferrer"><span>{t("反馈问题")}</span><small>{t("提交可复现的问题与建议")}</small><Icon name="external" size=15 /></a></li>
                    </ul>
                </section>
                <details class="about-shortcuts"><summary>{t("键盘快捷键")}</summary><dl class="info-list"><div><dt>"Ctrl + 1–5"</dt><dd>{t("切换页面")}</dd></div><div><dt>"Ctrl + ,"</dt><dd>{t("偏好设置")}</dd></div><div><dt>"Esc"</dt><dd>{t("关闭对话框")}</dd></div><div><dt>"Tab"</dt><dd>{t("移动焦点")}</dd></div></dl></details>
                <details class="about-licenses"><summary>{t("第三方开源许可")}</summary><pre class="license-text">{include_str!("../../../THIRD_PARTY_NOTICES.md")}</pre></details>
                <footer class="about-footer">{t("基于 StreamCap 开源项目")}</footer>
            </div>
        </div>
    }
}
