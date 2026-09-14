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

pub struct Scheduler {
    pacer: crate::pacing::Pacer,
    checks: Arc<
        std::sync::Mutex<std::collections::HashMap<String, tokio_util::sync::CancellationToken>>,
    >,
    schedule_edges: std::sync::Mutex<std::collections::HashMap<String, bool>>,
    live_sources: tokio::sync::RwLock<
        std::collections::HashMap<
            String,
            (String, std::time::Instant, crate::resolver::StreamInfo),
        >,
    >,
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
    async fn dispatch(
        mut self,
        process: &Arc<RecorderProcess>,
        postprocess: &crate::postprocess::Postprocessor,
        automation: &crate::automation::Automation,
        store: &Store,
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
            checks: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            schedule_edges: std::sync::Mutex::new(std::collections::HashMap::new()),
            live_sources: tokio::sync::RwLock::new(std::collections::HashMap::new()),
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
            starting: Arc::new(tokio::sync::Mutex::new(())),
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
        use futures::StreamExt;
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
                    for next in deadlines.values_mut() { *next = Instant::now() + interval; }
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
                    deadlines.insert(record.rec_id.clone(), Instant::now());
                }
            }
            let now = Instant::now();
            let due: Vec<_> = records
                .into_iter()
                .filter(|record| {
                    record.monitor_status
                        && deadlines
                            .get(&record.rec_id)
                            .is_some_and(|next| *next <= now)
                })
                .collect();
            for record in &due {
                deadlines.insert(record.rec_id.clone(), now + interval);
            }
            if !self.recording_enabled.load(Ordering::SeqCst) {
                continue;
            }
            let checks = futures::stream::iter(due)
                .map(|record| {
                    let scheduler = self.clone();
                    async move {
                        if let Err(error) = scheduler.check(record.rec_id.clone()).await {
                            log::debug!("检测未完成: {error}");
                        }
                        (record.rec_id, Instant::now() + interval)
                    }
                })
                .buffer_unordered(16)
                .collect::<Vec<_>>();
            let completed = tokio::select! { biased;
                _ = self.stopping.cancelled() => return,
                _ = shutdown.changed() => return,
                result = checks => result,
            };
            // A new task's first check never accelerates another room's existing deadline.
            for (id, next) in completed {
                deadlines.insert(id, next);
            }
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
        let info = match self.resolve_for(&rec_id).await {
            Ok(info) => info,
            Err(error) => return Ok(CheckOutcome::Failed(error)),
        };
        self.accept_resolved(rec_id, &info, false).await
    }

    async fn accept_resolved(
        &self,
        rec_id: String,
        info: &crate::resolver::StreamInfo,
        manual: bool,
    ) -> Result<CheckOutcome, String> {
        let _starting = self.starting.lock().await;
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

    /// Process exit ends recording, not the last verified platform live status.
    fn spawn_exit_watcher(&self, process: Arc<RecorderProcess>) {
        let store = self.store.clone();
        let engine = self.engine.clone();
        let starting = self.starting.clone();
        let postprocess = self.postprocess.clone();
        let conversion_plans = self.conversion_plans.clone();
        let automation = self.automation.clone();
        let stopping = self.stopping.clone();
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
                        if let Some(plan) = conversion_plans.lock().await.remove(&process.run_id) {
                            plan.dispatch(&process, &postprocess, &automation, &store)
                                .await;
                        }
                        log::info!("录制 {} 已结束（ffmpeg 退出码 {code:?}）", process.rec_id);
                        let updated = store
                            .update_for_run(&process.rec_id, process.run_id, |r| {
                                r.is_recording = false;
                                r.recorded_seconds = process.started_at.elapsed().as_secs_f64();
                                r.last_duration = Some(r.recorded_seconds);
                                r.recording_run = None;
                                r.speed = None;
                                r.recording_error = error.clone();
                            })
                            .await;
                        if let (Some(record), Some(error)) = (updated, error) {
                            store.snack(format!("{}：录制失败，{error}", record.streamer_name));
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
                let elapsed=process.started_at.elapsed().as_secs_f64();
                store.update_for_run(&process.rec_id,process.run_id,move|r|{r.speed=Some(speed);r.recorded_seconds=elapsed;}).await;
            }
        });
    }

    /// 手动开始录制：忽略监控开关，立即解析并开录。
    /// 未开播时返回错误——对应 Python `recording_button_on_click` 的「该直播间未开播」提示。
    pub async fn force_start(&self, rec_id: String) -> Result<CheckOutcome, String> {
        let _operation = self.begin_check(&rec_id)?;
        let info = self.resolve_for(&rec_id).await?;
        self.accept_resolved(rec_id, &info, true).await
    }

    /// 解析直播流（check 与 force_start 共用）。
    async fn resolve_for(&self, rec_id: &str) -> Result<crate::resolver::StreamInfo, String> {
        let Some(rec) = self.store.get(rec_id).await else {
            return Err(format!("任务不存在: {rec_id}"));
        };

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
        let key = rec.platform_key.as_deref().unwrap_or("custom");
        let stop = self
            .checks
            .lock()
            .expect("active checks")
            .get(rec_id)
            .cloned()
            .unwrap_or_else(|| self.stopping.clone());
        let _permit = self
            .pacer
            .acquire(key, maximum, Duration::from_secs(spacing), &stop)
            .await?;
        let cookie = self
            .config
            .read()
            .await
            .cookies_for_resolver()
            .map_err(|_| "Cookie 配置无法读取，未发送匿名请求")?
            .get(key)
            .cloned();
        let work = self.resolver.resolve(ResolveRequest {
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
        });
        let result = tokio::select! { biased; _=stop.cancelled()=>Err("检测已取消".into()), result=work=>result };
        if self
            .store
            .get(rec_id)
            .await
            .is_none_or(|current| current.url != rec.url || current.quality != rec.quality)
        {
            return Err("任务已更新，旧检测结果已丢弃".into());
        }
        self.pacer.result(key, &result);
        result
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
                    let _starting = self.starting.lock().await;
                    if self.stopping.is_cancelled() {
                        return;
                    }
                    let scheduler = self.clone();
                    self.background.spawn(async move {
                        if let Err(error) = scheduler.check(record.rec_id).await {
                            log::debug!("定时检测未执行: {error}");
                        }
                    });
                }
            }
        }
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
        let elapsed = process
            .as_ref()
            .map(|p| p.started_at.elapsed().as_secs_f64());
        if let Some(process) = process {
            if let Some(plan) = self.conversion_plans.lock().await.remove(&process.run_id) {
                plan.dispatch(&process, &self.postprocess, &self.automation, &self.store)
                    .await;
            }
        }
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
            })
            .await;
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

fn recording_proxy(config: &ConfigStore, platform: Option<&str>) -> Result<Option<String>, String> {
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
                    "offline".into(),
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
