# cachic - Tech Stack Patterns

**Purpose**: How to write Rust in this repository - crate choices, async rules, testing conventions.
**Updated**: 2026-09-01
**Status**: Conventions agreed in the plan; to be revised once real code exists.

---

## Crate Choices (plan section 3)

| Area | Crate(s) | Notes |
|---|---|---|
| Runtime | `tokio` (multi-thread) | |
| HTTP server | `hyper` 1.x, `hyper-util`, `http`, `http-body-util`, `bytes` | HTTP/1.x only client-side |
| Upstream client | `reqwest` (rustls, streaming) or `hyper-util` legacy client + `hyper-rustls` | Pool per host, no auto-redirect - handle redirects explicitly |
| DNS | `hickory-resolver` | Dedicated resolver, IPv4/IPv6 |
| TLS | `rustls`, `webpki-roots` | No OpenSSL; keeps static musl builds working |
| Store | `foyer` (hybrid cache), `redb` (object index) | Pinned versions, isolated behind a `store::Store` trait |
| Hashing | `blake3` (object ids), `xxhash-rust` (slice checksums) | |
| Config | `clap` (derive, env), `figment`/`config`, `serde`, `toml` | TOML for the optional rules file |
| Logging | `tracing`, `tracing-subscriber` (json) | Access log is a dedicated `tracing` target with its own formatter |
| Metrics | `metrics` + `metrics-exporter-prometheus` | foyer exposes its metrics through `metrics` |
| Admin HTTP | `axum` | Same runtime, separate port |
| Concurrency | `dashmap`, `arc-swap` (hot config), `tokio::sync` | |
| SNI | `tls-parser` (ClientHello) + `tokio::io::copy_bidirectional` | |
| Allocator | `mimalloc` (feature-gated) | Measure against system allocator in M0 |
| Testing | `cargo-nextest`, `proptest`, `cargo-fuzz`, `criterion`, `tempfile` | `mockcdn` lives in the testkit crate |

Adding a dependency means passing `cargo deny check`: permissive licences only, no duplicate
major versions without a reason, no advisories.

---

## Async and Backpressure

**The window is the backpressure.** Slice futures are awaited in order inside a bounded
`READAHEAD_SLICES` window. Do not spawn unbounded per-slice tasks; per-connection RAM must stay at
`READAHEAD_SLICES * slice_size`.

**Fills outlive connections.** A fill is owned by the store, not by the request that triggered it.
Never attach a fill future to a connection-scoped cancellation token - a client hanging up mid
download must not poison the cache for the next client.

**Coalesce, do not lock.** Concurrent misses for the same slice go through foyer's `fetch`, which
gives one upstream request and N streaming readers. This is the behaviour we are buying over
nginx's `proxy_cache_lock`; do not reintroduce a "wait for the other guy to finish" path.

**Blocking work goes to `spawn_blocking`.** Disk IO that foyer does not already own, and any
CPU-heavy hashing of large buffers, must not run on the async worker.

**Zero-copy fan-out.** Pass `bytes::Bytes` around; do not `to_vec()` slice payloads to hand them
to two consumers.

---

## Error Handling

- Library-ish modules return typed errors (`thiserror`); `main.rs` and request handlers use
  `anyhow`-style context at the boundary.
- An upstream failure is a response, not a panic. Panics in a request path must be impossible -
  `unwrap()` only where an invariant is genuinely local and documented.
- Every error path that a user could hit should be countable: increment a metric label, then log.

---

## Configuration

12-factor: environment variables are the primary surface, with an optional TOML rules file.
`clap` derive with `env` gives one definition per setting. Rules:

- Names are cache terms, not nginx terms: `CACHE_MEM_SIZE`, `CACHE_DISK_SIZE`, `CACHE_DATA_DIR`,
  `CACHE_MAX_AGE`, `SLICE_SIZE`, `READAHEAD_SLICES`, `UPSTREAM_DNS`.
- Sizes parse with units (`64GiB`, `2TB`); validation happens at startup, not first use.
- The config reference in `docs/` is generated from the schema, not hand-maintained.
- Changing `SLICE_SIZE` or the store format against an existing data dir aborts startup unless
  `FORCE_CONFIG=true`. That guard is a feature - do not soften it.

---

## Observability

- Logs are JSON on stdout via `tracing-subscriber`. No log files on a volume.
- The lancache-format access log is an optional additional `tracing` target for ecosystem
  dashboards - it is a compatibility shim, not the primary observability story.
- New behaviour ships with a metric. Cache status (`HIT`/`MISS`/`PARTIAL`), slice fetches, upstream
  errors, generation bumps, evictions and disk headroom all need to be visible without a debugger.

---

## Testing Conventions

Write the test with the code and run it. The layers (plan section 5):

| Level | What | Tooling |
|---|---|---|
| Unit | Range parsing, key normalisation against cache-domains fixtures, slice arithmetic, header filtering, config precedence | `proptest`, fixtures in `testdata/` |
| Fuzz | `Range`, `Content-Range`, cache-domains files, config file | `cargo-fuzz` (5 min in CI, longer nightly) |
| Component | Orchestrator against `mockcdn`: range-capable, range-ignoring, flaky 5xx, slow, changing validators mid-object, redirects, chunked, zero-length | in-process `mockcdn`, `#[tokio::test]` |
| Differential | Bytes through the proxy == bytes from `mockcdn` for random URLs/ranges (content is a deterministic `f(url, offset)`), repeated warm | testkit `differ` |
| Integration | Built binary + `mockcdn` + `lancache-dns` in compose; load generator; verify hashes and metrics | compose profile `ci` |
| Chaos | `kill -9` mid-fill, disk full on tmpfs, IO throttling via cgroups, DNS failure | compose profile `chaos` |
| Performance | `criterion` micro-benchmarks; macro harness vs monolithic | results committed to `docs/benchmarks/` |

Coverage via `cargo-llvm-cov`, target >= 80% on `services`, `orchestrator`, `store`, `proxy`.

**Container rule**: the shipped artefact is a container, so integration and chaos runs happen in
containers. Rebuild the image to deliver a change into it - never copy a binary into a running one.

---

## Dev Loop

`just` recipes are the single entry point, and CI runs the same recipes:

```
just fmt      # cargo fmt
just lint     # cargo fmt --check && cargo clippy --all-targets -D warnings && cargo deny check
just test     # cargo nextest run
just bench    # criterion
just image    # docker buildx build
just chart    # helm lint / ct lint
```

Anything a contributor must run before pushing belongs in a recipe, not in a README paragraph.

---

## Things Not To Do

- Do not use the system resolver for upstream lookups.
- Do not cancel a fill when the client disconnects.
- Do not treat the redb index as authoritative - slices are self-describing and the index is
  rebuildable; a bug that "fixes" a mismatch by trusting the index will serve wrong bytes.
- Do not add RFC 9111 freshness logic; this is an immutable-object cache with operator TTLs.
- Do not put emoji in code, strings, log output or generated files.
- Do not commit an unencrypted secret; signing and registry credentials come from 1Password/Vault
  into CI.
- Do not hand-write multi-object Kubernetes YAML files - one object per file,
  `resourcetype-name.yaml`.

---

## Related Documentation

- [Project Architecture](./project-architecture.md)
- `docs/cachic-IMPLEMENTATION-PLAN.md` sections 3, 5, 6

## Change Log

### 2026-09-01 - Initial creation
- Extracted from the implementation plan at Navigator init.
