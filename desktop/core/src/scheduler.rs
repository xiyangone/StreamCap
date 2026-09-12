//! 监控调度：周期性检测直播间状态、按需启停录制、磁盘空间保护。
//! 对应 Python 侧 `RecordingManager.setup_periodic_live_check` / `check_if_live` 的职责。

use crate::config::ConfigStore;
use crate::engine::{
    build_filename, build_output_dir, with_segment_suffix, Engine, FolderOptions, RecordOptions,
};
use crate::resolver::{ResolveRequest, Resolver};
use crate::store::Store;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// 检测结果，供 API 直接回显。
#[derive(Debug, Clone, PartialEq)]
pub enum CheckOutcome {
    /// 已开播并开始录制
    RecordingStarted,
    /// 已开播，正在录制中
    AlreadyRecording,
    /// 未开播，监控中
    Offline,
    /// 未开播且已停止录制
    StreamEnded,
    /// 解析失败
    Failed(String),
}

impl CheckOutcome {
    pub fn message(&self) -> String {
        match self {
            CheckOutcome::RecordingStarted => "已开播，开始录制".into(),
            CheckOutcome::AlreadyRecording => "正在录制中".into(),
            CheckOutcome::Offline => "未开播".into(),
            CheckOutcome::StreamEnded => "直播已结束，录制停止".into(),
            CheckOutcome::Failed(err) => format!("检测失败: {err}"),
        }
    }
}

pub struct Scheduler {
    store: Store,
    engine: Engine,
    config: Arc<RwLock<ConfigStore>>,
    resolver: Resolver,
    ffmpeg: Option<PathBuf>,
    recording_enabled: Arc<AtomicBool>,
    stopping: tokio_util::sync::CancellationToken,
    background: tokio_util::task::TaskTracker,
    starting: tokio::sync::Mutex<()>,
}

impl Scheduler {
    pub fn new(
        store: Store,
        engine: Engine,
        config: Arc<RwLock<ConfigStore>>,
        resolver: Resolver,
        ffmpeg: Option<PathBuf>,
        recording_enabled: Arc<AtomicBool>,
    ) -> Self {
        let stopping = resolver.cancellation();
        Self {
            store,
            engine,
            config,
            resolver,
            ffmpeg,
            recording_enabled,
            stopping,
            background: tokio_util::task::TaskTracker::new(),
            starting: tokio::sync::Mutex::new(()),
        }
    }

