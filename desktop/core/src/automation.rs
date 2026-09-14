//! Explicit native automation: validated argv scripts and cancellable shutdown scheduling.
use crate::{config::ConfigStore, store::Store};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{RwLock, Semaphore};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScriptSpec {
    pub argv: Vec<String>,
}
impl ScriptSpec {
    pub fn parse(value: &str) -> Result<Self, String> {
        let argv: Vec<String> = serde_json::from_str(value)
            .map_err(|_| "脚本命令必须是 JSON 参数数组，不通过 shell 执行")?;
        if argv.is_empty()
            || argv.len() > 64
            || argv.iter().any(|s| s.contains('\0') || s.len() > 8192)
            || !std::path::Path::new(&argv[0]).is_absolute()
            || argv[0].contains(['{', '}'])
            || (cfg!(windows)
                && !std::path::Path::new(&argv[0])
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("exe")))
        {
            return Err("脚本必须指定绝对可执行文件路径和有效参数".into());
        }
        Ok(Self { argv })
    }
    pub fn arguments(&self, file: &str, room: &str) -> Vec<String> {
        self.argv
            .iter()
            .map(|s| s.replace("{file}", file).replace("{room}", room))
            .collect()
    }
}
#[derive(Default)]
struct ShutdownPlan {
    quick: Option<chrono::DateTime<chrono::Local>>,
    scheduled: Option<(String, chrono::DateTime<chrono::Local>)>,
    requested: bool,
}
impl ShutdownPlan {
    fn due(&mut self, now: chrono::DateTime<chrono::Local>, enabled: bool, time: &str) -> bool {
        let parsed = chrono::NaiveTime::parse_from_str(time, "%H:%M")
            .or_else(|_| chrono::NaiveTime::parse_from_str(time, "%H:%M:%S"))
            .ok();
        if !enabled {
            self.scheduled = None;
        } else if self.scheduled.as_ref().is_none_or(|(key, _)| key != time) {
            self.scheduled = parsed
                .and_then(|at| next_shutdown(now, at))
                .map(|at| (time.to_owned(), at));
        }
        let quick = self.quick.is_some_and(|at| at <= now);
        let scheduled = self.scheduled.as_ref().is_some_and(|(_, at)| *at <= now);
        if !(quick || scheduled) || self.requested {
            return false;
        }
        self.requested = true;
        if quick {
            self.quick = None;
        }
        if scheduled {
            self.scheduled = parsed
                .and_then(|at| next_shutdown(now, at))
                .map(|at| (time.to_owned(), at));
        }
        true
    }
}
fn next_shutdown(
    now: chrono::DateTime<chrono::Local>,
    time: chrono::NaiveTime,
) -> Option<chrono::DateTime<chrono::Local>> {
    let today = now
        .date_naive()
        .and_time(time)
        .and_local_timezone(chrono::Local)
        .earliest()?;
    if today > now {
        Some(today)
    } else {
        now.date_naive()
            .succ_opt()?
            .and_time(time)
            .and_local_timezone(chrono::Local)
            .earliest()
    }
}

