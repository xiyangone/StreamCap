//! ffmpeg 录制引擎：命令构造与进程生命周期管理。
//! 参数与 Python `app/core/media/ffmpeg_builders/` 对齐，保证录制产物一致。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

pub const SUPPORTED_RECORD_FORMATS: &[&str] = &[
    "TS", "FLV", "MKV", "MOV", "MP4", "NUT", "WAV", "MP3", "WMA", "M4A", "AAC",
];
pub const DEFAULT_RW_TIMEOUT: &str = "15000000";
pub const DEFAULT_ANALYZEDURATION: &str = "20000000";
pub const DEFAULT_PROBESIZE: &str = "10000000";
pub const DEFAULT_BUFSIZE: &str = "8000k";
pub const DEFAULT_MAX_MUXING_QUEUE: &str = "1024";

pub const OVERSEAS_RW_TIMEOUT: &str = "50000000";
pub const OVERSEAS_ANALYZEDURATION: &str = "40000000";
pub const OVERSEAS_PROBESIZE: &str = "20000000";
pub const OVERSEAS_BUFSIZE: &str = "15000k";
pub const OVERSEAS_MAX_MUXING_QUEUE: &str = "2048";

pub const FFMPEG_USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 11; SAMSUNG SM-G973U) AppleWebKit/537.36 (KHTML, like Gecko) SamsungBrowser/14.2 Chrome/87.0.4280.141 Mobile Safari/537.36";