    /// 后台循环：按 loop_time_seconds 周期检测所有开启监控的任务。
    pub async fn run(self: Arc<Self>, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        let interval = {
            let config = self.config.read().await;
            config.get_i64("loop_time_seconds", 300).max(30) as u64
        };
        log::info!("监控调度启动，检测间隔 {interval}s");

        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(interval)) => {}
                _ = shutdown.changed() => {
                    log::info!("监控调度停止");
                    return;
                }
            }

            if !self.recording_enabled.load(Ordering::Relaxed) {
                continue;
            }

            for rec in self.store.all().await {
                if self.stopping.is_cancelled() {
                    return;
                }
                if !rec.monitor_status {
                    continue;
                }
                if let Err(err) = self.check(rec.rec_id.clone()).await {
                    log::debug!("检测 {} 失败: {err}", rec.streamer_name);
                }
            }
        }
    }

    /// 检测单个任务：解析 → 更新状态 → 按需启停录制。
    pub async fn check(&self, rec_id: String) -> Result<CheckOutcome, String> {
        if self.stopping.is_cancelled() {
            return Err("应用正在退出".into());
        }
        let Some(rec) = self.store.get(&rec_id).await else {
            return Err(format!("任务不存在: {rec_id}"));
        };

        let (proxy, quality) = {
            let config = self.config.read().await;
            let proxy = if config.get_bool("enable_proxy", true) {
                config.get_str("proxy_address", "")
            } else {
                String::new()
            };
            let quality = rec
                .quality
                .clone()
                .unwrap_or_else(|| config.get_str("record_quality", "OD"));
            (proxy, quality)
        };

        let cookie = rec
            .platform_key
            .as_ref()
            .and_then(|key| self.cookie_for(key));

        let request = ResolveRequest {
            url: rec.url.clone(),
            quality: Some(quality),
            proxy: if proxy.is_empty() { None } else { Some(proxy) },
            cookie,
            platform: rec.platform_key.clone(),
        };

        let info = match self.resolver.resolve(request).await {
            Ok(info) => info,
            Err(err) => {
                return Ok(CheckOutcome::Failed(err));
            }
        };

        if !info.is_live {
            let was_recording = self.engine.is_recording(&rec_id).await;
            if was_recording {
                self.stop_recording(&rec_id).await;
            }
            self.store
                .update(&rec_id, |r| {
                    r.is_live = false;
                    r.is_recording = false;
                    r.live_title = None;
                    r.speed = None;
                })
                .await;
            return Ok(if was_recording {
                CheckOutcome::StreamEnded
            } else {
                CheckOutcome::Offline
            });
        }

        // 已开播：更新直播信息
        let anchor = if info.anchor_name.is_empty() {
            rec.streamer_name.clone()
        } else {
            info.anchor_name.clone()
        };
        let live_title = info.title.clone();
        self.store
            .update(&rec_id, |r| {
                r.is_live = true;
                r.live_title = Some(live_title.clone());
                if r.streamer_name.is_empty() {
                    r.streamer_name = anchor.clone();
                }
            })
            .await;

        if self.engine.is_recording(&rec_id).await {
            return Ok(CheckOutcome::AlreadyRecording);
        }

        self.start_recording(rec_id.clone(), &info).await?;
        Ok(CheckOutcome::RecordingStarted)
    }

    /// 启动录制：解析流地址 → 构造输出路径 → 拉起 ffmpeg。
    pub async fn start_recording(
        &self,
        rec_id: String,
        info: &crate::resolver::StreamInfo,
    ) -> Result<(), String> {
        let _starting = self.starting.lock().await;
        if self.stopping.is_cancelled() || !self.recording_enabled.load(Ordering::SeqCst) {
            return Err("已暂停录制或正在退出".into());
        }
        let Some(ffmpeg) = self.ffmpeg.clone() else {
            return Err("未找到 ffmpeg，请先安装或放入用户数据目录".into());
        };

        let Some(rec) = self.store.get(&rec_id).await else {
            return Err(format!("任务不存在: {rec_id}"));
        };

        let (prefer_flv, folder_opts, config_snapshot) = {
            let config = self.config.read().await;
            let folder_opts = FolderOptions {
                by_platform: config.get_bool("folder_name_platform", true),
                by_author: config.get_bool("folder_name_author", true),
                by_time: config.get_bool("folder_name_time", false),
            };
            let prefer_flv = rec
                .flv_use_direct_download
                .unwrap_or_else(|| config.get_bool("flv_use_direct_download", false));
            (
                prefer_flv,
                folder_opts,
                config.get_str("video_format", "TS"),
            )
        };

        let Some(record_url) = info.pick_record_url(prefer_flv) else {
            return Err("解析结果中没有可用的录制地址".into());
        };

        let format = rec.record_format.clone().unwrap_or(config_snapshot);
        let anchor = if rec.streamer_name.is_empty() {
            info.anchor_name.clone()
        } else {
            rec.streamer_name.clone()
        };
        let segment_record = rec.segment_record.unwrap_or(false);

        // 仅在开启 filename_includes_title 时把直播标题并入文件名（与 Python 一致）
        let include_title = {
            let config = self.config.read().await;
            config.get_bool("filename_includes_title", false)
        };
        let title_for_filename = if include_title {
            Some(info.title.as_str())
        } else {
            None
        };

        let now = chrono::Local::now();
        let filename = build_filename(
            &anchor,
            title_for_filename,
            &now.format("%Y-%m-%d_%H-%M-%S").to_string(),
        );

        let root = {
            let config = self.config.read().await;
            config.recordings_root()
        };
        let output_dir = build_output_dir(
            &root,
            &info.platform,
            &anchor,
            &now.format("%Y-%m-%d").to_string(),
            &folder_opts,
        );

        let base_path = output_dir
            .join(filename)
            .to_string_lossy()
            .replace('\\', "/");
        let save_path = if segment_record {
            with_segment_suffix(&base_path, &format, true)
        } else {
            format!("{base_path}.{}", format.to_ascii_lowercase())
        };

        let options = RecordOptions {
            record_url,
            save_path,
            format: format.clone(),
            segment_record,
            segment_time: rec.segment_time.clone(),
            headers: None,
            proxy: None,
            platform_key: rec.platform_key.clone(),
            video_bitrate: rec.video_bitrate,
            is_overseas: is_overseas_platform(rec.platform_key.as_deref()),
        };

        let output_dir_str = output_dir.to_string_lossy().to_string();

        self.engine.start(&ffmpeg, &rec_id, &options).await?;

        self.store
            .update(&rec_id, move |r| {
                r.is_recording = true;
                r.recording_dir = Some(output_dir_str.clone());
                r.speed = Some("0 KB/s".into());
            })
            .await;

        // 速率采样：ffmpeg 以 -loglevel error 运行，stderr 没有进度行，
        // 只能按产出目录的字节增量计算（Python 侧该字段一直是静态占位符，此处改为真实值）。
        self.spawn_speed_sampler(rec_id.clone(), output_dir.clone());

        // 退出监听：ffmpeg 读完流会自行结束，必须感知并回写状态，
        // 否则任务会永远停留在「录制中」（对应 Python 每秒轮询 returncode 的处理）。
        self.spawn_exit_watcher(rec_id.clone());

        self.store.snack(format!("{anchor}: 开始录制"));
        Ok(())
    }

    /// 每秒检查 ffmpeg 是否已退出；退出后清理录制状态，让监控循环可再次拉起。
    fn spawn_exit_watcher(&self, rec_id: String) {
        let store = self.store.clone();
        let engine = self.engine.clone();

        let stopping = self.stopping.clone();
        self.background.spawn(async move {
            loop {
                tokio::select!{_=stopping.cancelled()=>return,_=tokio::time::sleep(Duration::from_secs(1))=>{}}

                match engine.poll_state(&rec_id).await {
                    crate::engine::ProcessState::Running => continue,
                    crate::engine::ProcessState::Exited(code) => {
                        log::info!("录制 {rec_id} 已结束（ffmpeg 退出码 {code:?}）");
                        store
                            .update(&rec_id, |r| {
                                r.is_recording = false;
                                r.is_live = false;
                                r.speed = None;
                            })
                            .await;
                        return;
                    }
                    // 已被停止流程移除，无需再监听
                    crate::engine::ProcessState::Unknown => return,
                }
            }
        });
    }

    /// 每 2 秒采样一次产出目录大小，换算为速率写回任务状态；录制结束后自行退出。
    fn spawn_speed_sampler(&self, rec_id: String, output_dir: PathBuf) {
        let store = self.store.clone();
        let engine = self.engine.clone();

        let stopping = self.stopping.clone();
        self.background.spawn(async move {
            let mut previous = dir_size_bytes(&output_dir);
            loop {
                tokio::select!{_=stopping.cancelled()=>return,_=tokio::time::sleep(Duration::from_secs(2))=>{}}

                if !engine.is_recording(&rec_id).await {
                    return;
                }

                let current = dir_size_bytes(&output_dir);
                let delta = current.saturating_sub(previous);
                previous = current;

                let speed = format!("{:.0} KB/s", delta as f64 / 2.0 / 1024.0);
                store
                    .update(&rec_id, move |r| {
                        r.speed = Some(speed.clone());
                    })
                    .await;
            }
        });
    }

    /// 手动开始录制：忽略监控开关，立即解析并开录。
    /// 未开播时返回错误——对应 Python `recording_button_on_click` 的「该直播间未开播」提示。
    pub async fn force_start(&self, rec_id: String) -> Result<CheckOutcome, String> {
        let info = self.resolve_for(&rec_id).await?;

        if !info.is_live {
            self.store
                .update(&rec_id, |r| {
                    r.is_live = false;
                })
                .await;
            return Err("该直播间未开播".into());
        }

        self.store
            .update(&rec_id, |r| {
                r.is_live = true;
                r.live_title = Some(info.title.clone());
            })
            .await;

        if self.engine.is_recording(&rec_id).await {
            return Ok(CheckOutcome::AlreadyRecording);
        }

        self.start_recording(rec_id, &info).await?;
        Ok(CheckOutcome::RecordingStarted)
    }

    /// 解析直播流（check 与 force_start 共用）。
    async fn resolve_for(&self, rec_id: &str) -> Result<crate::resolver::StreamInfo, String> {
        let Some(rec) = self.store.get(rec_id).await else {
            return Err(format!("任务不存在: {rec_id}"));
        };

        let (proxy, quality) = {
            let config = self.config.read().await;
            let proxy = if config.get_bool("enable_proxy", true) {
                config.get_str("proxy_address", "")
            } else {
                String::new()
            };
            let quality = rec
                .quality
                .clone()
                .unwrap_or_else(|| config.get_str("record_quality", "OD"));
            (proxy, quality)
        };

        let cookie = rec
            .platform_key
            .as_ref()
            .and_then(|key| self.cookie_for(key));

        self.resolver
            .resolve(ResolveRequest {
                url: rec.url.clone(),
                quality: Some(quality),
                proxy: if proxy.is_empty() { None } else { Some(proxy) },
                cookie,
                platform: rec.platform_key.clone(),
            })
            .await
    }

    /// 停止录制并更新状态。
    pub async fn stop_recording(&self, rec_id: &str) -> bool {
        let stopped = self.engine.stop(rec_id, 15).await;
        self.store
            .update(rec_id, |r| {
                r.is_recording = false;
                r.speed = None;
            })
            .await;
        stopped
    }

    pub async fn finish_background(&self) {
        let _starting = self.starting.lock().await;
        self.background.close();
        self.background.wait().await;
    }

    fn cookie_for(&self, platform_key: &str) -> Option<String> {
        self.config
            .try_read()
            .ok()
            .and_then(|config| config.cookies_for_resolver().get(platform_key).cloned())
    }

    /// 磁盘剩余空间检查（对应 Python check_free_space）。
    pub async fn check_free_space(&self) -> Result<(), String> {
        let (root, threshold_gb) = {
            let config = self.config.read().await;
            (
                config.recordings_root(),
                config.get_str("recording_space_threshold", "2.0"),
            )
        };
        let threshold: f64 = threshold_gb.parse().unwrap_or(2.0);

        let free = free_space_gb(&root);
        let Some(free) = free else {
            return Ok(());
        };

        if free < threshold {
            self.recording_enabled.store(false, Ordering::Relaxed);
            self.store
                .snack(format!("磁盘剩余空间不足 {threshold}GB，已暂停录制"));
            // 空间不足时停掉所有录制
            for id in self.engine.active_ids().await {
                self.stop_recording(&id).await;
            }
            return Err(format!("剩余空间 {free:.2}GB 低于阈值 {threshold}GB"));
        }

        self.recording_enabled.store(true, Ordering::Relaxed);
        Ok(())
    }
}

