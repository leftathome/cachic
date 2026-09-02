//! Read-ahead policy (FR-16).
//!
//! Two different things get called "read-ahead" and only one of them is speculative:
//!
//! 1. **Pipelining within a request.** The slices a request actually needs, fetched concurrently
//!    within a bounded window. This is not speculation - every slice fetched is one the client
//!    asked for - and it is what keeps a cold sequential download at line rate.
//! 2. **Prefetching beyond a request.** Fetching slices the client has *not* asked for, betting
//!    it will ask next. This is speculation, and it is where upstream amplification comes from.
//!
//! The first is always on. The second is only worth doing for a client that is clearly streaming
//! an object sequentially, and is actively harmful otherwise: Windows Update and Blizzard issue
//! scattered ranges into multi-gigabyte files, and prefetching around each one would multiply
//! upstream traffic for content nobody reads.
//!
//! The measured amplification on a cold sequential fill is exactly 1.00 (benchmark S4). Any
//! prefetch policy that makes that number worse for random-access clients is not worth its
//! throughput.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use crate::store::slice::ObjectId;

/// How many consecutive in-order requests before a client is judged to be streaming.
///
/// Two is too eager: a client fetching a header and then the body looks sequential for one step.
/// Three consecutive adjacent ranges is a pattern rather than a coincidence.
const SEQUENTIAL_THRESHOLD: u32 = 3;

/// Forget a client's access pattern after this long. A pattern from ten minutes ago says nothing
/// about what it is doing now, and the map would otherwise grow without bound.
const PATTERN_TTL: Duration = Duration::from_secs(60);

/// Hard cap on tracked objects.
///
/// The map is an optimisation, not state anything depends on, so it is bounded by capacity as
/// well as by age.
const MAX_TRACKED: usize = 1024;

/// What a request's access pattern suggests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// First sighting, or a jump. Fetch only what was asked for.
    Random,
    /// Consecutive in-order ranges. Prefetching ahead is likely to pay.
    Sequential,
}

#[derive(Debug, Clone, Copy)]
struct Pattern {
    /// The slice index immediately after the last range served.
    next_expected: u32,
    consecutive: u32,
    seen: Instant,
}

/// Tracks per-object access patterns.
///
/// Keyed by object rather than by client: a single object being streamed by several clients at
/// once is the case worth prefetching for, and the store deduplicates the fetches anyway.
#[derive(Debug, Default)]
pub struct ReadaheadPolicy {
    patterns: Mutex<HashMap<ObjectId, Pattern>>,
    window: u32,
}

impl ReadaheadPolicy {
    pub fn new(window: usize) -> Self {
        Self {
            patterns: Mutex::new(HashMap::new()),
            window: window as u32,
        }
    }

    /// Record a request for slices `first..=last` and classify the access.
    pub fn observe(&self, object: ObjectId, first: u32, last: u32) -> Access {
        let now = Instant::now();
        let mut patterns = self.patterns.lock().expect("readahead mutex poisoned");

        if patterns.len() >= MAX_TRACKED {
            // Expire first; that is usually enough on a cache with steady traffic.
            patterns.retain(|_, p| now.duration_since(p.seen) < PATTERN_TTL);
            // A burst of distinct objects produces entries that are all fresh, so expiry alone
            // cannot bound the map - and a cache serving millions of objects is exactly such a
            // burst. Drop the oldest half. Losing a pattern costs one missed prefetch, which is
            // a far better outcome than unbounded growth.
            if patterns.len() >= MAX_TRACKED {
                let mut ages: Vec<_> = patterns.iter().map(|(k, v)| (*k, v.seen)).collect();
                ages.sort_by_key(|(_, seen)| *seen);
                for (key, _) in ages.into_iter().take(MAX_TRACKED / 2) {
                    patterns.remove(&key);
                }
            }
        }

        let Some(entry) = patterns.get_mut(&object) else {
            // First sighting. Nothing to continue from, so this is not a step in a sequence
            // however tidy it looks; counting it would make a single request "sequential" one
            // step sooner than it should be.
            patterns.insert(
                object,
                Pattern {
                    next_expected: last.saturating_add(1),
                    consecutive: 0,
                    seen: now,
                },
            );
            return Access::Random;
        };

        if now.duration_since(entry.seen) >= PATTERN_TTL {
            *entry = Pattern {
                next_expected: last.saturating_add(1),
                consecutive: 0,
                seen: now,
            };
            return Access::Random;
        }

        let continues = first == entry.next_expected;
        entry.consecutive = if continues {
            entry.consecutive.saturating_add(1)
        } else {
            0
        };
        entry.next_expected = last.saturating_add(1);
        entry.seen = now;

        if entry.consecutive >= SEQUENTIAL_THRESHOLD {
            Access::Sequential
        } else {
            Access::Random
        }
    }

