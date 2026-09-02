//! cachic: an HTTP caching proxy for game-distribution and OS-update CDN traffic.
//!
//! Startup order matters and is deliberate:
//!
//! 1. Parse and validate configuration, and check the cache directory guard. A misconfiguration
//!    is a startup error, not a surprise an hour later.
//! 2. Start the admin listener early, so `/healthz` answers and `/readyz` reports "starting"
//!    while the store opens. A large cache takes time to open, and a process that is silent
//!    during that window looks dead to an orchestrator.
//! 3. Open the store and the index, then bind the data plane, then report ready.

use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use cachic::{
    admin::{
        api::{ApiState, AuthToken, LateApiState, ServiceInfo},
        AdminServer, AdminState, Readiness,
    },
    config::{units, Config},
    orchestrator::Orchestrator,
    proxy::server::{Server, ServerConfig},
    services::{domains, key::CompiledRule, matcher::Matcher},
    store::{hybrid::SliceStore, hybrid::StoreConfig, index::ObjectIndex},
    telemetry::{logs, metrics::Metrics},
    upstream::{
        client::{ClientConfig, UpstreamClient},
        resolver::UpstreamResolver,
    },
};
use clap::Parser;

/// `EX_CONFIG` from sysexits.h: the configuration is wrong, and restarting will not help.
const EXIT_CONFIG: u8 = 78;
/// `EX_UNAVAILABLE`: a resource we need is not available.
const EXIT_UNAVAILABLE: u8 = 69;

/// NFR-4's client connection ceiling.
const MAX_CLIENT_CONNECTIONS: u64 = 10_000;

/// How long to wait for in-flight requests before exiting anyway.
///
/// Shorter than a typical Kubernetes terminationGracePeriodSeconds (30s), so the process exits
/// on its own terms rather than being killed part-way through.
const DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let config = Config::parse();
    logs::init(config.log_format, &config.log_level);

    match run(config).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(Fatal::Config(message)) => {
            eprintln!("configuration error: {message}");
            std::process::ExitCode::from(EXIT_CONFIG)
        }
        Err(Fatal::Unavailable(message)) => {
            eprintln!("startup failed: {message}");
            std::process::ExitCode::from(EXIT_UNAVAILABLE)
        }
    }
}

enum Fatal {
    Config(String),
    Unavailable(String),
}

fn chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        out.push_str(&format!("\n  caused by: {cause}"));
        source = cause.source();
    }
    out
}

