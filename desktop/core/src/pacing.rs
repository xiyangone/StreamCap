//! Shared per-platform pacing, concurrency and cooldown for all requests.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::Notify, time::Instant};
use tokio_util::sync::CancellationToken;

/// Automatic room checks share one queue across platforms and event sources.
/// Jitter spreads their starts; it never shortens a room's configured poll interval.
#[derive(Clone)]
pub(crate) struct AutomaticPacer {
    next: Arc<tokio::sync::Mutex<Instant>>,
}
impl Default for AutomaticPacer {
    fn default() -> Self {
        Self {
            next: Arc::new(tokio::sync::Mutex::new(Instant::now())),
        }
    }
}
pub(crate) struct AutomaticPermit {
    next: tokio::sync::OwnedMutexGuard<Instant>,
}
impl Drop for AutomaticPermit {
    fn drop(&mut self) {
        let seconds = 20 + (uuid::Uuid::new_v4().as_u128() % 21) as u64;
        *self.next = Instant::now() + Duration::from_secs(seconds);
    }
}
impl AutomaticPacer {
    pub(crate) async fn acquire(
        &self,
        cancel: &CancellationToken,
    ) -> Result<AutomaticPermit, String> {
        let next = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err("自动检测已取消".into()),
            next = self.next.clone().lock_owned() => next,
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err("自动检测已取消".into()),
            _ = tokio::time::sleep_until(*next) => {},
        }
        Ok(AutomaticPermit { next })
    }
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
    pub async fn acquire(
        &self,
        key: &str,
        maximum: usize,
        spacing: Duration,
        cancel: &CancellationToken,
    ) -> Result<Permit, String> {
        let gate = self.gate(key);
        loop {
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
                let restricted = ["429", "403", "频繁", "限制访问", "风控", "captcha"]
                    .iter()
                    .any(|s| error.contains(s));
                if restricted {
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
    async fn automatic_checks_are_serial_and_wait_twenty_to_forty_seconds_after_completion() {
        let pacer = AutomaticPacer::default();
        let first = pacer.acquire(&CancellationToken::new()).await.unwrap();
        let next = pacer.clone();
        let waiter =
            tokio::spawn(async move { next.acquire(&CancellationToken::new()).await.unwrap() });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(
            !waiter.is_finished(),
            "only one automatic check may run at a time"
        );
        drop(first);
        tokio::time::advance(Duration::from_secs(19)).await;
        tokio::task::yield_now().await;
        assert!(
            !waiter.is_finished(),
            "queued checks must not start in a burst"
        );
        tokio::time::advance(Duration::from_secs(21)).await;
        drop(waiter.await.unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn automatic_queue_wait_is_cancelled_without_waiting_for_the_next_slot() {
        let pacer = AutomaticPacer::default();
        let first = pacer.acquire(&CancellationToken::new()).await.unwrap();
        let stop = CancellationToken::new();
        let waiting = pacer.clone();
        let token = stop.clone();
        let waiter = tokio::spawn(async move { waiting.acquire(&token).await.is_err() });
        tokio::task::yield_now().await;
        stop.cancel();
        assert!(waiter.await.unwrap());
        drop(first);
        let stopped = CancellationToken::new();
        stopped.cancel();
        assert!(pacer.acquire(&stopped).await.is_err());
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
