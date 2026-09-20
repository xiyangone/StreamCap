//! 监控调度：周期性检测直播间状态、按需启停录制、磁盘空间保护。
//! 对应 Python 侧 `RecordingManager.setup_periodic_live_check` / `check_if_live` 的职责。

use crate::config::ConfigStore;
use crate::engine::{
    build_filename, build_output_dir, with_segment_suffix, Engine, FolderOptions, RecordOptions,
    RecorderProcess,
};
use crate::postprocess::RemuxOptions;
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
    OutsideSchedule,
    NotifyOnly,
    MonitoringPaused,
}

impl CheckOutcome {
    pub fn message(&self) -> String {
        match self {
            CheckOutcome::RecordingStarted => "已开播，开始录制".into(),
            CheckOutcome::AlreadyRecording => "正在录制中".into(),
            CheckOutcome::Offline => "未开播".into(),
            CheckOutcome::StreamEnded => "直播已结束，录制停止".into(),
            CheckOutcome::Failed(err) => format!("检测失败: {err}"),
            CheckOutcome::OutsideSchedule => "当前不在定时录制时间段".into(),
            CheckOutcome::NotifyOnly => "直播中，仅通知不录制".into(),
            CheckOutcome::MonitoringPaused => "监控已暂停".into(),
        }
    }
}

type LiveSourceCache =
    std::collections::HashMap<String, (String, std::time::Instant, crate::resolver::StreamInfo)>;

pub struct Scheduler {
    pacer: crate::pacing::Pacer,
    automatic_pacer: crate::pacing::AutomaticPacer,
    checks: Arc<
        std::sync::Mutex<std::collections::HashMap<String, tokio_util::sync::CancellationToken>>,
    >,
    schedule_edges: std::sync::Mutex<std::collections::HashMap<String, bool>>,
    live_sources: Arc<tokio::sync::RwLock<LiveSourceCache>>,
    store: Store,
    engine: Engine,
    config: Arc<RwLock<ConfigStore>>,
    resolver: Resolver,
    ffmpeg: Option<PathBuf>,
    recording_enabled: Arc<AtomicBool>,
    stopping: tokio_util::sync::CancellationToken,
    background: tokio_util::task::TaskTracker,
    starting: Arc<tokio::sync::Mutex<()>>,
    interval_changed: tokio::sync::watch::Sender<u64>,
    monitor_requests: std::sync::Mutex<std::collections::HashSet<String>>,
    monitor_changed: tokio::sync::Notify,
    storage: crate::storage::Storage,
    pub postprocess: crate::postprocess::Postprocessor,
    pub notifications: crate::notifications::Notifications,
    pub automation: crate::automation::Automation,
    conversion_plans: Arc<tokio::sync::Mutex<std::collections::HashMap<uuid::Uuid, FinishPlan>>>,
}

struct CheckLease {
    id: String,
    checks: Arc<
        std::sync::Mutex<std::collections::HashMap<String, tokio_util::sync::CancellationToken>>,
    >,
}
impl Drop for CheckLease {
    fn drop(&mut self) {
        self.checks.lock().expect("active checks").remove(&self.id);
    }
}

struct FinishPlan {
    root: PathBuf,
    convert: bool,
    options: RemuxOptions,
    script: Option<crate::automation::ScriptSpec>,
    subtitles: Option<PathBuf>,
}
impl FinishPlan {
    // Caller holds the filesystem guard: transfer protection before releasing the recorder.
    fn spawn(
        self,
        process: Arc<RecorderProcess>,
        engine: Engine,
        postprocess: crate::postprocess::Postprocessor,
        automation: crate::automation::Automation,
        store: Store,
        background: &tokio_util::task::TaskTracker,
    ) {
        let mut paths = vec![process.output_path.clone()];
        if self.subtitles.is_some() {
            paths.push(process.output_path.with_extension("srt"));
        }
        let reservation = engine.reserve_media(paths);
        background.spawn(async move {
            self.dispatch(
                &process,
                &postprocess,
                &automation,
                &store,
                &engine,
                reservation,
            )
            .await;
        });
    }

