# Native packaging with bundled FFmpeg

The only current release target is a Windows x64 StreamCap.exe. It embeds frontend assets, shipped defaults and a compressed FFmpeg/ffprobe payload, not Python, Node or user data. WebView2 and the Microsoft Visual C++ v14 x64 runtime are system prerequisites. Recording tools are verified and extracted at startup without runtime downloads or PATH configuration.

From desktop, run npm run verify for the complete verification/build pipeline, or pwsh -NoProfile -File scripts/build-release.ps1 for a build only. The release script, npm build and the Tauri build hook share build-frontend.ps1, so they cannot silently package stale frontend assets.

Dependencies must first be fetched with cargo fetch --locked for the frontend, core and src-tauri manifests. Builds use locked/offline mode. Required tools are Rust 1.95.0 with wasm32-unknown-unknown/MSVC/Windows SDK, Node.js 24, bundled mpegts.js 1.8.2, Trunk 0.21.14 and wasm-bindgen-cli 0.2.129. Install Trunk and wasm-bindgen into desktop/target/tools; the helper verifies the CLI version against the frontend Cargo.lock.

Output is a new desktop/src-tauri/target/native-withFF-* directory containing StreamCap.exe and StreamCap.exe.sha256. Existing release files are never overwritten; no installer is produced.

`ffmpeg-bundle.json` pins Gyan FFmpeg 8.1.2, its archive URL/hash, and the names, sizes and hashes of FFmpeg, ffprobe, LICENSE and README.txt. `prepare-ffmpeg.ps1` downloads only on the build machine, verifies all files and creates `build/ffmpeg-bundle/<SHA-256>.bundle.zip` plus a relative `embedded.rs` include. A complete cache supports `-Offline`; corrupt cached files stop the build. The same preparation entry is used by frontend, release and verification scripts.

Startup uses `%APPDATA%\StreamCap\runtime\ffmpeg\<SHA-256>`, or the explicitly selected profile. It verifies existing files, serializes concurrent extraction and publishes only a complete verified directory. It does not overwrite user-managed tools, enable partial output or search PATH. Cache errors identify the exact directory to handle before restarting; the entire profile must not be cleared. GPL v3 LICENSE and the upstream source/build README are embedded alongside both tools.

## Verification boundaries

The pipeline runs Rust format, dependency audits, Clippy, tests, WASM release compilation, UI/layout regression and actual native close/tray/shutdown smoke. Tauri tests and Clippy follow this run's frontend and candidate build so frontendDist exists in a fresh checkout. Every later gate must pass before a candidate is uploaded as verified. A failed gate returns nonzero. Native smoke copies a candidate into a fresh path containing Chinese characters and spaces under src-tauri/target, then launches it with a different working directory, an empty PATH, a fresh Unicode profile and random loopback ports. It checks branding, styles, extracted hashes, the recording process path and relative recording paths. No production profile, real QR login or live platform request is used.

Install cargo-audit 0.22.2 into desktop/target/tools. Verification updates desktop/target/advisory-db online and audits all three lockfiles against the same revision; npm audit also requires network access. JSON reports retain the database commit, vulnerabilities and maintenance warnings. Vulnerabilities block the gate; maintenance warnings remain visible without advisory ignore rules. Playwright defaults to desktop/build/playwright unless PLAYWRIGHT_BROWSERS_PATH is explicitly set; use the same path when installing Chromium. A preliminary isolated headless launch checks browser availability.

Test evidence is stored under desktop/tests/artifacts. Preserve logs, result JSON and final visual evidence. Recycle Bin checks use new temporary fixture files, including a locked-file test proving that a failed recycle does not fall back to permanent deletion.

Isolated smoke mode requests a dynamic loopback debugging port through the WebView2 API. The script reads DevToolsActivePort from the fresh profile and verifies listener ancestry and process creation identity. It does not rely on environment-based WebView2 debug overrides under elevation. Normal startup does not enable this endpoint; failed diagnostics retain the port and process evidence.

Media gates generate synthetic H.264/AAC TS locally. They exercise growing preview, manual/natural/segmented completion, shutdown, cancellation, output conflicts and source protection. ffprobe and decode checks validate MP4 outputs. Native smoke must actually decode live TS and converted MP4 with tools extracted from the candidate EXE, then release players, readers and processes. The API preview regression also extracts the prepared payload before use and checks that an equivalent relative recording path is allowed while recording, whereas an actual root change is rejected.

## Data protection

The normal user-data location is %APPDATA%\StreamCap. Default recordings go to downloads beside the EXE; relative recording preferences use the same base. An absolute --data-dir selects a profile that instead becomes the base for default and relative recordings. Saving settings, guarding a root change and reading media use the same resolution rules. Absolute preferences and historical file paths are not migrated automatically; drive-relative forms such as C:captures are rejected. --api-port selects a local API port only in isolated smoke mode; frontend transport is initialized from the bound native address.

