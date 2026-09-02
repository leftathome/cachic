//! The configuration surface.
//!
//! Environment variables are primary (12-factor, G3), with an optional TOML file for per-service
//! rules that do not fit an env var. Precedence is env > file > defaults (FR-60).
//!
//! Settings are named in cache terms rather than nginx terms - bytes on disk, bytes in RAM, slice
//! size - because removing `keys_zone` sizing and loader parameters from the operator's vocabulary
//! is a stated product goal, not a cosmetic choice. Where monolithic's name means the same thing,
//! it is reused verbatim so an existing deployment's environment keeps working.
//!
//! See ADR 0005 and PRD section 8.

pub mod guard;
pub mod reference;
pub mod rules;
pub mod units;

use std::{net::IpAddr, path::PathBuf, time::Duration};

use clap::Parser;

use self::guard::{StoredConfig, STORE_FORMAT_VERSION};

fn parse_size_arg(s: &str) -> Result<u64, String> {
    units::parse_size(s).map_err(|e| e.to_string())
}

fn parse_duration_arg(s: &str) -> Result<Duration, String> {
    units::parse_duration(s).map_err(|e| e.to_string())
}

/// Everything cachic reads at startup.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "cachic",
    version,
    about = "HTTP caching proxy for game and OS update CDNs"
)]
pub struct Config {
    // --- Storage ------------------------------------------------------------------------------
    /// Disk tier capacity. Hard cap (FR-40).
    #[arg(long, env = "CACHE_DISK_SIZE", default_value = "1000g", value_parser = parse_size_arg)]
    pub cache_disk_size: u64,

    /// RAM tier capacity for hot slices. Hard cap. Index memory is reported separately and is
    /// roughly 400 bytes per stored slice; see the sizing guide.
    #[arg(long, env = "CACHE_MEM_SIZE", default_value = "2g", value_parser = parse_size_arg)]
    pub cache_mem_size: u64,

    /// How long an object stays cacheable.
    #[arg(long, env = "CACHE_MAX_AGE", default_value = "3560d", value_parser = parse_duration_arg)]
    pub cache_max_age: Duration,

    /// Slice size. Persisted with the cache; changing it requires FORCE_CONFIG=true (FR-10).
    #[arg(long, env = "CACHE_SLICE_SIZE", default_value = "1m", value_parser = parse_size_arg)]
    pub cache_slice_size: u64,

    /// Cache data directory.
    #[arg(long, env = "CACHE_DATA_DIR", default_value = "/data/cache")]
    pub cache_data_dir: PathBuf,

    /// Reduce the effective disk cap when the filesystem falls below this much free space (FR-46).
    #[arg(long, env = "MIN_FREE_DISK", default_value = "10g", value_parser = parse_size_arg)]
    pub min_free_disk: u64,

    /// Adopt the current settings even though they disagree with the cache directory. Existing
    /// slices become unreachable.
    #[arg(long, env = "FORCE_CONFIG", default_value_t = false)]
    pub force_config: bool,

    // --- Listeners ----------------------------------------------------------------------------
    #[arg(long, env = "HTTP_PORT", default_value_t = 80)]
    pub http_port: u16,

    #[arg(long, env = "HTTPS_PORT", default_value_t = 443)]
    pub https_port: u16,

    #[arg(long, env = "ADMIN_PORT", default_value_t = 9090)]
    pub admin_port: u16,

