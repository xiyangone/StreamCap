# StreamCap Native Desktop

StreamCap uses **Rust, Tauri 2 and Leptos/WASM**. The supported release target is **Windows x64 noFF**, delivered as one directly runnable executable. No Python/Flet/Node backend is installed or launched.

## Supported features

- Native protocols for 51 registered platforms and direct-media URLs; Kuaishou QR login verifies the account before explicitly saving credentials. Offline protocol fixtures do not certify every platform is currently reachable: API changes, account permissions, regions and rate limits still apply.
- Individual/batch editing, automatic streamer names, inherited settings, multi-window schedules and notify-only mode. Per-platform pacing also applies to manual requests.
- TS, FLV, MKV, MOV, MP4, NUT, WAV, MP3, WMA, M4A and AAC recording/segmentation; native FLV downloading, naming templates, duration/rate reporting and free-space protection.
- In-progress/completed TS preview, common web media, FFmpeg compatibility previews, cached live-source previews and PNG screenshots. Player assets are bundled.
- Lossless TS-to-MP4 remuxing after stop or natural completion. Optional source cleanup follows track/codec/packet/duration and full-decode verification; collisions, failures and cancellation preserve source files.
- Desktop notifications, DingTalk, WeCom, Feishu, Bark, ntfy, ServerChan, Telegram and SMTP, plus bounded post-recording executable jobs.
- Scheduled/countdown shutdown with a cancellable 60-second warning; recording and processing finish before shutdown, without forcing other applications closed.
- Integrated window controls, close/tray preferences, native folder selection, Chinese/English UI, shortcuts, last-page restoration, explicit release checks and verified FFmpeg installation.

Only the native Rust pipeline is maintained. Python/Flet, Docker Web and former macOS entrypoints are retired. New or resumed monitors receive an initial check, then retain independent configured intervals and platform pacing. Refreshing the interface never triggers platform checks.

## Runtime

Windows 10/11 x64, the system Microsoft Edge WebView2 Runtime, and the Microsoft Visual C++ v14 x64 runtime (VCRUNTIME140.dll / VCRUNTIME140_1.dll) are required. noFF does not bundle FFmpeg: recording uses FFmpeg from the user-data directory or PATH. MP4 remuxing also requires ffprobe alongside FFmpeg for output validation. Native FLV downloading needs no recording subprocess but requires FLV format with segmentation disabled. The explicit Gyan installer verifies SHA-256 and installs only FFmpeg/ffprobe into application data, without modifying PATH or replacing an existing installation. Node and Rust are development tools only.

User data defaults to %APPDATA%\StreamCap. Builds never overwrite tasks, cookies, account metadata, settings or downloaded recordings.

## Media lifecycle and automation

Automatic conversion and cleanup are snapshotted when recording starts. Manual library conversion keeps TS by default. The exact source is removed only after the MP4 passes track, codec, packet-count, duration and full-decode checks and is published under protection. Failed cleanup keeps both files and reports the problem; existing MP4 files are never replaced.

TS and compatibility previews support time-based seeking, including already-recorded portions of a growing recording and a return-to-latest control. ffprobe reads the recorded duration; FFmpeg seeks directly and streams at most 120 seconds per preview segment, continuing as needed without downloading the entire recording or creating a full MP4. Preview processes are cancelled and joined on close. Live-source cache entries expire after five minutes; preview never starts an implicit platform request. Timestamp SRT files use actual segment durations; subtitle failure keeps source media.

Shutdown waits up to 30 seconds for remux work. Interrupted jobs are persisted in media_jobs.json and may resume only in the same recordings root with unchanged file identities and no existing destination, retaining the original cleanup preference. Historical recordings are never scanned or converted automatically. A 4500-second setting is a platform-check interval, not a recording duration.

Schedules support multiple start times, fractional hours and overnight windows. Post-recording scripts must be JSON argv arrays beginning with an absolute executable path. {file} and {room} are argument substitutions, never shell fragments. Scripts receive only this recording's source files or newly converted MP4s, never historical conversions with the same filename. Media remains protected until scripts finish, and Windows Job Objects own script descendants. Shutdown timers work while the app is running, with a 60-second cancellable warning before stopping and finalizing recordings; failures prevent a power action.

FFmpeg recording requires an http:// proxy; SOCKS is available for resolution and native FLV downloading. Unsupported combinations fail instead of silently connecting directly. Ctrl+1 through Ctrl+5 navigate, and Ctrl+, opens Preferences. Language changes reload only the UI.

## Development

Install Rust 1.95.0 with MSVC/Windows SDK and Node.js 24. From the desktop directory:

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

The frontend helper verifies the project-local wasm-bindgen CLI against Cargo.lock. A missing or mismatched tool is an error, not an interpreter fallback.

Before UI verification, set $env:PLAYWRIGHT_BROWSERS_PATH = Join-Path (Get-Location) 'build/playwright' and run npx playwright install chromium. Run npm run verify for the complete native verification/build pipeline, or pwsh -NoProfile -File scripts/build-release.ps1 to build only. Output is a fresh desktop/src-tauri/target/native-noFF-* directory containing StreamCap.exe and its SHA256 file, not an installer.

Verification requires online dependency audits for all three Cargo.lock files and npm; audit failures block the build. Reports retain vulnerability findings and maintenance warnings separately. One Native validation CI workflow handles pushes, PRs and manual runs; only a successful manual run uploads the verified EXE.

See [packaging and verification](docs/packaging_en.md), [LICENSE](LICENSE) and [third-party notices](desktop/THIRD_PARTY_NOTICES.md).
