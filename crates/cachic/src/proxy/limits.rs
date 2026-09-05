//! Connection and concurrency limits.
//!
//! NFR-4 asks for 10,000 open client connections and 500 in-flight upstream fetches. Both are
//! bounds rather than targets: exceeding them must slow us down predictably, not exhaust memory
//! or an origin's connection limit.
//!
//! Client connections are *rejected* at the limit rather than queued. A queued connection looks
//! alive to the client while making no progress, and a game client that times out silently is
//! worse than one that fails fast and retries. Upstream fetches are *queued* instead, because
//! there the caller is us, back-pressure propagates naturally through the read-ahead window, and
//! dropping a fetch would mean failing a client request that could simply have waited.

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

/// Bounds the number of simultaneously open client connections, globally and per peer.
///
/// The global ceiling alone is not a defence. It is shared by the whole network, so one host that
/// reaches it denies service to every other client - and reaching it costs an attacker one socket
/// and one byte each (SR-07). The per-peer ceiling is what stops a single source consuming the
/// budget; the global one still bounds total memory.
#[derive(Debug)]
pub struct ConnectionLimit {
    open: AtomicU64,
    max: u64,
    rejected: AtomicU64,
    /// Per-peer ceiling. `None` disables per-peer accounting, which is what a caller that has no
    /// address to attribute a connection to must use.
    per_peer: Option<u64>,
    /// Open connections by source address. Entries are removed as they reach zero, so the map is
    /// bounded by the number of *currently connected* peers rather than every peer ever seen.
    peers: Mutex<HashMap<IpAddr, u64>>,
    rejected_per_peer: AtomicU64,
}

/// Decrements the count when dropped, so a connection cannot leak the slot it took even if its
/// task panics.
#[derive(Debug)]
pub struct ConnectionPermit {
    limit: Arc<ConnectionLimit>,
    peer: Option<IpAddr>,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.limit.open.fetch_sub(1, Ordering::Relaxed);
        if let Some(peer) = self.peer {
            let mut peers = self.limit.peers.lock().expect("peer map poisoned");
            if let Some(count) = peers.get_mut(&peer) {
                *count -= 1;
                if *count == 0 {
                    peers.remove(&peer);
                }
            }
        }
    }
}

impl ConnectionLimit {
    pub fn new(max: u64) -> Arc<Self> {
        Arc::new(Self {
            open: AtomicU64::new(0),
            max,
            rejected: AtomicU64::new(0),
            per_peer: None,
            peers: Mutex::new(HashMap::new()),
            rejected_per_peer: AtomicU64::new(0),
        })
    }

    /// Also cap how many of those connections any single source address may hold.
    pub fn with_per_peer(max: u64, per_peer: u64) -> Arc<Self> {
        Arc::new(Self {
            open: AtomicU64::new(0),
            max,
            rejected: AtomicU64::new(0),
            per_peer: Some(per_peer),
            peers: Mutex::new(HashMap::new()),
            rejected_per_peer: AtomicU64::new(0),
        })
    }

    /// Take a slot attributed to `peer`, or refuse.
    ///
    /// Refuses when either ceiling is reached. The per-peer slot is claimed first so that a
    /// refusal there does not disturb the global count.
    pub fn try_acquire_for(self: &Arc<Self>, peer: IpAddr) -> Option<ConnectionPermit> {
        if let Some(per_peer) = self.per_peer {
            let mut peers = self.peers.lock().expect("peer map poisoned");
            let count = peers.entry(peer).or_insert(0);
            if *count >= per_peer {
                self.rejected_per_peer.fetch_add(1, Ordering::Relaxed);
                self.rejected.fetch_add(1, Ordering::Relaxed);
                if *count == 0 {
                    peers.remove(&peer);
                }
                return None;
            }
            *count += 1;
            drop(peers);

            match self.acquire_global() {
                Some(mut permit) => {
                    permit.peer = Some(peer);
                    return Some(permit);
                }
                None => {
                    // The global ceiling refused, so give the per-peer slot back.
                    let mut peers = self.peers.lock().expect("peer map poisoned");
                    if let Some(count) = peers.get_mut(&peer) {
                        *count -= 1;
                        if *count == 0 {
                            peers.remove(&peer);
                        }
                    }
                    return None;
                }
            }
        }
        self.acquire_global()
    }

