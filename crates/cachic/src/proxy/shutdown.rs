//! Graceful shutdown.
//!
//! FR-62: stop accepting, finish in-flight slices, flush, exit within a bounded time.
//!
//! The bound is the important half. Kubernetes sends SIGTERM and then SIGKILLs after
//! `terminationGracePeriodSeconds` regardless, so an unbounded drain does not buy patience - it
//! just means the process is killed mid-flush instead of exiting deliberately. A hung upstream
//! must not be able to prevent exit.
//!
//! Readiness fails the moment draining starts, before anything else happens, so a load balancer
//! moves traffic away while in-flight work finishes rather than after.

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use tokio::sync::Notify;

/// Tracks in-flight work so a drain can wait for it.
#[derive(Debug)]
pub struct Drain {
    inflight: AtomicU64,
    idle: Notify,
    draining: std::sync::atomic::AtomicBool,
}

/// Held for the duration of a unit of work.
#[derive(Debug)]
pub struct Guard {
    drain: Arc<Drain>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.drain.inflight.fetch_sub(1, Ordering::AcqRel) == 1 {
            // Last one out: wake anyone waiting for idle.
            self.drain.idle.notify_waiters();
        }
    }
}

impl Default for Drain {
    fn default() -> Self {
        Self::new_inner()
    }
}

impl Drain {
    fn new_inner() -> Self {
        Self {
            inflight: AtomicU64::new(0),
            idle: Notify::new(),
            draining: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn new() -> Arc<Self> {
        Arc::new(Self::new_inner())
    }

    /// Register a unit of in-flight work.
    ///
    /// Returns `None` once draining has begun, so new work is refused rather than extending the
    /// drain indefinitely.
    pub fn enter(self: &Arc<Self>) -> Option<Guard> {
        if self.draining.load(Ordering::Relaxed) {
            return None;
        }
        self.inflight.fetch_add(1, Ordering::AcqRel);
        Some(Guard {
            drain: self.clone(),
        })
    }

    /// Register work that must complete even during a drain.
    ///
    /// Slice fills use this: a fill already in progress is worth finishing, since abandoning it
    /// wastes the bytes already fetched and leaves a partially useful object uncached (FR-31).
    pub fn enter_unconditional(self: &Arc<Self>) -> Guard {
        self.inflight.fetch_add(1, Ordering::AcqRel);
        Guard {
            drain: self.clone(),
        }
    }

    pub fn inflight(&self) -> u64 {
        self.inflight.load(Ordering::Relaxed)
    }

    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::Relaxed)
    }

    /// Stop accepting new work and wait for what is in flight, up to `timeout`.
    ///
    /// Returns whether everything finished. A `false` return is not a failure to handle, it is a
    /// fact to log: some work was still running when the deadline arrived.
    pub async fn drain(self: &Arc<Self>, timeout: Duration) -> bool {
        self.draining.store(true, Ordering::Relaxed);
        if self.inflight() == 0 {
            return true;
        }

        let wait = async {
            loop {
                // Register interest before re-checking, so a completion between the check and the
                // await cannot be missed.
                let notified = self.idle.notified();
                if self.inflight() == 0 {
                    return;
                }
                notified.await;
            }
        };

        tokio::time::timeout(timeout, wait).await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_idle_process_drains_immediately() {
        let drain = Drain::new();
        assert!(drain.drain(Duration::from_secs(5)).await);
    }

    #[tokio::test]
    async fn draining_waits_for_in_flight_work() {
        let drain = Drain::new();
        let guard = drain.enter().unwrap();
        assert_eq!(drain.inflight(), 1);

        let d = drain.clone();
        let waiter = tokio::spawn(async move { d.drain(Duration::from_secs(5)).await });

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "drain returned with work in flight");

        drop(guard);
        assert!(
            waiter.await.unwrap(),
            "drain did not complete when work finished"
        );
    }

    #[tokio::test]
    async fn a_hung_task_cannot_prevent_exit() {
        // Kubernetes will SIGKILL regardless, so an unbounded drain means being killed mid-flush
        // instead of exiting deliberately.
        let drain = Drain::new();
        let _stuck = drain.enter().unwrap();
        let finished = drain.drain(Duration::from_millis(150)).await;
        assert!(!finished, "drain claimed success with work still in flight");
    }

    #[tokio::test]
    async fn new_work_is_refused_once_draining() {
        let drain = Drain::new();
        assert!(drain.enter().is_some());
        let d = drain.clone();
        let _ = tokio::spawn(async move { d.drain(Duration::from_millis(50)).await }).await;
        assert!(drain.is_draining());
        assert!(
            drain.enter().is_none(),
            "accepting new work during a drain extends it indefinitely"
        );
    }

    #[tokio::test]
    async fn in_flight_fills_may_still_be_registered_during_a_drain() {
        // A fill already in progress is worth finishing: abandoning it wastes the bytes already
        // fetched and leaves the object uncached (FR-31).
        let drain = Drain::new();
        let d = drain.clone();
        let _ = tokio::spawn(async move { d.drain(Duration::from_millis(50)).await }).await;
        let guard = drain.enter_unconditional();
        assert_eq!(drain.inflight(), 1);
        drop(guard);
    }

    #[tokio::test]
    async fn a_completion_racing_the_drain_is_not_missed() {
        // The classic lost-wakeup: work finishing between the check and the await.
        for _ in 0..50 {
            let drain = Drain::new();
            let guard = drain.enter().unwrap();
            let d = drain.clone();
            let waiter = tokio::spawn(async move { d.drain(Duration::from_secs(2)).await });
            tokio::task::yield_now().await;
            drop(guard);
            assert!(
                waiter.await.unwrap(),
                "drain missed a completion and waited for the timeout"
            );
        }
    }
}
