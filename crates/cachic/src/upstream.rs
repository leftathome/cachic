//! Upstream fetching: HTTP client pool, the dedicated resolver, per-host limits,
//! retries, timeouts and the private-address guard.
//!
//! The system resolver is never consulted; see ADR 0008 and TASK-10.
