# TASK-01: Repository skeleton and dev loop

## Context
Milestone: M0 | Requirements: plan section 2 (repo layout), section 6 (CI)

Nothing exists but docs. Every other task needs a workspace to land in, a pinned toolchain so
results are reproducible, and a `just` recipe set that CI and contributors both call. Getting this
wrong once costs every later task, so it goes first.

## Implementation Plan
### Phase 1: Workspace
- [ ] `Cargo.toml` workspace with `crates/cachic` (bin) and `crates/cachic-testkit` (dev)
- [ ] `rust-toolchain.toml` pinning stable + `rustfmt`, `clippy`, `x86_64-unknown-linux-musl`
- [ ] Module skeleton under `crates/cachic/src/`: config, services, proxy, orchestrator,
      upstream, store, sni, admin, telemetry (each a stub with a doc comment)
- [ ] `main.rs` that parses `--version` and exits, so the binary builds from commit one

### Phase 2: Guardrails
- [ ] `deny.toml`: permissive licences only, advisories deny, bans on duplicate majors
- [ ] `.gitignore` for `target/`, benchmark scratch, local cache dirs
- [ ] `rustfmt.toml`, `clippy.toml` if defaults need adjusting

### Phase 3: Dev loop
- [ ] `justfile` with `fmt`, `lint`, `test`, `bench`, `image`, `chart`, `spike`
- [ ] `README.md` stub: what this is, status, how to build
- [ ] `LICENSE` (see Technical Decisions)

## Technical Decisions
- One binary crate with modules, not a crate per module. Split only when a module needs an
  independent release cadence (plan section 2).
- Licence: MIT or Apache-2.0 (dual) to stay compatible with the Rust ecosystem and with
  `cargo deny`'s permissive-only policy. Confirm with the owner before committing.
- `CARGO_TARGET_DIR` should point at native Linux storage when developing under WSL2; building on
  a `/mnt/c` DrvFs path is several times slower. Document in the README, do not hard-code.

## Dependencies
- Requires: Rust toolchain installed on the dev host
- Blocks: TASK-02, TASK-03, and everything after

## Completion Checklist
- [ ] `cargo build` succeeds from a clean checkout
- [ ] `just lint` and `just test` pass (test suite may be near-empty)
- [ ] `cargo deny check` passes
- [ ] Layout matches plan section 2
