# TASK-06: M0 architecture decision records

## Context
Milestone: M0 | Requirements: plan Appendix B

Eight decisions are load-bearing for everything in M1-M4. Written as MADR-format ADRs under
`docs/adr/NNNN-*.md` so that a future contributor can see what was decided, why, and what would
change the answer.

## Implementation Plan
- [ ] ADR 0001: Language and runtime (Rust + tokio) - cites TASK-04 numbers, records the Go
      fallback in Appendix A and its cost
- [ ] ADR 0002: HTTP layer (hyper vs Pingora) - cites TASK-05
- [ ] ADR 0003: Store engine (foyer) and object index (redb) - cites TASK-04, records the
      fallback design (per-object sparse files + bitmap, ~2 weeks)
- [ ] ADR 0004: Slice size, key scheme, generation semantics
- [ ] ADR 0005: Configuration surface and lancache env compatibility
- [ ] ADR 0006: Repository hosting and CI topology
- [ ] ADR 0007: Access-log compatibility with lancache tooling
- [ ] ADR 0008: Security posture - allow-listed upstreams, no TLS termination

## Technical Decisions
- MADR format, numbered, immutable once accepted. A reversal is a new ADR superseding the old one,
  never an edit to history.
- Each ADR must name what evidence would overturn it. An ADR with no falsifier is a preference,
  not a decision.

## Dependencies
- Requires: TASK-04, TASK-05
- Blocks: M0 completion; M1 implementation tasks reference these

## Completion Checklist
- [ ] All eight ADRs written and accepted
- [ ] Each cites its evidence
- [ ] Each states what would change the decision
- [ ] `docs/adr/` indexed from the docs site outline
