# TASK-21: Fuzzing in CI

## Context
Milestone: M2 | Requirements: plan section 5 (fuzz row)

Every parser here eats attacker-adjacent input from CDNs and from a community-maintained domain
list. Fuzzing is cheap insurance against a panic that takes the cache down for a whole LAN party.

## Implementation Plan
- [ ] `cargo-fuzz` targets: `Range` header, `Content-Range`, cache-domains files, config file
- [ ] Seed corpora from real fixtures
- [ ] 5-minute runs per target on every push; 30 minutes nightly
- [ ] Crash artefacts uploaded and minimised automatically
- [ ] Any found crash becomes a permanent unit-test case

## Technical Decisions
- Fuzz targets stay in the repo, not in a side branch, so they keep compiling as the parsers change.
- A parser that cannot be fuzzed in isolation is too entangled - that is a design signal, not a
  reason to skip fuzzing it.

## Dependencies
- Requires: TASK-08, TASK-09, TASK-07, TASK-02
- Blocks: M2 exit criteria

## Completion Checklist
- [ ] Four targets running in CI
- [ ] Seed corpora committed
- [ ] Nightly job reports separately from the per-push job
- [ ] Regression tests exist for every crash ever found
