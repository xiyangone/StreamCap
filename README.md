# StreamCap 原生桌面版

StreamCap 使用 **Rust + Tauri 2 + Leptos/WASM**，当前交付目标为 **Windows x64 noFF 单 EXE**。后端解析、任务管理、配置与调度均在 Rust 进程中运行，不安装或启动 Python/Flet/Node 服务。

## 当前能力

- 51 个平台注册、原生协议解析与媒体直链录制；快手支持扫码验证后显式保存登录信息。新增平台的离线协议夹具与鉴权失败测试不等同于全部平台当前在线可用，实际访问仍受平台接口、账号权限、地区及限流影响。
- 单项/批量任务编辑、名称自动识别、配置继承、多时间窗定时录制、仅通知模式；每平台限流与错峰，手动操作也遵守限流。
- TS/FLV/MKV/MOV/MP4/NUT/WAV/MP3/WMA/M4A/AAC 录制及分段，FLV 原生直下；文件名模板、标题目录、时长/速率与磁盘空间保护。
- TS 录中/录后预览，常见网页音视频格式、其他容器的兼容预览、直播源预览和截图；播放器资源随 EXE 打包。
- 停止或自然结束后无损转封装 MP4；完整验证成功后按开关删除对应源 TS。同名文件不覆盖，失败和取消保留源文件。
- 原生通知、钉钉、企业微信、飞书、Bark、ntfy、Server酱、Telegram、SMTP 邮件，以及有超时/退出保护的录后脚本。
- 定时/倒计时关机，60 秒可取消提示；先停止录制并等待媒体收尾，不强制关闭其他应用。
- 一体化标题栏、关闭确认/退出/托盘偏好、目录选择、中文/英文切换、快捷键与页面恢复；显式更新检查及 FFmpeg 安装引导。

当前只维护 Rust 原生版。旧 Python/Flet、Docker Web 和旧 macOS 打包入口已退役。新增或恢复监控的直播间会先检测一次，后续各自按配置间隔检查，并遵守平台节流。浏览器刷新不触发直播检测，以免因反复刷新造成平台限流。

## 运行要求

- Windows 10/11 x64，系统 Microsoft Edge WebView2 Runtime，以及 Microsoft Visual C++ v14 x64 运行库（VCRUNTIME140.dll / VCRUNTIME140_1.dll）。
- noFF 不捆绑 FFmpeg；普通录制使用用户数据目录下的 FFmpeg，其次 PATH。转 MP4、时间字幕和兼容预览还需要 FFmpeg/同目录 ffprobe。FLV 直下不启动 FFmpeg，但必须选择 FLV 并关闭分段。
- 设置中的安装工具会显式下载 Gyan Windows 包、验证 SHA-256，并仅提取 FFmpeg/ffprobe 到应用数据目录；不改 PATH、不覆盖已有工具。
- 运行发行 EXE 不需要 Python、Node、Rust 开发工具。WebView2 和录制时的 FFmpeg 子进程属于正常运行依赖。

用户数据默认位于 %APPDATA%\StreamCap，已有任务、Cookie、账号与设置不会被发行文件覆盖。下载位置遵循用户设置。

## 预览与转 MP4

在偏好设置中开启“录制结束转 MP4”，只影响之后启动的 TS 录制。开启“转换成功后清理源 TS”后，只有音视频轨道、编码、包数、时长及完整解码验证全部通过，且已发布的 MP4 与源 TS 身份仍一致，才清理本次源 TS。清理被锁定时保留 MP4 与 TS，并显示原因。媒体库的手动转换默认保留源 TS，不继承自动清理开关。

TS 与兼容格式支持按时间跳播，录制过程中也能回看已录部分，并返回最新位置。ffprobe 读取已录时长，FFmpeg 从目标时间生成最长 120 秒的预览片段；连续播放时自动衔接，不先下载整段录像或生成完整 MP4。关闭预览会取消并等待工具进程退出，不改原文件。直播源缓存有效期 5 分钟，过期需手动检测，不会因预览或刷新自动增加平台请求。

