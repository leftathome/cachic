//! Object-level fill for origins that ignore `Range`.
//!
//! Some CDNs answer a `Range` request with `200` and the whole object (FR-13). Without this path,
//! one client asking for 1 MiB of a 60 GB object pulls 60 GB, and thirty clients pull it thirty
//! times.
//!
//! Slice-level coalescing cannot help here: the store can only deduplicate what it can key, and a
//! single full-object stream is not a slice fetch. So this is a second, object-level single-flight
//! (FR-32): one task streams the body, cuts it into slices as it arrives, and publishes per-slice
//! readiness. Everyone else subscribes.
//!
//! The important property is that a subscriber waiting for slice `i` wakes when slice `i` lands,
//! not when the whole object finishes. On a 60 GB object over a WAN link that difference is hours.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use tokio::sync::watch;

use crate::store::slice::ObjectId;

/// How far a fill has progressed, or how it failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// Slices `0..count` are stored and readable.
    Ready { count: u32 },
    /// The fill finished; `count` slices were stored.
    Complete { count: u32 },
    /// The fill failed. Subscribers must be woken with an error rather than left waiting.
    Failed { reason: String },
}

impl Progress {
    /// Whether slice `index` is readable.
    pub fn covers(&self, index: u32) -> bool {
        match self {
            Progress::Ready { count } | Progress::Complete { count } => index < *count,
            Progress::Failed { .. } => false,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Progress::Complete { .. } | Progress::Failed { .. })
    }
}

/// A fill in progress, shared by the filler and its subscribers.
#[derive(Debug)]
pub struct Fill {
    tx: watch::Sender<Progress>,
}

impl Fill {
    fn new() -> Arc<Self> {
        let (tx, _rx) = watch::channel(Progress::Ready { count: 0 });
        // The initial receiver is dropped deliberately. Progress is published with
        // `send_replace`, which does not require a live receiver, so nothing is lost in the
        // window before the first subscriber attaches.
        Arc::new(Self { tx })
    }

    /// Publish that slices `0..count` are now readable.
    ///
    /// `send_replace` rather than `send`: tokio's `watch::Sender::send` *fails and leaves the
    /// value unchanged* when no receiver is currently alive. A filler that publishes progress
    /// before anyone subscribes would silently lose it, and every later subscriber would then
    /// wait forever for a slice that had already landed. `send_replace` always updates.
    pub fn publish(&self, count: u32) {
        self.tx.send_replace(Progress::Ready { count });
    }

    pub fn complete(&self, count: u32) {
        self.tx.send_replace(Progress::Complete { count });
    }

    pub fn fail(&self, reason: impl Into<String>) {
        self.tx.send_replace(Progress::Failed {
            reason: reason.into(),
        });
    }

    pub fn subscribe(&self) -> watch::Receiver<Progress> {
        self.tx.subscribe()
    }

    pub fn progress(&self) -> Progress {
        self.tx.borrow().clone()
    }
}

/// Whether this caller owns the fill or is subscribing to someone else's.
#[derive(Debug)]
pub enum Role {
    /// This caller must perform the fill and publish progress.
    Filler(Arc<Fill>),
    /// Someone else is filling; wait on this.
    Subscriber(watch::Receiver<Progress>),
}

/// Object-level single-flight registry.
#[derive(Debug, Default)]
pub struct FillRegistry {
    fills: Mutex<HashMap<ObjectId, Arc<Fill>>>,
}

impl FillRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim the fill for an object, or subscribe to the one already running.
    pub fn claim(&self, object: ObjectId) -> Role {
        let mut fills = self.fills.lock().expect("fill registry mutex poisoned");
        match fills.get(&object) {
            Some(fill) => Role::Subscriber(fill.subscribe()),
            None => {
                let fill = Fill::new();
                fills.insert(object, fill.clone());
                Role::Filler(fill)
            }
        }
    }

    /// Remove a finished fill.
    ///
    /// Called by the filler once it has published a terminal state. A subscriber that already
    /// holds a receiver keeps working: `watch` receivers outlive the sender's removal from the
    /// map, and the terminal value is retained.
    pub fn release(&self, object: &ObjectId) {
        let mut fills = self.fills.lock().expect("fill registry mutex poisoned");
        fills.remove(object);
    }

    pub fn active(&self) -> usize {
        self.fills
            .lock()
            .expect("fill registry mutex poisoned")
            .len()
    }
}

