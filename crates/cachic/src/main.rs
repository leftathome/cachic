//! cachic binary entry point.
//!
//! Parses and validates configuration, checks the cache directory guard, and reports what it
//! would run with. The proxy itself lands in TASK-09 through TASK-13; see
//! `.agent/tasks/TASK-INDEX.md`.

use cachic::config::{units, Config};
use clap::Parser;

fn main() -> std::process::ExitCode {
    let config = Config::parse();

    // Validation and the directory guard run before anything is opened or bound, so a
    // misconfiguration is a startup error rather than a surprise an hour later.
    let rules = match config.prepare() {
        Ok(rules) => rules,
        Err(e) => {
            eprintln!("configuration error: {e}");
            // Print the source chain: the guard's advice lives there.
            let mut source = std::error::Error::source(&e);
            while let Some(cause) = source {
                eprintln!("  caused by: {cause}");
                source = cause.source();
            }
            return std::process::ExitCode::from(78); // EX_CONFIG
        }
    };

    println!("cachic {}", cachic::VERSION);
    println!("  data directory     {}", config.cache_data_dir.display());
    println!(
        "  disk tier          {}",
        units::format_size(config.cache_disk_size)
    );
    println!(
        "  memory tier        {}",
        units::format_size(config.cache_mem_size)
    );
    println!(
        "  slice size         {}",
        units::format_size(config.cache_slice_size)
    );
    println!(
        "  read-ahead         {} slices ({} per connection)",
        config.readahead_slices,
        units::format_size(config.readahead_bytes_per_connection())
    );
    println!(
        "  upstream resolvers {}",
        config
            .upstream_dns
            .iter()
            .map(|a| a.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("  service rules      {} configured", rules.services.len());
    eprintln!();
    eprintln!("the proxy is not implemented yet; see .agent/tasks/TASK-INDEX.md");
    std::process::ExitCode::from(2)
}
