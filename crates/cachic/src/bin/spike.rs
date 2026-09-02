//! Runnable M0 spike proxy.
//!
//! Starts the prototype in front of a real or mock origin so it can be driven by hand, by a load
//! generator, or by a prefill tool. Throwaway; see `cachic::spike`.

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use cachic::spike::{
    proxy::{SpikeConfig, SpikeProxy},
    store::StoreConfig,
};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "spike",
    about = "M0 spike: slice-aware caching proxy prototype"
)]
struct Args {
    /// Address to listen on.
    #[arg(long, default_value = "127.0.0.1:8080", env = "SPIKE_LISTEN")]
    listen: SocketAddr,

    /// Origin base URL, e.g. http://127.0.0.1:9000
    #[arg(long, env = "SPIKE_ORIGIN")]
    origin: String,

    /// Cache data directory.
    #[arg(long, default_value = "/tmp/cachic-spike", env = "SPIKE_DATA_DIR")]
    data_dir: PathBuf,

    /// Slice size in bytes.
    #[arg(long, default_value_t = 1024 * 1024, env = "SPIKE_SLICE_SIZE")]
    slice_size: u32,

    /// Read-ahead window in slices. Per-connection memory is this times the slice size.
    #[arg(long, default_value_t = 4, env = "SPIKE_READAHEAD")]
    readahead: usize,

    /// Memory tier capacity in bytes.
    #[arg(long, default_value_t = 256 * 1024 * 1024, env = "SPIKE_MEM_BYTES")]
    mem_bytes: usize,

    /// Disk tier capacity in bytes.
    #[arg(long, default_value_t = 8 * 1024 * 1024 * 1024, env = "SPIKE_DISK_BYTES")]
    disk_bytes: usize,

    /// Use O_DIRECT for the disk tier. See docs/benchmarks/m0 - buffered was faster on the
    /// measurement host, and the right default is hardware-dependent.
    #[arg(long, default_value_t = false, env = "SPIKE_DIRECT_IO")]
    direct_io: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .json()
        .init();

    let args = Args::parse();
    let config = SpikeConfig {
        listen: args.listen,
        origin: args.origin.clone(),
        slice_size: args.slice_size,
        readahead: args.readahead,
        data_dir: args.data_dir.clone(),
        store: StoreConfig {
            memory_bytes: args.mem_bytes,
            disk_bytes: args.disk_bytes,
            block_bytes: 16 * 1024 * 1024,
            direct_io: args.direct_io,
        },
        upstream_timeout: Duration::from_secs(60),
    };

    let proxy = SpikeProxy::start(config).await?;
    tracing::info!(
        listen = %proxy.addr(),
        origin = %args.origin,
        slice_size = args.slice_size,
        readahead = args.readahead,
        "spike proxy listening"
    );

    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");
    proxy.close().await?;
    Ok(())
}
