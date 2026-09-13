# 原生 noFF 打包与验收

## 唯一发布路径

当前只交付 Windows x64 的 StreamCap.exe。它内嵌前端和默认配置，不内嵌 Python、Node、FFmpeg、用户设置、账号或录制文件。运行依赖系统 WebView2；实际录制仍需要 FFmpeg。

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
- Node.js 24/npm 用于 Tauri API 资源和 Playwright；不会随 EXE 启动 Node。
- Trunk 0.21.14、wasm-bindgen-cli 0.2.128 安装在 desktop/target/tools。升级 wasm-bindgen 库时必须同步项目本地 CLI，构建时会检查版本。
- Tauri 命令权限自动文件由 build.rs 生成，不手动维护；手写权限在 src-tauri/permissions/desktop.toml。

## 验收隔离

native-smoke.ps1 只接受项目 src-tauri/target 内的构建产物，使用新建的数据目录、随机 API/CDP 端口和自己的 WebView2 profile，不启动已部署的程序。源码测试使用模拟平台，本流程不重新扫码或访问真实直播间。

验证产物写入 desktop/tests/artifacts。保留 result.json、日志及最终截图；不能把用户目录当成测试临时目录。回收站测试仅使用新建夹具，回收失败验证必须确认原文件仍存在。

## 用户数据保护

默认用户目录为 %APPDATA%\StreamCap，下载路径由用户配置决定。--data-dir 可以显式指定独立数据目录；--api-port 可以指定本地端口，前端地址由原生启动配置注入。

不得整目录打包根 config、用户 profile、下载目录或旧备份。编译使用的默认配置只有 config/default_settings.json、config/language.json、config/version.json；保持既有任务与 Cookie 的持久化契约。不要清空系统 WebView2 数据，也不要因发布 noFF 而移除用户已有 FFmpeg。

向现有安装位置替换 EXE、提交 Git 或推送不属于构建脚本行为。
