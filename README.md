# StreamCap 原生桌面版

StreamCap 使用 **Rust + Tauri 2 + Leptos/WASM**，当前交付目标为 **Windows x64 noFF 单 EXE**。后端解析、任务管理、配置与调度均在 Rust 进程中运行，不安装或启动 Python/Flet/Node 服务。

## 当前能力

- 原生解析：抖音、快手、媒体直链；快手支持扫码验证后显式保存登录信息。
- 任务单项/批量编辑、监控、录制、配置继承、媒体预览与回收站操作。
- 原生标题栏、关闭确认、最小化到托盘、记住关闭偏好和退出收尾。
- 检测间隔保存后立即重新计时；媒体预览使用有界文件流和 HTTP Range。

尚未迁移的平台、定时录制/关机、消息推送、自动转码和自定义脚本不属于当前可用能力。界面不会把这些功能显示成已生效。当前维护与交付入口仅限 Rust 原生版；旧 Python/Flet、Docker Web 和旧 macOS 打包入口不再用于本版本。

## 运行要求

- Windows 10/11 x64，系统 Microsoft Edge WebView2 Runtime。
- noFF 不捆绑 FFmpeg；录制需要已有的 FFmpeg，优先使用用户数据目录下的 FFmpeg，其次 PATH。
- 运行发行 EXE 不需要 Python、Node、Rust 开发工具。WebView2 和录制时的 FFmpeg 子进程属于正常运行依赖。

用户数据默认位于 %APPDATA%\StreamCap，已有任务、Cookie、账号与设置不会被发行文件覆盖。下载位置遵循用户设置。

## 源码开发与构建

需要 Rust 1.95.0、MSVC C++ 构建工具、Windows SDK、Node.js 24 和 npm。下面从仓库的 desktop 目录执行：


```powershell
rustup target add wasm32-unknown-unknown
npm ci --cache .\build\npm-cache
cargo install trunk --version 0.21.14 --locked --root .\target\tools
cargo install wasm-bindgen-cli --version 0.2.128 --locked --root .\target\tools
cargo fetch --manifest-path Cargo.toml --locked
cargo fetch --manifest-path core/Cargo.toml --locked
cargo fetch --manifest-path src-tauri/Cargo.toml --locked
npm run tauri:dev
```

构建脚本检查项目本地 wasm-bindgen CLI 与前端锁文件一致；缺少工具或版本不符会明确报错，不静默切换运行方案。

```powershell
# 完整验证并生成 noFF EXE（首次测试须先安装 Playwright Chromium）
npx playwright install chromium
npm run verify

# 仅构建新的发布目录，不覆盖已有产物
pwsh -NoProfile -File .\scripts\build-release.ps1
```

产物：desktop/src-tauri/target/native-noFF-*/StreamCap.exe 和 StreamCap.exe.sha256。不生成安装器，不打包用户配置、Cookie 或下载文件。详细说明见 [打包与验收](docs/packaging.md)。

## 数据与安全

- 保持现有 JSON 任务/配置契约；保留缺省继承和用户扩展字段。
- 本地 API 只监听 loopback，校验 Host/Origin；桌面命令仅授予主窗口必要权限，CSP 只允许打包资源、IPC 和本次运行的本地服务。
- 媒体文件路径不能越出录制根目录；链接不被递归跟随。正在录制的文件及其父目录不允许回收。
- 回收不可用、文件被占用或操作失败时保留原文件，不退回永久删除。
- UI 和原生 smoke 使用隔离数据；平台自动测试使用本地夹具，不触发真实扫码或平台探测。

## 许可

项目许可见 [LICENSE](LICENSE)，移植算法与依赖声明见 [第三方许可](desktop/THIRD_PARTY_NOTICES.md)。