#[derive(Clone)]
pub struct Automation {
    config: Arc<RwLock<ConfigStore>>,
    store: Store,
    stop: CancellationToken,
    tasks: TaskTracker,
    slots: Arc<Semaphore>,
    admission: Arc<Mutex<()>>,
    shutdown_plan: Arc<Mutex<ShutdownPlan>>,
}
impl Automation {
    pub fn new(config: Arc<RwLock<ConfigStore>>, store: Store) -> Self {
        Self {
            config,
            store,
            stop: CancellationToken::new(),
            tasks: TaskTracker::new(),
            slots: Arc::new(Semaphore::new(16)),
            admission: Arc::new(Mutex::new(())),
            shutdown_plan: Arc::new(Mutex::new(ShutdownPlan::default())),
        }
    }
    pub fn quick_shutdown(&self, hours: Option<f64>) -> Result<(), String> {
        let when = match hours {
            Some(n) if n.is_finite() && n > 0.0 && n <= 168.0 => {
                Some(chrono::Local::now() + chrono::Duration::seconds((n * 3600.0).ceil() as i64))
            }
            Some(_) => return Err("关机倒计时应大于 0 且不超过 168 小时".into()),
            None => None,
        };
        let mut plan = self.shutdown_plan.lock().expect("shutdown schedule");
        plan.quick = when;
        plan.requested = false;
        self.store.emit(
            "shutdownSchedule",
            serde_json::json!({"at":when.map(|d|d.to_rfc3339())}),
        );
        Ok(())
    }
    pub async fn tick(&self) {
        let config = self.config.read().await;
        let enabled = config.get_bool("scheduled_shutdown_enabled", false);
        let time = config.get_str("scheduled_shutdown_time", "23:00");
        drop(config);
        let mut plan = self.shutdown_plan.lock().expect("shutdown schedule");
        if plan.due(chrono::Local::now(), enabled, &time) {
            self.store
                .emit("nativeShutdown", serde_json::json!({"countdownSeconds":60}));
        }
    }
    pub fn after_recording(
        &self,
        spec: ScriptSpec,
        root: PathBuf,
        pattern: PathBuf,
        room: String,
        task_id: String,
        processor: crate::postprocess::Postprocessor,
    ) {
        let _admission = self.admission.lock().expect("script admission");
        if self.stop.is_cancelled() {
            return;
        }
        let Ok(permit) = self.slots.clone().try_acquire_owned() else {
            self.store.snack("录后脚本队列已满");
            return;
        };
        let manager = self.clone();
        self.tasks.spawn(async move{
      let _permit=permit;
      let work=async {
        while processor.jobs().iter().any(|j|j.task_id.as_deref()==Some(&task_id)&&crate::media_safety::matches_output(&pattern,&root.join(&j.source))&&j.state.pending()){tokio::select!{biased;_=manager.stop.cancelled()=>return,_=tokio::time::sleep(Duration::from_millis(200))=>{}}}
        let root=match root.canonicalize(){Ok(root)=>root,Err(_)=>return};
        let paths=crate::media_safety::recording_outputs(&pattern).unwrap_or_default();let mut outputs=paths;
        for job in processor.jobs().into_iter().filter(|j|j.task_id.as_deref()==Some(&task_id)&&crate::media_safety::matches_output(&pattern,&root.join(&j.source))&&matches!(j.state,crate::model::MediaJobState::Complete|crate::model::MediaJobState::CleanupFailed)) {let source=root.join(&job.source);outputs.retain(|p|p!=&source);outputs.push(root.join(&job.output));}
        outputs.sort();outputs.dedup();
        for file in outputs{if manager.stop.is_cancelled(){return;}let args=spec.arguments(&file.to_string_lossy(),&room);let mut command=tokio::process::Command::new(&args[0]);command.args(&args[1..]).current_dir(&root).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true);
            #[cfg(windows)]command.creation_flags(0x08000000);
            match command.spawn(){Ok(mut child)=>{let _owned_job=match crate::owned_process::ChildJob::attach(&child){Ok(job)=>job,Err(_)=>{let _=child.kill().await;let _=child.wait().await;manager.store.snack("无法隔离录后脚本进程，已停止本次脚本");continue;}};let result=tokio::select!{biased;_=manager.stop.cancelled()=>None,result=tokio::time::timeout(Duration::from_secs(300),child.wait())=>result.ok().and_then(Result::ok)};match result{Some(status)if status.success()=>{},Some(_)=>manager.store.snack("录后脚本退出失败"),None=>{let _=child.kill().await;let _=child.wait().await;manager.store.snack("录后脚本已超时或取消");}}},Err(_)=>manager.store.snack("无法启动录后脚本，请检查路径和权限")}
        }
      };work.await;
    });
    }
    pub async fn shutdown(&self) {
        {
            let _admission = self.admission.lock().expect("script admission");
            self.stop.cancel();
            self.tasks.close();
        }
        self.tasks.wait().await;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn script_templates_are_arguments_not_shell_commands() {
        let s = ScriptSpec::parse(r#"["C:\\Tools\\processor.exe","{file}","{room}"]"#).unwrap();
        let args = s.arguments("C:\\Recordings\\x & echo wrong.ts", "room");
        assert_eq!(args.len(), 3);
        assert!(args[1].contains('&'));
        assert!(ScriptSpec::parse("echo hello").is_err());
        assert!(ScriptSpec::parse(r#"["relative.exe"]"#).is_err());
    }
}

#[cfg(test)]
mod schedule_tests {
    use super::*;
    #[test]
    fn enabling_after_the_scheduled_time_targets_tomorrow() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-13T20:00:00+08:00")
            .unwrap()
            .with_timezone(&chrono::Local);
        let past = now.time() - chrono::Duration::hours(1);
        let mut plan = ShutdownPlan::default();
        assert!(!plan.due(now, true, &past.format("%H:%M:%S").to_string()));
        assert!(plan.scheduled.as_ref().unwrap().1 > now);
    }
    #[test]
    fn quick_shutdown_is_one_event_and_can_be_cancelled() {
        let now = chrono::Local::now();
        let mut plan = ShutdownPlan {
            quick: Some(now),
            ..Default::default()
        };
        assert!(plan.due(now, false, "23:00"));
        assert!(!plan.due(now, false, "23:00"));
        plan.quick = None;
        plan.requested = false;
        assert!(!plan.due(now + chrono::Duration::hours(1), false, "23:00"));
    }
}
