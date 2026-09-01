# TASK-14: Testkit - mockcdn, differ, load generator

## Context
Milestone: M1 | Requirements: plan section 5

The differential tester is the main correctness argument for the whole project: for random URLs
and random ranges, bytes through the proxy must equal bytes from the origin. It has to be good.

## Implementation Plan
### Phase 1: mockcdn
- [ ] Deterministic content `f(url, offset)` - any byte verifiable without a reference copy
- [ ] Modes: range-capable, range-ignoring, flaky 5xx, slow, changing validators mid-object,
      redirects, chunked bodies, zero-length
- [ ] In-process on a random port for `#[tokio::test]`, and standalone for compose

### Phase 2: differ
- [ ] Random URL and range generation with a seed recorded on failure
- [ ] Cold and warm passes
- [ ] Shrinks a failure to a minimal reproducing case

### Phase 3: load generator
- [ ] N concurrent clients, configurable object mix and range distribution
- [ ] Reports throughput, TTFB percentiles and upstream amplification

## Technical Decisions
- The testkit is a dev-dependency crate, never linked into the shipped binary.
- A failing differ run must print a seed that reproduces it exactly. A flaky test with no
  reproducer is worse than no test.

## Dependencies
- Requires: TASK-01 (promoted from the TASK-03 spike code)
- Blocks: TASK-12 verification, TASK-20, TASK-25

## Completion Checklist
- [ ] All mockcdn modes exercised by tests
- [ ] Differ reproduces from a seed
- [ ] Load generator numbers agree with an independent tool (`oha`) within noise
