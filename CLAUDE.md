# cachic - Claude Code Configuration

## Context

`cachic` is a single-binary HTTP caching proxy for game-distribution and OS-update CDN traffic.
It replaces the nginx engine inside `lancachenet/monolithic` while keeping lancache's deployment
model: DNS points CDN hostnames at the cache, clients speak plain HTTP, and the cache fetches,
slices, stores and serves effectively-immutable content.

**Tech Stack**: Rust (tokio, hyper, reqwest/rustls, hickory-resolver, axum), foyer hybrid RAM+disk
store, redb object index, Prometheus metrics, Docker + Helm/Kubernetes

**Core Principle**: Slice-aware caching expressed in cache terms, not nginx terms - bounded memory,
self-describing slices, a rebuildable index, and observability as a first-class feature.

**Status**: M0-M4 delivered. TASK-01 through TASK-33 done, 369 tests, CI green, `v0.1.0-rc5`
published (multi-arch signed image, SBOM, Helm chart, binaries and a `cachic-tools` harness
tarball). TASK-34 is open.

Every finding from the rc1 hardware deployment is fixed; `docs/rc-test-plan.md` section F is the
regression check for them. What remains is validation on real hardware and real clients, which
needs the cluster, not more code. One measured open question: TASK-34, warm serving about 12%
behind monolithic once the working set is several times the RAM tier.

**Last Updated**: 2026-09-05
**Navigator Version**: 6.18.1

---

## Read These First

| Doc | What it settles |
|---|---|
| `docs/cachic-PRD.md` | Problem, goals/non-goals, personas, functional requirements (FR-xx) |
| `docs/cachic-IMPLEMENTATION-PLAN.md` | Language decision, architecture, repo layout, libraries, milestones M0-M4, testing, CI/CD, packaging |
| `.agent/DEVELOPMENT-README.md` | Navigator index - load this, not the whole `.agent/` tree |

The PRD and plan are authoritative for design questions. Do not invent architecture that
contradicts them; propose an amendment instead.

Load these when the work touches them, not before:

| Doc | When |
|---|---|
| `docs/known-limitations.md` | Anything about musl, static binaries, the glibc floor, or macOS |
| `docs/sizing.md` | Memory, CPU or capacity questions. The rule is `CACHE_MEM_SIZE + ~700 MiB`, measured |
| `docs/metrics.md` | "Can an operator see X?" - every series, grouped by the question it answers |
| `docs/rc-test-plan.md` | Anything a developer machine cannot prove; section F is the rc1 regression list |
| `.agent/sops/deployment/cutting-a-release.md` | **Before tagging.** A tag cannot be reused here |
| `.agent/sops/development/measuring-in-a-container.md` | **Before producing any performance or memory number** |

---

## Navigator Quick Start

**Every session begins with**: "Start my Navigator session"

That loads `.agent/DEVELOPMENT-README.md`, which indexes the system docs, task plans and SOPs.

**Core workflow**:
1. Start session -> navigator loads
2. Load only the task doc and system doc the current work needs
3. Implement, following the patterns below
4. Document -> "Archive TASK-XX documentation" when complete
5. Compact after isolated sub-tasks

---

## Project Code Standards

### Rust
- Pinned stable toolchain via `rust-toolchain.toml`; `cargo` workspace layout per plan section 2.
- Gate before every push: `cargo fmt --check`, `cargo clippy --all-targets -D warnings`,
  `cargo nextest run`, `cargo deny check`, and for chart changes `helm lint` plus
  `helm template | kubeconform -strict`.
- Set `CARGO_INCREMENTAL=0` for repeated runs. Incremental artefacts reached 45 GB here and filled
  the disk the WSL VM lives on.
- jemalloc is the global allocator on gnu targets, and the reason is measured: glibc's per-thread
  arenas fragment with 1 MiB slice buffers and RSS settles far above the configured tier. The
  `#[global_allocator]` is declared in `main.rs` **and** in `tests/perf_gate.rs`, because an
  integration test does not link `main.rs` and a gate that benchmarks a different allocator than
  the one that ships is measuring nothing.
