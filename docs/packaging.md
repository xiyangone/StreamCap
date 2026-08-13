# StreamCap 打包说明

本文档说明如何使用 PyInstaller 打包 StreamCap 桌面应用，以及如何准备可选的内置 FFmpeg / Node.js。

## 环境要求

- 使用目标平台本机打包：
  - macOS 包必须在 macOS 上打。
  - Windows 包必须在 Windows 上打。
- PyInstaller 不支持用 macOS 直接交叉打 Windows 包，反过来也一样。
- 先安装项目依赖，确保当前 Python 环境可以正常运行 StreamCap。

## 一键打包

在项目根目录执行：

```bash
python scripts/build.py
```

脚本会自动：

- 准备 Flet desktop 运行资源。
- 打包 `config` 下的 `default_settings.json`、`language.json`、`version.json`，以及 `locales`、`assets`。
- 打包 `streamget` 的数据文件。
- 如果存在内置 FFmpeg / Node.js，则一起打包。
- macOS 下隐藏外层 PyInstaller Dock 图标，只显示一个 StreamCap 熊猫图标。

> `config` 只打包上述三个随版本分发的默认配置，不能整目录打包。
> 从源码运行时用户数据目录就是仓库根目录，程序会在 `config/` 下生成
> `recordings.json`、`cookies.json`、`accounts.json`、`user_settings.json`、
> `web_auth.json` 等用户数据。这些文件都已 gitignore，`git status` 看不出来，
> 但整目录打包会把它们带进安装包，安装后覆盖使用者的真实配置。
> `app/core/runtime/paths.py` 另有一层防御：这几个文件只要目标已存在就不覆盖。

macOS 打包完成后产物为：

```text
dist/StreamCap.app
```

运行：

```bash
open dist/StreamCap.app
```

Windows 打包完成后产物为：

```text
dist/StreamCap/
├─ StreamCap.exe
└─ _internal/
   ├─ assets/
   ├─ config/
   ├─ locales/
   └─ ...
```

Windows 使用 PyInstaller one-dir 结构：外层保留 `StreamCap.exe` 作为用户入口，运行依赖、资源文件和 DLL 放在 `_internal` 目录中。

GitHub Actions 会自动把上传的 artifact 打成 zip，因此工作流直接上传 `dist` 下的应用目录，不再预先生成内层 zip。下载 `StreamCap-windows.zip` 后解压一次即可得到 `StreamCap` 文件夹。

## macOS 架构

默认按当前 Python 环境和系统架构打包。Apple Silicon 机器通常会打出 arm64 包。

可显式指定：

```bash
python scripts/build.py --target-arch arm64
```

不建议随意使用 `universal2`。如果 Python 或第三方 `.so` 依赖不是 universal2，PyInstaller 会报 `is not a fat binary`。

## 内置 FFmpeg

如果希望打包时携带 FFmpeg，先执行：

```bash
python scripts/download_ffmpeg.py
```

下载当前平台。下载两个平台：

```bash
python scripts/download_ffmpeg.py --platform all
```

文件会保存到：

```text
vendor/ffmpeg/macos/ffmpeg
vendor/ffmpeg/windows/ffmpeg.exe
```

脚本只提取 `ffmpeg` / `ffmpeg.exe`，不会保留 `ffplay`、`ffprobe`。

打包时 `scripts/build.py` 会自动检测这些文件；存在则打进包里。若不想打包 FFmpeg：

```bash
python scripts/build.py --no-bundle-ffmpeg
```

运行时逻辑：

- 如果系统 `PATH` 中已经有 `ffmpeg`，不会复制内置版本。
- 如果系统没有 `ffmpeg`，且包内带了 FFmpeg，则复制到用户数据目录。

目标位置：

```text
macOS:   ~/Library/Application Support/StreamCap/ffmpeg/ffmpeg
Windows: %APPDATA%\StreamCap\ffmpeg\ffmpeg.exe
```

## 内置 Node.js

如果希望打包时携带 Node.js，先执行：

```bash
python scripts/download_nodejs.py
```

下载当前平台。下载两个平台：

```bash
python scripts/download_nodejs.py --platform all
```

指定版本：

```bash
python scripts/download_nodejs.py --version 22.12.0
```

文件会保存到：

```text
vendor/node/macos/node
vendor/node/windows/node.exe
```

脚本只提取 `node` / `node.exe`，不会保留 `npm`、`npx`、headers 或 docs。

打包时 `scripts/build.py` 会自动检测这些文件；存在则打进包里。若不想打包 Node.js：

```bash
python scripts/build.py --no-bundle-node
```

运行时逻辑：

- 如果系统 `PATH` 中已经有 `node`，不会复制内置版本。
- 如果系统没有 `node`，且包内带了 Node.js，则复制到用户数据目录。
- 如果包内也没有 Node.js，首次启动时 `InstallationManager` 会弹窗，
  由 `app/scripts/node_install.py` 从 npmmirror 下载并解压，带进度提示。

目标位置：

```text
macOS:   ~/Library/Application Support/StreamCap/node/node
Windows: %APPDATA%\StreamCap\node\node.exe
```

> Node.js 只有 streamget 的 douyin / haixiu / liveme / migu 四个平台需要
> （用 execjs 执行 JS 签名）。Windows 下 `node.exe` 约 89MB，占包体近三成，
> 因此可按需用 `--no-bundle-node` 排除，交给运行时下载。

## macOS Flet 说明

StreamCap 使用 Flet 桌面模式。macOS 下 Flet 会使用 `Flet.app` 作为真正的窗口进程。

本项目打包后做了以下处理：

- 外层 `StreamCap.app` 作为后台 agent，不显示 Dock 图标。
- Flet 官方缓存不会被直接修改。
- 首次运行时会创建 StreamCap 专属 Flet 副本：

```text
~/Library/Application Support/StreamCap/flet_client/<版本>/StreamCap Flet.app
```

- 专属 Flet 副本会替换为 StreamCap 熊猫图标。

因此正常情况下 Dock 只显示一个熊猫图标。

如果升级 Flet 或图标后 macOS 仍显示旧图标，可以删除专属 Flet 缓存并重启 Dock：

```bash
rm -rf "$HOME/Library/Application Support/StreamCap/flet_client"
killall Dock
open dist/StreamCap.app
```

## 用户数据目录

打包运行时，配置、日志、FFmpeg、Node.js 等可变数据不会写入应用包或安装目录。

位置：

```text
macOS:   ~/Library/Application Support/StreamCap
Windows: %APPDATA%\StreamCap
```

Windows 默认录制保存目录为 `StreamCap.exe` 同级目录下的 `downloads`，避免大文件写入 C 盘用户数据目录。用户在设置页手动选择保存目录后，以用户设置为准。

源码运行时仍使用项目目录，方便开发调试。

## 常用命令

准备可选内置依赖：

```bash
python scripts/download_ffmpeg.py --platform all
python scripts/download_nodejs.py --platform all
```

打包：

```bash
python scripts/build.py
```

强制重新下载 Flet desktop 资源：

```bash
python scripts/build.py --refresh-flet
```

只打包应用，不内置 FFmpeg / Node.js：

```bash
python scripts/build.py --no-bundle-ffmpeg --no-bundle-node
```