    async fn dispatch(
        mut self,
        process: &Arc<RecorderProcess>,
        postprocess: &crate::postprocess::Postprocessor,
        automation: &crate::automation::Automation,
        store: &Store,
        engine: &Engine,
        reservation: crate::engine::MediaReservation,
    ) {
        if let Some(ffprobe) = &self.subtitles {
            if let Err(error) = crate::subtitles::generate(
                &self.root,
                &process.output_path,
                process.wall_started_at,
                ffprobe,
            )
            .await
            {
                self.options.delete_original = false;
                store.snack(format!("时间字幕未生成：{error}；本次源文件保留"));
            }
        }
        // Subtitles may probe many files. Only the short reservation/job handoff is locked.
        let filesystem = engine.filesystem_guard().await;
        drop(reservation);
        if self.convert {
            if let Err(error) = postprocess
                .enqueue_recording_locked(
                    &self.root,
                    &process.output_path,
                    &process.rec_id,
                    self.options,
                )
                .await
            {
                store.snack(format!("自动转 MP4 未开始：{error}；原 TS 保留"));
            }
        }
        drop(filesystem);
        if let Some(script) = self.script {
            let room = store
                .get(&process.rec_id)
                .await
                .map(|r| r.streamer_name)
                .unwrap_or_default();
            automation.after_recording(
                script,
                self.root,
                process.output_path.clone(),
                room,
                process.rec_id.clone(),
                postprocess.clone(),
            );
        }
    }
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
        let starting = store.lifecycle();
        let postprocess = crate::postprocess::Postprocessor::new(
            engine.clone(),
            store.clone(),
            ffmpeg.clone(),
            config.clone(),
        );
        let notifications = crate::notifications::Notifications::new(config.clone(), store.clone());
        let automation = crate::automation::Automation::new(config.clone(), store.clone());
        Self {
            notifications,
            automation,
            pacer: crate::pacing::Pacer::default(),
            automatic_pacer: crate::pacing::AutomaticPacer::default(),
            checks: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            schedule_edges: std::sync::Mutex::new(std::collections::HashMap::new()),
            live_sources: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            store,
            engine,
            config,
            resolver,
            ffmpeg,
            recording_enabled,
            storage: crate::storage::Storage::new(stopping.clone()),
            interval_changed: tokio::sync::watch::channel(0).0,
            monitor_requests: std::sync::Mutex::new(std::collections::HashSet::new()),
            monitor_changed: tokio::sync::Notify::new(),
            stopping,
            postprocess,
            conversion_plans: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            background: tokio_util::task::TaskTracker::new(),
            starting,
        }
    }

    /// Only committed interval changes re-arm the timer; no immediate platform request is sent.
    pub fn refresh_interval(&self) {
        self.interval_changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
    /// Queue only persisted task IDs. The monitoring service owns execution and shutdown.
    pub fn request_monitoring(&self, ids: impl IntoIterator<Item = String>) {
        if self.stopping.is_cancelled() {
            return;
        }
        self.monitor_requests
            .lock()
            .expect("monitor requests")
            .extend(ids);
        self.monitor_changed.notify_one();
    }

    async fn monitoring_interval(&self) -> Duration {
        Duration::from_secs(
            self.config
                .read()
                .await
                .get_i64("loop_time_seconds", 300)
                .max(30) as u64,
        )
    }

    pub async fn run(self: Arc<Self>, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        use tokio::time::Instant;
        let mut changed = self.interval_changed.subscribe();
        let mut interval = self.monitoring_interval().await;
        let mut deadlines: std::collections::HashMap<String, Instant> = self
            .store
            .all()
            .await
            .into_iter()
            .filter(|record| record.monitor_status)
            .map(|record| (record.rec_id, Instant::now()))
            .collect();
        for id in deadlines.keys() {
            self.store
                .update(id, |record| {
                    record.check_state = if record.is_recording {
                        "idle"
                    } else {
                        "queued"
                    }
                    .into();
                    record.next_check_at = Some(chrono::Utc::now().timestamp());
                })
                .await;
        }
        loop {
            let deadline = deadlines
                .values()
                .copied()
                .min()
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(86400));
            tokio::select! { biased;
                _ = self.stopping.cancelled() => return,
                _ = shutdown.changed() => return,
                _ = changed.changed() => {
                    interval = self.monitoring_interval().await;
                    for (id, next) in &mut deadlines {
                        *next = Instant::now() + interval;
                        self.store.update(id, |r| { r.check_state = "idle".into(); r.next_check_at = Some(chrono::Utc::now().timestamp() + interval.as_secs() as i64); }).await;
                    }
                    continue;
                },
                _ = self.monitor_changed.notified() => {},
                _ = tokio::time::sleep_until(deadline) => {},
            }
            let requested =
                std::mem::take(&mut *self.monitor_requests.lock().expect("monitor requests"));
            let records = self.store.all().await;
            deadlines.retain(|id, _| records.iter().any(|r| &r.rec_id == id && r.monitor_status));
            for record in &records {
                if record.monitor_status && requested.contains(&record.rec_id) {
                    let now = Instant::now();
                    deadlines
                        .entry(record.rec_id.clone())
                        .and_modify(|due| *due = (*due).min(now))
                        .or_insert(now);
                    self.store
                        .update(&record.rec_id, |r| {
                            if !r.is_recording && r.check_state != "checking" {
                                r.check_state = "queued".into();
                            }
                            r.next_check_at = Some(chrono::Utc::now().timestamp());
                        })
                        .await;
                }
            }
            let now = Instant::now();
            let due = records
                .into_iter()
                .filter(|record| {
                    record.monitor_status
                        && deadlines
                            .get(&record.rec_id)
                            .is_some_and(|next| *next <= now)
                })
                .min_by_key(|record| deadlines[&record.rec_id]);
            let Some(record) = due else { continue };
            if !self.recording_enabled.load(Ordering::SeqCst) || record.is_recording {
                deadlines.insert(record.rec_id.clone(), now + interval);
                self.store
                    .update(&record.rec_id, |r| {
                        r.next_check_at =
                            Some(chrono::Utc::now().timestamp() + interval.as_secs() as i64);
                        if r.is_recording {
                            r.check_state = "idle".into();
                        }
                    })
                    .await;
                continue;
            }
            self.store
                .update(&record.rec_id, |r| {
                    if r.check_state != "checking" {
                        r.check_state = "queued".into();
                    }
                })
                .await;
            let check = async {
                let _permit = self.automatic_pacer.acquire(&self.stopping).await?;
                log::info!(
                    "自动探测开始 task={} platform={} interval={}s",
                    record.rec_id,
                    record.platform_key.as_deref().unwrap_or("custom"),
                    interval.as_secs()
                );
                self.check(record.rec_id.clone()).await
            };
            let result = tokio::select! { biased;
                _ = self.stopping.cancelled() => return,
                _ = shutdown.changed() => return,
                result = check => result,
            };
            if let Err(error) = result {
                log::debug!("检测未完成: {error}");
            }
            // The next ordinary poll is measured per room, after its own completion.
            self.store
                .update(&record.rec_id, |r| {
                    if !self
                        .checks
                        .lock()
                        .expect("active checks")
                        .contains_key(&record.rec_id)
                    {
                        r.check_state = "idle".into();
                    }
                    r.next_check_at = r
                        .monitor_status
                        .then_some(chrono::Utc::now().timestamp() + interval.as_secs() as i64);
                })
                .await;
            deadlines.insert(record.rec_id, Instant::now() + interval);
        }
    }

    /// Detection and manual recording share metadata updates and name persistence.
    pub async fn check(&self, rec_id: String) -> Result<CheckOutcome, String> {
        let _operation = self.begin_check(&rec_id)?;
        if self.stopping.is_cancelled() {
            return Err("应用正在退出".into());
        }
        if self.store.get(&rec_id).await.is_none() {
            return Err(format!("任务不存在: {rec_id}"));
        }
        if let Some(rec) = self.store.get(&rec_id).await {
            if !rec.monitor_status {
                return Ok(CheckOutcome::MonitoringPaused);
            }
            if !crate::schedule::active(&rec)? {
                self.stop_recording(&rec_id).await;
                return Ok(CheckOutcome::OutsideSchedule);
            }
        }
        let (expected, info) = match self.resolve_for(&rec_id).await {
            Ok(resolved) => resolved,
            Err(error) => return Ok(CheckOutcome::Failed(error)),
        };
        self.accept_resolved(&expected, &info, false).await
    }

    /// Accept data from the same official room after an explicit human verification action.
    pub async fn report_kuaishou_access(&self, expected: &crate::model::Recording, error: &str) {
        self.store
            .update_when(
                &expected.rec_id,
                |r| {
                    !self.stopping.is_cancelled()
                        && r.platform_key.as_deref() == Some("kuaishou")
                        && r.url == expected.url
                        && r.quality == expected.quality
                        && r.recording_run == expected.recording_run
                },
                |r| {
                    r.check_error = Some(error.into());
                    r.verification_required =
                        crate::platforms::kuaishou::verification_required(error);
                    r.access_state = crate::platforms::kuaishou::access_state(error).into();
                    r.check_state = "idle".into();
                    r.last_check_at = Some(chrono::Utc::now().timestamp());
                },
            )
            .await;
    }

    /// Accept data from the same official room after an explicit human verification action.
    /// The original URL/quality and normal monitoring/schedule rules still apply.
    pub async fn accept_verified(
        &self,
        expected: &crate::model::Recording,
        info: &crate::resolver::StreamInfo,
    ) -> Result<CheckOutcome, String> {
        let _operation = self.begin_check(&expected.rec_id)?;
        let lifecycle = self.starting.lock().await;
        let current = self
            .store
            .get(&expected.rec_id)
            .await
            .ok_or("验证任务已移除")?;
        if current.platform_key.as_deref() != Some("kuaishou")
            || current.url != expected.url
            || current.quality != expected.quality
        {
            return Err("任务已更新，验证结果未应用".into());
        }
        if !current.monitor_status || !crate::schedule::active(&current)? {
            self.store
                .apply_stream_info(&current.rec_id, info)
                .await
                .map_err(|_| "无法保存验证结果")?;
            return Ok(if current.monitor_status {
                CheckOutcome::OutsideSchedule
            } else {
                CheckOutcome::MonitoringPaused
            });
        }
        drop(lifecycle);
        self.accept_resolved(&current, info, false).await
    }

    async fn accept_resolved(
        &self,
        expected: &crate::model::Recording,
        info: &crate::resolver::StreamInfo,
        manual: bool,
    ) -> Result<CheckOutcome, String> {
        let _starting = self.starting.lock().await;
        let rec_id = expected.rec_id.clone();
        if self.store.get(&rec_id).await.is_none_or(|current| {
            current.url != expected.url
                || current.quality != expected.quality
                || current.recording_run != expected.recording_run
        }) {
            return Err("任务已更新，旧检测结果已丢弃".into());
        }
        if self.stopping.is_cancelled() {
            return Err("应用正在退出".into());
        }
        if self
            .checks
            .lock()
            .expect("active checks")
            .get(&rec_id)
            .is_some_and(|token| token.is_cancelled())
        {
            return Err("检测已取消".into());
        }
        if !manual {
            let record = self
                .store
                .get(&rec_id)
                .await
                .ok_or_else(|| format!("任务不存在: {rec_id}"))?;
            if !record.monitor_status {
                return Ok(CheckOutcome::MonitoringPaused);
            }
            if !crate::schedule::active(&record)? {
                return Ok(CheckOutcome::OutsideSchedule);
            }
        }
        let previous = self.store.get(&rec_id).await.map(|r| r.is_live);
        self.store
            .apply_stream_info(&rec_id, info)
            .await
            .map_err(|error| format!("保存主播名称失败，任务未改动: {error}"))?
            .ok_or_else(|| format!("任务不存在: {rec_id}"))?;
        if let Some(rec) = self.store.get(&rec_id).await {
            if info.is_live {
                self.live_sources.write().await.insert(
                    rec_id.clone(),
                    (rec.url, std::time::Instant::now(), info.clone()),
                );
            } else {
                self.live_sources.write().await.remove(&rec_id);
            }
        }
        if previous.is_some_and(|old| old != info.is_live) {
            if let Some(record) = self.store.get(&rec_id).await {
                self.notifications.changed(&record, info.is_live).await;
            }
        }
        if !info.is_live {
            let was_recording = self.stop_recording_locked(&rec_id).await;
            return if manual {
                Err("该直播间未开播".into())
            } else {
                Ok(if was_recording {
                    CheckOutcome::StreamEnded
                } else {
                    CheckOutcome::Offline
                })
            };
        }
        if self.engine.is_recording(&rec_id).await {
            return Ok(CheckOutcome::AlreadyRecording);
        }
        if !manual
            && self
                .store
                .get(&rec_id)
                .await
                .is_some_and(|r| r.only_notify_no_record == Some(true))
        {
            return Ok(CheckOutcome::NotifyOnly);
        }
        self.start_recording_locked(rec_id, info).await?;
        Ok(CheckOutcome::RecordingStarted)
    }

    /// 启动录制：解析流地址 → 构造输出路径 → 拉起 ffmpeg。
    pub async fn start_recording(
        &self,
        rec_id: String,
        info: &crate::resolver::StreamInfo,
    ) -> Result<(), String> {
        let _starting = self.starting.lock().await;
        self.start_recording_locked(rec_id, info).await
    }

    async fn start_recording_locked(
        &self,
        rec_id: String,
        info: &crate::resolver::StreamInfo,
    ) -> Result<(), String> {
        let result = self.spawn_recording(rec_id.clone(), info).await;
        if let Err(error) = &result {
            if !self.stopping.is_cancelled() && !self.engine.is_recording(&rec_id).await {
                if let Some(record) = self
                    .store
                    .update(&rec_id, |r| {
                        r.is_recording = false;
                        r.recording_run = None;
                        r.speed = None;
                        r.recording_error = Some(error.clone());
                    })
                    .await
                {
                    self.store
                        .snack(format!("{}：录制失败，{error}", record.streamer_name));
                }
            }
        }
        result
    }

    async fn spawn_recording(
        &self,
        rec_id: String,
        info: &crate::resolver::StreamInfo,
    ) -> Result<(), String> {
        if self.stopping.is_cancelled() || !self.recording_enabled.load(Ordering::SeqCst) {
            return Err("已暂停录制或正在退出".into());
        }
        let ffmpeg = {
            let config = self.config.read().await;
            let bundled = config.workspace().user_data_dir.join("ffmpeg/ffmpeg.exe");
            if bundled.is_file() {
                Some(bundled)
            } else {
                self.ffmpeg.clone()
            }
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

        let source_preference = self
            .config
            .read()
            .await
            .get_str("default_live_source", "FLV");
        let selected = if source_preference == "HLS" && !info.m3u8_url.is_empty() {
            Some(info.m3u8_url.clone())
        } else {
            info.pick_record_url(prefer_flv)
        };
        let Some(mut record_url) = selected else {
            return Err("解析结果中没有可用的录制地址".into());
        };

        if self
            .config
            .read()
            .await
            .get_bool("force_https_recording", false)
            && record_url.starts_with("http://")
        {
            record_url = record_url.replacen("http://", "https://", 1);
        }
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
        let mut filename = build_filename(
            &anchor,
            title_for_filename,
            &now.format("%Y-%m-%d_%H-%M-%S").to_string(),
        );

        {
            let config = self.config.read().await;
            let template = config.get_str("custom_filename_template", "");
            if !template.is_empty() {
                filename = crate::engine::sanitize(
                    &template
                        .replace("{anchor_name}", &anchor)
                        .replace("{title}", title_for_filename.unwrap_or(""))
                        .replace("{time}", &now.format("%Y-%m-%d_%H-%M-%S").to_string())
                        .replace("{platform}", &info.platform),
                );
            }
            if config.get_bool("remove_emojis", false) {
                filename = crate::engine::without_emojis(&filename);
            }
            if segment_record {
                filename = format!("{filename}_{}", uuid::Uuid::new_v4());
            }
        }
        let (root, convert_to_mp4, remux_options) = {
            let config = self.config.read().await;
            (
                config.recordings_root(),
                config.get_bool("convert_to_mp4", false) && format.eq_ignore_ascii_case("TS"),
                RemuxOptions::from_config(&config),
            )
        };
        crate::media_safety::require_space(&root, remux_options.minimum_free_bytes, 0)
            .map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let mut output_dir = build_output_dir(
            &root,
            &info.platform,
            &anchor,
            &now.format("%Y-%m-%d").to_string(),
            &folder_opts,
        );

        if self
            .config
            .read()
            .await
            .get_bool("folder_name_title", false)
            && !info.title.is_empty()
        {
            output_dir.push(crate::engine::sanitize(&info.title));
        }
        std::fs::create_dir_all(&output_dir).map_err(|e| e.to_string())?;
        let base_path = output_dir
            .join(filename)
            .to_string_lossy()
            .replace('\\', "/");
        let save_path = if segment_record {
            with_segment_suffix(&base_path, &format, true)
        } else {
            format!("{base_path}.{}", format.to_ascii_lowercase())
        };

        let root_real = root.canonicalize().map_err(|e| e.to_string())?;
        let output_real = output_dir.canonicalize().map_err(|e| e.to_string())?;
        let relative_parent = output_real
            .strip_prefix(&root_real)
            .map_err(|_| "录制目录越界")?;
        let candidate = std::path::Path::new(&save_path)
            .file_name()
            .ok_or("录像文件名无效")?;
        let save_path = crate::media_safety::unique_output(&root, &relative_parent.join(candidate))
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .to_string();
        let options = RecordOptions {
            record_url,
            save_path,
            format: format.clone(),
            segment_record,
            segment_time: rec.segment_time.clone(),
            headers: recording_headers(rec.platform_key.as_deref()),
            proxy: recording_proxy(&*self.config.read().await, rec.platform_key.as_deref())?,
            platform_key: rec.platform_key.clone(),
            video_bitrate: rec.video_bitrate,
            is_overseas: is_overseas_platform(rec.platform_key.as_deref()),
        };

        let output_dir_str = output_dir.to_string_lossy().to_string();

        let script = {
            let config = self.config.read().await;
            if config.get_bool("execute_custom_script", false) {
                Some(crate::automation::ScriptSpec::parse(
                    &config.get_str("custom_script_command", ""),
                )?)
            } else {
                None
            }
        };
        let subtitles = if self
            .config
            .read()
            .await
            .get_bool("generate_time_subtitle_file", false)
        {
            Some(
                ffmpeg
                    .as_ref()
                    .and_then(|p| crate::paths::adjacent_ffprobe(p))
                    .ok_or("时间字幕需要 FFmpeg 同目录的 ffprobe")?,
            )
        } else {
            None
        };
        let process = if rec.flv_use_direct_download == Some(true) {
            self.engine.start_direct(&rec_id, &options).await?
        } else {
            self.engine
                .start(
                    ffmpeg.as_deref().ok_or("未找到 ffmpeg，请先安装工具")?,
                    &rec_id,
                    &options,
                )
                .await?
        };
        let run_id = process.run_id;
        self.conversion_plans.lock().await.insert(
            run_id,
            FinishPlan {
                root: root.clone(),
                convert: convert_to_mp4,
                options: remux_options,
                script,
                subtitles,
            },
        );

        self.store
            .update(&rec_id, move |r| {
                r.is_recording = true;
                r.recorded_seconds = 0.0;
                r.recording_run = Some(run_id);
                r.recording_error = None;
                r.recording_dir = Some(output_dir_str.clone());
                r.speed = Some("0 KB/s".into());
            })
            .await;

        // 速率采样：ffmpeg 以 -loglevel error 运行，stderr 没有进度行，
        // 只能按产出目录的字节增量计算（Python 侧该字段一直是静态占位符，此处改为真实值）。
        self.spawn_speed_sampler(process.clone());

        // 退出监听：ffmpeg 读完流会自行结束，必须感知并回写状态，
        // 否则任务会永远停留在「录制中」（对应 Python 每秒轮询 returncode 的处理）。
        self.spawn_exit_watcher(process);

        self.store.snack(format!("{anchor}: 开始录制"));
        Ok(())
    }

    /// Publish recording completion immediately, then recheck only this room's live status.
    /// EOF itself is not proof that a platform room is offline.
    fn spawn_exit_watcher(&self, process: Arc<RecorderProcess>) {
        let store = self.store.clone();
        let engine = self.engine.clone();
        let starting = self.starting.clone();
        let postprocess = self.postprocess.clone();
        let conversion_plans = self.conversion_plans.clone();
        let automation = self.automation.clone();
        let stopping = self.stopping.clone();
        let room_check = self.room_check();
        let live_sources = self.live_sources.clone();
        let background = self.background.clone();
        self.background.spawn(async move {
            loop {
                tokio::select! { biased;
                    _ = stopping.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                }
                let _starting = tokio::select! { biased;
                    _ = stopping.cancelled() => return,
                    guard = starting.lock() => guard,
                };
                let _filesystem = engine.filesystem_guard().await;
                match engine.poll_state(&process).await {
                    crate::engine::ProcessState::Running => continue,
                    crate::engine::ProcessState::Exited { code, error } => {
                        let refresh_needed = code == Some(0) && error.is_none();
                        let plan = conversion_plans.lock().await.remove(&process.run_id);
                        log::info!("录制 {} 已结束（ffmpeg 退出码 {code:?}）", process.rec_id);
                        let updated = store
                            .update_for_run(&process.rec_id, process.run_id, |r| {
                                r.is_recording = false;
                                r.recorded_seconds = process.recorded_seconds();
                                r.last_duration = Some(r.recorded_seconds);
                                // Retain the attempt identity until the read-only check is applied.
                                // Stop/edit/restart will invalidate it before an old reply can commit.
                                if !refresh_needed {
                                    r.recording_run = None;
                                }
                                r.speed = None;
                                r.recording_error = error.clone();
                            })
                            .await;
                        if let Some(plan) = plan {
                            plan.spawn(
                                process.clone(),
                                engine.clone(),
                                postprocess.clone(),
                                automation.clone(),
                                store.clone(),
                                &background,
                            );
                        }
                        if let (Some(record), Some(error)) = (&updated, error) {
                            store.snack(format!("{}：录制失败，{error}", record.streamer_name));
                        }
                        if updated.is_some() {
                            live_sources.write().await.remove(&process.rec_id);
                            drop(_filesystem);
                            drop(_starting);
                            if refresh_needed {
                                room_check
                                    .refresh_finished(&process.rec_id, process.run_id)
                                    .await;
                            }
                        }
                        return;
                    }
                    crate::engine::ProcessState::Unknown => return,
                }
            }
        });
    }

    /// 每 2 秒采样一次产出目录大小，换算为速率写回任务状态；录制结束后自行退出。
    fn spawn_speed_sampler(&self, process: Arc<RecorderProcess>) {
        let store = self.store.clone();
        let engine = self.engine.clone();
        let stopping = self.stopping.clone();
        let storage = self.storage.clone();
        self.background.spawn(async move{
            let mut previous=0;let mut sampled_at=tokio::time::Instant::now();
            loop{
                tokio::select!{biased;_=stopping.cancelled()=>return,_=tokio::time::sleep(Duration::from_secs(2))=>{}}
                if !engine.is_current(&process).await{return;}
                let pattern=process.output_path.clone();
                let current=match storage.blocking(move|cancel|{crate::storage::check_cancel(&cancel)?;crate::media_safety::output_bytes(&pattern)}).await{Ok(n)=>n,Err(e)=>{log::warn!("录制大小统计失败: {e}");continue;}};
                let seconds=sampled_at.elapsed().as_secs_f64().max(0.001);sampled_at=tokio::time::Instant::now();
                let speed=format!("{:.0} KB/s",current.saturating_sub(previous) as f64/seconds/1024.0);previous=current;
                process.observe_output_bytes(current);
                let elapsed=process.recorded_seconds();
                store.update_when(&process.rec_id, |r| r.is_recording && r.recording_run == Some(process.run_id), move|r|{r.speed=Some(speed);r.recorded_seconds=elapsed;}).await;
            }
        });
    }

    /// 手动开始录制：忽略监控开关，立即解析并开录。
    /// 未开播时返回错误——对应 Python `recording_button_on_click` 的「该直播间未开播」提示。
    pub async fn force_start(&self, rec_id: String) -> Result<CheckOutcome, String> {
        let _operation = self.begin_check(&rec_id)?;
        let (expected, info) = self.resolve_for(&rec_id).await?;
        self.accept_resolved(&expected, &info, true).await
    }

    /// All checks, including post-recording refreshes, share one resolver and pacer.
    fn room_check(&self) -> RoomCheck {
        RoomCheck {
            store: self.store.clone(),
            config: self.config.clone(),
            resolver: self.resolver.clone(),
            pacer: self.pacer.clone(),
            automatic_pacer: self.automatic_pacer.clone(),
            checks: self.checks.clone(),
            stopping: self.stopping.clone(),
            notifications: self.notifications.clone(),
        }
    }
    async fn resolve_for(
        &self,
        rec_id: &str,
    ) -> Result<(crate::model::Recording, crate::resolver::StreamInfo), String> {
        self.room_check().resolve_for(rec_id).await
    }
    pub async fn enforce_windows(self: &Arc<Self>) {
        self.automation.tick().await;
        let records = self.store.all().await;
        self.schedule_edges
            .lock()
            .expect("schedule edges")
            .retain(|id, _| {
                records.iter().any(|r| {
                    &r.rec_id == id && r.scheduled_recording == Some(true) && r.monitor_status
                })
            });
        for record in records {
            if record.scheduled_recording != Some(true) {
                continue;
            }
            let active = crate::schedule::active(&record).unwrap_or(false);
            if record.is_recording && !active {
                self.stop_recording(&record.rec_id).await;
            }
            if record.monitor_status {
                let previous = self
                    .schedule_edges
                    .lock()
                    .expect("schedule edges")
                    .insert(record.rec_id.clone(), active)
                    .unwrap_or(false);
                if active && !previous && !record.is_recording {
                    self.request_monitoring([record.rec_id]);
                }
            }
        }
    }

    /// Persist the transition before publishing it or touching an active recorder.
    pub async fn toggle_monitor(&self, rec_id: &str) -> std::io::Result<crate::model::Recording> {
        let _starting = self.starting.lock().await;
        let record = self.store.toggle_monitor_locked(rec_id).await?;
        if !record.monitor_status {
            self.stop_recording_locked(rec_id).await;
        }
        self.request_monitoring([rec_id.to_owned()]);
        Ok(self.store.get(rec_id).await.unwrap_or(record))
    }

    /// 停止录制并更新状态。
    pub async fn stop_recording(&self, rec_id: &str) -> bool {
        let _starting = self.starting.lock().await;
        self.stop_recording_locked(rec_id).await
    }

    async fn stop_recording_locked(&self, rec_id: &str) -> bool {
        if let Some(token) = self.checks.lock().expect("active checks").get(rec_id) {
            token.cancel();
        }
        let _filesystem = self.engine.filesystem_guard().await;
        let process = self.engine.process(rec_id).await;
        let stopped = self.engine.stop(rec_id, 15).await;
        let elapsed = process.as_ref().map(|p| p.recorded_seconds());
        self.store
            .update(rec_id, |r| {
                r.is_recording = false;
                if let Some(elapsed) = elapsed {
                    r.recorded_seconds = elapsed;
                    r.last_duration = Some(elapsed);
                }
                r.recording_run = None;
                r.recording_error = None;
                r.speed = None;
                r.check_state = "idle".into();
                if !r.monitor_status {
                    r.next_check_at = None;
                }
            })
            .await;
        if let Some(process) = process {
            let plan = self.conversion_plans.lock().await.remove(&process.run_id);
            if let Some(plan) = plan {
                plan.spawn(
                    process,
                    self.engine.clone(),
                    self.postprocess.clone(),
                    self.automation.clone(),
                    self.store.clone(),
                    &self.background,
                );
            }
        }
        stopped
    }

    pub async fn stop_all_for_shutdown(&self) {
        let _starting = self.starting.lock().await;
        for id in self.engine.active_ids().await {
            self.stop_recording_locked(&id).await;
        }
        self.engine.stop_all(10).await;
    }

    pub async fn finish_background(&self) {
        let _starting = self.starting.lock().await;
        self.background.close();
        drop(_starting);
        self.background.wait().await;
        self.storage.shutdown().await;
        self.postprocess.shutdown(Duration::from_secs(30)).await;
        self.automation.shutdown().await;
        self.notifications.shutdown().await;
    }

    fn begin_check(&self, rec_id: &str) -> Result<CheckLease, String> {
        self.room_check().begin_check(rec_id)
    }
    /// Preview reuses a fresh verified source and never performs a background room request.
    pub async fn preview_input(&self, rec_id: &str) -> Result<crate::preview::LiveInput, String> {
        let record = self.store.get(rec_id).await.ok_or("任务不存在")?;
        let cache = self.live_sources.read().await;
        let (url, when, info) = cache.get(rec_id).ok_or("请先检测直播状态，再预览直播源")?;
        if &record.url != url || !record.is_live || when.elapsed() > Duration::from_secs(300) {
            return Err("直播源已过期，请先手动检测状态".into());
        }
        let config = self.config.read().await;
        let prefer_flv = config.get_str("default_live_source", "FLV") != "HLS";
        let mut url = if !prefer_flv && !info.m3u8_url.is_empty() {
            info.m3u8_url.clone()
        } else {
            info.pick_record_url(prefer_flv).ok_or("当前直播源不可用")?
        };
        if config.get_bool("force_https_recording", false) && url.starts_with("http://") {
            url = url.replacen("http://", "https://", 1);
        }
        Ok(crate::preview::LiveInput {
            url,
            headers: recording_headers(record.platform_key.as_deref()),
            proxy: recording_proxy(&config, record.platform_key.as_deref())?,
        })
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

        if let Err(error) = crate::media_safety::require_space(
            &root,
            (threshold * 1024.0 * 1024.0 * 1024.0) as u64,
            0,
        ) {
            self.recording_enabled.store(false, Ordering::Relaxed);
            self.store.snack(format!("录制空间保护：{error}"));
            for id in self.engine.active_ids().await {
                self.stop_recording(&id).await;
            }
            return Err(error.to_string());
        }

        self.recording_enabled.store(true, Ordering::Relaxed);
        Ok(())
    }
}