The only embedded recording configuration is config/default_settings.json. Language and version data come from the native UI; legacy metadata is no longer seeded, and existing user copies are left untouched. Never package an entire user config/profile/download/backup directory. Keep the existing task and credential JSON contracts. Do not clear system WebView2 data or remove existing FFmpeg.

Custom command ACL is generated by build.rs with handwritten permissions in src-tauri/permissions/desktop.toml. The native verification gate requires the former Python/Flet/Docker entrypoints to be absent. Git commits, pushes and deployment overwrites are not performed by the build scripts.

## Extended regression coverage

Platform fixtures inject per-protocol HTTP responses; no production platform or login endpoint is contacted. This is protocol regression evidence, not certification of current external availability.

Media gates exercise each recording format with and without segmentation, verified cleanup, failure/locking, queue recovery, timestamp subtitles, native FLV downloading, compatibility previews and cached live-source previews. Script regressions cover reused filenames, failed or disabled conversion, and partially successful segmented conversions; only files from the current recording are passed to scripts. UI tests run the real release WASM with mocked account/tool/shutdown APIs and verify account drafts and secret handling during in-flight requests, release links, screenshots, player switching after cleanup, language, shortcuts and page restoration. They send no real notifications, power actions or user scripts.

The retired-entry gate also rejects old fonts, macOS icons, screenshots and static language/version metadata. Brand SVG masters, current runtime assets and license notices remain. To select a candidate directory, run `scripts/verify-native.ps1 -OutputDirectory <absolute-directory>`; it must be inside desktop/src-tauri/target and cannot overwrite an existing candidate EXE.

The Windows process-tree gate starts only a synthetic sleeping pwsh fixture and proves its owned descendants terminate with the Job Object. Settings regressions cover out-of-order reads, serialized appearance writes and unsaved drafts. Load regression updates five recordings among 100, 250 and 500 tasks, enforcing a one-second batch budget without replacing card nodes.

.github/workflows/test.yml is the single CI entry for pushes, PRs and workflow_dispatch. The bundle manifest participates in cache keys and cached payloads are rechecked. All triggers run the same verification and upload evidence; only successful manual runs upload StreamCap-windows-x64-withFF. Bundled licensing and source details are recorded in the extracted LICENSE/README.txt and desktop/THIRD_PARTY_NOTICES.md.

Time-seek validation uses a synthetic red/blue TS larger than 600 MiB, seeks forward and backward, and decodes the first returned frame. Native smoke drags the growing-recording timeline, returns to latest, and verifies recording remains active; changing only the displayed time is not a pass.

## Network, media and real-target validation

Platform transports are pooled by domain, proxy, credential identity, timeout and redirect policy (32 entries, five-minute idle expiry). QR login sessions remain separate. Native TLS enables ALPN without forcing HTTP/2. FFmpeg and ffprobe do not inherit proxy environment variables; disabling the application proxy still follows OS routing, including TUN. An OS route alone does not identify the final proxy outbound node.

Native FLV downloads follow at most five safe redirects, reject HTTPS downgrade and obvious public-to-loopback pivots, and strip credentials on cross-origin redirects. Directory listings return shallow metadata; size requests are asynchronous, bounded and cancelled when the user navigates away. The size cache is bounded to 64 entries and 30 seconds. Unknown sizes are not represented as zero.

Media metadata includes codec capability. Unsupported MP4 codecs use the existing bounded compatibility stream; a failed native decode switches at most once, and playback events cannot overwrite a decoding error. The full media-processing queue displays stage progress and elapsed time. Estimates describe a measured stage, not the entire queue. Source cleanup retains its existing validation.

Native logs rotate at approximately 4 MiB, retaining the current file and four archives. Startup preserves history and rotation rejects links. Tauri is updated to 2.12.0. Dependency warnings remain visible, with separate Windows-target reachability evidence; unavailable upstream fixes are not claimed as resolved.

For explicit real testing, run scripts/verify-real.ps1 with -Executable (a SHA256-verified project candidate), -ProfileDirectory (read only), -MediaRoot (read only), and -MpvPath (project-local benchmark component). It samples at most seven monitored rooms, obeys per-platform pacing and stops restricted platforms. Credentials remain in memory. No login, CAPTCHA interaction, or user recording task is started. A real preview is bounded to 15 seconds or 512 KiB. An available FLV can additionally be tested through a local relay with one controlled interruption, at most two upstream requests and about 4 MiB per request, writing only isolated output. Optional -ProxyConfig reads local sing-box settings and records only matching test-process route decisions, never controller credentials.

