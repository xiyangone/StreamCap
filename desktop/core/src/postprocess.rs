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
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, sync::Semaphore};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Clone)]
pub struct Postprocessor {
    inner: Arc<Inner>,
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
                journal_path: store.data_dir().join("config/media_jobs.json"),
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
        let config = self.inner.config.try_read().ok()?;
        let bundled = config.workspace().user_data_dir.join("ffmpeg/ffmpeg.exe");
        if bundled.is_file() {
            Some(bundled)
        } else {
            self.inner
                .ffmpeg
                .clone()
                .or_else(|| paths::find_ffmpeg(config.workspace()))
        }
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
            .map(|(_, job)| job.clone())
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
    ) -> io::Result<()> {
        let canonical_root = root.canonicalize()?;
        let targets = crate::media_safety::recording_outputs(pattern)?;
        let mut failures = Vec::new();
        for target in targets {
            let relative = target
                .strip_prefix(&canonical_root)
                .map_err(|_| invalid("录制输出超出原始保存根目录"))?
                .to_string_lossy()
                .replace('\\', "/");
            if let Err(error) = self
                .enqueue_locked(
                    root.to_path_buf(),
                    &relative,
                    Some(task_id.to_string()),
                    options,
                )
                .await
            {
                failures.push(error.to_string());
            }
        }
        if !failures.is_empty() {
            return Err(io::Error::other(failures.join("；")));
        }
        Ok(())
    }

    fn persist(&self) -> io::Result<()> {
        use std::io::Write;
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
        let parent = self
            .inner
            .journal_path
            .parent()
            .ok_or_else(|| invalid("处理队列目录无效"))?;
        std::fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".media-jobs-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, &self.inner.journal_path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
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
        let entries: Vec<JournalRecord> = serde_json::from_slice(&bytes)
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
        let mut resume = Vec::new();
        for mut entry in entries {
            storage::relative_path(&entry.job.source)?;
            storage::relative_path(&entry.job.output)?;
            if !entry.input.root.is_absolute() {
                return Err(invalid("处理队列目录无效"));
            }
            let source = entry.input.root.join(&entry.job.source);
            if entry.job.state.pending() || entry.job.state == MediaJobState::Cancelled {
                entry.job.state = MediaJobState::Cancelled;
                entry.job.message = "上次处理已中断，源文件未自动清理".into();
                let identity =
                    storage::open_conversion_source(&entry.input.root, &entry.job.source)
                        .and_then(|file| storage::source_identity(&file));
                if current_root.as_ref() == Some(&entry.input.root)
                    && identity.is_ok_and(|id| id == entry.input.identity)
                    && !entry.input.root.join(&entry.job.output).exists()
                {
                    resume.push((entry.input.clone(), entry.job.clone()));
                } else if entry.input.root.join(&entry.job.output).exists() {
                    entry.job.state = MediaJobState::CleanupFailed;
                    entry.job.message =
                        "上次处理已中断，已有 MP4 待核对；未重复转换或清理源文件".into();
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
                Err(error) => self.inner.store.snack(format!("上次处理暂未恢复：{error}")),
            }
        }
        Ok(count)
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
        self.transition(&id, MediaJobState::Running, "正在无损转封装为 MP4");
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
                        self.inner.store.snack("MP4 已生成，源 TS 清理未完成");
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
                self.transition(&id, MediaJobState::Failed, format!("{error}；原 TS 保留"));
                self.inner
                    .store
                    .snack(format!("转 MP4 失败：{error}；原 TS 保留"));
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
        let source_info = probe(&ffprobe, &input.source, "mpegts", &self.inner.stop).await?;
        self.transition(id, MediaJobState::Running, "正在无损转封装为 MP4");
        if std::fs::symlink_metadata(&input.output).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "同名 MP4 已存在，未覆盖",
            ));
        }
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
        )
        .await
        .map_err(|error| io::Error::other(format!("FFmpeg 转封装未完成: {error}")))?;
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
        let output_info = probe(&ffprobe, &input.temporary, "mov", &self.inner.stop).await?;
        if source_info.0 != output_info.0 {
            return Err(invalid("MP4 音视频轨道校验不一致"));
        }
        if let (Some(a), Some(b)) = (source_info.1, output_info.1) {
            if (a - b).abs() > 1.0_f64.max(a * 0.001) {
                return Err(invalid("MP4 时长校验不一致"));
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
            self.transition(id, MediaJobState::Running, "正在校验 MP4 完整性");
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
            run_tool(
                verify,
                &self.inner.stop,
                true,
                media_idle_budget(before.len()),
            )
            .await
            .map_err(|_| invalid("MP4 完整解码校验失败，源 TS 保留"))?;
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
    run_tool(command, stop, false, Duration::from_secs(600)).await
}
async fn run_tool(
    mut command: Command,
    stop: &CancellationToken,
    progress: bool,
    budget: Duration,
) -> io::Result<Vec<u8>> {
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
        let mut previous = 0;
        while let Some(line) = lines.next_line().await? {
            if let Some(value) = line
                .strip_prefix("out_time_us=")
                .and_then(|n| n.trim().parse::<u64>().ok())
            {
                if value > previous {
                    previous = value;
                    *reader_progress.lock().expect("media progress") = tokio::time::Instant::now();
                }
            }
        }
        Ok(Vec::new())
    });
    let err = tokio::spawn(drain_bounded(stderr, 64 * 1024));
    let status = tokio::select! { biased;
        _ = stop.cancelled() => { let _ = child.start_kill(); let _ = child.wait().await; None },
        _ = wait_for_stall(advanced, budget) => { let _ = child.start_kill(); let _ = child.wait().await; None },
        _ = tokio::time::sleep(Duration::from_secs(86400)) => { let _ = child.start_kill(); let _ = child.wait().await; None },
        result = child.wait() => Some(result),
    };
    let stdout = out.await.map_err(io::Error::other)??;
    let stderr = err.await.map_err(io::Error::other)??;
    if stdout.len() > 1024 * 1024 || stderr.len() > 64 * 1024 {
        return Err(io::Error::other("媒体工具响应超出限制"));
    }
    let status =
        status.ok_or_else(|| io::Error::new(io::ErrorKind::Interrupted, "处理已取消或超时"))??;
    if !status.success() {
        return Err(io::Error::other(format!(
            "媒体工具退出码 {:?}",
            status.code()
        )));
    }
    Ok(stdout)
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
    )
    .await?;
    let value: Value = serde_json::from_slice(&raw).map_err(io::Error::other)?;
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
