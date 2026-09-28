//! Shared per-platform pacing, concurrency and cooldown for all requests.
use std::{
    collections::{HashMap, VecDeque},
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
    waiters: VecDeque<Waiter>,
    next_ticket: u64,
    priority_streak: usize,
}
const PRIORITY_BURST: usize = 3;
const MAX_WAITERS: usize = 1024;
const AGING: Duration = Duration::from_secs(30);
struct Waiter {
    id: u64,
    priority: bool,
    queued: Instant,
}
impl GateState {
    fn selected(&self) -> Option<u64> {
        let normal = self.waiters.iter().find(|w| !w.priority);
        if let Some(normal) = normal {
            if self.priority_streak >= PRIORITY_BURST
                || (self.priority_streak > 0 && normal.queued.elapsed() >= AGING)
            {
                return Some(normal.id);
            }
        }
        self.waiters
            .iter()
            .find(|w| w.priority)
            .or(normal)
            .map(|w| w.id)
    }
}
pub(crate) struct PendingPermit {
    gate: Arc<Gate>,
    ticket: u64,
}
impl PendingPermit {
    pub(crate) fn set_priority(&self, priority: bool) {
        let mut state = self.gate.state.lock().expect("platform gate");
        let changed = state
            .waiters
            .iter_mut()
            .find(|w| w.id == self.ticket)
            .is_some_and(|waiter| {
                let changed = waiter.priority != priority;
                waiter.priority = priority;
                changed
            });
        drop(state);
        if changed {
            self.gate.changed.notify_waiters();
        }
    }
    pub(crate) fn rank(&self) -> usize {
        let state = self.gate.state.lock().expect("platform gate");
        if state.selected() == Some(self.ticket) {
            return 0;
        }
        state
            .waiters
            .iter()
            .position(|w| w.id == self.ticket)
            .map(|i| i + 1)
            .unwrap_or(usize::MAX)
    }
    pub(crate) fn try_acquire(&self, maximum: usize, spacing: Duration) -> Option<Permit> {
        let mut state = self.gate.state.lock().expect("platform gate");
        let now = Instant::now();
        if state.selected() != Some(self.ticket)
            || state.blocked > now
            || state.next > now
            || state.active >= maximum.clamp(1, 16)
        {
            return None;
        }
        let index = state.waiters.iter().position(|w| w.id == self.ticket)?;
        let waiter = state.waiters.remove(index)?;
        state.priority_streak = if waiter.priority {
            state.priority_streak.saturating_add(1).min(PRIORITY_BURST)
        } else {
            0
        };
        state.active += 1;
        state.next = now + spacing;
        drop(state);
        self.gate.changed.notify_waiters();
        Some(Permit {
            gate: self.gate.clone(),
        })
    }
}
impl Drop for PendingPermit {
    fn drop(&mut self) {
        self.gate
            .state
            .lock()
            .expect("platform gate")
            .waiters
            .retain(|w| w.id != self.ticket);
        self.gate.changed.notify_waiters();
    }
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
                        waiters: VecDeque::new(),
                        next_ticket: 0,
                        priority_streak: 0,
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
    pub(crate) fn enqueue(&self, key: &str, priority: bool) -> Result<PendingPermit, String> {
        let gate = self.gate(key);
        let mut state = gate.state.lock().expect("platform gate");
        if state.waiters.len() >= MAX_WAITERS {
            return Err("平台检测队列已满，请稍后重试".into());
        }
        let ticket = state.next_ticket;
        state.next_ticket = state.next_ticket.wrapping_add(1);
        state.waiters.push_back(Waiter {
            id: ticket,
            priority,
            queued: Instant::now(),
        });
        drop(state);
        gate.changed.notify_waiters();
        Ok(PendingPermit { gate, ticket })
    }
    #[cfg(test)]
    pub(crate) fn try_acquire(
        &self,
        key: &str,
        maximum: usize,
        spacing: Duration,
    ) -> Option<Permit> {
        self.enqueue(key, false).ok()?.try_acquire(maximum, spacing)
    }
    pub async fn acquire(
        &self,
        key: &str,
        maximum: usize,
        spacing: Duration,
        cancel: &CancellationToken,
    ) -> Result<Permit, String> {
        if cancel.is_cancelled() {
            return Err("检测已取消".into());
        }
        let pending = self.enqueue(key, true)?;
        loop {
            if cancel.is_cancelled() {
                return Err("检测已取消".into());
            }
            let notified = pending.gate.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let gate = self.status(key, maximum);
            if gate.reason == "cooldown" {
                return Err("平台冷却中，请等待后再检测".into());
            }
            if let Some(permit) = pending.try_acquire(maximum, spacing) {
                return Ok(permit);
            }
            let wait = gate
                .ready_at
                .filter(|at| *at > Instant::now())
                .map(|at| at.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::from_secs(1))
                .max(Duration::from_millis(25));
            tokio::select! {biased;
                _=cancel.cancelled()=>return Err("检测已取消".into()),
                _=&mut notified=>{},
                _=tokio::time::sleep(wait)=>{},
            }
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
    #[tokio::test(start_paused = true)]
    async fn priority_is_admitted_before_slots_but_normal_work_cannot_starve() {
        let pacer = Pacer::default();
        let normal = pacer.enqueue("a", false).unwrap();
        let urgent: Vec<_> = (0..4).map(|_| pacer.enqueue("a", true).unwrap()).collect();
        for ticket in urgent.iter().take(PRIORITY_BURST) {
            assert!(normal.try_acquire(1, Duration::ZERO).is_none());
            drop(ticket.try_acquire(1, Duration::ZERO).unwrap());
        }
        assert!(urgent[3].try_acquire(1, Duration::ZERO).is_none());
        drop(normal.try_acquire(1, Duration::ZERO).unwrap());
        drop(urgent[3].try_acquire(1, Duration::ZERO).unwrap());
        let aged = pacer.enqueue("a", false).unwrap();
        tokio::time::advance(AGING).await;
        let newer = pacer.enqueue("a", true).unwrap();
        assert!(newer.try_acquire(1, Duration::ZERO).is_none());
        drop(aged.try_acquire(1, Duration::ZERO).unwrap());
    }
    #[tokio::test(start_paused = true)]
    async fn cancellation_spacing_and_independent_platforms_share_the_same_queue() {
        let pacer = Pacer::default();
        let first = pacer.enqueue("a", false).unwrap();
        let holder = first.try_acquire(1, Duration::from_secs(3)).unwrap();
        let waiting = pacer.enqueue("a", true).unwrap();
        assert!(waiting.try_acquire(1, Duration::ZERO).is_none());
        drop(
            pacer
                .enqueue("b", true)
                .unwrap()
                .try_acquire(1, Duration::ZERO)
                .unwrap(),
        );
        drop(holder);
        assert!(waiting.try_acquire(1, Duration::ZERO).is_none());
        drop(waiting);
        tokio::time::advance(Duration::from_secs(3)).await;
        let normal = pacer.enqueue("a", false).unwrap();
        drop(normal.try_acquire(1, Duration::ZERO).unwrap());
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(pacer
            .acquire("a", 1, Duration::ZERO, &cancel)
            .await
            .is_err());
        assert!(pacer.gate("a").state.lock().unwrap().waiters.is_empty());
    }
    #[tokio::test]
    async fn aborting_a_waiter_removes_its_ticket_without_consuming_a_slot() {
        let pacer = Pacer::default();
        let held = pacer
            .enqueue("a", false)
            .unwrap()
            .try_acquire(1, Duration::ZERO)
            .unwrap();
        let cloned = pacer.clone();
        let task = tokio::spawn(async move {
            cloned
                .acquire("a", 1, Duration::ZERO, &CancellationToken::new())
                .await
        });
        for _ in 0..100 {
            if !pacer.gate("a").state.lock().unwrap().waiters.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(pacer.gate("a").state.lock().unwrap().waiters.len(), 1);
        task.abort();
        let _ = task.await;
        assert!(pacer.gate("a").state.lock().unwrap().waiters.is_empty());
        drop(held);
        let next = pacer.enqueue("a", false).unwrap();
        assert!(next.try_acquire(1, Duration::ZERO).is_some());
    }
    #[test]
    fn pending_queue_is_bounded_and_dropping_a_ticket_releases_capacity() {
        let pacer = Pacer::default();
        let mut tickets: Vec<_> = (0..MAX_WAITERS)
            .map(|_| pacer.enqueue("a", false).unwrap())
            .collect();
        assert!(pacer.enqueue("a", true).is_err());
        tickets.pop();
        assert!(pacer.enqueue("a", true).is_ok());
    }
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