/// HLS 媒体分片扩展名与容器不一致时需要放宽检查的平台。
pub const RELAXED_HLS_EXTENSION_CHECK_PLATFORMS: &[&str] = &["chzzk"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordOptions {
    pub record_url: String,
    pub save_path: String,
    pub format: String,
    #[serde(default)]
    pub segment_record: bool,
    #[serde(default)]
    pub segment_time: Option<String>,
    #[serde(default)]
    pub headers: Option<String>,
    #[serde(default)]
    pub proxy: Option<String>,
    #[serde(default)]
    pub platform_key: Option<String>,
    #[serde(default)]
    pub video_bitrate: Option<i64>,
    #[serde(default)]
    pub is_overseas: bool,
}

/// 构造 ffmpeg 命令行（不含可执行文件路径）。
pub fn build_command(options: &RecordOptions) -> Vec<String> {
    let (rw_timeout, analyzeduration, probesize, bufsize, max_muxing_queue) = if options.is_overseas
    {
        (
            OVERSEAS_RW_TIMEOUT,
            OVERSEAS_ANALYZEDURATION,
            OVERSEAS_PROBESIZE,
            OVERSEAS_BUFSIZE,
            OVERSEAS_MAX_MUXING_QUEUE,
        )
    } else {
        (
            DEFAULT_RW_TIMEOUT,
            DEFAULT_ANALYZEDURATION,
            DEFAULT_PROBESIZE,
            DEFAULT_BUFSIZE,
            DEFAULT_MAX_MUXING_QUEUE,
        )
    };

    let http_input =
        options.record_url.starts_with("http://") || options.record_url.starts_with("https://");
    let rtmp_input =
        options.record_url.starts_with("rtmp://") || options.record_url.starts_with("rtmps://");
    let protocols = if http_input {
        "http,https,tcp,tls,crypto,httpproxy"
    } else if rtmp_input {
        "rtmp,rtmps,tcp,tls"
    } else {
        "file,crypto"
    };
    let mut command: Vec<String> = vec![
        "-n",
        "-v",
        "verbose",
        "-rw_timeout",
        rw_timeout,
        "-loglevel",
        "error",
        "-hide_banner",
        "-nostats",
        "-progress",
        "pipe:1",
        "-stats_period",
        "2",
        "-protocol_whitelist",
        protocols,
        "-thread_queue_size",
        "1024",
        "-analyzeduration",
        analyzeduration,
        "-probesize",
        probesize,
        "-fflags",
        "+discardcorrupt+igndts",
        "-re",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    if let Some(key) = &options.platform_key {
        if RELAXED_HLS_EXTENSION_CHECK_PLATFORMS.contains(&key.as_str()) {
            command.push("-extension_picky".into());
            command.push("0".into());
        }
    }

    if http_input {
        command.extend(strings(&[
            "-user_agent",
            FFMPEG_USER_AGENT,
            "-reconnect",
            "1",
            "-reconnect_streamed",
            "1",
            "-reconnect_delay_max",
            "5",
        ]));
    }
    command.push("-i".into());
    command.push(options.record_url.clone());

    command.extend(
        [
            "-bufsize",
            bufsize,
            "-sn",
            "-dn",
            "-max_muxing_queue_size",
            max_muxing_queue,
            "-correct_ts_overflow",
            "1",
            "-avoid_negative_ts",
            "1",
            "-flush_packets",
            "1",
        ]
        .into_iter()
        .map(String::from),
    );

    // headers 与 proxy 属于输入选项：必须位于 -i 之前，否则 ffmpeg 会当作输出选项忽略。
    // 与 Python `_get_basic_ffmpeg_command` 的插入位置一致（headers 紧随 user_agent）。
    if let Some(headers) = options
        .headers
        .as_ref()
        .filter(|h| http_input && !h.is_empty())
    {
        let index = command
            .iter()
            .position(|a| a == "-protocol_whitelist" || a == "-i")
            .unwrap_or(command.len());
        command.insert(index, headers.clone());
        command.insert(index, "-headers".into());
    }
    if let Some(proxy) = options.proxy.as_ref().filter(|p| !p.is_empty()) {
        command.insert(0, proxy.clone());
        command.insert(0, "-http_proxy".into());
    }

    let bitrate_arg = options.video_bitrate.map(|bitrate| format!("{bitrate}k"));
    let video_codec: Vec<String> = match &bitrate_arg {
        Some(bitrate) => vec![
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            "veryfast".into(),
            "-b:v".into(),
            bitrate.clone(),
        ],
        None => vec!["-c:v".into(), "copy".into()],
    };

    let format = options.format.to_uppercase();
    let mut tail: Vec<String> = Vec::new();

    match format.as_str() {
        "TS" => {
            if options.segment_record {
                tail.extend(video_codec.iter().cloned());
                tail.extend(segment_args("mpegts", options));
            } else {
                tail.extend(video_codec.iter().cloned());
                tail.extend(strings(&["-c:a", "copy", "-map", "0", "-f", "mpegts"]));
                tail.extend(strings(&[
                    "-mpegts_flags",
                    "+resend_headers",
                    "-muxdelay",
                    "0",
                    "-muxpreload",
                    "0",
                ]));
            }
        }
        "MP4" => {
            if options.segment_record {
                tail.extend(video_codec.iter().cloned());
                tail.extend(strings(&["-c:a", "aac", "-map", "0", "-f", "segment"]));
                tail.extend(segment_time_args(options));
                tail.extend(strings(&[
                    "-segment_format",
                    "mp4",
                    "-reset_timestamps",
                    "1",
                    "-movflags",
                    "+frag_keyframe+empty_moov+faststart+delay_moov",
                    "-flags",
                    "global_header",
                ]));
            } else {
                tail.extend(strings(&["-map", "0"]));
                tail.extend(video_codec.iter().cloned());
                tail.extend(strings(&[
                    "-c:a",
                    "copy",
                    "-f",
                    "mp4",
                    "-movflags",
                    "+faststart",
                ]));
            }
        }
        "FLV" => {
            tail.extend(strings(&["-map", "0"]));
            tail.extend(video_codec.iter().cloned());
            tail.extend(strings(&["-c:a", "copy", "-bsf:a", "aac_adtstoasc"]));
            if options.segment_record {
                tail.extend(strings(&["-f", "segment"]));
                tail.extend(segment_time_args(options));
                tail.extend(strings(&[
                    "-segment_format",
                    "flv",
                    "-reset_timestamps",
                    "1",
                ]));
            } else {
                tail.extend(strings(&["-f", "flv"]));
            }
        }
        "MKV" => {
            tail.extend(strings(&["-flags", "global_header", "-map", "0"]));
            tail.extend(video_codec.iter().cloned());
            if options.segment_record {
                tail.extend(strings(&["-c:a", "aac", "-f", "segment"]));
                tail.extend(segment_time_args(options));
                tail.extend(strings(&[
                    "-segment_format",
                    "matroska",
                    "-reset_timestamps",
                    "1",
                ]));
            } else {
                tail.extend(strings(&["-c:a", "copy", "-f", "matroska"]));
            }
        }
        "MOV" => {
            tail.extend(video_codec.iter().cloned());
            if options.segment_record {
                tail.extend(strings(&["-c:a", "aac", "-map", "0", "-f", "segment"]));
                tail.extend(segment_time_args(options));
                tail.extend(strings(&[
                    "-segment_format",
                    "mov",
                    "-reset_timestamps",
                    "1",
                    "-movflags",
                    "+frag_keyframe+empty_moov+faststart",
                    "-flags",
                    "global_header",
                ]));
            } else {
                tail.extend(strings(&[
                    "-c:a",
                    "copy",
                    "-map",
                    "0",
                    "-f",
                    "mov",
                    "-flags",
                    "global_header",
                ]));
            }
        }
        "NUT" => {
            tail.extend(video_codec.iter().cloned());
            if options.segment_record {
                tail.extend(segment_args("nut", options));
            } else {
                tail.extend(strings(&[
                    "-c:a", "copy", "-map", "0:v?", "-map", "0:a?", "-f", "nut",
                ]));
            }
        }
        "WAV" | "MP3" | "WMA" | "M4A" | "AAC" => {
            let (codec, muxer) = match format.as_str() {
                "WAV" => ("pcm_s16le", "wav"),
                "MP3" => ("libmp3lame", "mp3"),
                "WMA" => ("wmav2", "asf"),
                "M4A" => ("aac", "mp4"),
                _ => ("aac", "adts"),
            };
            tail.extend(strings(&[
                "-vn", "-map", "0:a:0", "-c:a", codec, "-ar", "44100", "-ac", "2",
            ]));
            if format != "WAV" {
                tail.extend(strings(&["-b:a", "192k"]));
            }
            if options.segment_record {
                tail.extend(strings(&[
                    "-f",
                    "segment",
                    "-segment_format",
                    muxer,
                    "-reset_timestamps",
                    "1",
                ]));
                tail.extend(segment_time_args(options));
            } else {
                tail.extend(strings(&["-f", muxer]));
            }
        }
        _ => return Vec::new(),
    }

    command.extend(tail);
    command.push(options.save_path.clone());
    command
}

pub(crate) fn validate_recording_proxy(url: &str, proxy: Option<&str>) -> Result<(), String> {
    if let Some(proxy) = proxy.filter(|value| !value.is_empty()) {
        let proxy = reqwest::Url::parse(proxy).map_err(|_| "录制代理地址无效")?;
        if proxy.scheme() != "http" || proxy.host_str().is_none() {
            return Err("FFmpeg 录制请使用 http:// 代理；SOCKS 仅支持解析和 FLV 直下".into());
        }
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err("当前流协议不支持 HTTP 录制代理，请关闭该平台代理".into());
        }
    }
    Ok(())
}
fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn segment_time_args(options: &RecordOptions) -> Vec<String> {
    vec![
        "-segment_time".into(),
        options
            .segment_time
            .clone()
            .unwrap_or_else(|| "1800".into()),
    ]
}

fn segment_args(segment_format: &str, options: &RecordOptions) -> Vec<String> {
    let mut args = strings(&["-c:a", "copy", "-map", "0", "-f", "segment"]);
    args.extend(segment_time_args(options));
    args.extend(strings(&[
        "-segment_format",
        segment_format,
        "-reset_timestamps",
        "1",
        "-mpegts_flags",
        "+resend_headers",
        "-muxdelay",
        "0",
        "-muxpreload",
        "0",
    ]));
    args
}

/// 输出文件名：`主播名[_标题]_时间戳`，与 Python `_get_filename` 一致。
pub fn build_filename(anchor_name: &str, title: Option<&str>, timestamp: &str) -> String {
    let mut parts: Vec<String> = vec![sanitize(anchor_name)];
    if let Some(title) = title.filter(|t| !t.is_empty()) {
        // 与 Python `_clean_and_truncate_title` 一致：截断 30 字符、全角逗号转半角、去除空格
        let cleaned: String = sanitize(title)
            .chars()
            .take(30)
            .collect::<String>()
            .replace('，', ",")
            .replace(' ', "");
        if !cleaned.is_empty() {
            parts.push(cleaned);
        }
    }
    parts.push(timestamp.to_string());
    parts.join("_")
}

/// 输出目录：按平台 / 主播 / 日期分层，与 Python `_get_output_dir` 的开关一致。
#[derive(Debug, Clone, Default)]
pub struct FolderOptions {
    pub by_platform: bool,
    pub by_author: bool,
    pub by_time: bool,
}

pub fn build_output_dir(
    root: &Path,
    platform: &str,
    anchor_name: &str,
    date: &str,
    options: &FolderOptions,
) -> PathBuf {
    let mut dir = root.to_path_buf();
    if options.by_platform && !platform.is_empty() {
        dir.push(sanitize(platform));
    }
    if options.by_author && !anchor_name.is_empty() {
        dir.push(sanitize(anchor_name));
    }
    if options.by_time {
        dir.push(date);
    }
    dir
}

/// 去掉文件名中的非法字符并去除空格（与 Python 的 replace(" ", "_") 取向一致）。
pub fn sanitize(name: &str) -> String {
    let mut result: String = name
        .chars()
        .map(|c| {
            if c.is_control() || r#"<>:"/\|?*%"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .take(96)
        .collect();
    result = result.trim().trim_end_matches(['.', ' ']).to_string();
    let stem = result
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if result.is_empty()
        || stem == "CON"
        || stem == "PRN"
        || stem == "AUX"
        || stem == "NUL"
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
    {
        result.insert(0, '_');
    }
    result
}

pub fn without_emojis(value: &str) -> String {
    value.chars().filter(|c|!matches!(*c as u32,0x1F000..=0x1FAFF|0x2600..=0x27FF|0xFE00..=0xFE0F|0x200D|0x20E3)).collect()
}

/// 分段录制时 ffmpeg 需要 `_%03d` 序号占位。
pub fn with_segment_suffix(save_path: &str, format: &str, segment: bool) -> String {
    if segment {
        format!(
            "{}_%03d.{}",
            save_path.trim_end_matches('.'),
            format.to_lowercase()
        )
    } else {
        save_path.to_string()
    }
}

/// 运行中的录制进程句柄。
const FIRST_MEDIA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const STALLED_MEDIA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

struct RecordingProgress {
    started: tokio::time::Instant,
    advanced: tokio::time::Instant,
    bytes: u64,
    media_us: u64,
    frames: u64,
    has_media: bool,
    reported_time: bool,
    terminal_error: Option<&'static str>,
}
impl RecordingProgress {
    fn new() -> Self {
        let now = tokio::time::Instant::now();
        Self {
            started: now,
            advanced: now,
            bytes: 0,
            media_us: 0,
            frames: 0,
            has_media: false,
            reported_time: false,
            terminal_error: None,
        }
    }
    fn observe_line(&mut self, line: &str) {
        let Some((key, value)) = line.split_once('=') else {
            return;
        };
        let Ok(value) = value.trim().parse::<u64>() else {
            return;
        };
        let field = match key.trim() {
            "out_time_us" => {
                self.reported_time = true;
                &mut self.media_us
            }
            "frame" => &mut self.frames,
            _ => return,
        };
        if value > *field {
            *field = value;
            self.has_media = true;
            self.advanced = tokio::time::Instant::now();
        }
    }
    fn observe_bytes(&mut self, bytes: u64) {
        if bytes > self.bytes {
            self.bytes = bytes;
            self.has_media = true;
            self.advanced = tokio::time::Instant::now();
        }
    }
    fn seconds(&self) -> f64 {
        if self.reported_time {
            self.media_us as f64 / 1_000_000.0
        } else if self.has_media {
            (self.advanced - self.started).as_secs_f64()
        } else {
            0.0
        }
    }
    fn stalled(&self) -> Option<&'static str> {
        if self.has_media && self.advanced.elapsed() >= STALLED_MEDIA_TIMEOUT {
            Some("录制连续 90 秒无媒体产出，已停止并保留已录文件")
        } else if !self.has_media && self.started.elapsed() >= FIRST_MEDIA_TIMEOUT {
            Some("录制启动 120 秒未收到媒体，已停止等待")
        } else {
            None
        }
    }
}

pub struct RecorderProcess {
    pub rec_id: String,
    pub output_path: PathBuf,
    pub run_id: uuid::Uuid,
    pub started_at: std::time::Instant,
    pub wall_started_at: chrono::DateTime<chrono::Local>,
    child: Option<Arc<Mutex<Child>>>,
    direct: Option<crate::direct::Download>,
    stop_requested: AtomicBool,
    failure: Arc<Mutex<Option<String>>>,
    stderr_done: tokio_util::sync::CancellationToken,
    progress_done: tokio_util::sync::CancellationToken,
    progress: Arc<std::sync::Mutex<RecordingProgress>>,
}

impl RecorderProcess {
    /// 优雅停止：Windows 下向 stdin 写 `q` 让 ffmpeg 收尾；超时后强杀。
    pub async fn stop(&self, grace_secs: u64) {
        self.stop_requested.store(true, Ordering::SeqCst);
        self.finish(grace_secs).await;
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            self.progress_done.cancelled(),
        )
        .await;
    }
    pub fn observe_output_bytes(&self, bytes: u64) {
        self.progress
            .lock()
            .expect("recording progress")
            .observe_bytes(bytes);
    }
    pub fn recorded_seconds(&self) -> f64 {
        self.progress.lock().expect("recording progress").seconds()
    }
    async fn finish(&self, grace_secs: u64) {
        if let Some(direct) = &self.direct {
            direct.stop().await;
            return;
        }
        {
            let mut child = self.child.as_ref().expect("ffmpeg process").lock().await;
            if let Some(stdin) = child.stdin.as_mut() {
                use tokio::io::AsyncWriteExt;
                let _ = stdin.write_all(b"q").await;
                let _ = stdin.flush().await;
            }
        }

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(grace_secs);
        loop {
            {
                let mut child = self.child.as_ref().expect("ffmpeg process").lock().await;
                match child.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) => {}
                    Err(err) => {
                        log::warn!("等待 ffmpeg 退出失败: {err}");
                        return;
                    }
                }
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        let mut child = self.child.as_ref().expect("ffmpeg process").lock().await;
        if let Err(err) = child.kill().await {
            log::warn!("强制结束 ffmpeg 失败: {err}");
        }
    }
}

/// 录制进程状态查询结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessState {
    /// 仍在活动表中且进程未退出
    Running,
    /// 进程已自行退出（含退出码），已从活动表移除
    Exited {
        code: Option<i32>,
        error: Option<String>,
    },
    /// 不在活动表中（已停止或从未启动）
    Unknown,
}