退出时停止录制并等待最多 30 秒完成转封装；超时保留 TS。处理队列持久化到用户配置目录的 media_jobs.json，下次仅恢复队列中同一录制根目录、身份未改变且没有同名 MP4 的中断任务，沿用任务开始时的清理选择；不会扫描、批量转换或删除历史录像。

4500 秒属于直播检测间隔，不是录制或分段时长。时间字幕可生成同名 SRT，按真实分段时长累积墙钟时间；生成失败时本次源文件保留。

## 自动化与网络

- 定时任务支持如 08:00,23:00 与对应小时数 2,3，可跨午夜；时间窗开始触发一次受限检测，结束时停止该任务。
- 录后脚本必须是 JSON 参数数组，首项为绝对 EXE 路径，参数可用 {file}、{room}。仅处理本轮源文件或本轮转换成功的 MP4，不使用同名历史转换结果。不隐式拼接 shell；输出媒体在脚本结束前仍受文件保护，脚本失败会明确提示。Windows Job Object 负责清理脚本子进程树。
- 关机仅在应用运行时生效。到期会显示 60 秒可取消提示，收尾失败不继续提交系统关机。请先保存其他程序中的工作。
- FFmpeg 录制代理需使用 http://；SOCKS 可用于解析和 FLV 直下。不支持的组合明确报错，不偷偷直连。
- 快捷键：Ctrl+1 至 Ctrl+5 切换页面，Ctrl+, 打开设置。语言切换仅重载界面，不停止录制。

## 源码开发与构建

需要 Rust 1.95.0、MSVC C++ 构建工具、Windows SDK、Node.js 24 和 npm。下面从仓库的 desktop 目录执行：


```powershell
rustup target add wasm32-unknown-unknown
npm ci --cache .\build\npm-cache
cargo install trunk --version 0.21.14 --locked --root .\target\tools
cargo install wasm-bindgen-cli --version 0.2.128 --locked --root .\target\tools
cargo install cargo-audit --version 0.22.2 --locked --root .\target\tools
cargo fetch --manifest-path Cargo.toml --locked
cargo fetch --manifest-path core/Cargo.toml --locked
cargo fetch --manifest-path src-tauri/Cargo.toml --locked
npm run tauri:dev
```

构建脚本检查项目本地 wasm-bindgen CLI 与前端锁文件一致；缺少工具或版本不符会明确报错，不静默切换运行方案。

```powershell
# 完整验证并生成 noFF EXE（首次测试须先安装 Playwright Chromium）
$env:PLAYWRIGHT_BROWSERS_PATH = Join-Path (Get-Location) 'build/playwright'
npx playwright install chromium
npm run verify

# 仅构建新的发布目录，不覆盖已有产物
pwsh -NoProfile -File .\scripts\build-release.ps1
```

产物：desktop/src-tauri/target/native-noFF-*/StreamCap.exe 和 StreamCap.exe.sha256。不生成安装器，不打包用户配置、Cookie 或下载文件。验证包含三份 Cargo.lock 与 npm 的联网依赖审计，审计失败会阻断后续构建；报告保留已知漏洞与维护状态警告，不把两者混为一谈。CI 在同一个 Native validation 工作流中处理 push、PR 与手动运行，仅手动运行上传验证后的 EXE。详细说明见 [打包与验收](docs/packaging.md)。

## 数据与安全

- 保持现有 JSON 任务/配置契约；保留缺省继承和用户扩展字段。
- 本地 API 只监听 loopback，校验 Host/Origin；桌面命令仅授予主窗口必要权限，CSP 只允许打包资源、IPC 和本次运行的本地服务。
- 媒体文件路径不能越出录制根目录；链接不被递归跟随。正在录制的文件及其父目录不允许回收。
- 媒体库回收不可用、文件被占用或操作失败时保留原文件，不退回永久删除。自动转封装的删源是单独的显式开关，且仅针对成功验证的本次 TS。
- UI 和原生 smoke 使用隔离数据；平台自动测试使用本地夹具，不触发真实扫码或平台探测。

## 许可

项目许可见 [LICENSE](LICENSE)，移植算法与依赖声明见 [第三方许可](desktop/THIRD_PARTY_NOTICES.md)。