/// 海外平台使用更大的缓冲配置（与 Python is_overseas 取向一致）。

#[derive(Clone)]
struct RoomCheck {
    store: Store,
    config: Arc<RwLock<ConfigStore>>,
    resolver: Resolver,
    pacer: crate::pacing::Pacer,
    automatic_pacer: crate::pacing::AutomaticPacer,
    checks: Arc<
        std::sync::Mutex<std::collections::HashMap<String, tokio_util::sync::CancellationToken>>,
    >,
    stopping: tokio_util::sync::CancellationToken,
    notifications: crate::notifications::Notifications,
}
impl RoomCheck {
    async fn check_phase(&self, expected: &crate::model::Recording, phase: &str) {
        self.store
            .update_when(
                &expected.rec_id,
                |record| {
                    record.url == expected.url
                        && record.quality == expected.quality
                        && record.recording_run == expected.recording_run
                        && record.check_state != phase
                },
                |record| {
                    record.check_state = phase.into();
                    if phase == "checking" {
                        record.last_check_at = Some(chrono::Utc::now().timestamp());
                    }
                },
            )
            .await;
    }

    fn begin_check(&self, rec_id: &str) -> Result<CheckLease, String> {
        if self.stopping.is_cancelled() {
            return Err("应用正在退出".into());
        }
        let mut checks = self.checks.lock().expect("active checks");
        if checks.contains_key(rec_id) {
            return Err("此任务正在检测，请勿重复请求".into());
        }
        checks.insert(rec_id.to_owned(), self.stopping.child_token());
        Ok(CheckLease {
            id: rec_id.to_owned(),
            checks: self.checks.clone(),
        })
    }
    /// 解析直播流（check 与 force_start 共用）。
    async fn resolve_for(
        &self,
        rec_id: &str,
    ) -> Result<(crate::model::Recording, crate::resolver::StreamInfo), String> {
        let Some(rec) = self.store.get(rec_id).await else {
            return Err(format!("任务不存在: {rec_id}"));
        };

        let key = rec.platform_key.as_deref().unwrap_or("custom");
        let stop = self
            .checks
            .lock()
            .expect("active checks")
            .get(rec_id)
            .cloned()
            .unwrap_or_else(|| self.stopping.clone());
        self.check_phase(&rec, "queued").await;
        let work = async {
            let (proxy, quality, maximum, spacing) = {
                let config = self.config.read().await;
                let proxy = recording_proxy(&config, rec.platform_key.as_deref())?;
                let quality = rec
                    .quality
                    .clone()
                    .unwrap_or_else(|| config.get_str("record_quality", "OD"));
                (
                    proxy,
                    quality,
                    config
                        .get_i64("platform_max_concurrent_requests", 3)
                        .clamp(1, 16) as usize,
                    config.get_i64("platform_request_interval", 3).clamp(0, 300) as u64,
                )
            };
            let _permit = self
                .pacer
                .acquire(key, maximum, Duration::from_secs(spacing), &stop)
                .await?;
            self.check_phase(&rec, "checking").await;
            let cookie = self
                .config
                .read()
                .await
                .cookies_for_resolver()
                .map_err(|_| "Cookie 配置无法读取，未发送匿名请求")?
                .get(key)
                .cloned();
            self.resolver
                .resolve(ResolveRequest {
                    account: self
                        .config
                        .read()
                        .await
                        .account(key)
                        .map_err(|_| "账号配置无法读取，未发送匿名请求")?,
                    url: rec.url.clone(),
                    quality: Some(quality),
                    proxy,
                    cookie,
                    platform: rec.platform_key.clone(),
                })
                .await
        };
        let result = tokio::select! {
            biased;
            _=stop.cancelled()=>Err("检测已取消".into()),
            result=tokio::time::timeout(Duration::from_secs(60),work)=>result.unwrap_or_else(|_| Err("检测排队或解析超过 60 秒，请稍后重试".into())),
        };
        self.check_phase(&rec, "idle").await;
        if stop.is_cancelled() {
            return Err("检测已取消".into());
        }
        if self.store.get(rec_id).await.is_none_or(|current| {
            current.url != rec.url
                || current.quality != rec.quality
                || current.recording_run != rec.recording_run
        }) {
            return Err("任务已更新，旧检测结果已丢弃".into());
        }
        self.pacer.result(key, &result);
        if let Err(error) = &result {
            let verification =
                key == "kuaishou" && crate::platforms::kuaishou::verification_required(error);
            let page_check =
                key == "kuaishou" && crate::platforms::kuaishou::page_check_required(error);
            let changed = rec.check_error.as_ref() != Some(error)
                || rec.verification_required != verification;
            let updated = self
                .store
                .update_when(
                    rec_id,
                    |record| {
                        !stop.is_cancelled()
                            && record.url == rec.url
                            && record.quality == rec.quality
                            && record.recording_run == rec.recording_run
                    },
                    |record| {
                        record.check_error = Some(error.clone());
                        record.verification_required = verification;
                        record.access_state = if key == "kuaishou" {
                            crate::platforms::kuaishou::access_state(error).into()
                        } else {
                            String::new()
                        };
                        record.check_state = "idle".into();
                        record.last_check_at = Some(chrono::Utc::now().timestamp());
                    },
                )
                .await;
            if updated.is_none() {
                return Err("任务已更新，旧检测结果已丢弃".into());
            }
            if changed {
                // Never write cookie values, request URLs or platform response bodies to the log.
                log::warn!(
                    "平台检测未完成 task={} platform={} verification={}",
                    rec_id,
                    key,
                    verification
                );
            }
            if page_check {
                self.store.emit(
                    "kuaishouVerificationRequired",
                    serde_json::json!({"recId":rec_id}),
                );
            }
        }
        result.map(|info| (rec, info))
    }