/// 录制引擎：管理所有活动录制进程。
#[derive(Clone, Default)]
pub struct Engine {
    closing: Arc<std::sync::atomic::AtomicBool>,
    active: Arc<Mutex<HashMap<String, Arc<RecorderProcess>>>>,
    log_tasks: tokio_util::task::TaskTracker,
    filesystem: Arc<Mutex<()>>,
    media_reservations: Arc<std::sync::Mutex<HashMap<uuid::Uuid, Vec<PathBuf>>>>,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    /// A recycle operation holds this guard until the OS operation finishes.
    pub async fn filesystem_guard(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.filesystem.clone().lock_owned().await
    }
    pub async fn protects_path(&self, target: &Path) -> bool {
        self.active
            .lock()
            .await
            .values()
            .any(|process| output_is_protected(&process.output_path, target))
            || self
                .media_reservations
                .lock()
                .expect("media reservation lock")
                .values()
                .flatten()
                .any(|path| output_is_protected(path, target))
    }
    pub async fn is_recording_path(&self, target: &Path) -> bool {
        self.active
            .lock()
            .await
            .values()
            .any(|process| output_is_protected(&process.output_path, target))
    }
    pub async fn process(&self, rec_id: &str) -> Option<Arc<RecorderProcess>> {
        self.active.lock().await.get(rec_id).cloned()
    }
    /// Caller holds filesystem_guard while selecting files and registering this reservation.
    pub(crate) fn reserve_media(&self, paths: Vec<PathBuf>) -> MediaReservation {
        let id = uuid::Uuid::new_v4();
        self.media_reservations
            .lock()
            .expect("media reservation lock")
            .insert(id, paths);
        MediaReservation {
            id,
            reservations: self.media_reservations.clone(),
        }
    }

