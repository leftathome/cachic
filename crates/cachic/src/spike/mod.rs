//! M0 spike: a throwaway prototype proving out slice-aware caching over foyer.
//!
//! Deliberately not production code. It exists to answer the M0 questions (TASK-03, TASK-04):
//! does foyer's hybrid cache carry 1 MiB slices at the throughput we need, does its
//! `get_or_fetch` coalesce concurrent misses as FR-30 requires, and what does the resulting
//! request path look like in practice?
//!
//! What survives into M1 is the measurements and the ADRs, not this code.

pub mod proxy;
pub mod range;
pub mod slice;
pub mod store;
