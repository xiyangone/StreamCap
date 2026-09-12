//! ffmpeg 录制引擎：命令构造与进程生命周期管理。
//! 参数与 Python `app/core/media/ffmpeg_builders/` 对齐，保证录制产物一致。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

pub const SUPPORTED_RECORD_FORMATS: &[&str] = &["TS", "FLV", "MKV", "MOV", "MP4"];
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

    let mut command: Vec<String> = vec![
        "-y",
        "-v",
        "verbose",
        "-rw_timeout",
        rw_timeout,
        "-loglevel",
        "error",
        "-hide_banner",
        "-user_agent",
        FFMPEG_USER_AGENT,
        "-protocol_whitelist",
        "rtmp,crypto,file,http,https,tcp,tls,udp,rtp,httpproxy",
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

    command.push("-i".into());
    command.push(options.record_url.clone());

    command.extend(
        [
            "-bufsize",
            bufsize,
            "-sn",
            "-dn",
            "-reconnect_delay_max",
            "60",
            "-reconnect_streamed",
            "-reconnect_at_eof",
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
    if let Some(headers) = options.headers.as_ref().filter(|h| !h.is_empty()) {
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
                    "+faststart+frag_keyframe+empty_moov+delay_moov",
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
        other => {
            log::warn!("未支持的录制格式 {other}，回退为 TS");
            tail.extend(video_codec.iter().cloned());
            tail.extend(strings(&["-c:a", "copy", "-map", "0", "-f", "mpegts"]));
        }
    }

    command.extend(tail);
    command.push(options.save_path.clone());
    command
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
        let cleaned: String = title
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
    const ILLEGAL: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
    name.chars()
        .map(|c| if ILLEGAL.contains(&c) { '_' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
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
pub struct RecorderProcess {
    pub rec_id: String,
    child: Arc<Mutex<Child>>,
}

impl RecorderProcess {
    /// 优雅停止：Windows 下向 stdin 写 `q` 让 ffmpeg 收尾；超时后强杀。
    pub async fn stop(&self, grace_secs: u64) {
        {
            let mut child = self.child.lock().await;
            if let Some(stdin) = child.stdin.as_mut() {
                use tokio::io::AsyncWriteExt;
                let _ = stdin.write_all(b"q").await;
                let _ = stdin.flush().await;
            }
        }

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(grace_secs);
        loop {
            {
                let mut child = self.child.lock().await;
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

        let mut child = self.child.lock().await;
        if let Err(err) = child.kill().await {
            log::warn!("强制结束 ffmpeg 失败: {err}");
        }
    }
}

/// 录制进程状态查询结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    /// 仍在活动表中且进程未退出
    Running,
    /// 进程已自行退出（含退出码），已从活动表移除
    Exited(Option<i32>),
    /// 不在活动表中（已停止或从未启动）
    Unknown,
}

/// 录制引擎：管理所有活动录制进程。
#[derive(Clone, Default)]
pub struct Engine {
    closing: Arc<std::sync::atomic::AtomicBool>,
    active: Arc<Mutex<HashMap<String, Arc<RecorderProcess>>>>,
    log_tasks: tokio_util::task::TaskTracker,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
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

    /// 非阻塞查询进程是否已自行退出（ffmpeg 读完流后会自己结束，必须被感知，
    /// 否则任务会永远停留在「录制中」）。与 Python 每秒轮询 `returncode` 等价。
    pub async fn poll_state(&self, rec_id: &str) -> ProcessState {
        let Some(process) = self.active.lock().await.get(rec_id).cloned() else {
            return ProcessState::Unknown;
        };

        let exit_code = {
            let mut child = process.child.lock().await;
            match child.try_wait() {
                Ok(Some(status)) => status.code(),
                Ok(None) => return ProcessState::Running,
                Err(err) => {
                    log::warn!("查询 ffmpeg 状态失败 ({rec_id}): {err}");
                    None
                }
            }
        };

        let mut active = self.active.lock().await;
        if active
            .get(rec_id)
            .is_some_and(|current| !Arc::ptr_eq(current, &process))
        {
            return ProcessState::Running;
        }
        active.remove(rec_id);
        ProcessState::Exited(exit_code)
    }

    /// 启动录制。
    ///
    /// ffmpeg 以 `-loglevel error` 运行，stderr 只会输出错误，因此这里不做进度解析
    /// （录制速率由调度器按产出文件增量计算），只保留错误行用于诊断。
    pub async fn start(
        &self,
        ffmpeg: &Path,
        rec_id: &str,
        options: &RecordOptions,
    ) -> Result<Arc<RecorderProcess>, String> {
        if !SUPPORTED_RECORD_FORMATS.contains(&options.format.to_ascii_uppercase().as_str()) {
            return Err("此录制格式尚未迁移到原生版".into());
        }
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

        let args = build_command(options);
        log::info!("启动录制 {rec_id}");

        let mut command = Command::new(ffmpeg);
        command
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let mut child = command
            .spawn()
            .map_err(|e| format!("启动 ffmpeg 失败: {e}"))?;

        if let Some(stderr) = child.stderr.take() {
            let rec_id = rec_id.to_string();
            self.log_tasks.spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        log::warn!("ffmpeg[{rec_id}] {}", redact_stream_url(trimmed));
                    }
                }
            });
        }

        let process = Arc::new(RecorderProcess {
            rec_id: rec_id.to_string(),
            child: Arc::new(Mutex::new(child)),
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

fn redact_stream_url(message: &str) -> String {
    static URL: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"https?://\S+").expect("static regex"));
    URL.replace_all(message, "[stream-url]").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

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
