# 0006. Repository hosting and CI topology

- **Status**: Accepted
- **Date**: 2026-09-01
- **Context**: PRD section 12 (open question), plan section 6

## Decision

GitHub is canonical: `github.com/leftathome/cachic`. Every CI step is a `just` recipe, so a GitLab
mirror is a translation of the pipeline definition rather than a rewrite of the pipeline.

## Consequences

M0 ships five jobs - fmt/clippy, nextest, cargo-deny, typos, rustdoc - each calling the same recipe
a contributor calls locally. A green pipeline therefore means a working local loop, which is the
property that matters for the "contributor-friendly codebase" goal (G5).

Deferred deliberately: multi-arch image builds, signing, SBOM and chart publishing all belong to
TASK-26, and nightly fuzz and chaos jobs to TASK-20 and TASK-21. M0's CI only needs to prove the
gate works.

Supply-chain policy is settled here because it came up immediately:

- Permissive licences only. BSL-1.0 (xxhash-rust) and CDLA-Permissive-2.0 (webpki-roots, which
  covers Mozilla's root certificate *data* rather than code) were added to the allow list on
  review; both are genuinely permissive.
- `RUSTSEC-2024-0436` (`paste`, unmaintained) is ignored with a written reason. It is a proc-macro
  with no runtime code, reaching us only through `foyer-memory`, so it cannot be dropped
  independently of foyer. If ADR 0003 resolves away from foyer, this ignore should go with it.

## What would overturn this

A decision to host canonically on the homelab GitLab. The `just`-recipe discipline is what keeps
that a day's work rather than a rewrite, and it should be preserved for that reason alone.