- Typed errors (`thiserror`) inside modules, context at the boundary. No panics in request paths.
- Pass `bytes::Bytes` for slice payloads - no copies for fan-out.
- Blocking or CPU-heavy work goes to `spawn_blocking`, never onto an async worker.
- New dependencies must pass `cargo deny` (permissive licences, no advisories).

### Testing
- Write tests with the code and run them; do not defer. `cargo nextest run` is the loop.
- Layers: unit (`proptest` for range parsing), fuzz (`cargo-fuzz`), component against `mockcdn`,
  differential (proxy bytes == origin bytes), integration in compose, chaos, `criterion` benches.
- Coverage target >= 80% on `services`, `orchestrator`, `store`, `proxy`.
- The artefact is a container, so integration and chaos tests run in containers.
- **Prove a new test can fail.** Break the thing it guards, watch it fail with a message that
  names the real problem, then restore. Two real examples of checks that could not fail: a DNS
  regression test using `localhost`, which hickory resolves internally, so both code paths agreed
  and it passed against the *unfixed* code; and a metrics assertion matching `service="steam"` as
  a substring, which `cdn_service="steam"` also satisfies. A check that cannot fail is worse than
  no check, because it is counted as coverage.
- The performance gate's floor is a provisional constant, not the nginx figure the standard names.
  TASK-25 replaces it with a measured one.

### Operations
- Rebuild the image to deliver a code change into a container. Never copy files into a running one.
- Kubernetes YAML: one object per file, named `resourcetype-name.yaml`.
- Schema and config surface change over time - document them, and re-run any test that writes to a
  store or another service's API after a schema change.

---

## Forbidden Actions

- Never use the system resolver for upstream lookups - always the dedicated `UPSTREAM_DNS`
  resolver, or the proxy loops back through the intercepting DNS server. Concretely: keep the
  `.dns_resolver(GuardedResolver::new(...))` line in `upstream/client.rs`. Without it reqwest
  resolves independently, the address guard inspects one address while the socket connects to
  another, and FR-64 becomes bypassable rather than merely broken. This was shipped in rc1.
- Never cancel an in-flight slice fill because the client disconnected (FR-31).
- Never treat the redb object index as authoritative - slices are self-describing and the index is
  a rebuildable acceleration structure.
- Never soften the `CACHE_DATA_DIR/CONFIG` guard on `slice_size` / store format mismatch.
- Never commit an unencrypted secret. Credentials live in 1Password or Vault and sync into CI.
- Never put emoji in code, strings, log output or generated files.
- Never load the whole `.agent/` tree at once - it defeats the token budget.
- Never delete tests without a replacement.
- Never label a metric `service`. Kubernetes monitoring attaches its own and renames ours to
  `exported_service`, collapsing every per-CDN panel to one flat series. The label is
  `cdn_service`, and a test enforces it.
- Never treat a green CI job as proof an artefact is correct. A green job means the command exited
  zero: rc3 was entirely green and shipped an SBOM describing the base image and nothing else.
  Download the artefact and inspect it.
- Never delete or re-push a tag. Ref creation is restricted here, so a deleted tag stays deleted
  and the version number is spent - bump instead.
- Never publish a throughput or memory number without saying whether it was cold or warm, and what
  the working set was against the RAM tier. Omitting that produced a "62% of nginx" finding that
  did not survive a matched re-run.

---

## Documentation Structure

```
.agent/
|-- DEVELOPMENT-README.md      # Navigator (always load first)
|-- tasks/                     # Implementation plans, TASK-XX-slug.md
|-- system/                    # project-architecture.md, tech-stack-patterns.md
\-- sops/                      # integrations / debugging / development / deployment
    |-- deployment/cutting-a-release.md
    \-- development/measuring-in-a-container.md
```

Token-efficient loading: navigator ~2k, current task ~3k, one system doc ~5k, one SOP ~2k.

---

## Configuration

Navigator config lives in `.agent/.nav-config.json` (`task_prefix`: `TASK`,
`project_management`: `none`, `team_chat`: `none`). Change those if a tracker is adopted.

---

## Commit Guidelines

- Conventional commits - `type(scope): description` - because `git-cliff` generates the changelog.
- Types: feat, fix, docs, refactor, test, chore, perf, ci, build.
- Reference the task: `feat(store): add slice codec checksum TASK-07`.
