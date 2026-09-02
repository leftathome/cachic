//! Crash recovery (TASK-20, FR-43).
//!
//! Runs the real binary, fills the cache, kills it with SIGKILL - no graceful shutdown, no flush,
//! nothing - and restarts it. The cache must come back serving, without corruption.
//!
//! This spawns the actual process rather than exercising the library, because the thing being
//! tested is what survives a process boundary. A library-level test cannot tell the difference
//! between state that was persisted and state that was still in memory.

use std::{
    process::{Child, Command, Stdio},
    time::Duration,
};

use cachic_testkit::{
    content,
    mockcdn::{Config as CdnConfig, MockCdn},
};

const SLICE: &str = "32768";

struct Process(Child);

impl Process {
    /// SIGKILL, deliberately. SIGTERM would let the graceful path flush, which is the case this
    /// test is specifically not about.
    fn kill(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.kill();
    }
}

fn spawn(
    data_dir: &std::path::Path,
    domains_dir: &std::path::Path,
    http_port: u16,
    admin_port: u16,
) -> Process {
    let child = Command::new(env!("CARGO_BIN_EXE_cachic"))
        .env("CACHE_DATA_DIR", data_dir)
        .env("CACHE_DOMAINS_DIR", domains_dir)
        // The mock origin is on loopback, which the address guard refuses by default. In
        // production this stays off; see FR-64.
        .env("ALLOW_PRIVATE_UPSTREAMS", "true")
        .env("HTTP_PORT", http_port.to_string())
        .env("ADMIN_PORT", admin_port.to_string())
        .env("CACHE_DISK_SIZE", "64m")
        .env("CACHE_MEM_SIZE", "8m")
        .env("CACHE_SLICE_SIZE", SLICE)
        .env("LOG_LEVEL", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn cachic");
    Process(child)
}

async fn wait_ready(admin_port: u16) -> bool {
    for _ in 0..100 {
        if let Ok(r) = reqwest::get(format!("http://127.0.0.1:{admin_port}/readyz")).await {
            if r.status() == 200 {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// Spawn and wait for readiness, retrying on fresh ports if the child could not bind.
///
/// Port discovery binds an ephemeral port and releases it, which leaves a window for another
/// process to take it before the child does. Serialising these tests narrows that window but does
/// not close it - anything else on the machine can claim a port too. Retrying closes it.
async fn spawn_ready(
    data_dir: &std::path::Path,
    domains_dir: &std::path::Path,
) -> (Process, u16, u16) {
    for attempt in 0..5 {
        let (http, admin) = ports();
        let process = spawn(data_dir, domains_dir, http, admin);
        if wait_ready(admin).await {
            return (process, http, admin);
        }
        drop(process);
        tokio::time::sleep(Duration::from_millis(200 * (attempt + 1))).await;
    }
    panic!("cachic never became ready across five attempts");
}

/// Two free ports.
///
/// Discovered by binding ephemeral ports and releasing them, rather than derived from the process
/// id. Derived ranges collide: this file and `access_log.rs` both spawn the binary, and their
/// pid-derived ranges overlapped, so the suite failed only when run in parallel.
fn ports() -> (u16, u16) {
    let take = || {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("no free port")
            .local_addr()
            .unwrap()
            .port()
    };
    (take(), take())
}

#[tokio::test]
async fn the_cache_survives_sigkill_without_corruption() {
    let origin = MockCdn::start(CdnConfig::default()).await.unwrap();
    let scratch = tempdir();
    let size = 16 * 32 * 1024u64;
    let path = MockCdn::object_path("survivor", size);
    let expected = content::range(content::seed_for("survivor"), 0, size as usize);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let host = origin.addr().to_string();

    // Fill.
    let domains = write_domains(&scratch, &origin.addr().ip().to_string());
    let (mut process, http, admin) = spawn_ready(&scratch, &domains).await;
    let url = format!("http://127.0.0.1:{http}{path}");
    let _ = admin;
    let first = client.get(&url).header("host", &host).send().await.unwrap();
    assert_eq!(first.status(), 200);
    assert_eq!(first.bytes().await.unwrap().as_ref(), expected.as_slice());

    // Give the disk writes a moment, then kill without warning.
    tokio::time::sleep(Duration::from_millis(500)).await;
    process.kill();
    drop(process);
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Restart against the same data directory. A fresh port is fine: the test is about the data
    // surviving, not about the address.
    let (_restarted, http, _admin) = spawn_ready(&scratch, &domains).await;
    let url = format!("http://127.0.0.1:{http}{path}");

    // Whatever survived, what it serves must be correct. Losing cache content to a hard kill is
    // a performance problem; serving wrong bytes is a correctness one, and only the second is
    // unacceptable.
    let second = client.get(&url).header("host", &host).send().await.unwrap();
    assert_eq!(second.status(), 200);
    assert_eq!(
        second.bytes().await.unwrap().as_ref(),
        expected.as_slice(),
        "content served after a crash did not match the origin"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

#[tokio::test]
async fn a_crash_does_not_leave_the_cache_unopenable() {
    // The failure that turns a power cut into a manual recovery: a data directory the process
    // refuses to open on restart.
    let scratch = tempdir();
    let domains = write_domains(&scratch, "127.0.0.1");

    for _ in 0..3 {
        let (mut process, _http, _admin) = spawn_ready(&scratch, &domains).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        process.kill();
        drop(process);
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    let _ = std::fs::remove_dir_all(&scratch);
}

/// Write a cache-domains directory naming a single service for `host`.
fn write_domains(base: &std::path::Path, host: &str) -> std::path::PathBuf {
    let dir = base.join("domains");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("cache_domains.json"),
        r#"{"cache_domains":[{"name":"mock","domain_files":["mock.txt"]}]}"#,
    )
    .unwrap();
    std::fs::write(dir.join("mock.txt"), format!("{host}\n")).unwrap();
    dir
}

fn tempdir() -> std::path::PathBuf {
    let base = std::env::var("CACHIC_TEST_TMP").unwrap_or_else(|_| "/tmp".into());
    let path = std::path::Path::new(&base).join(format!(
        "cachic-crash-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}
