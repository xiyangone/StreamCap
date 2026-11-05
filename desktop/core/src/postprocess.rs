//! Bounded TS -> MP4 jobs. Source cleanup is opt-in and follows verified publication.
use crate::{
    config::ConfigStore,
    engine::{Engine, MediaReservation},
    model::{MediaJob, MediaJobState},
    paths, storage, Store,
};
use serde_json::Value;
use std::{
    io,
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, LazyLock, Mutex,
    },
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, sync::Semaphore};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Clone)]
pub struct Postprocessor {
    inner: Arc<Inner>,
}

#[derive(Default)]
pub(crate) struct RecordingBatch {
    pub jobs: Vec<MediaJob>,
    pub failures: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct RemuxOptions {
    pub delete_original: bool,
    pub minimum_free_bytes: u64,
}
impl RemuxOptions {
    pub fn from_config(config: &ConfigStore) -> Self {
        Self {
            delete_original: config.get_bool("delete_original", false),
            minimum_free_bytes: crate::media_safety::minimum_bytes(
                &config.get_str("recording_space_threshold", "2.0"),
            ),
        }
    }
}
struct Inner {
    config: Arc<tokio::sync::RwLock<ConfigStore>>,
    engine: Engine,
    store: Store,
    ffmpeg: Option<PathBuf>,
    jobs: Mutex<Vec<(PathBuf, MediaJob)>>,
    journal_path: PathBuf,
    journal_write: Mutex<()>,
    recovery: Mutex<std::collections::HashMap<String, RecoveryInput>>,
    accepting: AtomicBool,
    registration: tokio::sync::Mutex<()>,
    stop: CancellationToken,
    tasks: TaskTracker,
    slot: Arc<Semaphore>,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct RecoveryInput {
    root: PathBuf,
    identity: storage::SourceIdentity,
    options: RemuxOptions,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct JournalRecord {
    job: MediaJob,
    input: RecoveryInput,
}

/// A retry belongs to a task only when both the canonical path and recorded
/// file identity agree. Relative names alone are not unique across media roots.
fn source_task_id<'a>(
    source: &Path,
    input: &RecoveryInput,
    candidates: impl Iterator<Item = (&'a Path, &'a MediaJob, &'a RecoveryInput)>,
) -> Option<String> {
    let mut matches = candidates.filter_map(|(path, job, previous)| {
        (path == source && previous.root == input.root && previous.identity == input.identity)
            .then_some(job.task_id.as_deref())
            .flatten()
    });
    let id = matches.next()?;
    matches.all(|other| other == id).then(|| id.to_owned())
}

struct Input {
    root: PathBuf,
    relative: String,
    source: PathBuf,
    output: PathBuf,
    temporary: PathBuf,
    source_lease: Option<std::fs::File>,
    options: RemuxOptions,
    _reservation: MediaReservation,
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
impl Postprocessor {
    pub fn new(
        engine: Engine,
        store: Store,
        ffmpeg: Option<PathBuf>,
        config: Arc<tokio::sync::RwLock<ConfigStore>>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                journal_path: store.workspace().config_dir().join("media_jobs.json"),
                journal_write: Mutex::new(()),
                recovery: Mutex::new(std::collections::HashMap::new()),
                config,
                engine,
                store,
                ffmpeg,
                jobs: Mutex::new(Vec::new()),
                accepting: AtomicBool::new(true),
                registration: tokio::sync::Mutex::new(()),
                stop: CancellationToken::new(),
                tasks: TaskTracker::new(),
                slot: Arc::new(Semaphore::new(1)),
            }),
        }
    }
    fn executable(&self) -> Option<PathBuf> {
        paths::configured_ffmpeg(self.inner.store.workspace(), self.inner.ffmpeg.as_deref())
    }
    pub fn ready(&self) -> bool {
        self.executable()
            .is_some_and(|p| p.is_file() && paths::adjacent_ffprobe(&p).is_some())
    }
    pub fn jobs(&self) -> Vec<MediaJob> {
        self.inner
            .jobs
            .lock()
            .expect("media jobs lock")
            .iter()
            .scan(0usize, |position, (_, job)| {
                let mut job = job.clone();
                if job.state == MediaJobState::Waiting {
                    *position += 1;
                    job.queue_position = Some(*position);
                }
                if let Some(started) = job.started_at {
                    job.elapsed_seconds = job
                        .finished_at
                        .unwrap_or_else(|| chrono::Utc::now().timestamp())
                        .saturating_sub(started)
                        .max(0) as u64;
                }
                Some(job)
            })
            .collect()
    }
    pub fn pending(&self) -> usize {
        self.inner
            .jobs
            .lock()
            .expect("media jobs lock")
            .iter()
            .filter(|(_, job)| job.state.pending())
            .count()
    }
    pub async fn enqueue(
        &self,
        root: PathBuf,
        relative: &str,
        task_id: Option<String>,
    ) -> io::Result<MediaJob> {
        let _guard = self.inner.engine.filesystem_guard().await;
        let options = RemuxOptions::from_config(&*self.inner.config.read().await);
        self.enqueue_locked(root, relative, task_id, options).await
    }
    pub async fn enqueue_manual(
        &self,
        root: PathBuf,
        relative: &str,
        delete_original: bool,
    ) -> io::Result<MediaJob> {
        let _guard = self.inner.engine.filesystem_guard().await;
        let mut options = RemuxOptions::from_config(&*self.inner.config.read().await);
        options.delete_original = delete_original;
        self.enqueue_locked(root, relative, None, options).await
    }
    /// Caller holds the engine filesystem guard across recorder removal and job registration.
    pub(crate) async fn enqueue_locked(
        &self,
        root: PathBuf,
        relative: &str,
        task_id: Option<String>,
        options: RemuxOptions,
    ) -> io::Result<MediaJob> {
        let _registration = self.inner.registration.lock().await;
        if !self.inner.accepting.load(Ordering::SeqCst) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "转封装服务正在退出",
            ));
        }
        if !self.ready() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "转 MP4 需要 FFmpeg 和同目录的 ffprobe",
            ));
        }
        let source = storage::checked_target(&root, relative, false)?;
        if !source
            .extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("ts"))
        {
            return Err(invalid("无损转 MP4 当前仅接受 TS 文件"));
        }
        {
            let jobs = self.inner.jobs.lock().expect("media jobs lock");
            if let Some((_, job)) = jobs
                .iter()
                .find(|(path, job)| path == &source && job.state.pending())
            {
                return Ok(job.clone());
            }
            if jobs.iter().filter(|(_, job)| job.state.pending()).count() >= 128 {
                return Err(io::Error::new(io::ErrorKind::WouldBlock, "转封装队列已满"));
            }
        }
        if self.inner.engine.protects_path(&source).await {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "文件仍在录制或处理中，请停止后重试",
            ));
        }
        let output = source.with_extension("mp4");
        if std::fs::symlink_metadata(&output).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "同名 MP4 已存在，未覆盖任何文件",
            ));
        }
        let lease = storage::open_conversion_source(&root, relative)?;
        if lease.metadata()?.len() == 0 {
            return Err(invalid("TS 文件为空，未转换"));
        }
        crate::media_safety::require_space(
            &root,
            options.minimum_free_bytes,
            lease.metadata()?.len().saturating_add(16 * 1024 * 1024),
        )?;
        let recovery_input = RecoveryInput {
            root: root.canonicalize()?,
            identity: storage::source_identity(&lease)?,
            options,
        };
        let task_id = task_id.or_else(|| {
            let jobs = self.inner.jobs.lock().expect("media jobs lock");
            let recovery = self.inner.recovery.lock().expect("media recovery");
            source_task_id(
                &source,
                &recovery_input,
                jobs.iter().filter_map(|(path, job)| {
                    recovery
                        .get(&job.id)
                        .map(|input| (path.as_path(), job, input))
                }),
            )
        });
        let id = uuid::Uuid::new_v4().to_string();
        let temporary = source.with_file_name(format!(".streamcap-remux-{id}.mp4"));
        let reservation = self.inner.engine.reserve_media(vec![
            source.clone(),
            output.clone(),
            temporary.clone(),
        ]);
        let output_relative = output
            .strip_prefix(root.canonicalize()?)
            .map_err(|_| invalid("输出超出录制根目录"))?
            .to_string_lossy()
            .replace('\\', "/");
        let job = MediaJob {
            id: id.clone(),
            task_id,
            source: relative.replace('\\', "/"),
            output: output_relative,
            state: MediaJobState::Waiting,
            stage_percent: None,
            stage_eta_seconds: None,
            started_at: None,
            finished_at: None,
            elapsed_seconds: 0,
            queue_position: None,
            delete_original: options.delete_original,
            source_removed: false,
            message: if options.delete_original {
                "等待转 MP4；校验成功后清理源 TS"
            } else {
                "等待转 MP4；原 TS 保留"
            }
            .into(),
        };
        {
            let mut jobs = self.inner.jobs.lock().expect("media jobs lock");
            while jobs.len() >= 256 {
                let Some(index) = jobs.iter().position(|(_, item)| !item.state.pending()) else {
                    break;
                };
                jobs.remove(index);
            }
            jobs.push((source.clone(), job.clone()));
        }
        self.inner
            .recovery
            .lock()
            .expect("media recovery")
            .insert(id.clone(), recovery_input);
        if let Err(error) = self.persist() {
            self.inner
                .jobs
                .lock()
                .expect("media jobs lock")
                .retain(|(_, item)| item.id != id);
            self.inner
                .recovery
                .lock()
                .expect("media recovery")
                .remove(&id);
            return Err(io::Error::other(format!(
                "处理队列保存失败，原文件未改动：{error}"
            )));
        }
        self.publish(&job);
        let manager = self.clone();
        let input = Input {
            root,
            relative: job.source.clone(),
            source,
            output,
            temporary,
            source_lease: Some(lease),
            options,
            _reservation: reservation,
        };
        self.inner.tasks.spawn(async move {
            manager.execute(id, input).await;
        });
        Ok(job)
    }
    pub(crate) async fn enqueue_recording_locked(
        &self,
        root: &Path,
        pattern: &Path,
        task_id: &str,
        options: RemuxOptions,
    ) -> io::Result<RecordingBatch> {
        let canonical_root = root.canonicalize()?;
        let targets = crate::media_safety::recording_outputs(pattern)?;
        let relatives = targets
            .iter()
            .map(|target| {
                target
                    .strip_prefix(&canonical_root)
                    .map(|path| path.to_string_lossy().replace('\\', "/"))
                    .map_err(|_| invalid("录制输出超出原始保存根目录"))
            })
            .collect::<io::Result<Vec<_>>>()?;
        let mut batch = RecordingBatch::default();
        for relative in relatives {
            match self
                .enqueue_locked(
                    root.to_path_buf(),
                    &relative,
                    Some(task_id.to_string()),
                    options,
                )
                .await
            {
                Ok(job) => batch.jobs.push(job),
                Err(error) => batch.failures.push(error.to_string()),
            }
        }
        Ok(batch)
    }

    fn persist(&self) -> io::Result<()> {
        let _serial = self.inner.journal_write.lock().expect("media journal");
        let jobs = self.inner.jobs.lock().expect("media jobs lock");
        let mut recovery = self.inner.recovery.lock().expect("media recovery");
        recovery.retain(|id, _| jobs.iter().any(|(_, job)| &job.id == id));
        let entries = jobs
            .iter()
            .filter_map(|(_, job)| {
                recovery.get(&job.id).map(|input| JournalRecord {
                    job: job.clone(),
                    input: input.clone(),
                })
            })
            .collect::<Vec<_>>();
        let bytes = serde_json::to_vec(&entries)?;
        crate::config::write_atomic(&self.inner.journal_path, &bytes)
    }
    /// Restore only this queue's recorded identities. Never scan or convert historical files.
    pub async fn recover(&self) -> io::Result<usize> {
        use std::io::Read;
        let file = match std::fs::File::open(&self.inner.journal_path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        file.take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 2 * 1024 * 1024 {
            return Err(invalid("处理队列文件过大，未恢复"));
        }
        let mut entries: Vec<JournalRecord> = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("处理队列文件损坏，未改动媒体文件"))?;
        if entries.len() > 256 {
            return Err(invalid("处理队列条目过多，未恢复"));
        }
        let current_root = self
            .inner
            .config
            .read()
            .await
            .recordings_root()
            .canonicalize()
            .ok();
        let source_paths = entries
            .iter()
            .map(|entry| {
                let relative = storage::relative_path(&entry.job.source)?;
                storage::relative_path(&entry.job.output)?;
                if !entry.input.root.is_absolute() {
                    return Err(invalid("处理队列目录无效"));
                }
                // Keep missing-file history, but identify existing sources through
                // the same link-safe canonical path used when enqueueing them.
                Ok(
                    storage::checked_target(&entry.input.root, &entry.job.source, false)
                        .unwrap_or_else(|_| entry.input.root.join(relative)),
                )
            })
            .collect::<io::Result<Vec<_>>>()?;
        let latest_attempts = source_paths
            .iter()
            .enumerate()
            .map(|(index, source)| (source.clone(), index))
            .collect::<std::collections::HashMap<_, _>>();
        // Reconnect historical manual attempts without guessing from room names
        // or changing the result, cleanup choice, or recovery eligibility.
        let associations = entries
            .iter()
            .zip(&source_paths)
            .map(|(entry, source)| {
                entry.job.task_id.clone().or_else(|| {
                    source_task_id(
                        source,
                        &entry.input,
                        entries.iter().zip(&source_paths).map(|(candidate, path)| {
                            (path.as_path(), &candidate.job, &candidate.input)
                        }),
                    )
                })
            })
            .collect::<Vec<_>>();
        for (entry, task_id) in entries.iter_mut().zip(associations) {
            entry.job.task_id = task_id;
        }
        let mut resume = Vec::new();
        for (index, (mut entry, source)) in entries.into_iter().zip(source_paths).enumerate() {
            if entry.job.state.pending() || entry.job.state == MediaJobState::Cancelled {
                entry.job.state = MediaJobState::Cancelled;
                entry.job.message = "上次处理已中断，源文件未自动清理".into();
                if latest_attempts.get(&source) != Some(&index) {
                    // A newer retry or terminal result supersedes this attempt,
                    // including its source-cleanup preference.
                    entry.job.message = "已有更新的处理记录，未自动恢复该中断任务".into();
                } else {
                    let identity =
                        storage::open_conversion_source(&entry.input.root, &entry.job.source)
                            .and_then(|file| storage::source_identity(&file));
                    let output_exists =
                        std::fs::symlink_metadata(entry.input.root.join(&entry.job.output)).is_ok();
                    if current_root.as_ref() == Some(&entry.input.root)
                        && identity.is_ok_and(|id| id == entry.input.identity)
                        && !output_exists
                    {
                        resume.push((entry.input.clone(), entry.job.clone()));
                    } else if output_exists {
                        // Existence is not evidence of verified publication. In
                        // particular, never expose an unrelated MP4 as ready.
                        entry.job.state = MediaJobState::Failed;
                        entry.job.message =
                            "中断任务的同名 MP4 需人工核对；未自动恢复或删除文件".into();
                    }
                }
            }
            self.inner
                .recovery
                .lock()
                .expect("media recovery")
                .insert(entry.job.id.clone(), entry.input);
            self.inner
                .jobs
                .lock()
                .expect("media jobs lock")
                .push((source, entry.job));
        }
        self.persist()?;
        let mut count = 0;
        let _filesystem = self.inner.engine.filesystem_guard().await;
        for (input, job) in resume {
            match self
                .enqueue_locked(input.root, &job.source, job.task_id, input.options)
                .await
            {
                Ok(_) => count += 1,
                Err(error) => self
                    .inner
                    .store
                    .snack_error(format!("上次处理暂未恢复：{error}")),
            }
        }
        Ok(count)
    }

    fn progress_reporter(
        &self,
        id: &str,
        duration: Option<f64>,
        video_packets: Option<u64>,
    ) -> Option<ProgressObserver> {
        let duration = duration.filter(|n| n.is_finite() && *n > 0.0);
        let video_packets = video_packets.filter(|n| *n > 0);
        if duration.is_none() && video_packets.is_none() {
            return None;
        }
        let owner = self.clone();
        let id = id.to_owned();
        let began = std::time::Instant::now();
        Some(Arc::new(move |progress| {
            // With -copyts, FFmpeg's out_time can stay at zero or exclude an
            // initial interval. A single parsed video packet corresponds to a
            // frame, so prefer the measured frame count for that common case.
            let Some(fraction) = stage_fraction(progress, duration, video_packets) else {
                return;
            };
            let elapsed = began.elapsed().as_secs_f64();
            let percent = (fraction * 100.0).clamp(0.0, 99.0) as u8;
            let changed = owner
                .inner
                .jobs
                .lock()
                .expect("media jobs lock")
                .iter_mut()
                .find(|(_, job)| job.id == id)
                .and_then(|(_, job)| {
                    if !job.state.pending() || job.stage_percent == Some(percent) {
                        return None;
                    }
                    job.stage_percent = Some(percent);
                    job.stage_eta_seconds = if elapsed >= 1.0 && fraction > 0.0 && percent >= 5 {
                        Some(
                            ((1.0 - fraction).max(0.0) * elapsed / fraction)
                                .ceil()
                                .min(86400.0) as u64,
                        )
                    } else {
                        None
                    };
                    job.elapsed_seconds = job
                        .started_at
                        .map(|start| {
                            chrono::Utc::now().timestamp().saturating_sub(start).max(0) as u64
                        })
                        .unwrap_or(0);
                    Some(job.clone())
                });
            if let Some(job) = changed {
                owner.publish(&job);
            }
        }))
    }
    fn publish(&self, job: &MediaJob) {
        self.inner.store.emit(
            "mediaJob",
            serde_json::to_value(job).expect("media job JSON"),
        );
    }
    fn transition(&self, id: &str, state: MediaJobState, message: impl Into<String>) {
        let changed = {
            let mut jobs = self.inner.jobs.lock().expect("media jobs lock");
            jobs.iter_mut()
                .find(|(_, job)| job.id == id)
                .map(|(_, job)| {
                    let now = chrono::Utc::now().timestamp();
                    if state != MediaJobState::Waiting && job.started_at.is_none() {
                        job.started_at = Some(now);
                    }
                    if !state.pending() {
                        job.finished_at = Some(now);
                    }
                    job.elapsed_seconds = job
                        .started_at
                        .map(|start| now.saturating_sub(start).max(0) as u64)
                        .unwrap_or(0);
                    job.stage_percent = if state == MediaJobState::Complete {
                        Some(100)
                    } else {
                        None
                    };
                    job.stage_eta_seconds = None;
                    job.queue_position = None;
                    job.state = state;
                    job.message = message.into();
                    job.clone()
                })
        };
        if let Some(job) = changed {
            if let Err(error) = self.persist() {
                log::warn!("媒体队列状态保存失败: {error}");
            }
            self.publish(&job);
            // Queue positions change when a worker starts or finishes, not on
            // every progress tick. Keep SSE-only clients consistent with GET.
            for waiting in self
                .jobs()
                .into_iter()
                .filter(|queued| queued.state == MediaJobState::Waiting)
            {
                self.publish(&waiting);
            }
        }
    }
    async fn execute(&self, id: String, mut input: Input) {
        let permit = tokio::select! { biased;
            _ = self.inner.stop.cancelled() => None,
            result = self.inner.slot.clone().acquire_owned() => result.ok(),
        };
        let Some(_permit) = permit else {
            self.transition(
                &id,
                MediaJobState::Cancelled,
                "已取消；原 TS 保留，可稍后重试",
            );
            return;
        };
        self.transition(&id, MediaJobState::Running, "正在读取源文件音视频与时长");
        let result = self.convert(&id, &input).await;
        if result.is_err()
            && std::fs::symlink_metadata(&input.temporary)
                .is_ok_and(|m| m.is_file() && !storage::is_link(&m))
        {
            // This UUID-named file was created only by this job; never touch the source or destination.
            if let Err(error) = std::fs::remove_file(&input.temporary) {
                log::warn!("未能清理本次转封装临时文件: {error}");
            }
        }
        match result {
            Ok(_verified_output_lease) => {
                if input.options.delete_original {
                    self.transition(
                        &id,
                        MediaJobState::Cleaning,
                        "MP4 已校验，正在清理对应源 TS",
                    );
                    let _filesystem = self.inner.engine.filesystem_guard().await;
                    let cleanup = if self.inner.stop.is_cancelled() {
                        Err(io::Error::new(io::ErrorKind::Interrupted, "退出时取消清理"))
                    } else {
                        storage::remove_verified_source(
                            &input.root,
                            &input.relative,
                            input.source_lease.take().expect("source lease"),
                        )
                    };
                    if let Err(error) = cleanup {
                        self.transition(
                            &id,
                            MediaJobState::CleanupFailed,
                            format!("MP4 已完成，源 TS 未清理：{error}"),
                        );
                        self.inner.store.snack_error("MP4 已生成，源 TS 清理未完成");
                        return;
                    }
                    if let Some((_, job)) = self
                        .inner
                        .jobs
                        .lock()
                        .expect("media jobs lock")
                        .iter_mut()
                        .find(|(_, j)| j.id == id)
                    {
                        job.source_removed = true;
                    }
                    self.transition(
                        &id,
                        MediaJobState::Complete,
                        "MP4 已生成并校验，源 TS 已清理",
                    );
                } else {
                    self.transition(&id, MediaJobState::Complete, "MP4 已生成并校验；原 TS 保留");
                }
                self.inner.store.snack("MP4 转封装完成");
            }
            Err(error) if self.inner.stop.is_cancelled() => {
                log::debug!("转封装退出取消: {error}");
                self.transition(
                    &id,
                    MediaJobState::Cancelled,
                    "退出时已取消；原 TS 保留，可稍后重试",
                );
            }
            Err(error) => {
                log::warn!("转 MP4 失败 [{}]: {error}", input.relative);
                self.transition(&id, MediaJobState::Failed, format!("{error}；原 TS 保留"));
                self.inner
                    .store
                    .snack_error(format!("转 MP4 失败：{error}；原 TS 保留"));
            }
        }
    }
    async fn convert(&self, id: &str, input: &Input) -> io::Result<std::fs::File> {
        crate::media_safety::require_space(
            &input.root,
            input.options.minimum_free_bytes,
            input
                .source_lease
                .as_ref()
                .expect("source lease")
                .metadata()?
                .len()
                .saturating_add(16 * 1024 * 1024),
        )?;
        let ffmpeg = self.executable().ok_or_else(|| invalid("缺少 FFmpeg"))?;
        let ffprobe =
            paths::adjacent_ffprobe(&ffmpeg).ok_or_else(|| invalid("缺少同目录 ffprobe"))?;
        let before = input
            .source_lease
            .as_ref()
            .expect("source lease")
            .metadata()?;
        let current = storage::checked_target(&input.root, &input.relative, false)?;
        if current != input.source {
            return Err(invalid("源文件位置已改变"));
        }
        let source_info = probe(&ffprobe, &input.source, "mpegts", &self.inner.stop)
            .await
            .map_err(|error| stage_error("源 TS 读取失败", error))?;
        let video_packets = single_video_packets(&source_info);
        self.transition(id, MediaJobState::Running, "正在无损转封装为 MP4");
        if std::fs::symlink_metadata(&input.output).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "同名 MP4 已存在，未覆盖",
            ));
        }
        // Preserve TS discontinuities; normalize only the initial timestamp offset.
        let mut command = command(&ffmpeg);
        command
            .args([
                "-v",
                "error",
                "-nostdin",
                "-nostats",
                "-progress",
                "pipe:1",
                "-stats_period",
                "2",
                "-n",
                "-copyts",
                "-start_at_zero",
                "-protocol_whitelist",
                "file",
                "-f",
                "mpegts",
                "-i",
            ])
            .arg(&input.source)
            .args([
                "-map",
                "0:v?",
                "-map",
                "0:a?",
                "-c",
                "copy",
                "-movflags",
                "+faststart",
                "-f",
                "mp4",
            ])
            .arg(&input.temporary);
        run_tool(
            command,
            &self.inner.stop,
            true,
            media_idle_budget(before.len()),
            self.progress_reporter(id, source_info.1, video_packets),
        )
        .await
        .map_err(|error| stage_error("FFmpeg 转封装未完成", error))?;
        let temporary_relative = input
            .temporary
            .strip_prefix(input.root.canonicalize()?)
            .map_err(|_| invalid("临时输出越界"))?
            .to_string_lossy()
            .replace('\\', "/");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&input.temporary)?
            .sync_all()?;
        // Keep the exact output immutable throughout probing and full decoding.
        let verified_output = storage::open_conversion_source(&input.root, &temporary_relative)?;
        self.transition(id, MediaJobState::Verifying, "正在校验 MP4 音视频与时长");
        let output_info = probe(&ffprobe, &input.temporary, "mov", &self.inner.stop)
            .await
            .map_err(|error| stage_error("MP4 读取校验失败", error))?;
        if source_info.0 != output_info.0 {
            return Err(invalid("MP4 音视频轨道校验不一致"));
        }
        if let (Some(a), Some(b)) = (source_info.1, output_info.1) {
            if (a - b).abs() > 1.0_f64.max(a * 0.001) {
                return Err(invalid(&format!(
                    "MP4 时长校验不一致（源 TS {a:.3} 秒，MP4 {b:.3} 秒）"
                )));
            }
        }
        if input
            .source_lease
            .as_ref()
            .expect("source lease")
            .metadata()?
            .len()
            != before.len()
        {
            return Err(invalid("源文件仍有变化，未发布 MP4"));
        }
        if self.inner.stop.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "已取消"));
        }
        if input.options.delete_original {
            self.transition(id, MediaJobState::Verifying, "正在校验 MP4 完整性");
            let mut verify = self::command(&ffmpeg);
            verify
                .args([
                    "-v",
                    "error",
                    "-nostdin",
                    "-nostats",
                    "-progress",
                    "pipe:1",
                    "-stats_period",
                    "2",
                    "-xerror",
                    "-protocol_whitelist",
                    "file",
                    "-threads",
                    "2",
                    "-i",
                ])
                .arg(&input.temporary)
                .args([
                    "-map",
                    "0:v?",
                    "-map",
                    "0:a?",
                    "-fps_mode:v",
                    "passthrough",
                    "-enc_time_base:v",
                    "-1",
                    "-f",
                    "null",
                    "-",
                ]);
            let decoded = run_tool(
                verify,
                &self.inner.stop,
                true,
                media_idle_budget(before.len()),
                self.progress_reporter(id, output_info.1, video_packets),
            )
            .await
            .map_err(|error| stage_error("MP4 完整解码校验失败", error))?;
            // With -v error, diagnostics disqualify cleanup even when FFmpeg exits 0.
            if decoded
                .stderr
                .iter()
                .any(|byte| !byte.is_ascii_whitespace())
            {
                return Err(invalid(&format!(
                    "MP4 完整解码校验失败：{}",
                    diagnostic_summary(&decoded.stderr)
                )));
            }
        }
        if self.inner.stop.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "发布前已取消"));
        }
        let identity = storage::source_identity(&verified_output)?;
        drop(verified_output);
        storage::publish_new_file(&input.temporary, &input.output)?;
        let relative = input
            .output
            .strip_prefix(input.root.canonicalize()?)
            .map_err(|_| invalid("输出越界"))?
            .to_string_lossy()
            .replace('\\', "/");
        let published = storage::open_conversion_source(&input.root, &relative)?;
        if storage::source_identity(&published)? != identity {
            return Err(invalid("发布后的 MP4 已改变，源 TS 保留"));
        }
        // The returned lease prevents the published MP4 being overwritten or removed
        // until source cleanup finishes, including while waiting for the filesystem lock.
        Ok(published)
    }
    pub async fn shutdown(&self, grace: Duration) {
        let registration = self.inner.registration.lock().await;
        self.inner.accepting.store(false, Ordering::SeqCst);
        self.inner.tasks.close();
        drop(registration);
        if tokio::time::timeout(grace, self.inner.tasks.wait())
            .await
            .is_err()
        {
            self.inner.stop.cancel();
            self.inner.tasks.wait().await;
        }
    }
}
pub(crate) fn command(program: &Path) -> Command {
    let mut command = Command::new(program);
    crate::network::isolate_media_proxy(&mut command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    command
}
fn media_idle_budget(bytes: u64) -> Duration {
    // Faststart can relocate the complete output without advancing media time.
    Duration::from_secs((180 + bytes / (16 * 1024 * 1024)).min(3600))
}
fn probe_budget(bytes: u64) -> Duration {
    Duration::from_secs((600 + bytes / (1024 * 1024)).min(7200))
}
fn stage_error(stage: &str, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{stage}：{error}"))
}
fn diagnostic_summary(bytes: &[u8]) -> String {
    static PRIVATE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r#"(?i)\b(?:https?|rtmps?|rtsp|socks5h?)://[^\s"'<>]+|(?:authorization|cookie|token|secret|password|api[-_]?key)\s*[:=][^\r\n]*"#,
        )
        .expect("media diagnostic redaction")
    });
    let text = String::from_utf8_lossy(bytes);
    let redacted = PRIVATE.replace_all(&text, "[已隐藏敏感信息]");
    redacted
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(4)
        .collect::<Vec<_>>()
        .join(" · ")
        .chars()
        .filter(|c| !c.is_control())
        .take(768)
        .collect()
}
#[derive(Clone, Copy, Default)]
struct ToolProgress {
    micros: u64,
    frames: u64,
    bytes: u64,
}
impl ToolProgress {
    fn observe(&mut self, line: &str) -> bool {
        let Some((key, value)) = line.split_once('=') else {
            return false;
        };
        let Ok(value) = value.trim().parse::<u64>() else {
            return false;
        };
        let field = match key {
            "out_time_us" => &mut self.micros,
            "frame" => &mut self.frames,
            "total_size" => &mut self.bytes,
            _ => return false,
        };
        if value <= *field {
            return false;
        }
        *field = value;
        true
    }
}
type ProgressObserver = Arc<dyn Fn(ToolProgress) + Send + Sync>;
fn stage_fraction(
    progress: ToolProgress,
    duration: Option<f64>,
    video_packets: Option<u64>,
) -> Option<f64> {
    if let Some(packets) = video_packets.filter(|n| *n > 0) {
        return (progress.frames > 0).then_some(progress.frames as f64 / packets as f64);
    }
    duration
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0 && progress.micros > 0)
        .map(|seconds| progress.micros as f64 / 1_000_000.0 / seconds)
}
fn single_video_packets(info: &MediaInfo) -> Option<u64> {
    let mut videos = info.0.iter().filter(|(kind, _, _)| kind == "video");
    let packets = videos.next()?.2;
    (videos.next().is_none() && packets > 0).then_some(packets)
}
async fn drain_bounded(
    mut reader: impl tokio::io::AsyncRead + Unpin,
    limit: usize,
) -> io::Result<Vec<u8>> {
    let mut retained = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            return Ok(retained);
        }
        let keep = count.min((limit + 1).saturating_sub(retained.len()));
        retained.extend_from_slice(&buffer[..keep]);
    }
}
async fn wait_for_stall(advanced: Arc<Mutex<tokio::time::Instant>>, budget: Duration) {
    loop {
        let deadline = *advanced.lock().expect("media progress") + budget;
        tokio::time::sleep_until(deadline).await;
        if advanced.lock().expect("media progress").elapsed() >= budget {
            return;
        }
    }
}
pub(crate) async fn run(command: Command, stop: &CancellationToken) -> io::Result<Vec<u8>> {
    run_tool(command, stop, false, Duration::from_secs(600), None)
        .await
        .map(|output| output.stdout)
}
async fn run_tool(
    mut command: Command,
    stop: &CancellationToken,
    progress: bool,
    budget: Duration,
    observer: Option<ProgressObserver>,
) -> io::Result<Output> {
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("无法读取处理结果"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("无法读取处理错误"))?;
    let advanced = Arc::new(Mutex::new(tokio::time::Instant::now()));
    let reader_progress = advanced.clone();
    let out = tokio::spawn(async move {
        if !progress {
            return drain_bounded(stdout, 1024 * 1024).await;
        }
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(stdout).lines();
        let mut measured = ToolProgress::default();
        let mut changed = false;
        while let Some(line) = lines.next_line().await? {
            if measured.observe(&line) {
                changed = true;
                *reader_progress.lock().expect("media progress") = tokio::time::Instant::now();
            }
            if line.starts_with("progress=") && changed {
                if let Some(observer) = &observer {
                    observer(measured);
                }
                changed = false;
            }
        }
        Ok(Vec::new())
    });
    let err = tokio::spawn(drain_bounded(stderr, 64 * 1024));
    let status = tokio::select! { biased;
        _ = stop.cancelled() => {
            let _ = child.start_kill(); let _ = child.wait().await;
            Err(io::Error::new(io::ErrorKind::Interrupted, "处理已取消"))
        },
        _ = wait_for_stall(advanced, budget) => {
            let _ = child.start_kill(); let _ = child.wait().await;
            Err(io::Error::new(io::ErrorKind::TimedOut, format!("处理超时：{} 秒没有新增进度", budget.as_secs())))
        },
        _ = tokio::time::sleep(Duration::from_secs(86400)) => {
            let _ = child.start_kill(); let _ = child.wait().await;
            Err(io::Error::new(io::ErrorKind::TimedOut, "处理超过 24 小时上限"))
        },
        result = child.wait() => result,
    };
    let stdout = out.await.map_err(io::Error::other)??;
    let stderr = err.await.map_err(io::Error::other)??;
    let status = status?;
    let output_limit = stdout.len() > 1024 * 1024 || stderr.len() > 64 * 1024;
    if !status.success() || output_limit {
        let diagnostic = diagnostic_summary(&stderr);
        return Err(io::Error::other(format!(
            "媒体工具退出码 {}{}{}",
            status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "未知".into()),
            if output_limit {
                "；响应超出限制，以下诊断已截断"
            } else {
                ""
            },
            if diagnostic.is_empty() {
                String::new()
            } else {
                format!("：{diagnostic}")
            }
        )));
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}
type MediaInfo = (Vec<(String, String, u64)>, Option<f64>);
async fn probe(
    program: &Path,
    file: &Path,
    format: &str,
    stop: &CancellationToken,
) -> io::Result<MediaInfo> {
    let mut cmd = command(program);
    cmd.args([
        "-v",
        "error",
        "-protocol_whitelist",
        "file",
        "-f",
        format,
        "-show_entries",
        "format=duration:stream=codec_type,codec_name,nb_read_packets",
        "-count_packets",
        "-of",
        "json",
    ])
    .arg(file);
    let raw = run_tool(
        cmd,
        stop,
        false,
        probe_budget(std::fs::metadata(file)?.len()),
        None,
    )
    .await?;
    let value: Value = serde_json::from_slice(&raw.stdout).map_err(io::Error::other)?;
    let mut streams: Vec<_> = value["streams"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|stream| {
            let kind = stream["codec_type"].as_str()?;
            let codec = stream["codec_name"].as_str()?;
            matches!(kind, "video" | "audio").then(|| {
                (
                    kind.to_string(),
                    codec.to_string(),
                    stream["nb_read_packets"]
                        .as_str()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0),
                )
            })
        })
        .collect();
    if streams.is_empty() {
        return Err(invalid("未检测到有效音视频轨道"));
    }
    streams.sort();
    let duration = value["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n > 0.0);
    if format == "mov" && duration.is_none() {
        return Err(invalid("MP4 时长无效"));
    }
    Ok((streams, duration))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recovery_fixture() -> (tempfile::TempDir, PathBuf, Postprocessor) {
        let directory = tempfile::tempdir().unwrap();
        let workspace = crate::Workspace::from_repo_root(directory.path());
        workspace.ensure_ready().unwrap();
        let root = directory.path().join("downloads");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("resume.ts"), b"preserved source").unwrap();
        let ffmpeg = directory.path().join("ffmpeg.exe");
        std::fs::write(&ffmpeg, b"not executed").unwrap();
        std::fs::write(
            ffmpeg.with_file_name(if cfg!(windows) {
                "ffprobe.exe"
            } else {
                "ffprobe"
            }),
            b"not executed",
        )
        .unwrap();
        let manager = Postprocessor::new(
            Engine::new(),
            Store::new(workspace.clone()),
            Some(ffmpeg),
            Arc::new(tokio::sync::RwLock::new(
                ConfigStore::load(workspace).unwrap(),
            )),
        );
        (directory, root, manager)
    }

    fn recovery_record(
        root: &Path,
        id: &str,
        state: MediaJobState,
        delete_original: bool,
    ) -> JournalRecord {
        JournalRecord {
            job: serde_json::from_value(serde_json::json!({
                "id": id, "taskId": null, "source": "resume.ts", "output": "resume.mp4",
                "state": state, "deleteOriginal": delete_original,
                "sourceRemoved": false, "message": ""
            }))
            .unwrap(),
            input: RecoveryInput {
                root: root.to_path_buf(),
                identity: storage::source_identity(
                    &storage::open_conversion_source(root, "resume.ts").unwrap(),
                )
                .unwrap(),
                options: RemuxOptions {
                    delete_original,
                    minimum_free_bytes: 0,
                },
            },
        }
    }

    #[tokio::test]
    async fn manual_history_inherits_only_verified_source_task_identity() {
        for variant in ["same", "other-root", "replaced-source", "ambiguous-task"] {
            let (_directory, root, manager) = recovery_fixture();
            let mut original = recovery_record(&root, "automatic", MediaJobState::Failed, true);
            original.job.task_id = Some("room-task".into());
            let mut retry = recovery_record(&root, "manual", MediaJobState::Complete, false);
            if variant == "other-root" {
                retry.input.root = root.join("other-root");
            } else if variant == "replaced-source" {
                retry.input.identity.modified += 1;
            }
            let mut entries = vec![original, retry];
            if variant == "ambiguous-task" {
                let mut another =
                    recovery_record(&root, "another-task", MediaJobState::Failed, false);
                another.job.task_id = Some("different-room".into());
                entries.push(another);
            }
            std::fs::write(
                &manager.inner.journal_path,
                serde_json::to_vec(&entries).unwrap(),
            )
            .unwrap();
            assert_eq!(manager.recover().await.unwrap(), 0);
            let jobs = manager.jobs();
            let retry = jobs.iter().find(|job| job.id == "manual").unwrap();
            assert_eq!(
                retry.task_id.as_deref(),
                (variant == "same").then_some("room-task"),
                "{variant}"
            );
            assert_eq!(retry.state, MediaJobState::Complete);
            assert!(!retry.delete_original);
            assert!(!retry.source_removed);
            assert_eq!(
                std::fs::read(root.join("resume.ts")).unwrap(),
                b"preserved source"
            );
            assert!(!root.join("resume.mp4").exists());
            manager.shutdown(Duration::ZERO).await;
        }
    }

    #[tokio::test]
    async fn new_manual_retry_inherits_task_but_not_automatic_cleanup() {
        let (_directory, root, manager) = recovery_fixture();
        let mut original = recovery_record(&root, "automatic", MediaJobState::Failed, true);
        original.job.task_id = Some("room-task".into());
        std::fs::write(
            &manager.inner.journal_path,
            serde_json::to_vec(&[original]).unwrap(),
        )
        .unwrap();
        assert_eq!(manager.recover().await.unwrap(), 0);
        let slot = manager.inner.slot.clone().acquire_owned().await.unwrap();
        let retry = manager
            .enqueue_manual(root.clone(), "resume.ts", false)
            .await
            .unwrap();
        assert_eq!(retry.task_id.as_deref(), Some("room-task"));
        assert!(!retry.delete_original);
        manager.shutdown(Duration::ZERO).await;
        drop(slot);
        assert_eq!(
            std::fs::read(root.join("resume.ts")).unwrap(),
            b"preserved source"
        );
    }

    #[tokio::test]
    async fn recovery_uses_latest_attempt_once_and_preserves_its_cleanup_choice() {
        let (_directory, root, manager) = recovery_fixture();
        let mut latest =
            recovery_record(&root, "latest-keep-source", MediaJobState::Cancelled, false);
        latest.job.source = "./resume.ts".into();
        let entries = [
            recovery_record(&root, "old-cleanup", MediaJobState::Cancelled, true),
            latest,
        ];
        std::fs::write(
            &manager.inner.journal_path,
            serde_json::to_vec(&entries).unwrap(),
        )
        .unwrap();
        let slot = manager.inner.slot.clone().acquire_owned().await.unwrap();
        let resumed = manager.recover().await.unwrap();
        let pending = manager
            .jobs()
            .into_iter()
            .filter(|job| job.state.pending())
            .collect::<Vec<_>>();
        manager.shutdown(Duration::ZERO).await;
        drop(slot);
        assert_eq!(pending.len(), 1);
        assert!(
            !pending[0].delete_original,
            "an old cancelled cleanup must not override a later keep-source retry"
        );
        assert_eq!(resumed, 1, "count attempts, not superseded journal entries");
        assert_eq!(
            std::fs::read(root.join("resume.ts")).unwrap(),
            b"preserved source"
        );
        assert!(!root.join("resume.mp4").exists());
    }

    #[tokio::test]
    async fn recovery_does_not_revive_cancelled_history_after_a_newer_terminal_attempt() {
        for state in [
            MediaJobState::Complete,
            MediaJobState::CleanupFailed,
            MediaJobState::Failed,
        ] {
            let (_directory, root, manager) = recovery_fixture();
            let entries = [
                recovery_record(&root, "old-cancelled", MediaJobState::Cancelled, true),
                recovery_record(&root, "latest-terminal", state, false),
            ];
            std::fs::write(
                &manager.inner.journal_path,
                serde_json::to_vec(&entries).unwrap(),
            )
            .unwrap();
            let slot = manager.inner.slot.clone().acquire_owned().await.unwrap();
            let resumed = manager.recover().await.unwrap();
            manager.shutdown(Duration::ZERO).await;
            drop(slot);
            assert_eq!(
                resumed, 0,
                "newer {state:?} must supersede cancelled history"
            );
            assert_eq!(manager.jobs().len(), 2);
            assert_eq!(
                std::fs::read(root.join("resume.ts")).unwrap(),
                b"preserved source"
            );
            assert!(!root.join("resume.mp4").exists());
        }
    }

    #[tokio::test]
    async fn recovery_never_marks_an_unverified_output_collision_as_ready() {
        let (_directory, root, manager) = recovery_fixture();
        let entry = recovery_record(&root, "interrupted", MediaJobState::Verifying, true);
        std::fs::write(
            &manager.inner.journal_path,
            serde_json::to_vec(&[entry]).unwrap(),
        )
        .unwrap();
        std::fs::write(root.join("resume.mp4"), b"unverified existing output").unwrap();
        let resumed = manager.recover().await.unwrap();
        manager.shutdown(Duration::ZERO).await;
        assert_eq!(resumed, 0);
        let jobs = manager.jobs();
        assert!(
            !matches!(
                jobs[0].state,
                MediaJobState::Complete | MediaJobState::CleanupFailed
            ),
            "an existing file alone does not prove verified MP4 publication: {:?}",
            jobs[0].state
        );
        assert_eq!(
            std::fs::read(root.join("resume.mp4")).unwrap(),
            b"unverified existing output"
        );
        assert_eq!(
            std::fs::read(root.join("resume.ts")).unwrap(),
            b"preserved source"
        );
    }

    #[test]
    fn copyts_progress_uses_frames_even_when_the_media_clock_is_zero_or_offset() {
        let mut progress = ToolProgress::default();
        assert!(progress.observe("frame=25"));
        assert!(!progress.observe("out_time_us=0"));
        assert_eq!(stage_fraction(progress, Some(100.0), Some(100)), Some(0.25));
        assert!(progress.observe("out_time_us=90000000"));
        assert_eq!(stage_fraction(progress, Some(100.0), Some(100)), Some(0.25));
        assert_eq!(stage_fraction(progress, Some(100.0), None), Some(0.9));
        assert_eq!(stage_fraction(progress, None, None), None);
    }

    #[test]
    fn only_new_frames_bytes_or_media_time_count_as_tool_activity() {
        let mut progress = ToolProgress::default();
        for line in [
            "frame=N/A",
            "out_time_us=-1",
            "speed=10x",
            "progress=continue",
        ] {
            assert!(!progress.observe(line), "{line}");
        }
        assert!(progress.observe("total_size=4096"));
        assert!(!progress.observe("total_size=4096"));
        assert!(!progress.observe("total_size=2048"));
        assert!(progress.observe("frame=10"));
        assert!(!progress.observe("frame=10"));
        assert!(progress.observe("out_time_us=1000000"));
    }

    #[test]
    fn media_diagnostics_are_bounded_and_redact_credentials_before_publication() {
        let diagnostic = diagnostic_summary(
            b"[h264] reference picture missing\nhttps://user:password@media.invalid/live?token=abc\nCookie: private=value\nAuthorization: Bearer confidential\n",
        );
        assert!(diagnostic.contains("reference picture missing"));
        for secret in [
            "password",
            "media.invalid",
            "abc",
            "private",
            "confidential",
        ] {
            assert!(!diagnostic.contains(secret), "{diagnostic}");
        }
        assert_eq!(diagnostic_summary(&[b'x'; 2000]).chars().count(), 768);
        assert!(diagnostic_summary(b" \r\n\t").is_empty());
    }

    #[test]
    fn stage_context_keeps_timeout_distinct_from_user_cancellation() {
        for kind in [io::ErrorKind::TimedOut, io::ErrorKind::Interrupted] {
            let error = stage_error("MP4 完整解码校验失败", io::Error::new(kind, "diagnostic"));
            assert_eq!(error.kind(), kind);
            assert!(error.to_string().contains("diagnostic"));
        }
    }

    #[tokio::test]
    async fn readiness_does_not_depend_on_a_settings_write_lock() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = crate::Workspace::from_repo_root(dir.path());
        workspace.ensure_ready().unwrap();
        let ffmpeg = dir.path().join("ffmpeg.exe");
        std::fs::write(&ffmpeg, b"not executed").unwrap();
        std::fs::write(
            ffmpeg.with_file_name(if cfg!(windows) {
                "ffprobe.exe"
            } else {
                "ffprobe"
            }),
            b"not executed",
        )
        .unwrap();
        let config = Arc::new(tokio::sync::RwLock::new(
            ConfigStore::load(workspace.clone()).unwrap(),
        ));
        let manager = Postprocessor::new(
            Engine::new(),
            Store::new(workspace),
            Some(ffmpeg),
            config.clone(),
        );
        assert!(manager.ready());
        let _settings_write = config.write().await;
        assert!(
            manager.ready(),
            "a busy configuration is not a missing FFmpeg installation"
        );
    }
    #[tokio::test(start_paused = true)]
    async fn continuing_progress_outlives_old_total_timeout_but_idle_still_expires() {
        let advanced = Arc::new(Mutex::new(tokio::time::Instant::now()));
        let clock = advanced.clone();
        let waiter = tokio::spawn(wait_for_stall(clock, Duration::from_secs(180)));
        for _ in 0..8 {
            tokio::time::advance(Duration::from_secs(100)).await;
            *advanced.lock().unwrap() = tokio::time::Instant::now();
            tokio::task::yield_now().await;
            assert!(!waiter.is_finished());
        }
        tokio::time::advance(Duration::from_secs(179)).await;
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        tokio::time::advance(Duration::from_secs(1)).await;
        waiter.await.unwrap();
    }

    #[tokio::test]
    async fn bounded_output_keeps_draining_after_its_retention_limit() {
        use tokio::io::AsyncWriteExt;
        let (mut writer, reader) = tokio::io::duplex(32);
        let producer = tokio::spawn(async move {
            writer.write_all(&vec![b'x'; 131072]).await.unwrap();
        });
        let output = tokio::time::timeout(Duration::from_secs(2), drain_bounded(reader, 64))
            .await
            .unwrap()
            .unwrap();
        producer.await.unwrap();
        assert_eq!(output, vec![b'x'; 65]);
    }
    #[tokio::test]
    async fn queued_jobs_are_protected_deduplicated_and_cancelled_at_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = crate::Workspace::from_repo_root(dir.path());
        workspace.ensure_ready().unwrap();
        let ffmpeg = dir.path().join("ffmpeg.exe");
        std::fs::write(&ffmpeg, b"not executed").unwrap();
        std::fs::write(
            ffmpeg.with_file_name(if cfg!(windows) {
                "ffprobe.exe"
            } else {
                "ffprobe"
            }),
            b"not executed",
        )
        .unwrap();
        let root = dir.path().join("downloads");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("queued.ts");
        std::fs::write(&source, b"preserved fixture").unwrap();
        let engine = Engine::new();
        let manager = Postprocessor::new(
            engine.clone(),
            Store::new(workspace.clone()),
            Some(ffmpeg.clone()),
            Arc::new(tokio::sync::RwLock::new(
                ConfigStore::load(workspace).unwrap(),
            )),
        );
        let slot = manager.inner.slot.clone().acquire_owned().await.unwrap();
        let job = manager
            .enqueue(root.clone(), "queued.ts", None)
            .await
            .unwrap();
        assert_eq!(
            manager
                .enqueue(root.clone(), "queued.ts", None)
                .await
                .unwrap()
                .id,
            job.id
        );
        assert!(engine.protects_path(&source.canonicalize().unwrap()).await);
        assert!(engine.protects_path(&root.canonicalize().unwrap()).await);
        #[cfg(windows)]
        assert!(
            std::fs::OpenOptions::new()
                .write(true)
                .open(&source)
                .is_err(),
            "queued conversion must lease the source read-only"
        );
        manager.shutdown(Duration::ZERO).await;
        drop(slot);
        assert_eq!(manager.jobs()[0].state, MediaJobState::Cancelled);
        assert_eq!(manager.pending(), 0);
        assert!(!engine.protects_path(&root.canonicalize().unwrap()).await);
        assert_eq!(std::fs::read(&source).unwrap(), b"preserved fixture");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        assert!(manager.enqueue(root, "queued.ts", None).await.is_err());
    }
}
