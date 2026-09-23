use crate::api::gateway;
use crate::app::components::Icon;
use crate::app::i18n::t;
use leptos::prelude::*;

#[component]
pub fn AboutView() -> impl IntoView {
    let state = gateway::app_state();
    let update = RwSignal::new(None::<gateway::UpdateCheck>);
    let update_message = RwSignal::new(String::new());
    let checking = RwSignal::new(false);
    let check = move |_| {
        checking.set(true);
        leptos::task::spawn_local(async move {
            match gateway::check_update().await {
                Ok(value) => {
                    let _ = update.try_set(Some(value));
                    let _ = update_message.try_set(t("已读取最新发行版").into());
                }
                Err(error) => {
                    let _ = update_message.try_set(error);
                }
            }
            let _ = checking.try_set(false);
        });
    };
    view! {
        <div class="page about-page"><header class="page-header"><div><h1>{t("关于 StreamCap")}</h1><p>{t("记录直播，珍藏热爱。")}</p></div></header>
            <section class="about-hero glass"><div class="about-brand-mark"><img src="/logo-mark.png" alt=t("StreamCap 熊猫标识") /></div><span class="eyebrow">"YOUR LIVE RECORDING WORKSPACE"</span><h2>"StreamCap"</h2><p>{t("多平台直播与媒体直链录制，文件保存在本地。")}</p><span class="version-badge">{t("桌面版 ")}{env!("CARGO_PKG_VERSION")}</span></section>
            <div class="about-grid"><section class="about-detail glass"><h3><Icon name="monitor" size=19 />{t("应用信息")}</h3><dl class="info-list"><div><dt>{t("运行平台")}</dt><dd>{t("Windows 桌面版")}</dd></div><div><dt>{t("本地服务版本")}</dt><dd>{move || if state.status.get().version.is_empty() { t("未连接").into() } else { state.status.get().version }}</dd></div><div><dt>{t("构建标识")}</dt><dd class="break-anywhere">{move || if state.status.get().build_id.is_empty() { t("未连接").into() } else { state.status.get().build_id }}</dd></div><div><dt>{t("本地解析组件")}</dt><dd>{move || if state.status.get().resolver_ready { t("已就绪") } else { t("未就绪") }}</dd></div><div><dt>{t("许可协议")}</dt><dd>"Apache License 2.0"</dd></div></dl></section>
                <section class="about-detail glass"><h3><Icon name="link" size=19 />{t("项目与支持")}</h3><a class="resource-link" href="https://github.com/xiyangone/StreamCap" target="_blank" rel="noopener noreferrer"><span><strong>{t("项目主页")}</strong><small>{t("源代码与版本信息")}</small></span><Icon name="external" size=17 /></a><a class="resource-link" href="https://github.com/ihmily/StreamCap/wiki" target="_blank" rel="noopener noreferrer"><span><strong>{t("使用文档")}</strong><small>{t("平台支持与操作指南")}</small></span><Icon name="external" size=17 /></a><a class="resource-link" href="https://github.com/xiyangone/StreamCap/issues" target="_blank" rel="noopener noreferrer"><span><strong>{t("反馈问题")}</strong><small>{t("提交可复现的问题与建议")}</small></span><Icon name="external" size=17 /></a></section>
            </div><footer class="about-footer"><span>{t("基于 StreamCap 开源项目")}</span><span>{t("感谢每一位贡献者")}</span></footer>
            <section class="about-detail glass"><h3>{t("版本检查")}</h3><p>{t("仅检查当前项目发行版，不自动下载或替换程序。")}</p><button class="button secondary" type="button" disabled=move || checking.get() on:click=check>{move || if checking.get() { t("检查中…") } else { t("检查更新") }}</button><p role="status">{move || update_message.get()}</p><Show when=move || update.get().is_some()><a class="resource-link" href=move || update.get().map(|value| value.release.html_url).unwrap_or_default() target="_blank" rel="noopener noreferrer">{move || update.get().map(|value| value.release.tag_name).unwrap_or_default()}</a></Show><p class="field-hint">{t("快捷键：Ctrl+1 至 Ctrl+5 切换页面，Ctrl+, 打开设置。")}</p></section>
            <details class="import-help"><summary>{t("第三方开源许可")}</summary><pre style="white-space:pre-wrap;font-size:11px;padding-top:12px">{include_str!("../../../THIRD_PARTY_NOTICES.md")}</pre></details>
        </div>
    }
}
