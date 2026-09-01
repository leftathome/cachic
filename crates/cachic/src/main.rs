//! cachic: an HTTP caching proxy for game-distribution and OS-update CDN traffic.
//!
//! This binary is a skeleton (TASK-01). The M0 spike lives in `src/bin/spike.rs` and is
//! deliberately throwaway; the modules below are filled in during M1.

pub mod admin;
pub mod config;
pub mod orchestrator;
pub mod proxy;
pub mod services;
pub mod sni;
pub mod store;
pub mod telemetry;
pub mod upstream;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version" | "-V") => println!("cachic {VERSION}"),
        Some(other) => {
            eprintln!("cachic {VERSION}: unrecognised argument {other:?}");
            eprintln!("the proxy is not implemented yet; see .agent/tasks/TASK-INDEX.md");
            std::process::exit(2);
        }
        None => {
            eprintln!("cachic {VERSION}: not implemented yet");
            eprintln!("see .agent/tasks/TASK-INDEX.md for the milestone plan");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    /// The version string is compiled in and is what `--version` prints; a build that loses it
    /// would still link, so assert it is present and non-empty.
    #[test]
    fn version_is_populated() {
        assert!(!super::VERSION.is_empty());
    }
}