async fn run(config: Config) -> Result<(), Fatal> {
    let rules = config.prepare().map_err(|e| Fatal::Config(chain(&e)))?;

    let (metrics, foyer_registry) = Metrics::new()
        .map_err(|e| Fatal::Unavailable(format!("cannot create the metrics registry: {e}")))?;
    let metrics = Arc::new(metrics);
    let readiness = Arc::new(Readiness::new());

    // Admin first: an orchestrator watching /readyz should see "starting", not a refused
    // connection, while a large cache directory opens.
    let admin_addr = SocketAddr::from(([0, 0, 0, 0], config.admin_port));
    let admin_state = AdminState {
        metrics: metrics.clone(),
        readiness: readiness.clone(),
    };
    // Bound before the store opens, so a liveness probe never gets connection-refused while a
    // large cache directory is recovering - being restarted mid-recovery is exactly wrong. The
    // operator API's state is published below; until then its endpoints report 503.
    let late_api = LateApiState::new();
    let admin = AdminServer::bind_with_api(admin_addr, admin_state, late_api.clone())
        .await
        .map_err(|e| Fatal::Unavailable(format!("cannot bind the admin port {admin_addr}: {e}")))?;
    tracing::info!(addr = %admin.addr(), "admin listening");

    let store = SliceStore::open_with_metrics(
        &config.cache_data_dir.join("slices"),
        &StoreConfig::from_config(&config),
        Some(foyer_registry),
    )
    .await
    .map_err(|e| Fatal::Unavailable(chain(&e)))?;
    let index = Arc::new(
        ObjectIndex::open(&config.cache_data_dir.join("index.redb"))
            .map_err(|e| Fatal::Unavailable(chain(&e)))?,
    );
    readiness.set_store_open(true);
    tracing::info!(dir = %config.cache_data_dir.display(), "store open");

    let domain_list = domains::bundled()
        .map_err(|e| Fatal::Unavailable(format!("the bundled domain list is unusable: {e}")))?;
    let matcher = Arc::new(Matcher::build(&domain_list));
    tracing::info!(
        services = matcher.service_count(),
        patterns = matcher.pattern_count(),
        "domain list loaded"
    );

    let resolver = Arc::new(
        UpstreamResolver::new(&config.upstream_dns, false).map_err(|e| Fatal::Config(chain(&e)))?,
    );
    let upstream = UpstreamClient::new(
        resolver,
        ClientConfig {
            max_inflight: config.upstream_max_inflight,
            ..ClientConfig::default()
        },
    )
    .map_err(|e| Fatal::Unavailable(chain(&e)))?;

    let store_handle = store.clone();
    let index_handle = index.clone();
    let service_infos: Vec<ServiceInfo> = domain_list
        .services
        .iter()
        .map(|s| ServiceInfo {
            name: s.name.clone(),
            patterns: s.patterns.len(),
        })
        .collect();

    let orchestrator = Arc::new(Orchestrator::new(
        store,
        index,
        upstream,
        config.cache_slice_size as u32,
        config.readahead_slices,
    ));

    let mut compiled = HashMap::new();
    for name in domain_list.services.iter().map(|s| s.name.clone()) {
        let rule = rules.for_service(&name);
        let compiled_rule =
            CompiledRule::compile(&name, rule).map_err(|e| Fatal::Config(chain(&e)))?;
        compiled.insert(name, compiled_rule);
    }

    let connections = cachic::proxy::limits::ConnectionLimit::new(MAX_CLIENT_CONNECTIONS);
    let drain = cachic::proxy::shutdown::Drain::new();

    let http_addr = SocketAddr::from(([0, 0, 0, 0], config.http_port));
    let server = Server::bind(
        http_addr,
        Arc::new(ServerConfig {
            orchestrator,
            matcher,
            rules: Arc::new(rules),
            compiled: Arc::new(compiled),
            hostname: hostname(),
            passthrough_unknown_hosts: config.passthrough_unknown_hosts,
            connections: connections.clone(),
            drain: drain.clone(),
        }),
    )
    .await
    .map_err(|e| Fatal::Unavailable(format!("cannot bind the HTTP port {http_addr}: {e}")))?;
    readiness.set_listeners_bound(true);

    late_api.set(ApiState {
        store: store_handle,
        index: index_handle,
        drain: drain.clone(),
        readiness: readiness.clone(),
        token: AuthToken::new(&config.admin_token),
        services: Arc::new(service_infos),
        data_dir: config.cache_data_dir.clone(),
        configured_disk_bytes: config.cache_disk_size,
        min_free_bytes: config.min_free_disk,
        slice_size: config.cache_slice_size as u32,
    });
    tracing::info!(
        authenticated = !config.admin_token.is_empty(),
        "admin API available"
    );

    tracing::info!(
        addr = %server.addr(),
        slice_size = %units::format_size(config.cache_slice_size),
        disk = %units::format_size(config.cache_disk_size),
        memory = %units::format_size(config.cache_mem_size),
        readahead_per_connection = %units::format_size(config.readahead_bytes_per_connection()),
        "cachic ready"
    );

    shutdown_signal().await;

    // Readiness fails first, before anything else happens, so a load balancer moves traffic away
    // while in-flight work finishes rather than after (FR-62).
    readiness.begin_drain();
    tracing::info!(
        inflight = drain.inflight(),
        timeout_secs = DRAIN_TIMEOUT.as_secs(),
        "draining"
    );

    let finished = drain.drain(DRAIN_TIMEOUT).await;
    if finished {
        tracing::info!("in-flight requests completed");
    } else {
        // Not a failure to handle, a fact to record. Kubernetes will SIGKILL after its grace
        // period regardless, so exiting deliberately beats being killed mid-flush.
        tracing::warn!(
            inflight = drain.inflight(),
            "drain timed out; exiting with work still in flight"
        );
    }

    tracing::info!(
        rejected_connections = connections.rejected(),
        "shutdown complete"
    );
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

/// The name reported in `X-LanCache-Processed-By`.
fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "cachic".to_string())
}