    async fn refresh_finished(&self, rec_id: &str, run_id: uuid::Uuid) {
        let Ok(_permit) = self.automatic_pacer.acquire(&self.stopping).await else {
            return;
        };
        let _operation = loop {
            if self.stopping.is_cancelled()
                || self.store.get(rec_id).await.is_none_or(|record| {
                    record.recording_run != Some(run_id) || record.is_recording
                })
            {
                return;
            }
            if let Ok(operation) = self.begin_check(rec_id) {
                break operation;
            }
            tokio::select! {
                biased;
                _ = self.stopping.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        };
        let Some(expected) = self.store.get(rec_id).await else {
            return;
        };
        if expected.recording_run != Some(run_id) || expected.is_recording {
            return;
        }
        let result = self.resolve_for(rec_id).await.map(|(_, info)| info);
        let cancelled = self.stopping.is_cancelled()
            || self
                .checks
                .lock()
                .expect("active checks")
                .get(rec_id)
                .is_none_or(|token| token.is_cancelled());
        if cancelled {
            return;
        }
        self.apply_finished(&expected, &result).await;
    }

    async fn apply_finished(
        &self,
        expected: &crate::model::Recording,
        result: &Result<crate::resolver::StreamInfo, String>,
    ) {
        let updated = self
            .store
            .update_when(
                &expected.rec_id,
                |record| {
                    !self.stopping.is_cancelled()
                        && expected.recording_run.is_some()
                        && record.recording_run == expected.recording_run
                        && record.url == expected.url
                        && record.quality == expected.quality
                        && !record.is_recording
                },
                |record| {
                    record.recording_run = None;
                    record.check_state = "idle".into();
                    record.last_check_at = Some(chrono::Utc::now().timestamp());
                    match result {
                        Ok(info) => {
                            record.last_success_at = record.last_check_at;
                            record.is_live = info.is_live;
                            record.live_title = (info.is_live && !info.title.is_empty())
                                .then(|| info.title.clone());
                            record.check_error = None;
                            record.verification_required = false;
                            record.access_state.clear();
                            if !info.is_live {
                                record.recording_error = None;
                            }
                        }
                        Err(error) => {
                            record.check_error = Some(error.clone());
                            record.verification_required = record.platform_key.as_deref()
                                == Some("kuaishou")
                                && crate::platforms::kuaishou::verification_required(error);
                            record.access_state =
                                if record.platform_key.as_deref() == Some("kuaishou") {
                                    crate::platforms::kuaishou::access_state(error).into()
                                } else {
                                    String::new()
                                };
                        }
                    }
                },
            )
            .await;
        if let Some(record) = updated {
            if result.is_ok() && record.is_live != expected.is_live {
                let mut notification = record.clone();
                if !record.is_live {
                    notification.live_title = expected.live_title.clone();
                }
                self.notifications
                    .changed(&notification, record.is_live)
                    .await;
            }
            log::info!(
                "录制结束后的状态复核 task={} readable={} live={}",
                record.rec_id,
                result.is_ok(),
                record.is_live
            );
        }
    }
}

fn is_overseas_platform(platform_key: Option<&str>) -> bool {
    const OVERSEAS: &[&str] = &[
        "tiktok", "soop", "pandatv", "winktv", "flextv", "popkontv", "twitch", "liveme",
        "showroom", "chzzk", "shopee", "youtube", "langlive", "bigo", "blued",
    ];
    platform_key
        .map(|key| OVERSEAS.contains(&key))
        .unwrap_or(false)
}

/// Query the Windows filesystem directly; no console subprocess or locale-dependent parsing.
#[cfg(test)]
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

/// Shared proxy selection for platform requests, recording and the human verification window.
pub fn recording_proxy(
    config: &ConfigStore,
    platform: Option<&str>,
) -> Result<Option<String>, String> {
    if !config.get_bool("enable_proxy", false) {
        return Ok(None);
    }
    let platforms = config.get_str("default_platform_with_proxy", "");
    let selected = platforms
        .split([',', '，'])
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(crate::platforms::catalog::canonical_key)
        .collect::<Vec<_>>();
    let key = crate::platforms::catalog::canonical_key(platform.unwrap_or("custom"));
    if !selected.is_empty() && !selected.contains(&key) {
        return Ok(None);
    }
    let address = config.get_str("proxy_address", "");
    if address.trim().is_empty() {
        return Err("已启用代理但地址为空，未直接联网".into());
    }
    let parsed = reqwest::Url::parse(&address).map_err(|_| "代理地址无效，未直接联网")?;
    if !matches!(parsed.scheme(), "http" | "https" | "socks5" | "socks5h")
        || parsed.host_str().is_none()
    {
        return Err("代理协议无效，未直接联网".into());
    }
    Ok(Some(address))
}
fn recording_headers(key: Option<&str>) -> Option<String> {
    let referer = match key.unwrap_or("") {
        "bilibili" => "https://live.bilibili.com/",
        "douyin" => "https://live.douyin.com/",
        "kuaishou" => "https://live.kuaishou.com/",
        "huya" => "https://www.huya.com/",
        "douyu" => "https://www.douyu.com/",
        _ => return None,
    };
    Some(format!("Referer: {referer}\r\n"))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Recording;
    use crate::paths::Workspace;

    async fn finished_recording_fixture() -> (tempfile::TempDir, crate::api::ApiState, Recording) {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::api::bootstrap(Workspace::from_repo_root(dir.path()))
            .await
            .unwrap();
        state
            .config
            .write()
            .await
            .update_user_config(
                serde_json::json!({"loop_time_seconds":"4500","platform_request_interval":"0"})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        let mut record = Recording::new(
            "finished".into(),
            "http://127.0.0.1:1/fixture.ts".into(),
            "Fixture".into(),
        );
        record.platform_key = Some("custom".into());
        record.monitor_status = true;
        record.is_live = true;
        record.is_recording = false;
        record.live_title = Some("Previous live title".into());
        record.recorded_seconds = 42.0;
        record.last_duration = Some(42.0);
        record.recording_run = Some(uuid::Uuid::new_v4());
        record.recording_error = Some("Previous transport error".into());
        state.store.insert(vec![record.clone()]).await.unwrap();
        (dir, state, record)
    }

    #[tokio::test]
    async fn finished_recording_refresh_uses_confirmed_state_and_keeps_monitoring_intent() {
        for (monitored, outcome) in [
            (
                true,
                Ok(crate::resolver::StreamInfo {
                    is_live: false,
                    ..Default::default()
                }),
            ),
            (
                false,
                Ok(crate::resolver::StreamInfo {
                    is_live: false,
                    ..Default::default()
                }),
            ),
            (
                true,
                Ok(crate::resolver::StreamInfo {
                    is_live: true,
                    title: "Still live".into(),
                    ..Default::default()
                }),
            ),
            (true, Err("平台请求超时".to_string())),
        ] {
            let (_dir, state, mut expected) = finished_recording_fixture().await;
            expected.monitor_status = monitored;
            state
                .store
                .update("finished", |record| record.monitor_status = monitored)
                .await;
            state
                .scheduler
                .room_check()
                .apply_finished(&expected, &outcome)
                .await;
            let current = state.store.get("finished").await.unwrap();
            assert_eq!(current.monitor_status, monitored);
            assert_eq!(current.check_state, "idle");
            assert!(current.last_check_at.is_some());
            assert_eq!(current.last_success_at.is_some(), outcome.is_ok());
            assert!(!current.is_recording);
            assert_eq!(current.last_duration, Some(42.0));
            assert_eq!(current.recorded_seconds, 42.0);
            assert!(current.recording_run.is_none());
            match outcome {
                Ok(info) => {
                    assert_eq!(current.is_live, info.is_live);
                    assert_eq!(current.live_title, info.is_live.then_some(info.title));
                    assert!(current.check_error.is_none());
                    if !info.is_live {
                        assert!(current.recording_error.is_none());
                    }
                }
                Err(error) => {
                    assert!(
                        current.is_live,
                        "an unreadable room is not evidence of offline"
                    );
                    assert_eq!(current.check_error, Some(error));
                    assert!(current.recording_error.is_some());
                }
            }
            assert!(
                state.engine.active_ids().await.is_empty(),
                "status refresh never restarts recording"
            );
            assert_eq!(
                state.config.read().await.get_i64("loop_time_seconds", 0),
                4500
            );
            crate::api::shutdown(&state).await.unwrap();
        }
    }

    #[tokio::test]
    async fn finished_recording_refresh_cannot_overwrite_replacement_edits_or_cancellation() {
        for mutation in ["replacement", "url", "quality", "stopped", "cancelled"] {
            let (_dir, state, expected) = finished_recording_fixture().await;
            state
                .store
                .update("finished", |record| match mutation {
                    "replacement" => {
                        record.recording_run = Some(uuid::Uuid::new_v4());
                        record.is_recording = true;
                    }
                    "url" => record.url = "http://127.0.0.1:1/replacement.ts".into(),
                    "quality" => record.quality = Some("HD".into()),
                    "stopped" => record.recording_run = None,
                    _ => {}
                })
                .await;
            if mutation == "cancelled" {
                state.scheduler.stopping.cancel();
            }
            let before = state.store.get("finished").await.unwrap();
            let mut events = state.store.subscribe();
            state
                .scheduler
                .room_check()
                .apply_finished(&expected, &Ok(crate::resolver::StreamInfo::default()))
                .await;
            assert_eq!(state.store.get("finished").await.unwrap(), before);
            assert!(events.try_recv().is_err());
            crate::api::shutdown(&state).await.unwrap();
        }
    }

    #[tokio::test]
    async fn finished_recording_refresh_reuses_the_resolver_without_waiting_for_the_interval() {
        let (_dir, state, expected) = finished_recording_fixture().await;
        state
            .scheduler
            .room_check()
            .refresh_finished("finished", expected.recording_run.unwrap())
            .await;
        let current = state.store.get("finished").await.unwrap();
        assert!(
            current.is_live,
            "a readable direct source stays live without starting a recorder"
        );
        assert!(!current.is_recording && current.recording_run.is_none());
        assert!(current.live_title.is_none() && current.check_error.is_none());
        assert!(state.engine.active_ids().await.is_empty());
        crate::api::shutdown(&state).await.unwrap();
    }

    #[tokio::test]
    async fn confirmed_offline_updates_name_and_clears_only_recording_state() {
        for manual in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let workspace = Workspace::from_repo_root(dir.path());
            workspace.ensure_ready().unwrap();
            let store = Store::new(workspace.clone());
            let mut rec = Recording::new(
                "offline".into(),
                "https://media.invalid/live.flv".into(),
                String::new(),
            );
            rec.is_live = true;
            rec.recording_error = Some("上次拉流失败".into());
            store.insert(vec![rec]).await.unwrap();
            let resolver = Resolver::new();
            let scheduler = Scheduler::new(
                store.clone(),
                Engine::new(),
                Arc::new(RwLock::new(ConfigStore::load(workspace).unwrap())),
                resolver.clone(),
                None,
                Arc::new(AtomicBool::new(true)),
            );
            let result = scheduler
                .accept_resolved(
                    &store.get("offline").await.unwrap(),
                    &crate::resolver::StreamInfo {
                        anchor_name: "已下播主播".into(),
                        is_live: false,
                        ..Default::default()
                    },
                    manual,
                )
                .await;
            if manual {
                assert!(result.unwrap_err().contains("未开播"));
            } else {
                assert_eq!(result.unwrap(), CheckOutcome::Offline);
            }
            let current = store.get("offline").await.unwrap();
            assert_eq!(current.streamer_name, "已下播主播");
            assert!(!current.is_live && !current.is_recording);
            assert!(current.recording_error.is_none() && current.live_title.is_none());
            resolver.shutdown().await;
            scheduler.finish_background().await;
        }
    }

    #[tokio::test]
    async fn delayed_resolved_source_cannot_apply_after_an_edit() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::from_repo_root(dir.path());
        workspace.ensure_ready().unwrap();
        let store = Store::new(workspace.clone());
        let expected = Recording::new(
            "edited".into(),
            "https://media.invalid/old.flv".into(),
            "Original".into(),
        );
        store.insert(vec![expected.clone()]).await.unwrap();
        let resolver = Resolver::new();
        let scheduler = Scheduler::new(
            store.clone(),
            Engine::new(),
            Arc::new(RwLock::new(ConfigStore::load(workspace).unwrap())),
            resolver.clone(),
            None,
            Arc::new(AtomicBool::new(true)),
        );
        let lifecycle = store.lifecycle_guard().await;
        let info = crate::resolver::StreamInfo {
            anchor_name: "Stale source".into(),
            is_live: false,
            ..Default::default()
        };
        let mut pending = Box::pin(scheduler.accept_resolved(&expected, &info, false));
        assert!(futures::poll!(pending.as_mut()).is_pending());
        store
            .edit_many_locked(&lifecycle, &["edited".into()], |record| {
                record.url = "https://media.invalid/new.flv".into()
            })
            .await
            .unwrap();
        let current = store.get("edited").await.unwrap();
        drop(lifecycle);
        assert!(pending.await.unwrap_err().contains("旧检测结果已丢弃"));
        assert_eq!(store.get("edited").await.unwrap(), current);
        resolver.shutdown().await;
        scheduler.finish_background().await;
    }

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