    // --- Upstream -----------------------------------------------------------------------------
    /// Resolvers used for upstream lookups. Never the system resolver: in a lancache deployment
    /// the system resolver is the one lying about CDN hostnames, and using it loops traffic back
    /// into this cache (FR-03).
    #[arg(
        long,
        env = "UPSTREAM_DNS",
        value_delimiter = ' ',
        default_value = "1.1.1.1 1.0.0.1"
    )]
    pub upstream_dns: Vec<IpAddr>,

    /// Global cap on concurrent upstream fetches.
    #[arg(long, env = "UPSTREAM_MAX_INFLIGHT", default_value_t = 256)]
    pub upstream_max_inflight: usize,

    /// Prefetch this many slices ahead on sequential reads. Per-connection memory is this
    /// multiplied by the slice size (FR-16).
    #[arg(long, env = "READAHEAD_SLICES", default_value_t = 4)]
    pub readahead_slices: usize,

    /// Allow upstream fetches to private, loopback and link-local addresses.
    ///
    /// Off by default and should stay that way: without the guard, anyone on the LAN can point
    /// the cache at a router's admin interface or a cloud metadata endpoint and have it fetch and
    /// serve the result (FR-64). Turn it on only to cache from a deliberate internal mirror.
    #[arg(long, env = "ALLOW_PRIVATE_UPSTREAMS", default_value_t = false)]
    pub allow_private_upstreams: bool,

    /// Proxy hosts that match no service, instead of returning 404. Off by default: with it on
    /// and no allow-list, the cache is an open proxy on the LAN (FR-64).
    #[arg(long, env = "PASSTHROUGH_UNKNOWN_HOSTS", default_value_t = false)]
    pub passthrough_unknown_hosts: bool,

    // --- Domain list --------------------------------------------------------------------------
    #[arg(
        long,
        env = "CACHE_DOMAINS_REPO",
        default_value = "https://github.com/uklans/cache-domains"
    )]
    pub cache_domains_repo: String,

    /// How often to refresh the domain list. Zero disables refresh, for air-gapped installs.
    #[arg(long, env = "CACHE_DOMAINS_REFRESH", default_value = "24h", value_parser = parse_duration_arg)]
    pub cache_domains_refresh: Duration,

    /// Load the domain list from this directory instead of the bundled snapshot.
    ///
    /// The directory must be laid out like `uklans/cache-domains`: a `cache_domains.json` naming
    /// each service and the `.txt` files listing its hostnames. Useful for a custom service, for
    /// an air-gapped site pinning its own copy, and for testing against a local origin.
    #[arg(long, env = "CACHE_DOMAINS_DIR")]
    pub cache_domains_dir: Option<PathBuf>,

    /// Optional TOML file of per-service rules.
    #[arg(long, env = "CACHE_RULES_FILE")]
    pub rules_file: Option<PathBuf>,

    // --- Observability ------------------------------------------------------------------------
    #[arg(long, env = "LOG_FORMAT", default_value = "json")]
    pub log_format: LogFormat,

    #[arg(long, env = "LOG_LEVEL", default_value = "info")]
    pub log_level: String,

    /// Bearer token for the admin API. Empty means unauthenticated, which is only safe because
    /// the admin port is bound to loopback or a cluster network by default (FR-54).
    #[arg(long, env = "ADMIN_TOKEN", default_value = "")]
    pub admin_token: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum LogFormat {
    /// Structured JSON on stdout. The supported format.
    Json,
    /// monolithic's `cachelog` format, for existing dashboards (FR-52).
    Lancache,
}

