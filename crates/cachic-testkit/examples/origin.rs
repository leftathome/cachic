//! A mock origin and a DNS server that points every name at it.
//!
//! Exists so cachic can be exercised as a *running process* rather than as a library linked into
//! a test: pointed at this, a containerised cachic fetches from a controllable origin and resolves
//! every CDN hostname to it. That is what makes a memory measurement under a cgroup limit possible,
//! and the numbers in the chart's sizing guidance come from exactly this setup.
//!
//! It is a test fixture. It serves generated data, trusts everything, and must not be exposed
//! anywhere but a test host.

use std::net::{Ipv4Addr, SocketAddr};

use cachic_testkit::{
    mockcdn::{Config, MockCdn},
    mockdns::MockDns,
};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "origin", about = "mock CDN origin and DNS server")]
struct Args {
    /// Port for the mock origin.
    #[arg(long, default_value_t = 80)]
    http_port: u16,
    /// Port for the mock DNS server. Needs privileges for the default.
    #[arg(long, default_value_t = 53)]
    dns_port: u16,
    /// Address to bind, and the address DNS hands out for every name.
    #[arg(long, default_value = "127.0.0.1")]
    address: Ipv4Addr,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let origin = MockCdn::start_on(
        SocketAddr::from((args.address, args.http_port)),
        Config::default(),
    )
    .await?;
    let dns = MockDns::start_on(
        SocketAddr::from((args.address, args.dns_port)),
        args.address,
    )
    .await
    .map_err(|e| anyhow::anyhow!("cannot bind DNS on port {}: {e}", args.dns_port))?;

    println!("origin  http://{}", origin.addr());
    println!(
        "dns     {} (answers every name with {})",
        dns.addr(),
        args.address
    );
    println!("objects /o/<name>/<size>");

    // Report often enough to see whether traffic is actually arriving.
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
    loop {
        ticker.tick().await;
        let stats = origin.stats();
        println!(
            "origin: {} requests ({} ranged), {} MiB served",
            stats.requests(),
            stats.range_requests(),
            stats.bytes_served() / (1024 * 1024),
        );
    }
}
