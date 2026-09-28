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
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::Instant;

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
const LIVE_SOURCE_TTL: Duration = Duration::from_secs(300);
const AUTOMATIC_LIMIT: usize = 3;
const RECOVERY_DELAYS: [u64; 3] = [5, 15, 45];
const RECOVERY_HEALTHY_SECONDS: f64 = 60.0;

#[derive(Clone, Copy)]
struct CheckDeadline {
    instant: Instant,
    timestamp: i64,
}
impl CheckDeadline {
    fn after(delay: Duration) -> Self {
        Self {
            instant: Instant::now() + delay,
            timestamp: chrono::Utc::now().timestamp() + delay.as_secs() as i64,
        }
    }
}
#[derive(Clone, Copy)]
struct RecoveryRequest {
    revision: u64,
    due: CheckDeadline,
}
type RecoveryRequests = Arc<std::sync::Mutex<HashMap<String, RecoveryRequest>>>;

struct CheckCompletion {
    expected: crate::model::Recording,
    recovering: bool,
    result: Result<CheckOutcome, String>,
}

fn retryable_recording_error(error: &str) -> bool {
    !crate::pacing::restricted(error)
        && (transient_finished_check(error)
            || [
                "HTTP 404",
                "拉流超时",
                "连接播放服务器被拒绝",
                "FLV 拉流连接",
                "90 秒无媒体产出",
                "120 秒未收到媒体",
                "平台解析超时",
                "平台请求超时",
                "无法连接平台",
                "排队或解析超过",
                "传输或 TLS",
                "未返回目标房间",
            ]
            .iter()
            .any(|marker| error.contains(marker)))
}

fn requires_manual_access(record: &crate::model::Recording) -> bool {
    record.verification_required
        || matches!(
            record.access_state.as_str(),
            "captcha" | "loginRequired" | "loginPrompt"
        )
}

fn next_window(record: &crate::model::Recording) -> CheckDeadline {
    use chrono::Timelike;
    let now = chrono::Local::now().time().num_seconds_from_midnight();
    let seconds = record
        .scheduled_start_time
        .as_deref()
        .unwrap_or("")
        .split(',')
        .filter_map(|start| {
            chrono::NaiveTime::parse_from_str(start.trim(), "%H:%M:%S")
                .or_else(|_| chrono::NaiveTime::parse_from_str(start.trim(), "%H:%M"))
                .ok()
        })
        .map(|start| (start.num_seconds_from_midnight() + 86400 - now) % 86400)
        .filter(|seconds| *seconds > 0)
        .min()
        .unwrap_or(86400);
    CheckDeadline::after(Duration::from_secs(seconds as u64))
}