/// A configuration that cannot work, caught at startup rather than at first use.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{setting} is {value}, which must be {requirement}")]
    Invalid {
        setting: &'static str,
        value: String,
        requirement: &'static str,
    },
    #[error("{a} ({a_value}) must not exceed {b} ({b_value})")]
    Ordering {
        a: &'static str,
        a_value: String,
        b: &'static str,
        b_value: String,
    },
    #[error(transparent)]
    Guard(#[from] guard::GuardError),
    #[error(transparent)]
    Rules(#[from] rules::RulesError),
}

impl Config {
    /// The settings the stored data depends on.
    pub fn stored(&self) -> StoredConfig {
        StoredConfig {
            slice_size: self.cache_slice_size,
            store_format_version: STORE_FORMAT_VERSION,
        }
    }

    /// Reject configurations that cannot work, before anything is opened or bound.
    ///
    /// Validation belongs here rather than at first use: an operator should learn that
    /// `CACHE_MEM_SIZE` exceeds `CACHE_DISK_SIZE` when the process starts, not an hour later when
    /// the first eviction runs.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |setting, value: String, requirement| ConfigError::Invalid {
            setting,
            value,
            requirement,
        };

        if self.cache_slice_size == 0 || !self.cache_slice_size.is_power_of_two() {
            return Err(invalid(
                "CACHE_SLICE_SIZE",
                units::format_size(self.cache_slice_size),
                "a power of two, so slice boundaries align with the object offsets clients request",
            ));
        }
        if self.cache_slice_size > u32::MAX as u64 {
            return Err(invalid(
                "CACHE_SLICE_SIZE",
                units::format_size(self.cache_slice_size),
                "at most 4 GiB",
            ));
        }
        if self.cache_disk_size < self.cache_slice_size {
            return Err(ConfigError::Ordering {
                a: "CACHE_SLICE_SIZE",
                a_value: units::format_size(self.cache_slice_size),
                b: "CACHE_DISK_SIZE",
                b_value: units::format_size(self.cache_disk_size),
            });
        }
        if self.cache_mem_size < self.cache_slice_size {
            return Err(ConfigError::Ordering {
                a: "CACHE_SLICE_SIZE",
                a_value: units::format_size(self.cache_slice_size),
                b: "CACHE_MEM_SIZE",
                b_value: units::format_size(self.cache_mem_size),
            });
        }
        if self.readahead_slices == 0 {
            return Err(invalid(
                "READAHEAD_SLICES",
                "0".into(),
                "at least 1, since a request must be able to fetch the slice it needs",
            ));
        }
        if self.upstream_max_inflight == 0 {
            return Err(invalid(
                "UPSTREAM_MAX_INFLIGHT",
                "0".into(),
                "at least 1, or no upstream fetch could ever start",
            ));
        }
        if self.upstream_dns.is_empty() {
            return Err(invalid(
                "UPSTREAM_DNS",
                "empty".into(),
                "at least one resolver: the system resolver is never used, so there would be no \
                 way to resolve an upstream",
            ));
        }
        if self.http_port == self.admin_port || self.https_port == self.admin_port {
            return Err(invalid(
                "ADMIN_PORT",
                self.admin_port.to_string(),
                "different from HTTP_PORT and HTTPS_PORT: the admin surface must not be reachable \
                 on the data plane",
            ));
        }
        if self.http_port == self.https_port {
            return Err(invalid(
                "HTTPS_PORT",
                self.https_port.to_string(),
                "different from HTTP_PORT",
            ));
        }
        Ok(())
    }

    /// Validate, check the cache directory guard, and load the rules file.
    pub fn prepare(&self) -> Result<rules::Rules, ConfigError> {
        self.validate()?;
        guard::check(&self.cache_data_dir, &self.stored(), self.force_config)?;
        let rules = match &self.rules_file {
            Some(path) => rules::Rules::load(path)?,
            None => rules::Rules::default(),
        };
        Ok(rules)
    }

    /// Parse from an explicit argument list. Exists so tests can build a configuration without
    /// touching the process environment.
    #[doc(hidden)]
    pub fn try_parse_from_for_test(args: &[&str]) -> Result<Self, clap::Error> {
        <Self as clap::Parser>::try_parse_from(args)
    }

    /// Per-connection memory implied by the read-ahead window, for the sizing guide and logs.
    pub fn readahead_bytes_per_connection(&self) -> u64 {
        self.cache_slice_size * self.readahead_slices as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Config {
        Config::parse_from(["cachic"])
    }

    #[test]
    fn defaults_match_the_prd() {
        let c = base();
        assert_eq!(c.cache_disk_size, 1000 * (1 << 30));
        assert_eq!(c.cache_mem_size, 2 * (1 << 30));
        assert_eq!(c.cache_max_age.as_secs(), 3560 * 86_400);
        assert_eq!(c.cache_slice_size, 1 << 20);
        assert_eq!(c.min_free_disk, 10 * (1 << 30));
        assert_eq!(c.cache_data_dir, PathBuf::from("/data/cache"));
        assert_eq!(c.http_port, 80);
        assert_eq!(c.https_port, 443);
        assert_eq!(c.admin_port, 9090);
        assert_eq!(c.readahead_slices, 4);
        assert_eq!(c.upstream_max_inflight, 256);
        assert!(!c.passthrough_unknown_hosts);
        assert_eq!(c.log_format, LogFormat::Json);
        assert_eq!(
            c.upstream_dns,
            vec![
                "1.1.1.1".parse::<IpAddr>().unwrap(),
                "1.0.0.1".parse::<IpAddr>().unwrap()
            ]
        );
        c.validate().unwrap();
    }

    #[test]
    fn command_line_overrides_defaults() {
        let c = Config::parse_from([
            "cachic",
            "--cache-disk-size",
            "2t",
            "--readahead-slices",
            "8",
        ]);
        assert_eq!(c.cache_disk_size, 2 * (1 << 40));
        assert_eq!(c.readahead_slices, 8);
    }

    #[test]
    fn upstream_dns_accepts_a_space_separated_list() {
        // monolithic spells it this way, so an existing deployment's value must work verbatim.
        let c = Config::parse_from(["cachic", "--upstream-dns", "9.9.9.9 8.8.8.8"]);
        assert_eq!(c.upstream_dns.len(), 2);
    }

    #[test]
    fn rejects_a_non_power_of_two_slice_size() {
        let c = Config::parse_from(["cachic", "--cache-slice-size", "1000000"]);
        let err = c.validate().unwrap_err();
        assert!(err.to_string().contains("power of two"), "{err}");
    }

    #[test]
    fn rejects_a_slice_larger_than_the_tiers() {
        let c = Config::parse_from([
            "cachic",
            "--cache-slice-size",
            "4g",
            "--cache-mem-size",
            "1g",
        ]);
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_an_admin_port_on_the_data_plane() {
        // Serving the admin API on the same port as cached traffic would expose purge and drain
        // to every client on the LAN.
        let c = Config::parse_from(["cachic", "--admin-port", "80"]);
        let err = c.validate().unwrap_err();
        assert!(err.to_string().contains("data plane"), "{err}");
    }

    #[test]
    fn rejects_zero_readahead_and_zero_inflight() {
        assert!(Config::parse_from(["cachic", "--readahead-slices", "0"])
            .validate()
            .is_err());
        assert!(
            Config::parse_from(["cachic", "--upstream-max-inflight", "0"])
                .validate()
                .is_err()
        );
    }

    #[test]
    fn reports_the_readahead_memory_arithmetic() {
        // The number an operator needs to size a box.
        let c = Config::parse_from([
            "cachic",
            "--readahead-slices",
            "8",
            "--cache-slice-size",
            "1m",
        ]);
        assert_eq!(c.readahead_bytes_per_connection(), 8 << 20);
    }

    #[test]
    fn invalid_values_name_the_variable_and_the_requirement() {
        let err = Config::parse_from(["cachic", "--readahead-slices", "0"])
            .validate()
            .unwrap_err();
        let text = err.to_string();
        assert!(text.contains("READAHEAD_SLICES"), "{text}");
        assert!(text.contains("at least 1"), "{text}");
    }
}