    /// Connections refused because one peer had too many.
    pub fn rejected_per_peer(&self) -> u64 {
        self.rejected_per_peer.load(Ordering::Relaxed)
    }

    pub fn per_peer(&self) -> Option<u64> {
        self.per_peer
    }

    /// Take a slot without attributing it to a peer.
    pub fn try_acquire(self: &Arc<Self>) -> Option<ConnectionPermit> {
        self.acquire_global()
    }

    fn acquire_global(self: &Arc<Self>) -> Option<ConnectionPermit> {
        // Compare-and-swap rather than fetch_add-then-check: the latter briefly exceeds the limit
        // and, under a connection storm, that overshoot is exactly when it matters.
        let mut current = self.open.load(Ordering::Relaxed);
        loop {
            if current >= self.max {
                self.rejected.fetch_add(1, Ordering::Relaxed);
                return None;
            }
            match self.open.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Some(ConnectionPermit {
                        limit: self.clone(),
                        peer: None,
                    })
                }
                Err(actual) => current = actual,
            }
        }
    }

    pub fn open(&self) -> u64 {
        self.open.load(Ordering::Relaxed)
    }

    pub fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }

    pub fn max(&self) -> u64 {
        self.max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_are_returned_when_dropped() {
        let limit = ConnectionLimit::new(2);
        let a = limit.try_acquire().unwrap();
        let b = limit.try_acquire().unwrap();
        assert_eq!(limit.open(), 2);
        assert!(limit.try_acquire().is_none());
        drop(a);
        assert_eq!(limit.open(), 1);
        assert!(limit.try_acquire().is_some());
        drop(b);
    }

    #[test]
    fn a_leaked_task_cannot_leak_its_slot() {
        // The permit decrements on drop, so a panicking connection task returns its slot.
        let limit = ConnectionLimit::new(1);
        let result = std::panic::catch_unwind(|| {
            let _permit = limit.try_acquire().unwrap();
            panic!("connection task exploded");
        });
        assert!(result.is_err());
        assert_eq!(limit.open(), 0, "the slot was not returned");
    }

    #[test]
    fn rejections_are_counted_for_metrics() {
        let limit = ConnectionLimit::new(1);
        let _held = limit.try_acquire().unwrap();
        assert!(limit.try_acquire().is_none());
        assert!(limit.try_acquire().is_none());
        assert_eq!(limit.rejected(), 2);
    }

    #[test]
    fn the_limit_is_never_exceeded_under_concurrency() {
        // fetch_add-then-check would briefly overshoot, which is precisely what matters during a
        // connection storm.
        let limit = ConnectionLimit::new(50);
        let max_seen = Arc::new(AtomicU64::new(0));
        std::thread::scope(|scope| {
            for _ in 0..16 {
                let limit = limit.clone();
                let max_seen = max_seen.clone();
                scope.spawn(move || {
                    for _ in 0..500 {
                        if let Some(permit) = limit.try_acquire() {
                            let open = limit.open();
                            max_seen.fetch_max(open, Ordering::Relaxed);
                            drop(permit);
                        }
                    }
                });
            }
        });
        assert!(
            max_seen.load(Ordering::Relaxed) <= 50,
            "observed {} open connections against a limit of 50",
            max_seen.load(Ordering::Relaxed)
        );
        assert_eq!(limit.open(), 0, "permits leaked");
    }

    #[test]
    fn a_zero_limit_refuses_everything_rather_than_panicking() {
        let limit = ConnectionLimit::new(0);
        assert!(limit.try_acquire().is_none());
    }
}