    pub fn begin_shutdown(&self) {
        self.closing
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub async fn is_recording(&self, rec_id: &str) -> bool {
        self.active.lock().await.contains_key(rec_id)
    }

    pub async fn active_ids(&self) -> Vec<String> {
        self.active.lock().await.keys().cloned().collect()
    }

    pub async fn is_current(&self, process: &Arc<RecorderProcess>) -> bool {
        !process.stop_requested.load(Ordering::SeqCst)
            && self
                .active
                .lock()
                .await
                .get(&process.rec_id)
                .is_some_and(|current| Arc::ptr_eq(current, process))
    }

    /// Poll the exact process that this watcher started, never a later attempt with the same task ID.
    pub async fn poll_state(&self, process: &Arc<RecorderProcess>) -> ProcessState {
        if !self.is_current(process).await {
            return ProcessState::Unknown;
        }
        let stalled = process
            .progress
            .lock()
            .expect("recording progress")
            .stalled();
        if let Some(reason) = stalled {
            let running = if let Some(direct) = &process.direct {
                direct.result.lock().await.is_none()
            } else {
                process
                    .child
                    .as_ref()
                    .expect("ffmpeg process")
                    .lock()
                    .await
                    .try_wait()
                    .is_ok_and(|status| status.is_none())
            };
            if running {
                process
                    .progress
                    .lock()
                    .expect("recording progress")
                    .terminal_error = Some(reason);
                process.finish(5).await;
            }
        }
        let exit_code = if let Some(direct) = &process.direct {
            let result = direct.result.lock().await;
            match result.as_ref() {
                None => return ProcessState::Running,
                Some(Ok(())) => Some(0),
                Some(Err(error)) => {
                    *process.failure.lock().await = Some(error.clone());
                    Some(1)
                }
            }
        } else {
            let mut child = process.child.as_ref().expect("ffmpeg process").lock().await;
            match child.try_wait() {
                Ok(Some(status)) => status.code(),
                Ok(None) => return ProcessState::Running,
                Err(err) => {
                    log::warn!("查询 ffmpeg 状态失败 ({}): {err}", process.rec_id);
                    return ProcessState::Running;
                }
            }
        };
        // FFmpeg can exit before the asynchronous stderr reader has consumed its final error.
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            process.stderr_done.cancelled(),
        )
        .await;
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            process.progress_done.cancelled(),
        )
        .await;
        let health_error = process
            .progress
            .lock()
            .expect("recording progress")
            .terminal_error
            .map(str::to_owned);
        let error = if health_error.is_some() {
            health_error
        } else if exit_code == Some(0) {
            None
        } else {
            Some(
                process
                    .failure
                    .lock()
                    .await
                    .clone()
                    .unwrap_or_else(|| match exit_code {
                        Some(code) => format!("FFmpeg 异常退出（退出码 {code}）"),
                        None => "FFmpeg 异常退出（无退出码）".into(),
                    }),
            )
        };
        let mut active = self.active.lock().await;
        if process.stop_requested.load(Ordering::SeqCst)
            || !active
                .get(&process.rec_id)
                .is_some_and(|current| Arc::ptr_eq(current, process))
        {
            return ProcessState::Unknown;
        }
        active.remove(&process.rec_id);
        ProcessState::Exited {
            code: exit_code,
            error,
        }
    }

    /// 启动录制。
    ///
    /// Progress has its own stdout pipe; stderr remains a bounded diagnostic channel.
    pub async fn start(
        &self,
        ffmpeg: &Path,
        rec_id: &str,
        options: &RecordOptions,
    ) -> Result<Arc<RecorderProcess>, String> {
        validate_recording_proxy(&options.record_url, options.proxy.as_deref())?;
        if !SUPPORTED_RECORD_FORMATS.contains(&options.format.to_ascii_uppercase().as_str()) {
            return Err("不支持的录制格式".into());
        }
        let _filesystem = self.filesystem_guard().await;
        let mut active = self.active.lock().await;
        if self.closing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("录制引擎正在关闭".into());
        }
        if active.contains_key(rec_id) {
            return Err("此任务已经在录制".into());
        }
        if let Some(parent) = Path::new(&options.save_path).parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建输出目录失败: {e}"))?;
        }

        let output = Path::new(&options.save_path);
        let output_path = output
            .parent()
            .ok_or("输出目录无效")?
            .canonicalize()
            .map_err(|e| e.to_string())?
            .join(output.file_name().ok_or("输出文件名无效")?);
        if self
            .media_reservations
            .lock()
            .expect("media reservation lock")
            .values()
            .flatten()
            .any(|path| output_is_protected(&output_path, path))
        {
            return Err("同名录制文件正在转 MP4，请稍后重试".into());
        }
        if std::fs::symlink_metadata(&output_path).is_ok()
            || !crate::media_safety::recording_outputs(&output_path)
                .map_err(|e| e.to_string())?
                .is_empty()
            || active.values().any(|p| {
                output_is_protected(&p.output_path, &output_path)
                    || output_is_protected(&output_path, &p.output_path)
            })
        {
            return Err("同名输出已存在，未覆盖任何录像".into());
        }
        let args = build_command(options);
        log::info!("启动录制 {rec_id}");

        let mut command = Command::new(ffmpeg);
        command
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let mut child = command
            .spawn()
            .map_err(|e| format!("启动 ffmpeg 失败: {e}"))?;

        let failure = Arc::new(Mutex::new(None));
        let progress = Arc::new(std::sync::Mutex::new(RecordingProgress::new()));
        let progress_done = tokio_util::sync::CancellationToken::new();
        if let Some(stdout) = child.stdout.take() {
            let progress = progress.clone();
            let done = progress_done.clone().drop_guard();
            self.log_tasks.spawn(async move {
                let _done = done;
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    progress
                        .lock()
                        .expect("recording progress")
                        .observe_line(&line);
                }
            });
        } else {
            progress_done.cancel();
        }
        let stderr_done = tokio_util::sync::CancellationToken::new();
        if let Some(stderr) = child.stderr.take() {
            let rec_id = rec_id.to_string();
            let failure = failure.clone();
            let done = stderr_done.clone().drop_guard();
            self.log_tasks.spawn(async move {
                let _done = done;
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        if let Some(summary) = recording_failure_summary(trimmed) {
                            failure
                                .lock()
                                .await
                                .get_or_insert_with(|| summary.to_string());
                        }
                        log::warn!("ffmpeg[{rec_id}] {}", redact_stream_url(trimmed));
                    }
                }
            });
        } else {
            stderr_done.cancel();
        }

        let process = Arc::new(RecorderProcess {
            rec_id: rec_id.to_string(),
            output_path,
            run_id: uuid::Uuid::new_v4(),
            started_at: std::time::Instant::now(),
            wall_started_at: chrono::Local::now(),
            child: Some(Arc::new(Mutex::new(child))),
            direct: None,
            stop_requested: AtomicBool::new(false),
            failure,
            stderr_done,
            progress_done,
            progress,
        });
        active.insert(rec_id.to_string(), process.clone());
        Ok(process)
    }

    pub async fn start_direct(
        &self,
        rec_id: &str,
        options: &RecordOptions,
    ) -> Result<Arc<RecorderProcess>, String> {
        if options.format != "FLV" || options.segment_record {
            return Err("FLV 直下需要选择 FLV 格式并关闭分段".into());
        }
        let _filesystem = self.filesystem_guard().await;
        let mut active = self.active.lock().await;
        if self.closing.load(Ordering::SeqCst) || active.contains_key(rec_id) {
            return Err("任务已在运行或正在退出".into());
        }
        let path = Path::new(&options.save_path);
        let output_path = path
            .parent()
            .ok_or("输出目录无效")?
            .canonicalize()
            .map_err(|_| "输出目录无效")?
            .join(path.file_name().ok_or("输出文件名无效")?);
        if self
            .media_reservations
            .lock()
            .expect("media reservations")
            .values()
            .flatten()
            .any(|p| p == &output_path)
        {
            return Err("文件正在后处理中".into());
        }
        let direct = crate::direct::Download::start(
            &output_path,
            &options.record_url,
            options.proxy.as_deref(),
            options.headers.as_deref(),
            &self.log_tasks,
        )
        .await?;
        let done = tokio_util::sync::CancellationToken::new();
        done.cancel();
        let process = Arc::new(RecorderProcess {
            rec_id: rec_id.to_string(),
            output_path,
            run_id: uuid::Uuid::new_v4(),
            started_at: std::time::Instant::now(),
            wall_started_at: chrono::Local::now(),
            child: None,
            direct: Some(direct),
            stop_requested: AtomicBool::new(false),
            failure: Arc::new(Mutex::new(None)),
            stderr_done: done.clone(),
            progress_done: done,
            progress: Arc::new(std::sync::Mutex::new(RecordingProgress::new())),
        });
        active.insert(rec_id.to_string(), process.clone());
        Ok(process)
    }

    /// Keep the process tracked while stopping, even if the caller's request is cancelled.
    pub async fn stop(&self, rec_id: &str, grace_secs: u64) -> bool {
        let process = self.active.lock().await.get(rec_id).cloned();
        if let Some(process) = process {
            process.stop(grace_secs).await;
            let mut active = self.active.lock().await;
            if active.get(rec_id).is_some_and(|p| Arc::ptr_eq(p, &process)) {
                active.remove(rec_id);
            }
            true
        } else {
            false
        }
    }
    pub async fn stop_all(&self, grace_secs: u64) {
        self.begin_shutdown();
        let ids = self.active_ids().await;
        futures::future::join_all(ids.iter().map(|id| self.stop(id, grace_secs))).await;
        self.log_tasks.close();
        self.log_tasks.wait().await;
    }
}

