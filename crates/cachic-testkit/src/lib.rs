//! Development-only test harness for cachic.
//!
//! Contains the deterministic content generator, the mock CDN origin and (from TASK-14) the
//! differential tester and load generator. Never linked into the shipped binary.

pub mod content;
pub mod mockcdn;
