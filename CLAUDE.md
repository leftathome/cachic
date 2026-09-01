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

**Status**: Greenfield. `docs/cachic-PRD.md` and `docs/cachic-IMPLEMENTATION-PLAN.md` are written;
M0 (spike + ADRs + repo skeleton) has not started. No code exists yet.

**Last Updated**: 2026-09-01
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
  `cargo nextest run`, `cargo deny check`.
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

### Operations
- Rebuild the image to deliver a code change into a container. Never copy files into a running one.
- Kubernetes YAML: one object per file, named `resourcetype-name.yaml`.
- Schema and config surface change over time - document them, and re-run any test that writes to a
  store or another service's API after a schema change.

---

## Forbidden Actions

- Never use the system resolver for upstream lookups - always the dedicated `UPSTREAM_DNS`
  resolver, or the proxy will loop back through the intercepting DNS server.
- Never cancel an in-flight slice fill because the client disconnected (FR-31).
- Never treat the redb object index as authoritative - slices are self-describing and the index is
  a rebuildable acceleration structure.
- Never soften the `CACHE_DATA_DIR/CONFIG` guard on `slice_size` / store format mismatch.
- Never commit an unencrypted secret. Credentials live in 1Password or Vault and sync into CI.
- Never put emoji in code, strings, log output or generated files.
- Never load the whole `.agent/` tree at once - it defeats the token budget.
- Never delete tests without a replacement.

---

## Documentation Structure

```
.agent/
|-- DEVELOPMENT-README.md      # Navigator (always load first)
|-- tasks/                     # Implementation plans, TASK-XX-slug.md
|-- system/                    # project-architecture.md, tech-stack-patterns.md
\-- sops/                      # integrations / debugging / development / deployment
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