fn output_is_protected(output: &Path, target: &Path) -> bool {
    if output.starts_with(target) {
        return true;
    }
    let Some(parent) = output.parent() else {
        return true;
    };
    if target.parent() != Some(parent) {
        return false;
    }
    let pattern = output.file_name().unwrap_or_default().to_string_lossy();
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    if let Some((prefix, suffix)) = pattern.split_once("%03d") {
        name.strip_prefix(prefix)
            .and_then(|s| s.strip_suffix(suffix))
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
    } else {
        target == output
    }
}

/// Only fixed summaries reach the UI; never expose raw stderr or signed stream URLs.
fn recording_failure_summary(message: &str) -> Option<&'static str> {
    let message = message.to_ascii_lowercase();
    if message.contains("404 not found") || message.contains("server returned 404") {
        Some("播放地址返回 HTTP 404")
    } else if message.contains("403 forbidden") || message.contains("server returned 403") {
        Some("播放地址拒绝访问（HTTP 403）")
    } else if message.contains("401 unauthorized") || message.contains("server returned 401") {
        Some("播放地址需要登录（HTTP 401）")
    } else if message.contains("timed out") {
        Some("拉流超时，请检查网络后重试")
    } else if message.contains("connection refused") {
        Some("连接播放服务器被拒绝")
    } else if message.contains("no space left on device") {
        Some("录制磁盘空间不足")
    } else if message.contains("permission denied") {
        Some("无法访问录制文件或播放地址，请检查权限")
    } else {
        None
    }
}

