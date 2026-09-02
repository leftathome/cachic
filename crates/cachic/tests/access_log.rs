//! The access log, end to end (TASK-24).
//!
//! Runs the real binary in both log formats and inspects what it writes, because the value of
//! FR-52 is entirely in what an existing dashboard can parse - a unit test of the formatter
//! proves the string, not that anything emits it.

use std::{
    io::Read,
    process::{Child, Command, Stdio},
    time::Duration,
};

use cachic_testkit::mockcdn::{Config as CdnConfig, MockCdn};

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let base = std::env::var("CACHIC_TEST_TMP").unwrap_or_else(|_| "/tmp".into());
    let path = std::path::Path::new(&base).join(format!(
        "cachic-log-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn write_domains(base: &std::path::Path, host: &str) -> std::path::PathBuf {
    let dir = base.join("domains");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("cache_domains.json"),
        r#"{"cache_domains":[{"name":"steam","domain_files":["steam.txt"]}]}"#,
    )
    .unwrap();
    std::fs::write(dir.join("steam.txt"), format!("{host}\n")).unwrap();
    dir
}

/// Run one request through the binary in the given log format and return what it wrote.
async fn capture_log(tag: &str, format: &str, http: u16, admin: u16) -> String {
    let origin = MockCdn::start(CdnConfig::default()).await.unwrap();
    let dir = scratch(tag);
    let domains = write_domains(&dir, &origin.addr().ip().to_string());

    let mut child = Process(
        Command::new(env!("CARGO_BIN_EXE_cachic"))
            .env("CACHE_DATA_DIR", &dir)
            .env("CACHE_DOMAINS_DIR", &domains)
            .env("ALLOW_PRIVATE_UPSTREAMS", "true")
            .env("HTTP_PORT", http.to_string())
            .env("ADMIN_PORT", admin.to_string())
            .env("CACHE_DISK_SIZE", "64m")
            .env("CACHE_MEM_SIZE", "8m")
            .env("LOG_FORMAT", format)
            .env("LOG_LEVEL", "info")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cachic"),
    );

    for _ in 0..100 {
        if let Ok(r) = reqwest::get(format!("http://127.0.0.1:{admin}/readyz")).await {
            if r.status() == 200 {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let path = MockCdn::object_path("depot", 4096);
    let response = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{http}{path}"))
        .header("host", origin.addr().to_string())
        .header("user-agent", "Valve/Steam HTTP Client 1.0")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    // The internal attribution header must not reach a client.
    assert!(
        response.headers().get("x-cachic-service").is_none(),
        "the internal service header leaked to the client"
    );
    let _ = response.bytes().await;

    tokio::time::sleep(Duration::from_millis(400)).await;
    let mut stdout = child.0.stdout.take().unwrap();
    let _ = child.0.kill();
    let _ = child.0.wait();
    drop(child);

    let mut buffer = String::new();
    let _ = stdout.read_to_string(&mut buffer);
    let _ = std::fs::remove_dir_all(&dir);
    buffer
}

fn ports(offset: u16) -> (u16, u16) {
    let base = 22_000 + (std::process::id() % 1_000) as u16 + offset;
    (base, base + 1)
}

#[tokio::test]
async fn json_format_logs_a_structured_access_event() {
    let (http, admin) = ports(0);
    let output = capture_log("json", "json", http, admin).await;

    let access: Vec<&str> = output
        .lines()
        .filter(|l| l.contains("cachic::access"))
        .collect();
    assert!(
        !access.is_empty(),
        "no access event was logged at all:\n{output}"
    );

    let line = access[0];
    let value: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|e| {
        panic!("access line is not JSON: {e}\n{line}");
    });
    let fields = &value["fields"];
    // The fields FR-51 names, each of which someone will build an alert on.
    for field in [
        "service",
        "host",
        "method",
        "path",
        "status",
        "bytes",
        "cache",
        "upstream_seconds",
    ] {
        assert!(!fields[field].is_null(), "missing field {field} in {line}");
    }
    assert_eq!(fields["service"], "steam");
    assert_eq!(fields["method"], "GET");
    assert_eq!(fields["status"], 200);
    assert_eq!(fields["cache"], "MISS");
}

#[tokio::test]
async fn lancache_format_is_parsable_by_an_existing_dashboard() {
    // The point of FR-52: LANCache Manager and friends parse this positionally.
    let (http, admin) = ports(10);
    let output = capture_log("lancache", "lancache", http, admin).await;

    let line = output
        .lines()
        .find(|l| l.starts_with("[steam]"))
        .unwrap_or_else(|| panic!("no lancache-format access line:\n{output}"));

    // Field by field, the way a positional parser reads it.
    assert!(line.starts_with("[steam] 127.0.0.1 / - - - ["), "{line}");
    assert!(
        line.contains("] \"GET /o/depot/4096 HTTP/1.1\" 200 4096 "),
        "request, status and byte count are not where a parser expects them: {line}"
    );
    assert!(line.contains("\"Valve/Steam HTTP Client 1.0\""), "{line}");
    assert!(
        line.ends_with("\"MISS\""),
        "cache status must be last: {line}"
    );

    // And the timestamp must be a real Common Log Format time, not a placeholder.
    let timestamp = line
        .split('[')
        .nth(2)
        .and_then(|s| s.split(']').next())
        .unwrap_or("");
    assert!(
        timestamp.contains("/20") && timestamp.ends_with("+0000"),
        "timestamp {timestamp:?} is not Common Log Format: {line}"
    );
}
