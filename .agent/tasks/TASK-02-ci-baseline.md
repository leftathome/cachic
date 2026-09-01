# TASK-02: CI baseline

## Context
Milestone: M0 | Requirements: plan section 6

M0's exit criteria include "CI green on lint/test". CI must call the same `just` recipes a
contributor calls, so that a green pipeline actually means a working local loop.

## Implementation Plan
### Phase 1: Per-push pipeline
- [ ] `cargo fmt --check`
- [ ] `cargo clippy --all-targets -D warnings`
- [ ] `cargo nextest run`
- [ ] `cargo deny check` (licences, advisories)
- [ ] Typo check
- [ ] Image build (no push)

### Phase 2: Caching and matrix
- [ ] Cargo registry + target cache keyed on `Cargo.lock`
- [ ] Build on amd64; arm64 build deferred to TASK-26 (release pipeline)

### Phase 3: Hygiene
- [ ] CODEOWNERS, issue/PR templates
- [ ] DCO sign-off or CLA decision recorded

## Technical Decisions
- Canonical hosting is an open question (PRD section 12). Write `.github/workflows/` first since
  the remote is GitHub, but keep every step as a `just` recipe so a GitLab mirror is a thin
  translation rather than a rewrite.
- Nightly jobs (fuzz, chaos, integration load, MSRV) are deferred to TASK-21 and TASK-20.

## Dependencies
- Requires: TASK-01
- Blocks: M0 exit criteria

## Completion Checklist
- [ ] Pipeline green on a pushed branch
- [ ] Every CI step is also a local `just` recipe
- [ ] Failure output is readable without opening the raw log