/// 递归统计目录字节数（速度采样用）。
fn dir_size_bytes(path: &std::path::Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let child = entry.path();
            if child.is_dir() {
                total += dir_size_bytes(&child);
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

/// 海外平台使用更大的缓冲配置（与 Python is_overseas 取向一致）。
fn is_overseas_platform(platform_key: Option<&str>) -> bool {
    const OVERSEAS: &[&str] = &[
        "tiktok",
        "soop",
        "pandalive",
        "winktv",
        "flextv",
        "popkontv",
        "twitch",
        "liveme",
        "showroom",
        "chzzk",
        "shopee",
        "youtube",
        "lang",
        "bigo",
        "blued",
    ];
    platform_key
        .map(|key| OVERSEAS.contains(&key))
        .unwrap_or(false)
}

/// Query the Windows filesystem directly; no console subprocess or locale-dependent parsing.
fn free_space_gb(path: &std::path::Path) -> Option<f64> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let directory = path.ancestors().find(|p| p.is_dir())?;
        let wide: Vec<u16> = directory
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut available = 0_u64;
        // SAFETY: the NUL-terminated path and output pointer remain valid throughout this call.
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then_some(available as f64 / 1024.0 / 1024.0 / 1024.0)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Recording;
    use crate::paths::Workspace;

    #[test]
    fn overseas_platforms_use_larger_buffers() {
        assert!(is_overseas_platform(Some("twitch")));
        assert!(is_overseas_platform(Some("chzzk")));
        assert!(!is_overseas_platform(Some("douyin")));
        assert!(!is_overseas_platform(None));
    }

    #[test]
    fn outcome_messages_are_user_facing() {
        assert_eq!(CheckOutcome::RecordingStarted.message(), "已开播，开始录制");
        assert!(CheckOutcome::Failed("超时".into())
            .message()
            .contains("超时"));
    }

    #[tokio::test]
    async fn check_missing_recording_reports_error() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::from_repo_root(dir.path());
        ws.ensure_ready().unwrap();
        let store = Store::new(ws.clone());
        let config = Arc::new(RwLock::new(ConfigStore::load(ws).unwrap()));
        let scheduler = Scheduler::new(
            store,
            Engine::new(),
            config,
            Resolver::new(),
            None,
            Arc::new(AtomicBool::new(true)),
        );

        let result = scheduler.check("nope".into()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("任务不存在"));
    }

    #[tokio::test]
    async fn start_recording_without_ffmpeg_fails_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::from_repo_root(dir.path());
        ws.ensure_ready().unwrap();
        let store = Store::new(ws.clone());
        store
            .add_many(vec![Recording::new("r1".into(), "u".into(), "n".into())])
            .await;
        let config = Arc::new(RwLock::new(ConfigStore::load(ws).unwrap()));
        let scheduler = Scheduler::new(
            store,
            Engine::new(),
            config,
            Resolver::new(),
            None,
            Arc::new(AtomicBool::new(true)),
        );

        let info = crate::resolver::StreamInfo {
            m3u8_url: "https://a/x.m3u8".into(),
            ..Default::default()
        };
        let err = scheduler
            .start_recording("r1".into(), &info)
            .await
            .unwrap_err();
        assert!(err.contains("ffmpeg"));
    }
}

#[cfg(all(test, windows))]
#[test]
fn native_free_space_query_handles_unicode_and_missing_child_paths() {
    let directory = tempfile::Builder::new()
        .prefix("原生磁盘验收")
        .tempdir()
        .unwrap();
    let space = free_space_gb(&directory.path().join("future-recording"));
    assert!(space.is_some_and(|n| n.is_finite() && n >= 0.0));
}
