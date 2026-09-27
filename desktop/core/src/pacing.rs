//! Shared per-platform pacing, concurrency and cooldown for all requests.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::Notify, time::Instant};
use tokio_util::sync::CancellationToken;

/// Least recently served platform first, with per-room deadlines breaking ties.
/// A busy/cooling platform is skipped rather than occupying a global worker.
#[derive(Clone, Default)]
pub(crate) struct FairQueue {
    turns: HashMap<String, u64>,
    turn: u64,
}
impl FairQueue {
    pub(crate) fn rank(&self, platform: &str) -> u64 {
        self.turns.get(platform).copied().unwrap_or(0)
    }
    pub(crate) fn served(&mut self, platform: &str) {
        self.turn = self.turn.saturating_add(1);
        self.turns.insert(platform.to_owned(), self.turn);
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct GateStatus {
    pub reason: &'static str,
    pub ready_at: Option<Instant>,
}

pub(crate) fn restricted(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "429",
        "403",
        "401",
        "冷却",
        "过快",
        "频繁",
        "限制访问",
        "访问受限",
        "风控",
        "captcha",
        "验证码",
        "登录",
        "cookie",
        "credential",
        "rate limit",
    ]
    .iter()
    .any(|marker| error.contains(marker))
}

struct GateState {
    active: usize,
    next: Instant,
    blocked: Instant,
    failures: u32,
}
struct Gate {
    state: Mutex<GateState>,
    changed: Notify,
}
#[derive(Clone, Default)]
pub struct Pacer {
    gates: Arc<Mutex<HashMap<String, Arc<Gate>>>>,
}
pub struct Permit {
    gate: Arc<Gate>,
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.gate.state.lock().expect("platform gate").active -= 1;
        self.gate.changed.notify_waiters();
    }
}
impl Pacer {
    fn gate(&self, key: &str) -> Arc<Gate> {
        self.gates
            .lock()
            .expect("platform gates")
            .entry(key.to_string())
            .or_insert_with(|| {
                Arc::new(Gate {
                    state: Mutex::new(GateState {
                        active: 0,
                        next: Instant::now(),
                        blocked: Instant::now(),
                        failures: 0,
                    }),
                    changed: Notify::new(),
                })
            })
            .clone()
    }
    pub(crate) fn status(&self, key: &str, maximum: usize) -> GateStatus {
        let gate = self.gate(key);
        let state = gate.state.lock().expect("platform gate");
        let now = Instant::now();
        if state.blocked > now {
            GateStatus {
                reason: "cooldown",
                ready_at: Some(state.blocked),
            }
        } else if state.active >= maximum.clamp(1, 16) {
            GateStatus {
                reason: "platformBusy",
                ready_at: None,
            }
        } else if state.next > now {
            GateStatus {
                reason: "platformSpacing",
                ready_at: Some(state.next),
            }
        } else {
            GateStatus {
                reason: "queue",
                ready_at: Some(now),
            }
        }
    }
    pub(crate) fn try_acquire(
        &self,
        key: &str,
        maximum: usize,
        spacing: Duration,
    ) -> Option<Permit> {
        let gate = self.gate(key);
        let mut state = gate.state.lock().expect("platform gate");
        let now = Instant::now();
        if state.blocked > now || state.next > now || state.active >= maximum.clamp(1, 16) {
            return None;
        }
        state.active += 1;
        state.next = now + spacing;
        drop(state);
        Some(Permit { gate })
    }
    pub async fn acquire(
        &self,
        key: &str,
        maximum: usize,
        spacing: Duration,
        cancel: &CancellationToken,
    ) -> Result<Permit, String> {
        let gate = self.gate(key);
        loop {
            if cancel.is_cancelled() {
                return Err("检测已取消".into());
            }
            let notify = gate.changed.notified();
            let wait = {
                let mut state = gate.state.lock().expect("platform gate");
                let now = Instant::now();
                if state.blocked > now {
                    return Err(format!(
                        "平台冷却中，请在 {} 秒后重试",
                        (state.blocked - now).as_secs() + 1
                    ));
                }
                if state.active < maximum.clamp(1, 16) && state.next <= now {
                    state.active += 1;
                    state.next = now + spacing;
                    return Ok(Permit { gate: gate.clone() });
                }
                state
                    .next
                    .saturating_duration_since(now)
                    .max(Duration::from_millis(50))
            };
            tokio::select! {biased;_=cancel.cancelled()=>return Err("应用正在退出".into()),_=notify=>{},_=tokio::time::sleep(wait)=>{}}
        }
    }
    pub fn result(&self, key: &str, result: &Result<crate::resolver::StreamInfo, String>) {
        let gate = self.gate(key);
        let mut state = gate.state.lock().expect("platform gate");
        match result {
            Ok(_) => {
                state.failures = 0;
            }
            Err(error) => {
                if [
                    "429",
                    "403",
                    "频繁",
                    "限制访问",
                    "访问受限",
                    "风控",
                    "captcha",
                ]
                .iter()
                .any(|marker| error.to_ascii_lowercase().contains(marker))
                {
                    state.failures = state.failures.saturating_add(1);
                    state.blocked = Instant::now()
                        + Duration::from_secs(
                            (600_u64 * (1_u64 << state.failures.min(6).saturating_sub(1)))
                                .min(14400),
                        );
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn platforms_rotate_without_starving_the_third_platform() {
        let mut queue = FairQueue::default();
        let mut order = Vec::new();
        for _ in 0..9 {
            let key = ["a", "b", "c"]
                .into_iter()
                .min_by_key(|key| queue.rank(key))
                .unwrap();
            order.push(key);
            queue.served(key);
        }
        assert_eq!(order, ["a", "b", "c", "a", "b", "c", "a", "b", "c"]);
    }
    #[tokio::test(start_paused = true)]
    async fn admission_is_nonblocking_and_shares_manual_limits() {
        let p = Pacer::default();
        let first = p.try_acquire("a", 1, Duration::from_secs(3)).unwrap();
        assert!(p.try_acquire("a", 1, Duration::ZERO).is_none());
        assert!(p.try_acquire("b", 1, Duration::ZERO).is_some());
        assert_eq!(p.status("a", 1).reason, "platformBusy");
        drop(first);
        assert_eq!(p.status("a", 1).reason, "platformSpacing");
        tokio::time::advance(Duration::from_secs(3)).await;
        assert!(p.try_acquire("a", 1, Duration::ZERO).is_some());
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(p.acquire("b", 1, Duration::ZERO, &cancelled).await.is_err());
        p.result("a", &Err("HTTP 429".into()));
        assert_eq!(p.status("a", 1).reason, "cooldown");
        assert!(p.try_acquire("a", 1, Duration::ZERO).is_none());
    }
    #[tokio::test]
    async fn shared_limit_and_spacing_apply_to_all_callers() {
        let p = Pacer::default();
        let stop = CancellationToken::new();
        let first = p
            .acquire("site", 1, Duration::from_millis(60), &stop)
            .await
            .unwrap();
        assert!(tokio::time::timeout(
            Duration::from_millis(20),
            p.acquire("site", 1, Duration::ZERO, &stop)
        )
        .await
        .is_err());
        drop(first);
        let next = p.acquire("site", 1, Duration::ZERO, &stop).await.unwrap();
        drop(next);
    }
    #[tokio::test]
    async fn restricted_platform_does_not_make_a_second_request() {
        let p = Pacer::default();
        p.result("site", &Err("HTTP 429".into()));
        assert!(p
            .acquire("site", 3, Duration::ZERO, &CancellationToken::new())
            .await
            .is_err());
    }
}