fn redact_stream_url(message: &str) -> String {
    static URL: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"https?://\S+").expect("static regex"));
    URL.replace_all(message, "[stream-url]").into_owned()
}

/// Keeps queued and running conversions protected from in-app recycle operations.
pub(crate) struct MediaReservation {
    id: uuid::Uuid,
    reservations: Arc<std::sync::Mutex<HashMap<uuid::Uuid, Vec<PathBuf>>>>,
}
impl Drop for MediaReservation {
    fn drop(&mut self) {
        if let Ok(mut paths) = self.reservations.lock() {
            paths.remove(&self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn media_progress_uses_startup_and_idle_budgets_without_counting_wait_time() {
        let mut progress = RecordingProgress::new();
        tokio::time::advance(std::time::Duration::from_secs(119)).await;
        progress.observe_line("out_time_us=N/A");
        progress.observe_line("frame=0");
        assert_eq!(progress.stalled(), None);
        assert_eq!(progress.seconds(), 0.0);
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        assert!(progress.stalled().unwrap().contains("120"));

        let mut progress = RecordingProgress::new();
        progress.observe_line("out_time_us=2000000");
        tokio::time::advance(std::time::Duration::from_secs(89)).await;
        progress.observe_line("out_time_us=2000000");
        assert_eq!(progress.stalled(), None);
        assert_eq!(progress.seconds(), 2.0);
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        assert!(progress.stalled().unwrap().contains("90"));
        progress.observe_bytes(1024);
        assert_eq!(progress.stalled(), None);
        assert_eq!(progress.seconds(), 2.0);
        tokio::time::advance(std::time::Duration::from_secs(90)).await;
        progress.observe_bytes(1024);
        assert!(
            progress.stalled().is_some(),
            "unchanged bytes cannot reset the idle clock"
        );
        progress.observe_line("frame=10");
        assert_eq!(progress.stalled(), None);
    }

    #[tokio::test]
    async fn finish_reservations_protect_segments_and_sidecars_without_holding_global_lock() {
        let engine = Engine::new();
        let root = tempfile::tempdir().unwrap();
        let guard = engine.filesystem_guard().await;
        let reservation = engine.reserve_media(vec![
            root.path().join("take_%03d.ts"),
            root.path().join("take_%03d.srt"),
        ]);
        drop(guard);
        let guard = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            engine.filesystem_guard(),
        )
        .await
        .unwrap();
        assert!(engine.protects_path(&root.path().join("take_001.ts")).await);
        assert!(
            engine
                .protects_path(&root.path().join("take_020.srt"))
                .await
        );
        assert!(engine.protects_path(root.path()).await);
        assert!(!engine.protects_path(&root.path().join("other.ts")).await);
        drop(reservation);
        assert!(!engine.protects_path(root.path()).await);
        drop(guard);
    }
    #[test]
    fn failure_summaries_are_bounded_and_never_include_stream_credentials() {
        assert_eq!(
            recording_failure_summary("[in#0] Error opening input: Server returned 404 Not Found"),
            Some("播放地址返回 HTTP 404")
        );
        assert_eq!(
            recording_failure_summary("Connection timed out"),
            Some("拉流超时，请检查网络后重试")
        );
        assert_eq!(
            recording_failure_summary("No space left on device"),
            Some("录制磁盘空间不足")
        );
        assert_eq!(
            recording_failure_summary(
                "Error opening input https://media.invalid/private.flv?token=fixture-only"
            ),
            None
        );
        assert_eq!(
            recording_failure_summary("Unrecognized arbitrary server error"),
            None
        );
    }

    #[test]
    fn protects_active_output_patterns_and_ancestors_not_other_files() {
        let root = Path::new("recordings/author");
        let pattern = root.join("session_%03d.ts");
        for target in [
            root.to_owned(),
            PathBuf::from("recordings"),
            root.join("session_001.ts"),
            root.join("session_1000.ts"),
        ] {
            assert!(output_is_protected(&pattern, &target));
        }
        for target in [
            root.join("old.ts"),
            root.join("session_x.ts"),
            PathBuf::from("other/session_001.ts"),
        ] {
            assert!(!output_is_protected(&pattern, &target));
        }
        assert!(output_is_protected(
            &root.join("one.mp4"),
            &root.join("one.mp4")
        ));
        assert!(!output_is_protected(
            &root.join("one.mp4"),
            &root.join("two.mp4")
        ));
    }

    fn base_options(format: &str) -> RecordOptions {
        RecordOptions {
            record_url: "https://example.com/live.m3u8".into(),
            save_path: "Z:/demo/out/test.ts".into(),
            format: format.into(),
            segment_record: false,
            segment_time: None,
            headers: None,
            proxy: None,
            platform_key: None,
            video_bitrate: None,
            is_overseas: false,
        }
    }

    #[test]
    fn ts_command_contains_required_input_and_output_flags() {
        let cmd = build_command(&base_options("TS"));
        assert!(cmd.contains(&"-user_agent".to_string()));
        assert!(cmd.contains(&"-rw_timeout".to_string()));
        assert!(cmd.windows(2).any(|w| w == ["-f", "mpegts"]));
        assert_eq!(cmd.last().unwrap(), "Z:/demo/out/test.ts");
    }

    #[test]
    fn segment_record_switches_to_segment_muxer() {
        let mut options = base_options("TS");
        options.segment_record = true;
        options.segment_time = Some("900".into());

        let cmd = build_command(&options);
        assert!(cmd.windows(2).any(|w| w == ["-f", "segment"]));
        assert!(cmd.windows(2).any(|w| w == ["-segment_time", "900"]));
    }

    #[test]
    fn bitrate_enables_transcode_else_copy() {
        let copy_cmd = build_command(&base_options("TS"));
        assert!(copy_cmd.windows(2).any(|w| w == ["-c:v", "copy"]));

        let mut options = base_options("TS");
        options.video_bitrate = Some(3000);
        let transcode_cmd = build_command(&options);
        assert!(transcode_cmd.windows(2).any(|w| w == ["-c:v", "libx264"]));
        assert!(transcode_cmd.iter().any(|a| a == "3000k"));
    }

    #[test]
    fn proxy_and_headers_are_injected_before_input() {
        let mut options = base_options("TS");
        options.proxy = Some("http://127.0.0.1:7890".into());
        options.headers = Some("Referer: https://live.example.com\r\n".into());

        let cmd = build_command(&options);
        let proxy_at = cmd.iter().position(|a| a == "-http_proxy").unwrap();
        let headers_at = cmd.iter().position(|a| a == "-headers").unwrap();
        let input_at = cmd.iter().position(|a| a == "-i").unwrap();
        assert!(proxy_at < input_at);
        assert!(headers_at < input_at);
    }

    #[test]
    fn chzzk_gets_relaxed_hls_extension_check() {
        let mut options = base_options("TS");
        options.platform_key = Some("chzzk".into());
        assert!(build_command(&options)
            .iter()
            .any(|a| a == "-extension_picky"));

        let mut other = base_options("TS");
        other.platform_key = Some("douyin".into());
        assert!(!build_command(&other)
            .iter()
            .any(|a| a == "-extension_picky"));
    }

    #[test]
    fn overseas_profile_uses_larger_buffers() {
        let mut options = base_options("TS");
        options.is_overseas = true;
        let cmd = build_command(&options);
        assert!(cmd
            .windows(2)
            .any(|w| w == ["-rw_timeout", OVERSEAS_RW_TIMEOUT]));
        assert!(cmd.windows(2).any(|w| w == ["-bufsize", OVERSEAS_BUFSIZE]));
    }

    #[test]
    fn mp4_uses_faststart_movflags() {
        let cmd = build_command(&base_options("MP4"));
        assert!(cmd.windows(2).any(|w| w == ["-f", "mp4"]));
        assert!(cmd.iter().any(|a| a.contains("faststart")));
    }

    #[test]
    fn filename_joins_anchor_title_timestamp() {
        assert_eq!(
            build_filename("主播A", Some("直播标题"), "2026-09-10_12-00-00"),
            "主播A_直播标题_2026-09-10_12-00-00"
        );
        assert_eq!(
            build_filename("主播A", None, "2026-09-10_12-00-00"),
            "主播A_2026-09-10_12-00-00"
        );
    }

    #[test]
    fn filename_cleans_title_like_python() {
        // 全角逗号转半角、空格去除，与 Python _clean_and_truncate_title 一致
        assert_eq!(build_filename("主播", Some("a，b c"), "t"), "主播_a,bc_t");
    }

    #[test]
    fn filename_truncates_long_title_to_30_chars() {
        let long_title = "标".repeat(50);
        let name = build_filename("主播", Some(&long_title), "t");
        assert_eq!(name, format!("主播_{}_t", "标".repeat(30)));
    }

    #[test]
    fn output_dir_respects_folder_switches() {
        let root = Path::new("Z:/demo/downloads");
        let dir = build_output_dir(
            root,
            "抖音",
            "主播A",
            "2026-09-10",
            &FolderOptions {
                by_platform: true,
                by_author: true,
                by_time: false,
            },
        );
        assert_eq!(dir, root.join("抖音").join("主播A"));
    }

    #[test]
    fn segment_suffix_added_only_when_segmenting() {
        assert_eq!(with_segment_suffix("Z:/o/a", "TS", true), "Z:/o/a_%03d.ts");
        assert_eq!(with_segment_suffix("Z:/o/a", "TS", false), "Z:/o/a");
    }

    #[test]
    fn sanitize_replaces_illegal_path_chars() {
        assert_eq!(sanitize("a/b:c*d"), "a_b_c_d");
    }

    #[tokio::test]
    async fn stop_is_noop_for_unknown_recording() {
        let engine = Engine::new();
        assert!(!engine.stop("missing", 1).await);
        assert!(!engine.is_recording("missing").await);
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;
    fn options(url: &str) -> RecordOptions {
        RecordOptions {
            record_url: url.into(),
            save_path: "fixture.ts".into(),
            format: "TS".into(),
            segment_record: false,
            segment_time: None,
            headers: Some("Referer: https://example.test/\r\n".into()),
            proxy: None,
            platform_key: None,
            video_bitrate: None,
            is_overseas: false,
        }
    }
    #[test]
    fn http_playlists_cannot_read_local_files_and_rtmp_has_no_http_options() {
        let http = build_command(&options("https://media.example.test/stream.m3u8"));
        let at = http
            .iter()
            .position(|s| s == "-protocol_whitelist")
            .unwrap();
        assert!(!http[at + 1].split(',').any(|s| s == "file"));
        let rtmp = build_command(&options("rtmp://media.example.test/live"));
        assert!(!rtmp
            .iter()
            .any(|s| s == "-user_agent" || s == "-headers" || s == "-reconnect"));
    }
    #[test]
    fn unsupported_proxy_protocols_fail_instead_of_being_ignored() {
        assert!(validate_recording_proxy(
            "https://example.test/live",
            Some("socks5://127.0.0.1:1080")
        )
        .is_err());
        assert!(validate_recording_proxy(
            "rtmp://example.test/live",
            Some("http://127.0.0.1:8080")
        )
        .is_err());
        assert!(validate_recording_proxy(
            "https://example.test/live",
            Some("http://127.0.0.1:8080")
        )
        .is_ok());
    }
}
