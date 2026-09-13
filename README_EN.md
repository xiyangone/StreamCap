# StreamCap Native Desktop

StreamCap uses **Rust, Tauri 2 and Leptos/WASM**. The supported release target is **Windows x64 noFF**, delivered as one directly runnable executable. No Python/Flet/Node backend is installed or launched.

## Supported features

- Native Douyin, Kuaishou and direct-media resolution; verified Kuaishou QR login with explicit credential saving.
- Recording tasks, monitoring, individual/batch editing, inherited settings, media previews and safe Recycle Bin operations.
- Frameless native window controls, close confirmation, tray mode, remembered preferences and ordered shutdown.
- Live interval updates and bounded Range-enabled media streaming.

Other former platforms, scheduling/shutdown automation, notifications, post-recording transcoding and custom scripts are not implemented in this native edition. Only the Rust native pipeline is maintained for this edition; legacy Python/Flet, Docker Web and macOS entrypoints are not used.

## Runtime

Windows 10/11 x64 and the system Microsoft Edge WebView2 Runtime are required. noFF does not bundle FFmpeg: recording uses FFmpeg from the user-data directory or PATH. Node and Rust are development tools only.

User data defaults to %APPDATA%\StreamCap. Builds never overwrite tasks, cookies, account metadata, settings or downloaded recordings.

## Development

Install Rust 1.95.0 with MSVC/Windows SDK and Node.js 24. From the desktop directory:

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

The frontend helper verifies the project-local wasm-bindgen CLI against Cargo.lock. A missing or mismatched tool is an error, not an interpreter fallback.

Run npm run verify for the complete native verification/build pipeline (install Playwright Chromium first), or pwsh -NoProfile -File scripts/build-release.ps1 to build only. Output is a fresh desktop/src-tauri/target/native-noFF-* directory containing StreamCap.exe and its SHA256 file, not an installer.

See [packaging and verification](docs/packaging_en.md), [LICENSE](LICENSE) and [third-party notices](desktop/THIRD_PARTY_NOTICES.md).