/// Wait until `index` is readable, or the fill fails.
pub async fn wait_for(mut rx: watch::Receiver<Progress>, index: u32) -> Result<(), String> {
    loop {
        {
            let progress = rx.borrow_and_update().clone();
            if progress.covers(index) {
                return Ok(());
            }
            if let Progress::Failed { reason } = &progress {
                return Err(reason.clone());
            }
            if let Progress::Complete { count } = progress {
                // The fill finished without reaching this slice, which means the object is
                // shorter than the request assumed.
                return Err(format!(
                    "fill completed with {count} slices; slice {index} was never produced"
                ));
            }
        }
        if rx.changed().await.is_err() {
            // The filler was dropped without publishing a terminal state, which is a bug rather
            // than a normal outcome. Report it instead of hanging forever.
            return Err("the fill was abandoned without completing".to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::store::slice::object_id;

    #[test]
    fn the_first_caller_fills_and_the_rest_subscribe() {
        let registry = FillRegistry::new();
        let object = object_id("/a");
        assert!(matches!(registry.claim(object), Role::Filler(_)));
        assert!(matches!(registry.claim(object), Role::Subscriber(_)));
        assert!(matches!(registry.claim(object), Role::Subscriber(_)));
        assert_eq!(registry.active(), 1);
    }

    #[test]
    fn distinct_objects_fill_independently() {
        let registry = FillRegistry::new();
        assert!(matches!(registry.claim(object_id("/a")), Role::Filler(_)));
        assert!(matches!(registry.claim(object_id("/b")), Role::Filler(_)));
        assert_eq!(registry.active(), 2);
    }

    #[test]
    fn releasing_lets_a_later_request_fill_again() {
        let registry = FillRegistry::new();
        let object = object_id("/a");
        let _ = registry.claim(object);
        registry.release(&object);
        assert!(matches!(registry.claim(object), Role::Filler(_)));
    }

    #[tokio::test]
    async fn a_subscriber_wakes_when_its_slice_lands_not_at_completion() {
        // The property that matters. On a 60 GB object over a WAN link, waiting for completion
        // rather than for your own slice is the difference between seconds and hours.
        let registry = FillRegistry::new();
        let object = object_id("/a");
        let Role::Filler(fill) = registry.claim(object) else {
            panic!("first claim must be the filler");
        };
        let Role::Subscriber(rx) = registry.claim(object) else {
            panic!("second claim must be a subscriber");
        };

        let waiter = tokio::spawn(async move { wait_for(rx, 2).await });

        // Slices 0 and 1 land; the waiter for slice 2 must still be waiting.
        fill.publish(1);
        fill.publish(2);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "woke before its slice landed");

        // Slice 2 lands. The waiter must wake now, long before the fill completes.
        fill.publish(3);
        let result = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("waiter did not wake when its slice landed")
            .unwrap();
        assert!(result.is_ok());
        assert!(!fill.progress().is_terminal(), "the fill has not finished");
    }

    #[tokio::test]
    async fn progress_published_before_anyone_subscribes_is_not_lost() {
        // tokio's watch::Sender::send fails and leaves the value unchanged when no receiver is
        // alive. Using it here made every subscriber that attached after the first publish wait
        // forever for a slice that had already landed.
        let registry = FillRegistry::new();
        let object = object_id("/a");
        let Role::Filler(fill) = registry.claim(object) else {
            unreachable!()
        };
        fill.publish(7);
        assert_eq!(fill.progress(), Progress::Ready { count: 7 });
    }

    #[tokio::test]
    async fn an_already_available_slice_returns_immediately() {
        let registry = FillRegistry::new();
        let object = object_id("/a");
        let Role::Filler(fill) = registry.claim(object) else {
            unreachable!()
        };
        fill.publish(10);
        let Role::Subscriber(rx) = registry.claim(object) else {
            unreachable!()
        };
        wait_for(rx, 3).await.unwrap();
    }

    #[tokio::test]
    async fn a_failed_fill_wakes_subscribers_with_the_reason() {
        // Subscribers must not be left waiting on a fill that has died.
        let registry = FillRegistry::new();
        let object = object_id("/a");
        let Role::Filler(fill) = registry.claim(object) else {
            unreachable!()
        };
        let Role::Subscriber(rx) = registry.claim(object) else {
            unreachable!()
        };

        let waiter = tokio::spawn(async move { wait_for(rx, 5).await });
        fill.fail("upstream returned 503");

        let result = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("subscriber was left hanging after the fill failed")
            .unwrap();
        assert_eq!(result.unwrap_err(), "upstream returned 503");
    }

    #[tokio::test]
    async fn completion_short_of_the_requested_slice_is_an_error_not_a_hang() {
        let registry = FillRegistry::new();
        let object = object_id("/a");
        let Role::Filler(fill) = registry.claim(object) else {
            unreachable!()
        };
        let Role::Subscriber(rx) = registry.claim(object) else {
            unreachable!()
        };

        let waiter = tokio::spawn(async move { wait_for(rx, 9).await });
        fill.complete(4);

        let result = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("subscriber hung after the fill completed short")
            .unwrap();
        assert!(result.unwrap_err().contains("never produced"));
    }

    #[tokio::test]
    async fn an_abandoned_fill_does_not_hang_subscribers_forever() {
        // Dropping the filler without publishing a terminal state is a bug; the subscriber must
        // report it rather than wait for a wake-up that will never come.
        let registry = FillRegistry::new();
        let object = object_id("/a");
        let Role::Filler(fill) = registry.claim(object) else {
            unreachable!()
        };
        let Role::Subscriber(rx) = registry.claim(object) else {
            unreachable!()
        };
        registry.release(&object);
        drop(fill);

        let result = tokio::time::timeout(Duration::from_secs(2), wait_for(rx, 1))
            .await
            .expect("subscriber hung on an abandoned fill");
        assert!(result.unwrap_err().contains("abandoned"));
    }

    #[test]
    fn progress_covers_only_slices_that_have_landed() {
        assert!(Progress::Ready { count: 3 }.covers(2));
        assert!(!Progress::Ready { count: 3 }.covers(3));
        assert!(Progress::Complete { count: 3 }.covers(0));
        assert!(!Progress::Failed { reason: "x".into() }.covers(0));
    }
}
