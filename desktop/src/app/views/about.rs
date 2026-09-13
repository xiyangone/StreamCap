use crate::api::gateway;
use crate::app::components::Icon;
use leptos::prelude::*;

#[component]
pub fn AboutView() -> impl IntoView {
    let state = gateway::app_state();
    view! {
        <div class="page about-page"><header class="page-header"><div><h1>"关于 StreamCap"</h1><p>"记录直播，珍藏热爱。"</p></div></header>
            <section class="about-hero glass"><div class="about-brand-mark"><img src="/logo-mark.png" alt="StreamCap 熊猫标识" /></div><span class="eyebrow">"YOUR LIVE RECORDING WORKSPACE"</span><h2>"StreamCap"</h2><p>"抖音、快手与媒体直链录制，文件保存在本地。"</p><span class="version-badge">"桌面版 "{env!("CARGO_PKG_VERSION")}</span></section>
            <div class="about-grid"><section class="about-detail glass"><h3><Icon name="monitor" size=19 />"应用信息"</h3><dl class="info-list"><div><dt>"运行平台"</dt><dd>"Windows 桌面版"</dd></div><div><dt>"本地服务版本"</dt><dd>{move || if state.status.get().version.is_empty() { "未连接".into() } else { state.status.get().version }}</dd></div><div><dt>"本地解析组件"</dt><dd>{move || if state.status.get().resolver_ready { "已就绪" } else { "未就绪" }}</dd></div><div><dt>"许可协议"</dt><dd>"Apache License 2.0"</dd></div></dl></section>
                <section class="about-detail glass"><h3><Icon name="link" size=19 />"项目与支持"</h3><a class="resource-link" href="https://github.com/xiyangone/StreamCap" target="_blank" rel="noopener noreferrer"><span><strong>"项目主页"</strong><small>"源代码与版本信息"</small></span><Icon name="external" size=17 /></a><a class="resource-link" href="https://github.com/ihmily/StreamCap/wiki" target="_blank" rel="noopener noreferrer"><span><strong>"使用文档"</strong><small>"平台支持与操作指南"</small></span><Icon name="external" size=17 /></a><a class="resource-link" href="https://github.com/xiyangone/StreamCap/issues" target="_blank" rel="noopener noreferrer"><span><strong>"反馈问题"</strong><small>"提交可复现的问题与建议"</small></span><Icon name="external" size=17 /></a></section>
            </div><footer class="about-footer"><span>"基于 StreamCap 开源项目"</span><span>"感谢每一位贡献者"</span></footer>
            <details class="import-help"><summary>"第三方开源许可"</summary><pre style="white-space:pre-wrap;font-size:11px;padding-top:12px">{include_str!("../../../THIRD_PARTY_NOTICES.md")}</pre></details>
        </div>
    }
}