pub struct Scheduler {
    pacer: crate::pacing::Pacer,
    recovery_requests: RecoveryRequests,
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
    monitor_requests: Arc<std::sync::Mutex<HashMap<String, CheckDeadline>>>,
    monitor_changed: Arc<tokio::sync::Notify>,
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
                store.snack_error(format!("时间字幕未生成：{error}；本次源文件保留"));
            }
        }
        let room = if self.script.is_some() {
            store
                .get(&process.rec_id)
                .await
                .map(|record| record.streamer_name)
                .unwrap_or_default()
        } else {
            String::new()
        };
        // Subtitles may probe many files. Only the short reservation/job handoff is locked.
        let filesystem = engine.filesystem_guard().await;
        drop(reservation);
        let mut conversions = Vec::new();
        if self.convert {
            match postprocess
                .enqueue_recording_locked(
                    &self.root,
                    &process.output_path,
                    &process.rec_id,
                    self.options,
                )
                .await
            {
                Ok(batch) => {
                    conversions = batch.jobs;
                    if !batch.failures.is_empty() {
                        store.snack_error(format!(
                            "部分文件未转 MP4：{}；对应原 TS 保留",
                            batch.failures.join("；")
                        ));
                    }
                }
                Err(error) => {
                    store.snack_error(format!("自动转 MP4 未开始：{error}；原 TS 保留"));
                }
            }
        }
        if let Some(script) = self.script {
            let mut paths = vec![process.output_path.clone()];
            if self.convert {
                paths.push(process.output_path.with_extension("mp4"));
            }
            if self.subtitles.is_some() {
                paths.push(process.output_path.with_extension("srt"));
            }
            automation.after_recording(
                script,
                crate::automation::ScriptInput {
                    root: self.root,
                    pattern: process.output_path.clone(),
                    room,
                    conversions,
                    processor: postprocess.clone(),
                    reservation: engine.reserve_media(paths),
                },
            );
        }
        drop(filesystem);
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
            recovery_requests: Arc::default(),
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
            monitor_requests: Arc::default(),
            monitor_changed: Arc::new(tokio::sync::Notify::new()),
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
            .extend(
                ids.into_iter()
                    .map(|id| (id, CheckDeadline::after(Duration::ZERO))),
            );
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
        let mut changed = self.interval_changed.subscribe();
        let mut interval = self.monitoring_interval().await;
        let mut deadlines: HashMap<String, CheckDeadline> = HashMap::new();
        let mut fair = crate::pacing::FairQueue::default();
        let mut pending: HashMap<String, (u64, crate::pacing::PendingPermit)> = HashMap::new();
        let mut urgent = HashSet::new();
        let mut running = tokio::task::JoinSet::<CheckCompletion>::new();
        loop {
            if self.stopping.is_cancelled() || *shutdown.borrow() || shutdown.has_changed().is_err()
            {
                running.abort_all();
                while running.join_next().await.is_some() {}
                self.recovery_requests
                    .lock()
                    .expect("recovery requests")
                    .clear();
                for record in self.store.all().await {
                    if !record.is_recording {
                        self.store
                            .update(&record.rec_id, |r| {
                                r.clear_queue();
                                r.check_state = "idle".into();
                                r.next_check_at = None;
                            })
                            .await;
                    }
                }
                return;
            }
            let requested =
                std::mem::take(&mut *self.monitor_requests.lock().expect("monitor requests"));
            let records = self.store.all().await;
            let ids: HashSet<_> = records
                .iter()
                .filter(|r| r.monitor_status)
                .map(|r| r.rec_id.clone())
                .collect();
            deadlines.retain(|id, _| ids.contains(id));
            urgent.retain(|id| ids.contains(id));
            for (id, due) in &requested {
                if due.instant <= Instant::now() {
                    urgent.insert(id.clone());
                } else {
                    urgent.remove(id);
                }
            }
            for record in &records {
                if record.monitor_status {
                    if let Some(due) = requested.get(&record.rec_id) {
                        deadlines.insert(record.rec_id.clone(), *due);
                    } else {
                        deadlines
                            .entry(record.rec_id.clone())
                            .or_insert_with(|| CheckDeadline::after(Duration::ZERO));
                    }
                }
            }
            let recovery = {
                let mut recovery = self.recovery_requests.lock().expect("recovery requests");
                recovery.retain(|id, request| {
                    records.iter().any(|r| {
                        &r.rec_id == id
                            && r.schedule_revision == request.revision
                            && r.monitor_status
                            && !r.is_recording
                            && !requires_manual_access(r)
                    })
                });
                recovery.clone()
            };
            let checking: HashSet<_> = self
                .checks
                .lock()
                .expect("active checks")
                .keys()
                .cloned()
                .collect();
            let (maximum, spacing) = {
                let config = self.config.read().await;
                (
                    config
                        .get_i64("platform_max_concurrent_requests", 3)
                        .clamp(1, 16) as usize,
                    Duration::from_secs(
                        config.get_i64("platform_request_interval", 3).clamp(0, 300) as u64,
                    ),
                )
            };
            let mut candidates = Vec::new();
            for record in records {
                if checking.contains(&record.rec_id) || record.check_state == "rechecking" {
                    continue;
                }
                if !record.monitor_status {
                    self.show_wait(&record, "idle", "paused", None, None, None)
                        .await;
                    continue;
                }
                if record.is_recording {
                    if deadlines[&record.rec_id].instant <= Instant::now() {
                        deadlines.insert(record.rec_id.clone(), CheckDeadline::after(interval));
                    }
                    self.show_wait(&record, "idle", "recording", None, None, None)
                        .await;
                    continue;
                }
                if requires_manual_access(&record) {
                    self.show_wait(&record, "idle", "verification", None, None, None)
                        .await;
                    continue;
                }
                if !crate::schedule::active(&record).unwrap_or(false) {
                    self.recovery_requests
                        .lock()
                        .expect("recovery requests")
                        .remove(&record.rec_id);
                    if record.check_wait_reason != "schedule" {
                        deadlines.insert(record.rec_id.clone(), next_window(&record));
                    }
                    let due = deadlines[&record.rec_id];
                    self.show_wait(
                        &record,
                        "idle",
                        "schedule",
                        None,
                        Some(due.timestamp),
                        Some(due.timestamp),
                    )
                    .await;
                    continue;
                }
                if !self.recording_enabled.load(Ordering::SeqCst) {
                    continue;
                }
                let recovering = recovery.contains_key(&record.rec_id);
                let due = recovery
                    .get(&record.rec_id)
                    .map(|r| r.due)
                    .unwrap_or(deadlines[&record.rec_id]);
                if due.instant > Instant::now() {
                    let reason = if recovering {
                        "recovery"
                    } else if record.recovery_attempt >= RECOVERY_DELAYS.len() as u32 {
                        "recoveryExhausted"
                    } else {
                        "interval"
                    };
                    self.show_wait(
                        &record,
                        if recovering { "recovering" } else { "idle" },
                        reason,
                        None,
                        Some(due.timestamp),
                        Some(deadlines[&record.rec_id].timestamp),
                    )
                    .await;
                } else {
                    candidates.push((record, due, recovering));
                }
            }
            // Register before occupying global workers or platform slots, including normal work.
            candidates.sort_by_key(|(record, due, _)| (due.instant, record.rec_id.clone()));
            let eligible: HashMap<_, _> = candidates
                .iter()
                .map(|(record, _, _)| (record.rec_id.clone(), record.schedule_revision))
                .collect();
            pending.retain(|id, (revision, _)| eligible.get(id) == Some(revision));
            for (record, _, recovering) in &candidates {
                if let Some((_, ticket)) = pending.get(&record.rec_id) {
                    ticket.set_priority(urgent.contains(&record.rec_id) || *recovering);
                } else {
                    if let Ok(ticket) = self.pacer.enqueue(
                        record.platform_key.as_deref().unwrap_or("custom"),
                        urgent.contains(&record.rec_id) || *recovering,
                    ) {
                        pending.insert(record.rec_id.clone(), (record.schedule_revision, ticket));
                    }
                }
            }
            // Simulate the exact admission order once per platform, including manual waiters.
            // A FIFO rank for all but the first priority ticket skips otherwise-ready work.
            let mut snapshots = HashMap::new();
            let mut ranks = HashMap::new();
            for (record, _, _) in &candidates {
                let key = record.platform_key.as_deref().unwrap_or("custom");
                let snapshot = snapshots
                    .entry(key.to_owned())
                    .or_insert_with(|| self.pacer.queue_ranks(key));
                let rank = pending
                    .get(&record.rec_id)
                    .map(|(_, ticket)| ticket.rank_in(snapshot))
                    .unwrap_or(usize::MAX);
                ranks.insert(record.rec_id.clone(), rank);
            }
            // Platform turns and ticket rank decide admission; queued positions describe this pass.
            let mut projected = fair.clone();
            let mut ordered = Vec::new();
            while !candidates.is_empty() {
                let index = (0..candidates.len())
                    .min_by_key(|&i| {
                        let (r, due, _) = &candidates[i];
                        (
                            projected.rank(r.platform_key.as_deref().unwrap_or("custom")),
                            ranks.get(&r.rec_id).copied().unwrap_or(usize::MAX),
                            due.instant,
                            r.rec_id.clone(),
                        )
                    })
                    .expect("nonempty queue");
                let candidate = candidates.remove(index);
                projected.served(candidate.0.platform_key.as_deref().unwrap_or("custom"));
                ordered.push(candidate);
            }
            let mut position = 0;
            for (record, due, recovering) in ordered {
                let key = record.platform_key.as_deref().unwrap_or("custom");
                let gate = self.pacer.status(key, maximum);
                let capacity = running.len() < AUTOMATIC_LIMIT;
                let first = pending
                    .get(&record.rec_id)
                    .is_some_and(|(_, ticket)| ticket.rank() == 0);
                let ready = gate.reason == "queue" && capacity && first;
                if !ready {
                    position += 1;
                    let reason = if gate.reason == "queue" && !capacity {
                        "capacity"
                    } else {
                        gate.reason
                    };
                    let earliest = gate.ready_at.map(|at| {
                        due.timestamp.max(
                            chrono::Utc::now().timestamp()
                                + at.saturating_duration_since(Instant::now())
                                    .as_secs_f64()
                                    .ceil() as i64,
                        )
                    });
                    self.show_wait(
                        &record,
                        if recovering { "recovering" } else { "queued" },
                        reason,
                        Some(position),
                        if reason == "capacity" { None } else { earliest },
                        Some(deadlines[&record.rec_id].timestamp),
                    )
                    .await;
                    continue;
                }
                self.show_wait(
                    &record,
                    "queued",
                    "queue",
                    None,
                    Some(due.timestamp),
                    Some(deadlines[&record.rec_id].timestamp),
                )
                .await;
                if self.store.get(&record.rec_id).await.is_none_or(|r| {
                    r.schedule_revision != record.schedule_revision
                        || !r.monitor_status
                        || r.is_recording
                        || requires_manual_access(&r)
                }) {
                    continue;
                }
                let Ok(operation) = self.begin_check(&record.rec_id) else {
                    continue;
                };
                let Some(permit) = pending
                    .get(&record.rec_id)
                    .and_then(|(_, ticket)| ticket.try_acquire(maximum, spacing))
                else {
                    continue;
                };
                pending.remove(&record.rec_id);
                urgent.remove(&record.rec_id);
                fair.served(key);
                if recovering {
                    self.recovery_requests
                        .lock()
                        .expect("recovery requests")
                        .remove(&record.rec_id);
                }
                if self
                    .store
                    .update_when(
                        &record.rec_id,
                        |r| r.schedule_revision == record.schedule_revision && r.monitor_status,
                        |r| {
                            r.clear_queue();
                            if !recovering {
                                r.recovery_attempt = 0;
                            }
                        },
                    )
                    .await
                    .is_none()
                {
                    continue;
                }
                let scheduler = self.clone();
                running.spawn(async move {
                    let _operation = operation;
                    let result = scheduler
                        .check_with_permit(
                            record.rec_id.clone(),
                            Some(permit),
                            Some(record.schedule_revision),
                        )
                        .await;
                    CheckCompletion {
                        expected: record,
                        recovering,
                        result,
                    }
                });
            }
            tokio::select! { biased;
                _ = self.stopping.cancelled() => {},
                _ = shutdown.changed() => {},
                _ = changed.changed() => {
                    interval = self.monitoring_interval().await;
                    for due in deadlines.values_mut() { *due = CheckDeadline::after(interval); }
                },
                _ = self.monitor_changed.notified() => {},
                completion = running.join_next(), if !running.is_empty() => {
                    match completion {
                        Some(Ok(done)) => {
                            let id = &done.expected.rec_id;
                            if let Some(current) = self.store.get(id).await.filter(|r| r.schedule_revision == done.expected.schedule_revision) {
                                let due = CheckDeadline::after(interval);
                                deadlines.insert(id.clone(), due);
                                self.store.update_when(id, |r| r.schedule_revision == current.schedule_revision,
                                    |r| { r.next_check_at = r.monitor_status.then_some(due.timestamp); }).await;
                                let error = match &done.result {
                                    Err(error) => Some(error),
                                    Ok(CheckOutcome::Failed(error)) if done.recovering => Some(error),
                                    _ => None,
                                };
                                if error.is_some_and(|error| retryable_recording_error(error)) {
                                    self.room_check().request_recovery(&current).await;
                                } else if !current.is_recording && !self.recovery_requests.lock().expect("recovery requests").contains_key(id) {
                                    self.store.update_when(id, |r| r.schedule_revision == current.schedule_revision, |r| {
                                        r.clear_queue();
                                        if matches!(done.result, Ok(CheckOutcome::Offline | CheckOutcome::NotifyOnly | CheckOutcome::StreamEnded)) { r.recovery_attempt = 0; }
                                    }).await;
                                }
                            }
                        },
                        Some(Err(error)) => log::error!("自动检测任务未完成: {error}"),
                        None => {},
                    }
                },
                _ = tokio::time::sleep(Duration::from_secs(1)) => {},
            }
        }
    }

    async fn show_wait(
        &self,
        expected: &crate::model::Recording,
        phase: &str,
        reason: &str,
        position: Option<usize>,
        ready_at: Option<i64>,
        next_at: Option<i64>,
    ) {
        self.store
            .update_when(
                &expected.rec_id,
                |r| {
                    r.schedule_revision == expected.schedule_revision
                        && r.is_recording == expected.is_recording
                        && !self
                            .checks
                            .lock()
                            .expect("active checks")
                            .contains_key(&r.rec_id)
                        && (r.check_state != phase
                            || r.check_wait_reason != reason
                            || r.queue_position != position
                            || r.check_ready_at != ready_at
                            || r.next_check_at != next_at)
                },
                |r| {
                    r.check_state = phase.into();
                    r.check_wait_reason = reason.into();
                    r.queue_position = position;
                    r.check_ready_at = ready_at;
                    r.next_check_at = next_at;
                    if matches!(reason, "paused" | "schedule" | "verification" | "recording") {
                        r.recovery_at = None;
                    }
                },
            )
            .await;
    }

    /// Detection and manual recording share metadata updates and name persistence.
    pub async fn check(&self, rec_id: String) -> Result<CheckOutcome, String> {
        let _operation = self.begin_check(&rec_id)?;
        self.reset_manual_recovery(&rec_id).await;
        let revision = self.store.get(&rec_id).await.map(|r| r.schedule_revision);
        let result = self.check_with_permit(rec_id.clone(), None, revision).await;
        let _lifecycle = self.starting.lock().await;
        self.defer_normal_check(&rec_id, revision).await;
        result
    }

    async fn check_with_permit(
        &self,
        rec_id: String,
        permit: Option<crate::pacing::Permit>,
        revision: Option<u64>,
    ) -> Result<CheckOutcome, String> {
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
        let (expected, info) = match self
            .room_check()
            .resolve_with_permit(&rec_id, true, permit, revision)
            .await
        {
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
                        && r.schedule_revision == expected.schedule_revision
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
        if !current.same_schedule_as(expected)
            || current.platform_key.as_deref() != Some("kuaishou")
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
                || current.schedule_revision != expected.schedule_revision
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
                let mut sources = self.live_sources.write().await;
                sources.retain(|_, (_, when, _)| when.elapsed() <= LIVE_SOURCE_TTL);
                sources.insert(
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
            let was_recording = self.stop_recording_locked(&rec_id, false).await;
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
                        .snack_error(format!("{}：录制失败，{error}", record.streamer_name));
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
        let ffmpeg =
            crate::paths::configured_ffmpeg(self.store.workspace(), self.ffmpeg.as_deref());

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
                r.clear_queue();
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
                                } else {
                                    r.check_state = "rechecking".into();
                                }
                                r.speed = None;
                                r.recording_error = error.clone();
                                r.clear_queue();
                                if process.recorded_seconds() >= RECOVERY_HEALTHY_SECONDS {
                                    r.recovery_attempt = 0;
                                }
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
                            store.snack_error(format!(
                                "{}：录制失败，{error}",
                                record.streamer_name
                            ));
                        }
                        if updated.is_some() {
                            live_sources.write().await.remove(&process.rec_id);
                            drop(_filesystem);
                            drop(_starting);
                            if let Some(record) = &updated {
                                room_check.defer_after_exit(record).await;
                            }
                            if refresh_needed {
                                room_check
                                    .refresh_finished(&process.rec_id, process.run_id)
                                    .await;
                            } else if let Some(record) = updated.as_ref().filter(|r| {
                                r.recording_error
                                    .as_deref()
                                    .is_some_and(retryable_recording_error)
                            }) {
                                room_check.request_recovery(record).await;
                            }
                        }
                        return;
                    }
                    crate::engine::ProcessState::Unknown => return,
                }
            }
        });
    }

    /// Progress is published every second; disk sampling stays on its own two-second cadence.
    fn spawn_speed_sampler(&self, process: Arc<RecorderProcess>) {
        let speed = Arc::new(std::sync::Mutex::new(None::<String>));
        let sampled_speed = speed.clone();
        let sampler_process = process.clone();
        let sampler_engine = self.engine.clone();
        let sampler_stop = self.stopping.clone();
        let storage = self.storage.clone();
        self.background.spawn(async move {
            let mut previous = 0;
            let mut sampled_at = tokio::time::Instant::now();
            loop {
                tokio::select! { biased;
                    _ = sampler_stop.cancelled() => return,
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {},
                }
                if !sampler_engine.is_current(&sampler_process).await {
                    return;
                }
                let pattern = sampler_process.output_path.clone();
                let current = match storage
                    .blocking(move |cancel| {
                        crate::storage::check_cancel(&cancel)?;
                        crate::media_safety::output_bytes(&pattern)
                    })
                    .await
                {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        log::warn!("录制大小统计失败: {error}");
                        continue;
                    }
                };
                let seconds = sampled_at.elapsed().as_secs_f64().max(0.001);
                sampled_at = tokio::time::Instant::now();
                *sampled_speed.lock().expect("recording speed") = Some(format!(
                    "{:.0} KB/s",
                    current.saturating_sub(previous) as f64 / seconds / 1024.0
                ));
                previous = current;
                sampler_process.observe_output_bytes(current);
            }
        });
        let store = self.store.clone();
        let engine = self.engine.clone();
        let stopping = self.stopping.clone();
        self.background.spawn(async move {
            let mut ticks = tokio::time::interval(Duration::from_secs(1));
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            ticks.tick().await;
            loop {
                tokio::select! { biased; _ = stopping.cancelled() => return, _ = ticks.tick() => {} }
                if !engine.is_current(&process).await { return; }
                let sampled = speed.lock().expect("recording speed").clone();
                let elapsed = process.recorded_seconds();
                store.update_when(&process.rec_id, |r| r.is_recording && r.recording_run == Some(process.run_id), move |r| {
                    if let Some(speed) = sampled { r.speed = Some(speed); }
                    r.recorded_seconds = elapsed;
                }).await;
            }
        });
    }

    /// 手动开始录制：忽略监控开关，立即解析并开录。
    /// 未开播时返回错误——对应 Python `recording_button_on_click` 的「该直播间未开播」提示。
    pub async fn force_start(&self, rec_id: String) -> Result<CheckOutcome, String> {
        let _operation = self.begin_check(&rec_id)?;
        self.reset_manual_recovery(&rec_id).await;
        let revision = self.store.get(&rec_id).await.map(|r| r.schedule_revision);
        let result = async {
            let (expected, info) = self.resolve_for(&rec_id).await?;
            self.accept_resolved(&expected, &info, true).await
        }
        .await;
        let _lifecycle = self.starting.lock().await;
        self.defer_normal_check(&rec_id, revision).await;
        result
    }

    async fn reset_manual_recovery(&self, rec_id: &str) {
        self.recovery_requests
            .lock()
            .expect("recovery requests")
            .remove(rec_id);
        self.store
            .update(rec_id, |r| {
                r.schedule_revision = r.schedule_revision.wrapping_add(1);
                r.clear_queue();
                r.recovery_attempt = 0;
            })
            .await;
    }

    async fn defer_normal_check(&self, rec_id: &str, revision: Option<u64>) {
        let due = CheckDeadline::after(self.monitoring_interval().await);
        if let Some(record) = self
            .store
            .update_when(
                rec_id,
                |r| Some(r.schedule_revision) == revision,
                |r| {
                    r.next_check_at =
                        (r.monitor_status && !r.is_recording).then_some(due.timestamp);
                },
            )
            .await
        {
            if record.monitor_status {
                self.monitor_requests
                    .lock()
                    .expect("monitor requests")
                    .insert(rec_id.to_owned(), due);
            }
        }
        self.monitor_changed.notify_one();
    }

    /// Called only after a committed edit, while the API still owns the lifecycle lock.
    pub(crate) async fn edited(
        &self,
        records: &[crate::model::Recording],
        previous: &[crate::model::Recording],
    ) {
        for record in records {
            if let Some(token) = self
                .checks
                .lock()
                .expect("active checks")
                .get(&record.rec_id)
            {
                token.cancel();
            }
            self.recovery_requests
                .lock()
                .expect("recovery requests")
                .remove(&record.rec_id);
            let needs_initial_check = record.monitor_status
                && !record.is_recording
                && previous
                    .iter()
                    .find(|old| old.rec_id == record.rec_id)
                    .is_some_and(|old| {
                        !old.monitor_status
                            || crate::model::room_identity(&old.url)
                                != crate::model::room_identity(&record.url)
                    });
            if needs_initial_check {
                self.request_monitoring([record.rec_id.clone()]);
            } else {
                self.defer_normal_check(&record.rec_id, Some(record.schedule_revision))
                    .await;
            }
        }
    }

    /// All checks, including post-recording refreshes, share one resolver and pacer.
    fn room_check(&self) -> RoomCheck {
        RoomCheck {
            store: self.store.clone(),
            config: self.config.clone(),
            resolver: self.resolver.clone(),
            pacer: self.pacer.clone(),
            checks: self.checks.clone(),
            stopping: self.stopping.clone(),
            notifications: self.notifications.clone(),
            starting: self.starting.clone(),
            recovery_requests: self.recovery_requests.clone(),
            monitor_changed: self.monitor_changed.clone(),
            monitor_requests: self.monitor_requests.clone(),
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
            self.stop_recording_locked(rec_id, true).await;
        } else {
            self.store.update(rec_id, |r| r.invalidate_schedule()).await;
        }
        self.request_monitoring([rec_id.to_owned()]);
        Ok(self.store.get(rec_id).await.unwrap_or(record))
    }

    /// 停止录制并更新状态。
    pub async fn stop_recording(&self, rec_id: &str) -> bool {
        let _starting = self.starting.lock().await;
        self.stop_recording_locked(rec_id, true).await
    }

    async fn stop_recording_locked(&self, rec_id: &str, invalidate: bool) -> bool {
        self.recovery_requests
            .lock()
            .expect("recovery requests")
            .remove(rec_id);
        if let Some(token) = self.checks.lock().expect("active checks").get(rec_id) {
            token.cancel();
        }
        let _filesystem = self.engine.filesystem_guard().await;
        let process = self.engine.process(rec_id).await;
        let stopped = self.engine.stop(rec_id, 15).await;
        let elapsed = process.as_ref().map(|p| p.recorded_seconds());
        self.store
            .update(rec_id, |r| {
                if invalidate {
                    r.invalidate_schedule();
                } else {
                    r.clear_queue();
                }
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
        if invalidate {
            let revision = self.store.get(rec_id).await.map(|r| r.schedule_revision);
            self.defer_normal_check(rec_id, revision).await;
        }
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
            self.stop_recording_locked(&id, true).await;
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
    /// Called only after task removal has committed; active recordings remain protected by Store.
    pub async fn forget_recordings(&self, ids: &[String]) {
        {
            let checks = self.checks.lock().expect("active checks");
            for id in ids {
                if let Some(cancel) = checks.get(id) {
                    cancel.cancel();
                }
            }
        }
        self.monitor_requests
            .lock()
            .expect("monitor requests")
            .retain(|id, _| !ids.contains(id));
        self.recovery_requests
            .lock()
            .expect("recovery requests")
            .retain(|id, _| !ids.contains(id));
        self.schedule_edges
            .lock()
            .expect("schedule edges")
            .retain(|id, _| !ids.contains(id));
        self.live_sources
            .write()
            .await
            .retain(|id, _| !ids.contains(id));
        self.monitor_changed.notify_one();
    }

    /// Preview reuses a fresh verified source and never performs a background room request.
    pub async fn preview_input(&self, rec_id: &str) -> Result<crate::preview::LiveInput, String> {
        let record = self.store.get(rec_id).await.ok_or("任务不存在")?;
        let (url, when, info) = {
            let mut cache = self.live_sources.write().await;
            let expired = cache
                .get(rec_id)
                .is_some_and(|(_, when, _)| when.elapsed() > LIVE_SOURCE_TTL);
            cache.retain(|_, (_, when, _)| when.elapsed() <= LIVE_SOURCE_TTL);
            cache.get(rec_id).cloned().ok_or(if expired {
                "直播源已过期，请先手动检测状态"
            } else {
                "请先检测直播状态，再预览直播源"
            })?
        };
        if record.url != url || !record.is_live || when.elapsed() > LIVE_SOURCE_TTL {
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
            self.store.snack_error(format!("录制空间保护：{error}"));
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
    checks: Arc<
        std::sync::Mutex<std::collections::HashMap<String, tokio_util::sync::CancellationToken>>,
    >,
    stopping: tokio_util::sync::CancellationToken,
    notifications: crate::notifications::Notifications,
    starting: Arc<tokio::sync::Mutex<()>>,
    recovery_requests: RecoveryRequests,
    monitor_changed: Arc<tokio::sync::Notify>,
    monitor_requests: Arc<std::sync::Mutex<HashMap<String, CheckDeadline>>>,
}

fn transient_finished_check(error: &str) -> bool {
    // Never turn authentication, a challenge, or rate limiting into a retry burst.
    if [
        "429",
        "403",
        "冷却",
        "过快",
        "频繁",
        "限制访问",
        "风控",
        "captcha",
        "验证码",
        "需要登录",
    ]
    .iter()
    .any(|s| error.contains(s))
    {
        return false;
    }
    [
        "空响应",
        "未返回目标房间",
        "没有可验证的房间数据",
        "请求超时",
        "平台解析超时",
        "传输或 TLS",
        "HTTP 502",
        "HTTP 503",
        "HTTP 504",
    ]
    .iter()
    .any(|s| error.contains(s))
}

impl RoomCheck {
    async fn defer_after_exit(&self, expected: &crate::model::Recording) {
        let _lifecycle = self.starting.lock().await;
        let interval = self
            .config
            .read()
            .await
            .get_i64("loop_time_seconds", 300)
            .max(30) as u64;
        let due = CheckDeadline::after(Duration::from_secs(interval));
        let updated = self
            .store
            .update_when(
                &expected.rec_id,
                |r| {
                    r.schedule_revision == expected.schedule_revision
                        && r.recording_run == expected.recording_run
                        && r.monitor_status
                        && !r.is_recording
                },
                |r| r.next_check_at = Some(due.timestamp),
            )
            .await;
        if updated.is_some() {
            self.monitor_requests
                .lock()
                .expect("monitor requests")
                .insert(expected.rec_id.clone(), due);
            self.monitor_changed.notify_one();
        }
    }

    async fn request_recovery(&self, expected: &crate::model::Recording) -> bool {
        let _lifecycle = self.starting.lock().await;
        let Some(record) = self.store.get(&expected.rec_id).await.filter(|r| {
            !self.stopping.is_cancelled()
                && r.schedule_revision == expected.schedule_revision
                && r.url == expected.url
                && r.quality == expected.quality
                && r.recording_run == expected.recording_run
                && r.monitor_status
                && !r.is_recording
                && r.only_notify_no_record != Some(true)
                && !requires_manual_access(r)
                && crate::schedule::active(r).unwrap_or(false)
                && !r
                    .check_error
                    .as_deref()
                    .is_some_and(crate::pacing::restricted)
                && !r
                    .recording_error
                    .as_deref()
                    .is_some_and(crate::pacing::restricted)
        }) else {
            return false;
        };
        if self
            .recovery_requests
            .lock()
            .expect("recovery requests")
            .contains_key(&record.rec_id)
        {
            return false;
        }
        let Some(delay) = RECOVERY_DELAYS.get(record.recovery_attempt as usize) else {
            self.store
                .update(&record.rec_id, |r| {
                    r.clear_queue();
                    r.check_wait_reason = "recoveryExhausted".into();
                    r.check_state = "idle".into();
                })
                .await;
            return false;
        };
        let due = CheckDeadline::after(Duration::from_secs(*delay));
        self.recovery_requests
            .lock()
            .expect("recovery requests")
            .insert(
                record.rec_id.clone(),
                RecoveryRequest {
                    revision: record.schedule_revision,
                    due,
                },
            );
        self.store
            .update(&record.rec_id, |r| {
                r.clear_queue();
                r.recovery_attempt += 1;
                r.recovery_at = Some(due.timestamp);
                r.check_ready_at = Some(due.timestamp);
                r.check_wait_reason = "recovery".into();
                r.check_state = "recovering".into();
            })
            .await;
        self.monitor_changed.notify_one();
        true
    }

    async fn check_phase(&self, expected: &crate::model::Recording, phase: &str) {
        self.store
            .update_when(
                &expected.rec_id,
                |record| {
                    record.url == expected.url
                        && record.schedule_revision == expected.schedule_revision
                        && record.quality == expected.quality
                        && record.recording_run == expected.recording_run
                        && record.check_state != phase
                },
                |record| {
                    record.check_state = phase.into();
                    if phase != "queued" {
                        record.clear_queue();
                    }
                    if phase == "checking" || phase == "rechecking" {
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
        self.resolve_for_mode(rec_id, true).await
    }

    async fn resolve_for_mode(
        &self,
        rec_id: &str,
        publish_failure: bool,
    ) -> Result<(crate::model::Recording, crate::resolver::StreamInfo), String> {
        self.resolve_with_permit(rec_id, publish_failure, None, None)
            .await
    }

    async fn resolve_with_permit(
        &self,
        rec_id: &str,
        publish_failure: bool,
        permit: Option<crate::pacing::Permit>,
        revision: Option<u64>,
    ) -> Result<(crate::model::Recording, crate::resolver::StreamInfo), String> {
        let Some(rec) = self.store.get(rec_id).await else {
            return Err(format!("任务不存在: {rec_id}"));
        };
        if revision.is_some_and(|revision| revision != rec.schedule_revision) {
            return Err("任务已更新，旧检测结果已丢弃".into());
        }

        let key = rec.platform_key.as_deref().unwrap_or("custom");
        let stop = self
            .checks
            .lock()
            .expect("active checks")
            .get(rec_id)
            .cloned()
            .unwrap_or_else(|| self.stopping.clone());
        self.check_phase(
            &rec,
            if publish_failure {
                "queued"
            } else {
                "rechecking"
            },
        )
        .await;
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
            let _permit = match permit {
                Some(permit) => permit,
                None => {
                    let gate = self.pacer.status(key, maximum);
                    self.store
                        .update_when(
                            rec_id,
                            |r| r.schedule_revision == rec.schedule_revision,
                            |r| {
                                r.check_wait_reason = gate.reason.into();
                                r.check_ready_at = gate.ready_at.map(|at| {
                                    chrono::Utc::now().timestamp()
                                        + at.saturating_duration_since(Instant::now()).as_secs()
                                            as i64
                                });
                            },
                        )
                        .await;
                    self.pacer
                        .acquire(key, maximum, Duration::from_secs(spacing), &stop)
                        .await?
                }
            };
            self.check_phase(
                &rec,
                if publish_failure {
                    "checking"
                } else {
                    "rechecking"
                },
            )
            .await;
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
        if publish_failure {
            self.check_phase(&rec, "idle").await;
        }
        if stop.is_cancelled() {
            return Err("检测已取消".into());
        }
        if self.store.get(rec_id).await.is_none_or(|current| {
            current.url != rec.url
                || current.schedule_revision != rec.schedule_revision
                || current.quality != rec.quality
                || current.recording_run != rec.recording_run
        }) {
            return Err("任务已更新，旧检测结果已丢弃".into());
        }
        self.pacer.result(key, &result);
        if let Err(error) = &result {
            if !publish_failure {
                return Err(error.clone());
            }
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
                            && record.schedule_revision == rec.schedule_revision
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
        self.refresh_finished_with(rec_id, run_id, || async {
            self.resolve_for_mode(rec_id, false)
                .await
                .map(|(_, info)| info)
        })
        .await;
    }

    async fn refresh_finished_with<F, Fut>(&self, rec_id: &str, run_id: uuid::Uuid, mut resolve: F)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<crate::resolver::StreamInfo, String>>,
    {
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
        self.check_phase(&expected, "rechecking").await;
        let mut result = resolve().await;
        for delay in [3, 8] {
            if !result
                .as_ref()
                .is_err_and(|error| transient_finished_check(error))
            {
                break;
            }
            let token = self
                .checks
                .lock()
                .expect("active checks")
                .get(rec_id)
                .cloned();
            let Some(token) = token else {
                return;
            };
            tokio::select! { biased;
                _ = token.cancelled() => return,
                _ = self.stopping.cancelled() => return,
                _ = tokio::time::sleep(Duration::from_secs(delay)) => {},
            }
            if self.store.get(rec_id).await.is_none_or(|r| {
                r.recording_run != Some(run_id)
                    || r.schedule_revision != expected.schedule_revision
                    || r.is_recording
                    || r.url != expected.url
                    || r.quality != expected.quality
            }) {
                return;
            }
            result = resolve().await;
        }
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
                        && record.schedule_revision == expected.schedule_revision
                        && record.recording_run == expected.recording_run
                        && record.url == expected.url
                        && record.quality == expected.quality
                        && !record.is_recording
                },
                |record| {
                    record.recording_run = None;
                    record.check_state = "idle".into();
                    record.clear_queue();
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
                                record.recovery_attempt = 0;
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
            if result.as_ref().is_ok_and(|info| info.is_live)
                || result
                    .as_ref()
                    .is_err_and(|error| retryable_recording_error(error))
            {
                self.request_recovery(&record).await;
            }
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

    async fn settle() {
        for _ in 0..60 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn resumed_monitor_is_prioritized_before_a_busy_platform_slot() {
        let root = tempfile::tempdir().unwrap();
        let state = crate::api::bootstrap(Workspace::from_repo_root(root.path()))
            .await
            .unwrap();
        state.config.write().await.update_user_config(serde_json::json!({"platform_request_interval":"0","platform_max_concurrent_requests":"1","loop_time_seconds":"4500"}).as_object().unwrap().clone()).unwrap();
        let records = ["a-normal", "b-normal", "z-resumed"]
            .into_iter()
            .map(|id| {
                let mut r =
                    Recording::new(id.into(), format!("http://127.0.0.1:1/{id}.mp4"), id.into());
                r.only_notify_no_record = Some(true);
                r
            })
            .collect();
        state.store.insert(records).await.unwrap();
        let held = state
            .scheduler
            .pacer
            .enqueue("custom", false)
            .unwrap()
            .try_acquire(1, Duration::ZERO)
            .unwrap();
        let guard = state.store.lifecycle_guard().await;
        let (stop, rx) = tokio::sync::watch::channel(false);
        let run = tokio::spawn(state.scheduler.clone().run(rx));
        settle().await;
        state.scheduler.request_monitoring(["z-resumed".into()]);
        settle().await;
        drop(held);
        tokio::time::advance(Duration::from_secs(1)).await;
        settle().await;
        assert_eq!(
            state
                .scheduler
                .checks
                .lock()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["z-resumed"]
        );
        drop(guard);
        settle().await;
        for _ in 0..4 {
            tokio::time::advance(Duration::from_secs(1)).await;
            settle().await;
        }
        assert!(state.store.all().await.iter().all(|r| r.is_live));
        stop.send(true).unwrap();
        run.await.unwrap();
        crate::api::shutdown(&state).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn mixed_priority_queue_fills_available_slots_without_an_extra_tick() {
        let root = tempfile::tempdir().unwrap();
        let state = crate::api::bootstrap(Workspace::from_repo_root(root.path()))
            .await
            .unwrap();
        state.config.write().await.update_user_config(serde_json::json!({"platform_request_interval":"0","platform_max_concurrent_requests":"16","loop_time_seconds":"4500"}).as_object().unwrap().clone()).unwrap();
        let records = ["a-normal", "b-normal", "y-priority", "z-priority"]
            .into_iter()
            .map(|id| {
                let mut record =
                    Recording::new(id.into(), format!("http://127.0.0.1:1/{id}.mp4"), id.into());
                record.only_notify_no_record = Some(true);
                record
            })
            .collect();
        state.store.insert(records).await.unwrap();
        let held: Vec<_> = (0..16)
            .map(|_| {
                state
                    .scheduler
                    .pacer
                    .enqueue("custom", false)
                    .unwrap()
                    .try_acquire(16, Duration::ZERO)
                    .unwrap()
            })
            .collect();
        let guard = state.store.lifecycle_guard().await;
        let (stop, rx) = tokio::sync::watch::channel(false);
        let run = tokio::spawn(state.scheduler.clone().run(rx));
        settle().await;
        drop(held);
        state
            .scheduler
            .request_monitoring(["y-priority".into(), "z-priority".into()]);
        settle().await;
        let mut admitted = state
            .scheduler
            .checks
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        admitted.sort();
        let capacity_filled = admitted.len() == AUTOMATIC_LIMIT;
        drop(guard);
        stop.send(true).unwrap();
        run.await.unwrap();
        crate::api::shutdown(&state).await.unwrap();
        assert!(
            capacity_filled,
            "available global slot left idle: {admitted:?}"
        );
        assert_eq!(admitted, ["a-normal", "y-priority", "z-priority"]);
    }

    #[tokio::test(start_paused = true)]
    async fn automatic_workers_are_bounded_and_do_not_wait_inside_platform_slots() {
        let root = tempfile::tempdir().unwrap();
        let state = crate::api::bootstrap(Workspace::from_repo_root(root.path()))
            .await
            .unwrap();
        state
            .config
            .write()
            .await
            .update_user_config(
                serde_json::json!({"platform_request_interval":"0",
            "platform_max_concurrent_requests":"16","loop_time_seconds":"4500"})
                .as_object()
                .unwrap()
                .clone(),
            )
            .unwrap();
        state
            .store
            .insert(
                (0..7)
                    .map(|n| {
                        let mut r = Recording::new(
                            format!("q-{n}"),
                            format!("http://127.0.0.1:1/{n}.mp4"),
                            n.to_string(),
                        );
                        r.only_notify_no_record = Some(true);
                        r
                    })
                    .collect(),
            )
            .await
            .unwrap();
        let guard = state.store.lifecycle_guard().await;
        let (stop, rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(state.scheduler.clone().run(rx));
        settle().await;
        assert_eq!(
            state.scheduler.checks.lock().unwrap().len(),
            AUTOMATIC_LIMIT
        );
        let records = state.store.all().await;
        assert_eq!(
            records
                .iter()
                .filter(|r| r.check_wait_reason == "capacity")
                .count(),
            4
        );
        assert_eq!(
            records
                .iter()
                .filter_map(|r| r.queue_position)
                .collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        drop(guard);
        settle().await;
        assert!(state.store.all().await.iter().all(|r| r.is_live));
        assert!(state.scheduler.checks.lock().unwrap().is_empty());
        stop.send(true).unwrap();
        task.await.unwrap();
        crate::api::shutdown(&state).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn cooling_and_outside_window_rooms_never_block_a_ready_platform() {
        let root = tempfile::tempdir().unwrap();
        let state = crate::api::bootstrap(Workspace::from_repo_root(root.path()))
            .await
            .unwrap();
        let mut restricted =
            Recording::new("a".into(), "http://127.0.0.1:1/a.mp4".into(), "A".into());
        restricted.platform_key = Some("restricted-fixture".into());
        let mut scheduled =
            Recording::new("b".into(), "http://127.0.0.1:1/b.mp4".into(), "B".into());
        scheduled.scheduled_recording = Some(true);
        scheduled.scheduled_start_time = Some(
            (chrono::Local::now() + chrono::Duration::hours(2))
                .format("%H:%M:%S")
                .to_string(),
        );
        scheduled.monitor_hours = Some("0.1".into());
        let mut ready = Recording::new("c".into(), "http://127.0.0.1:1/c.mp4".into(), "C".into());
        ready.only_notify_no_record = Some(true);
        state
            .store
            .insert(vec![restricted, scheduled, ready])
            .await
            .unwrap();
        state
            .scheduler
            .pacer
            .result("restricted-fixture", &Err("HTTP 429".into()));
        let (stop, rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(state.scheduler.clone().run(rx));
        settle().await;
        let a = state.store.get("a").await.unwrap();
        let b = state.store.get("b").await.unwrap();
        assert_eq!(a.check_wait_reason, "cooldown");
        assert!(a.last_check_at.is_none());
        assert_eq!(b.check_wait_reason, "schedule");
        assert!(b.last_check_at.is_none());
        assert!(state.store.get("c").await.unwrap().is_live);
        stop.send(true).unwrap();
        task.await.unwrap();
        crate::api::shutdown(&state).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn recovery_is_bounded_and_runtime_only() {
        let (_root, state, _) = finished_recording_fixture().await;
        state
            .store
            .update("finished", |r| {
                r.recording_run = None;
                r.recording_error = None;
            })
            .await;
        for (index, delay) in RECOVERY_DELAYS.into_iter().enumerate() {
            let expected = state.store.get("finished").await.unwrap();
            assert!(
                state
                    .scheduler
                    .room_check()
                    .request_recovery(&expected)
                    .await
            );
            assert!(
                !state
                    .scheduler
                    .room_check()
                    .request_recovery(&expected)
                    .await,
                "duplicate recovery is coalesced"
            );
            let current = state.store.get("finished").await.unwrap();
            assert_eq!(current.recovery_attempt, index as u32 + 1);
            assert_eq!(current.check_state, "recovering");
            let request = state
                .scheduler
                .recovery_requests
                .lock()
                .unwrap()
                .remove("finished")
                .unwrap();
            assert_eq!(
                request.due.instant - Instant::now(),
                Duration::from_secs(delay)
            );
            let json = current.to_storage();
            for key in [
                "recovery_at",
                "recoveryAt",
                "recovery_attempt",
                "queue_position",
                "queuePosition",
                "schedule_revision",
            ] {
                assert!(
                    json.get(key).is_none(),
                    "runtime field {key} must not enter the saved schema"
                );
            }
        }
        let current = state.store.get("finished").await.unwrap();
        assert!(
            !state
                .scheduler
                .room_check()
                .request_recovery(&current)
                .await
        );
        assert_eq!(
            state.store.get("finished").await.unwrap().check_wait_reason,
            "recoveryExhausted"
        );
        crate::api::shutdown(&state).await.unwrap();
    }

    #[tokio::test]
    async fn recovery_never_overrides_stop_edits_authentication_or_notify_only() {
        for action in [
            "stop",
            "pause-resume",
            "edit",
            "captcha",
            "rate-limit",
            "notify",
        ] {
            let (_root, state, _) = finished_recording_fixture().await;
            state
                .store
                .update("finished", |r| {
                    r.recording_run = None;
                    r.recording_error = None;
                })
                .await;
            let original = state.store.get("finished").await.unwrap();
            match action {
                "stop" => {
                    state.scheduler.stop_recording("finished").await;
                }
                "pause-resume" => {
                    state.scheduler.toggle_monitor("finished").await.unwrap();
                    state.scheduler.toggle_monitor("finished").await.unwrap();
                }
                "edit" => {
                    state
                        .store
                        .update("finished", |r| r.invalidate_schedule())
                        .await;
                }
                "captcha" => {
                    state
                        .store
                        .update("finished", |r| r.verification_required = true)
                        .await;
                }
                "rate-limit" => {
                    state
                        .store
                        .update("finished", |r| r.check_error = Some("HTTP 429".into()))
                        .await;
                }
                "notify" => {
                    state
                        .store
                        .update("finished", |r| r.only_notify_no_record = Some(true))
                        .await;
                }
                _ => unreachable!(),
            }
            assert!(
                !state
                    .scheduler
                    .room_check()
                    .request_recovery(&original)
                    .await,
                "{action}"
            );
            assert!(state.scheduler.recovery_requests.lock().unwrap().is_empty());
            crate::api::shutdown(&state).await.unwrap();
        }
        for error in [
            "HTTP 403",
            "HTTP 401",
            "验证码",
            "Cookie 无效",
            "录制磁盘空间不足",
            "无法访问录制文件",
            "未找到 ffmpeg",
        ] {
            assert!(!retryable_recording_error(error), "{error}");
        }
        for error in [
            "播放地址返回 HTTP 404",
            "拉流超时，请检查网络后重试",
            "FLV 拉流连接中断",
            "平台返回 HTTP 503",
        ] {
            assert!(retryable_recording_error(error), "{error}");
        }
    }

    #[tokio::test]
    async fn live_source_expiry_and_task_removal_release_cached_state() {
        let (_dir, state, expected) = finished_recording_fixture().await;
        state.store.update("finished", |r| r.is_live = true).await;
        let info = crate::resolver::StreamInfo {
            is_live: true,
            record_url: "https://media.example.invalid/stream.flv".into(),
            ..Default::default()
        };
        state.scheduler.live_sources.write().await.insert(
            "finished".into(),
            (
                expected.url.clone(),
                std::time::Instant::now() - LIVE_SOURCE_TTL - Duration::from_secs(1),
                info.clone(),
            ),
        );
        assert!(state
            .scheduler
            .preview_input("finished")
            .await
            .err()
            .unwrap()
            .contains("过期"));
        assert!(state.scheduler.live_sources.read().await.is_empty());
        state.scheduler.live_sources.write().await.insert(
            "finished".into(),
            (expected.url, std::time::Instant::now(), info),
        );
        assert!(state.scheduler.preview_input("finished").await.is_ok());
        let lease = state.scheduler.begin_check("finished").unwrap();
        let cancel = state
            .scheduler
            .checks
            .lock()
            .unwrap()
            .get("finished")
            .unwrap()
            .clone();
        state.scheduler.request_monitoring(["finished".into()]);
        state
            .scheduler
            .schedule_edges
            .lock()
            .unwrap()
            .insert("finished".into(), true);
        let ids = ["finished".into()];
        state.store.remove(&ids).await.unwrap();
        state.scheduler.forget_recordings(&ids).await;
        assert!(cancel.is_cancelled());
        assert!(state.scheduler.live_sources.read().await.is_empty());
        assert!(state.scheduler.monitor_requests.lock().unwrap().is_empty());
        assert!(state.scheduler.schedule_edges.lock().unwrap().is_empty());
        drop(lease);
        assert!(state.scheduler.checks.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn ended_room_empty_response_is_retried_without_publishing_a_false_live_or_error() {
        let (_dir, state, expected) = finished_recording_fixture().await;
        state
            .store
            .update("finished", |r| {
                r.check_error = None;
                r.recording_error = None;
            })
            .await;
        let mut events = state.store.subscribe();
        let calls = std::cell::Cell::new(0);
        let started = tokio::time::Instant::now();
        state
            .scheduler
            .room_check()
            .refresh_finished_with("finished", expected.recording_run.unwrap(), || {
                let attempt = calls.get();
                calls.set(attempt + 1);
                async move {
                    if attempt == 0 {
                        Err("平台返回空响应（未验证直播状态）".into())
                    } else {
                        Ok(crate::resolver::StreamInfo {
                            is_live: false,
                            ..Default::default()
                        })
                    }
                }
            })
            .await;
        assert_eq!(calls.get(), 2);
        assert!(started.elapsed() >= Duration::from_secs(3));
        let current = state.store.get("finished").await.unwrap();
        assert!(!current.is_live && !current.is_recording && current.monitor_status);
        assert!(current.check_error.is_none() && current.live_title.is_none());
        while let Ok(event) = events.try_recv() {
            if event.topic == "update" {
                assert!(
                    event.payload["checkError"].is_null(),
                    "transient errors stay inside recheck: {:?}",
                    event.payload
                );
            }
        }
        crate::api::shutdown(&state).await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn ended_room_rechecks_are_bounded_and_do_not_retry_rate_limits() {
        for (error, wanted) in [
            ("平台返回空响应（未验证直播状态）", 3),
            ("平台返回 HTTP 429", 1),
        ] {
            let (_dir, state, expected) = finished_recording_fixture().await;
            let calls = std::cell::Cell::new(0);
            state
                .scheduler
                .room_check()
                .refresh_finished_with("finished", expected.recording_run.unwrap(), || {
                    calls.set(calls.get() + 1);
                    async { Err(error.to_owned()) }
                })
                .await;
            assert_eq!(calls.get(), wanted);
            let current = state.store.get("finished").await.unwrap();
            assert!(!current.is_recording && current.check_error.is_some());
            assert!(
                current.is_live,
                "unreadable is not proof of offline; UI excludes stale state"
            );
            assert!(current.recording_run.is_none());
            crate::api::shutdown(&state).await.unwrap();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn editing_or_stopping_during_finished_recheck_cancels_late_results() {
        let (_dir, state, expected) = finished_recording_fixture().await;
        let calls = std::cell::Cell::new(0);
        let store = state.store.clone();
        state
            .scheduler
            .room_check()
            .refresh_finished_with("finished", expected.recording_run.unwrap(), || {
                calls.set(calls.get() + 1);
                let store = store.clone();
                async move {
                    store
                        .update("finished", |r| {
                            r.recording_run = None;
                            r.check_state = "idle".into();
                        })
                        .await;
                    Err("平台返回空响应".into())
                }
            })
            .await;
        assert_eq!(calls.get(), 1);
        assert!(state
            .store
            .get("finished")
            .await
            .unwrap()
            .check_error
            .is_none());
        crate::api::shutdown(&state).await.unwrap();
    }

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
            let recovering = monitored
                && (outcome.as_ref().is_ok_and(|info| info.is_live)
                    || outcome
                        .as_ref()
                        .is_err_and(|error| retryable_recording_error(error)));
            assert_eq!(
                current.check_state,
                if recovering { "recovering" } else { "idle" }
            );
            assert_eq!(current.recovery_at.is_some(), recovering);
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
