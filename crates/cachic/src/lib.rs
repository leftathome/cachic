//! cachic: an HTTP caching proxy for game-distribution and OS-update CDN traffic.
//!
//! The modules below are the M1 structure and are currently stubs. The M0 spike lives under
//! [`spike`] and is deliberately throwaway: it exists to falsify the plan's central bet (that
//! foyer removes the need to write a cache engine) before that bet is expensive to unwind.

pub mod admin;
pub mod config;
pub mod orchestrator;
pub mod proxy;
pub mod services;
pub mod sni;
pub mod spike;
pub mod store;
pub mod telemetry;
pub mod upstream;

/// The crate version, as printed by `cachic --version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
