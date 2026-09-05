# cachic - Development Documentation Navigator

**Project**: Single-binary HTTP caching proxy for game-distribution and OS-update CDN traffic; a drop-in replacement for the nginx engine inside `lancachenet/monolithic`.
**Tech Stack**: Rust (tokio, hyper, reqwest/rustls, hickory-resolver), foyer hybrid RAM+disk store, redb object index, axum admin API, Prometheus metrics, Docker + Helm/Kubernetes
**Status**: Greenfield - PRD and implementation plan written, M0 (spike + ADRs) not started
**Updated**: 2026-09-01

---

## Source of Truth

Two documents define this project. Read them before proposing architecture:

| Doc | What it settles |
|---|---|
| [`docs/cachic-PRD.md`](../docs/cachic-PRD.md) | Problem, goals/non-goals, personas, functional requirements (FR-xx), scenarios |
| [`docs/cachic-IMPLEMENTATION-PLAN.md`](../docs/cachic-IMPLEMENTATION-PLAN.md) | Language decision, architecture, repo layout, library choices, milestones M0-M4, testing, CI/CD, packaging |

Navigator docs under `.agent/` are the working layer on top of those: task plans, architecture
notes as code lands, and SOPs. When the two disagree, the plan wins until it is explicitly amended.

---

## Quick Start for Development

### New to This Project?
**Read in this order:**
1. `docs/cachic-PRD.md` sections 1-5 - what this is and why
2. [Project Architecture](./system/project-architecture.md) - components, request flow, storage model
3. [Tech Stack Patterns](./system/tech-stack-patterns.md) - Rust/tokio/foyer conventions for this codebase
4. `docs/cachic-IMPLEMENTATION-PLAN.md` section 4 - which milestone we are in

### Starting a New Feature?
1. Check `tasks/` for an existing plan covering it
2. Read the matching FR-xx in the PRD - most behaviour is already specified
3. Read the relevant `system/` doc
4. Check `sops/` for procedures that touch it
5. Create `.agent/tasks/TASK-XX-slug.md` before writing code