The real-media runner uses a fresh profile, random API/CDP ports, listener ancestry verification, and source hashes. The mpv control uses CPU decoding with null video output; it does not validate libmpv embedding or GPU presentation and is not shipped with the application. verify-native.ps1 -IncludeReal accepts the same explicit paths and treats real-test gaps as non-success. Keep evidence and candidate outputs; remove only explicitly identified temporary files.


## September 27, 2026 acceptance contract

Local events stay on one SSE connection with five-second heartbeats, a twenty-second watchdog and snapshot recovery; lag drops the stale broadcast backlog before resync. Switching to WebSocket does not provide upstream platform start notifications. Room detection retains user-configured intervals and platform backoff.

Kuaishou uses native HTTP resolution and explicit QR login only. Business response 400002 means the current room content could not be read; it is neither HTTP 429 nor evidence that the room is offline. Manual CAPTCHA/browser-inspection windows, browser-cookie writeback and page-driven task recovery have been removed. Real HTTP rate limits still pause requests, and credential edits or login-session resets do not clear cooldowns. Access errors remain visible, QR login still requires explicit credential saving, and existing credentials or historical profiles are not deleted.

Completed-file compatibility playback keeps one backpressured producer across the former 120-second boundary. Read-ahead targets thirty seconds and backward buffers are trimmed between thirty and sixty seconds, with possible network-block/decoder overshoot. Growing files still resume after the probed snapshot tail. Cold seeks retain decoder startup costs; arbitrary zero-latency seeking is not promised. Original media remains read-only.

Both verification scripts accept -ArtifactsDirectory pointing to a fresh project tests/artifacts directory. Offline gates and real-service gaps remain separate. Root StreamCap.exe and its SHA-256 are candidate deliverables, ignored by Git and never automatically deployed. Retain logs, reports, screenshots and candidates; only confirmed temporary targets move to de for manual cleanup.

Committed room changes queue one paced check for monitored tasks, just like resuming monitoring. Cosmetic edits do not trigger extra platform requests. Normal checks retain the user interval: 4500 seconds means 75 minutes, not push-based detection. Paused, scheduled and notification-only behavior is preserved.

Recording and library previews share one application-level panel. Task cards and list rows expose Preview and Mini player directly, outside the overflow menu; active tasks on the overview also open the mini player directly. The floating player can switch between recordings and the existing live source, expand or close without starting a stream for every task. Navigation and expansion retain the decoder, position, pause and volume state. Closing releases preview resources without stopping recording. Native MP4 startup uses the actual first buffered timestamp rather than waiting at an unplayable zero timestamp.

The overview has no right-hand machine-status, detection-queue or recording-policy panel; the library has no right-hand processing queue or history panel. Both use full-width content. Background queues and persistence are unchanged, and task cards retain processing progress and errors. Manual MP4 conversion from a library file row always keeps the source TS, regardless of the automatic cleanup preference.

## 2026-09-27 dependency update batch

This batch pins Tauri/API/CLI 2.12.0, tauri-build 2.7.0, dialog 2.8.0, notification 2.5.0, Leptos 0.8.21, Router 0.8.16, wasm-bindgen and its project CLI 0.2.129, js-sys/web-sys 0.3.106, and rquickjs 0.14.0. Transitive resolutions are recorded in the three Cargo.lock files. The reqwest 0.12 TLS path, user-directory behavior and monitoring policy are unchanged. Downloads use a project-local Cargo cache and tools stay in desktop/target/tools; system PATH is unchanged. New candidates require the complete verification gate without overwriting existing candidates or the running installation.

## 2026-09-28 runtime improvements

- Recording previews follow the latest task state by ID. Deletion closes the task preview and cancels file requests; ordinary state updates retain the player. Independent library previews are unaffected by unrelated task deletion.
- Platform checks queue before taking execution slots. New/resumed/manual checks receive bounded priority: at most three consecutive priority admissions, with aging opportunities for normal checks after 30 seconds. Platform rotation, concurrency, spacing and cooldown remain enforced. Each platform allows at most 1024 waiters; cancellation removes the ticket without disabling monitoring.
- External notifications use an in-memory queue of at most 128 pending/in-flight messages and four sender workers. Per-task delivery order is preserved, consecutive same-state events are coalesced, and overflow is reported. This is not a durable queue: shutdown cancels sends and clears pending messages.
- Media sorting supports name, modification time and size in both directions, remembered within the desktop profile. Directories remain first, unknown sizes follow known values of the same kind, and asynchronous size updates do not block navigation.
- A single-target read-only Kuaishou probe distinguishes transport, business readability and target identity. Stream URLs alone do not prove a room is live. Without verified conflicting live data, state precedence and saved cookies are unchanged.
