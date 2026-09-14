# 原生 noFF 打包与验收

## 唯一发布路径

当前只交付 Windows x64 的 StreamCap.exe。它内嵌前端和默认配置，不内嵌 Python、Node、FFmpeg、用户设置、账号或录制文件。运行依赖系统 WebView2 和 Microsoft Visual C++ v14 x64 运行库；常规录制需要独立的 FFmpeg，原生 FLV 直下除外。

所有命令从 desktop 目录执行。工具安装步骤见仓库 README。首次构建先用 cargo fetch --locked 为前端、core 和 src-tauri 三个 manifest 准备依赖；正式构建使用锁文件和离线模式。

```powershell
npm run verify
```

统一验收脚本执行 Rust 格式/Clippy/测试、WASM 构建、UI 与布局回归、原生 EXE 的关闭和托盘 smoke，并写入新的测试证据目录。任一必要检查失败会返回非零，不发布成功报告。

```powershell
pwsh -NoProfile -File .\scripts\build-release.ps1
```

仅构建时，脚本先调用同一个 build-frontend.ps1，再构建 Rust shell。直接运行 npm run tauri:build 也会通过 Tauri 的 beforeBuildCommand 重建前端。三个入口不依赖旧 dist。

输出目录为 desktop/src-tauri/target/native-noFF-时间戳/。已有 StreamCap.exe 不会被覆盖。交付文件是 StreamCap.exe 与 StreamCap.exe.sha256，不是安装器。

## 工具和依赖

- Rust 1.95.0、wasm32-unknown-unknown、MSVC C++ 工具与 Windows SDK。
- Node.js 24/npm 用于 Tauri API、锁定版本的 mpegts.js 本地资源和 Playwright；不会随 EXE 启动 Node。
- FFmpeg 与同目录 ffprobe 用于生成隔离媒体夹具和校验转封装；noFF 成品不捆绑这两个工具。
- Trunk 0.21.14、wasm-bindgen-cli 0.2.128 安装在 desktop/target/tools。升级 wasm-bindgen 库时必须同步项目本地 CLI，构建时会检查版本。
- Tauri 命令权限自动文件由 build.rs 生成，不手动维护；手写权限在 src-tauri/permissions/desktop.toml。

## 验收隔离

native-smoke.ps1 只接受项目 src-tauri/target 内的构建产物，使用新建的数据目录、随机 API/CDP 端口和自己的 WebView2 profile，不启动已部署的程序。源码测试使用模拟平台，本流程不重新扫码或访问真实直播间。

媒体门禁生成合成 H.264/AAC TS，检查增量预览、手动停止、自然结束、分段和退出后的转换；ffprobe 与解码校验必须通过。原生窗口必须实际播放录中的 TS 和生成的 MP4，关闭后播放器/连接/子进程不得残留。取消、损坏输入、同名输出及文件保护均有回归，不以“播放器容器显示正常”替代实际播放。

验证产物写入 desktop/tests/artifacts。保留 result.json、日志及最终截图；不能把用户目录当成测试临时目录。回收站测试仅使用新建夹具，回收失败验证必须确认原文件仍存在。

## 用户数据保护

默认用户目录为 %APPDATA%\StreamCap，下载路径由用户配置决定。--data-dir 可以显式指定独立数据目录；--api-port 可以指定本地端口，前端地址由原生启动配置注入。

不得整目录打包根 config、用户 profile、下载目录或旧备份。编译使用的默认配置只有 config/default_settings.json、config/language.json、config/version.json；保持既有任务与 Cookie 的持久化契约。不要清空系统 WebView2 数据，也不要因发布 noFF 而移除用户已有 FFmpeg。

向现有安装位置替换 EXE、提交 Git 或推送不属于构建脚本行为。

## 本轮补齐的验证范围

- 扩展平台通过注入式 HTTP 响应夹具逐分支验证；测试从不发送真实平台或登录请求。夹具覆盖不代表外站当前可用性证明。
- 全录制格式分别验证普通与分段文件；同时覆盖 TS 清理、失败保留、锁定源文件、队列重启恢复、时间字幕、FLV 原生直下及兼容/直播源预览。
- UI 测试运行真正的发布 WASM，模拟账号/工具/关机端点，验证截图、删源后切换播放器、语言切换、快捷键和页面恢复。真实通知、关机及用户脚本不在自动验收中执行。
- Windows 脚本进程树门禁仅启动合成的 pwsh 睡眠进程，校验关闭 Job Object 后父子进程均退出，不执行用户脚本。
- 原生关闭/托盘 smoke 使用新数据目录。noFF 只保留一条 Rust 后端路径，不启动 Python 或 Node。
- 依赖风险核对可使用项目内 cargo-audit（0.22.2）与 npm audit；保留审计数据库版本和报告，不以编译成功替代漏洞检查。