    /// Slices to prefetch beyond `last`, given the access pattern and the object's extent.
    ///
    /// Empty for random access, which is the whole point.
    pub fn prefetch(&self, access: Access, last: u32, last_slice: u32) -> std::ops::Range<u32> {
        if access != Access::Sequential || self.window == 0 {
            return 0..0;
        }
        let start = last.saturating_add(1);
        let end = start
            .saturating_add(self.window)
            .min(last_slice.saturating_add(1));
        if start >= end {
            return 0..0;
        }
        start..end
    }

    pub fn window(&self) -> u32 {
        self.window
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.patterns.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::slice::object_id;

    #[test]
    fn a_single_request_is_not_sequential() {
        let p = ReadaheadPolicy::new(8);
        assert_eq!(p.observe(object_id("/a"), 0, 0), Access::Random);
    }

    #[test]
    fn three_consecutive_ranges_are_sequential() {
        // Two is too eager: a client fetching a header and then a body looks sequential for one
        // step, and prefetching on that would be speculation on a single data point.
        let p = ReadaheadPolicy::new(8);
        let o = object_id("/a");
        assert_eq!(p.observe(o, 0, 0), Access::Random, "first sighting");
        assert_eq!(
            p.observe(o, 1, 1),
            Access::Random,
            "one step is not a pattern"
        );
        assert_eq!(
            p.observe(o, 2, 2),
            Access::Random,
            "two steps is not a pattern"
        );
        assert_eq!(p.observe(o, 3, 3), Access::Sequential);
    }

    #[test]
    fn a_jump_resets_the_pattern() {
        // The Windows Update shape: scattered ranges into a large file. Prefetching around each
        // would multiply upstream traffic for content nobody reads.
        let p = ReadaheadPolicy::new(8);
        let o = object_id("/a");
        for i in 0..5 {
            p.observe(o, i, i);
        }
        assert_eq!(p.observe(o, 5, 5), Access::Sequential);
        assert_eq!(p.observe(o, 900, 900), Access::Random, "a jump must reset");
        assert_eq!(p.observe(o, 901, 901), Access::Random);
    }

    #[test]
    fn multi_slice_ranges_chain_correctly() {
        let p = ReadaheadPolicy::new(8);
        let o = object_id("/a");
        assert_eq!(p.observe(o, 0, 3), Access::Random);
        assert_eq!(p.observe(o, 4, 7), Access::Random);
        assert_eq!(p.observe(o, 8, 11), Access::Random);
        assert_eq!(p.observe(o, 12, 15), Access::Sequential);
        assert_eq!(p.observe(o, 16, 19), Access::Sequential);
    }

    #[test]
    fn objects_are_tracked_independently() {
        let p = ReadaheadPolicy::new(8);
        let a = object_id("/a");
        let b = object_id("/b");
        for i in 0..4 {
            p.observe(a, i, i);
        }
        assert_eq!(p.observe(a, 4, 4), Access::Sequential);
        // Interleaving another object must not disturb the first.
        assert_eq!(p.observe(b, 100, 100), Access::Random);
        assert_eq!(p.observe(a, 5, 5), Access::Sequential);
    }

    #[test]
    fn random_access_prefetches_nothing() {
        // The property that protects upstream amplification.
        let p = ReadaheadPolicy::new(8);
        assert!(p.prefetch(Access::Random, 5, 1000).is_empty());
    }

    #[test]
    fn sequential_access_prefetches_the_window() {
        let p = ReadaheadPolicy::new(8);
        assert_eq!(p.prefetch(Access::Sequential, 5, 1000), 6..14);
    }

    #[test]
    fn prefetch_never_runs_past_the_end_of_the_object() {
        let p = ReadaheadPolicy::new(8);
        assert_eq!(p.prefetch(Access::Sequential, 8, 10), 9..11);
        assert!(p.prefetch(Access::Sequential, 10, 10).is_empty());
        // And cannot overflow at the top of the index space.
        assert!(p
            .prefetch(Access::Sequential, u32::MAX, u32::MAX)
            .is_empty());
    }

    #[test]
    fn a_zero_window_disables_prefetching_entirely() {
        let p = ReadaheadPolicy::new(0);
        assert!(p.prefetch(Access::Sequential, 5, 1000).is_empty());
    }

    #[test]
    fn the_pattern_map_stays_bounded() {
        // Without expiry this grows with every object ever requested, which on a cache serving
        // millions of objects is a leak.
        let p = ReadaheadPolicy::new(8);
        for i in 0..2000u32 {
            p.observe(object_id(&format!("/obj-{i}")), 0, 0);
        }
        assert!(
            p.tracked() <= MAX_TRACKED,
            "pattern map grew to {} entries against a cap of {MAX_TRACKED}",
            p.tracked()
        );
    }
}
