# TASK-32: Documentation site

## Context
Milestone: M4 | Requirements: FR-74, G5; plan section 8

A contributor-friendly codebase (G5) with no documentation site is not contributor friendly. This
is also where the migration story lives, which decides whether anyone actually switches.

## Implementation Plan
- [ ] mdBook in `docs/`, published with Pages
- [ ] Quickstart (compose), Kubernetes (Helm + Flux), configuration reference (generated from the
      schema), service rules, migration from lancache, observability with the metrics catalogue,
      benchmarks, architecture, ADRs, contributing
- [ ] README: what it is, 30-second compose start, links
- [ ] `ARCHITECTURE.md` mirroring plan section 1, checked for currency in review
- [ ] Docs build wired into CI so a broken link fails the build

## Technical Decisions
- The configuration reference is generated from the schema, never hand-maintained - a config doc
  that drifts is worse than none.
- The migration guide states what does not carry over (nginx cache directory, LANCache Manager's
  directory browsing) as prominently as what does.

## Dependencies
- Requires: TASK-07, TASK-23, TASK-24, TASK-25, TASK-06
- Blocks: v1.0

## Completion Checklist
- [ ] Site builds in CI with no broken links
- [ ] Config reference matches the binary's actual flags
- [ ] Migration guide walked through by someone running monolithic
- [ ] ARCHITECTURE.md matches the code