### Fixing a Bug?
1. Check [`sops/debugging/`](#debugging) for known issues
2. Reproduce with a test first (see testing strategy, plan section 5)
3. After fixing, write an SOP if the cause was non-obvious

---

## Documentation Structure

```
.agent/
|-- DEVELOPMENT-README.md     <- You are here (navigator)
|-- .nav-config.json          <- Navigator configuration
|
|-- tasks/                    <- Implementation plans (TASK-XX-slug.md)
|
|-- system/                   <- Living architecture documentation
|   |-- project-architecture.md
|   \-- tech-stack-patterns.md
|
|-- sops/                     <- Standard Operating Procedures
|   |-- integrations/         # cache-domains, lancache-dns, prefill tools, LANCache Manager
|   |-- debugging/            # recurring failures and their fixes
|   |-- development/          # dev loop, testing, release hygiene
|   \-- deployment/           # image, Helm chart, Flux, upgrades
|
\-- grafana/                  <- Navigator metrics dashboard (Grafana + Prometheus)
```

---

## Documentation Index

### System Architecture (`system/`)

#### [Project Architecture](./system/project-architecture.md)
**When to read**: Starting work on any component, understanding how slices flow

**Contains**: component map, request flow, storage model, non-obvious behaviours, repo layout, milestone status

#### [Tech Stack Patterns](./system/tech-stack-patterns.md)
**When to read**: Writing Rust in this repo

**Contains**: crate choices and why, async/backpressure rules, error handling, config surface, testing conventions, things not to do

---

### Implementation Plans (`tasks/`)

**Index**: [`tasks/TASK-INDEX.md`](./tasks/TASK-INDEX.md) - all 33 tasks by milestone with
status and exit criteria. Read the index, then only the task you are working on.

**Format**: `TASK-XX-feature-slug.md`

Each task should name the milestone (M0-M4) and the FR-xx requirements it satisfies, so that a
task doc plus the PRD section is enough context to implement without re-reading everything.

```markdown
# TASK-XX: [Feature Name]

## Context
Milestone: M1 | Requirements: FR-12, FR-13
[Why building this now]

## Implementation Plan
### Phase 1: [Name]
- [ ] Sub-task

## Technical Decisions
[Crate choices, trade-offs, links to ADRs]

## Dependencies
[Requires / blocks]

## Completion Checklist
- [ ] Tests written and passing (`cargo nextest run`)
- [ ] `cargo fmt --check` and `cargo clippy -D warnings` clean
- [ ] System docs updated
- [ ] Metrics/logs emitted for new behaviour
```

---

### Standard Operating Procedures (`sops/`)

#### Integrations (`sops/integrations/`)
Ecosystem glue: `uklans/cache-domains` snapshot refresh, `lancache-dns` wiring, SteamPrefill /
Epic / Battle.net prefill verification, LANCache Manager compatibility, access-log format.

#### Debugging (`sops/debugging/`)
Slice/range mismatches, validator-change generation bumps, foyer recovery, disk-full behaviour,
DNS loops (the proxy must never use the intercepting resolver).

#### Development (`sops/development/`)
`just` dev loop, running `mockcdn`, differential and chaos suites, fuzz targets, coverage.

- **[Measuring in a container, and against monolithic](./sops/development/measuring-in-a-container.md)**
  — how to stand up the origin, drive load against a running proxy, and what makes a number worth
  publishing. Read before producing any throughput or memory figure.

#### Deployment (`sops/deployment/`)
Multi-arch image build, cosign/SBOM, Helm chart release, Flux HelmRelease, data-volume upgrades.

- **[Cutting a release](./sops/deployment/cutting-a-release.md)** — the procedure, and the six
  traps that each cost a tag during 0.1.0-rc1 through rc5. Read before tagging: a tag cannot be
  reused here.

**SOP Template**:
```markdown
# SOP: [Process Name]

## Context
[When/why you need this]

## Problem
[What went wrong or needs doing]

## Solution
1. [Step]
2. [Step]

## Prevention
- [ ] Check to add

## Related
- system/[doc].md, TASK-XX
```

---

## When to Read What

### Scenario: Implementing a milestone deliverable
1. Plan section 4 -> the milestone's deliverables and exit criteria
2. PRD -> the FR-xx being satisfied
3. `system/project-architecture.md` -> where it fits
4. `system/tech-stack-patterns.md` -> how to write it here
5. Create `tasks/TASK-XX-*.md`, then implement

### Scenario: Touching the store or slice format
1. Plan section 1.3 (storage model) - slices are self-describing; the index is rebuildable
2. Any ADR on slice size / key scheme (M0 deliverable)
3. Confirm the on-disk config guard still rejects mismatched `slice_size`

### Scenario: Debugging a cache correctness issue
1. `sops/debugging/`
2. Reproduce against `mockcdn` with the differential tester before touching production paths

### Scenario: Context running low
Load only: this file (~2k), the current task doc (~3k), one system doc (~5k), one SOP (~2k).
Then compact.

---

## Project Conventions

- **Language**: Rust, pinned stable via `rust-toolchain.toml`. Community layout, `cargo` workspace.
- **Testing**: `cargo nextest run`; `proptest` for range parsing; differential tests against
  `mockcdn`; chaos suite for crash/disk-full. Coverage target >= 80% on `services`,
  `orchestrator`, `store`, `proxy`. Write the test with the code, run it, do not defer.
- **Lint gate**: `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, `cargo deny check`.
- **Containers**: if it runs in a container, test it in a container; rebuild the image to deliver
  code changes, never copy files into a running container.
- **Secrets**: never commit an unencrypted secret. Registry/signing credentials live in 1Password
  or Vault and are synchronised to CI at build time.
- **Kubernetes YAML**: one object per file, named `resourcetype-name.yaml`.
- **No emoji** in code, strings, or generated output.
- **Commits**: `type(scope): description`, conventional-commit types (drives `git-cliff`).

---

## Milestone Status

| Milestone | Scope | Status |
|---|---|---|
| M0 | Spike, measurements, ADRs, repo skeleton | Not started |
| M1 | MVP proxy v0.1 (services, range GET/HEAD, orchestrator, store, telemetry, image) | Not started |
| M2 | Robustness v0.2 (no_ranges filler, generations, admin API, chaos suite, fuzz) | Not started |
| M3 | Deployment and parity v0.3 (Helm, Flux, access log, benchmarks, release pipeline) | Not started |
| M4 | v1.0 (SNI pass-through, rule parity review, soak, docs, announce) | Not started |

Update this table when a milestone opens or closes; it is the fastest orientation signal in the repo.

---

**Last Updated**: 2026-09-01
**Powered By**: Navigator 6.18.1
