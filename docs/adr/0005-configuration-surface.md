# 0005. Configuration surface and lancache compatibility

- **Status**: Accepted
- **Date**: 2026-09-01
- **Context**: plan section 1.3, FR-10, FR-60

## Decision

- Environment variables are the primary surface (12-factor, G3), with an optional TOML file for
  per-service rules. Precedence: env > file > defaults.
- Reuse monolithic's variable names wherever the meaning matches; document every divergence in the
  migration guide.
- Settings are named in cache terms, not nginx terms: `CACHE_MEM_SIZE`, `CACHE_DISK_SIZE`,
  `CACHE_DATA_DIR`, `CACHE_MAX_AGE`, `SLICE_SIZE`, `READAHEAD_SLICES`, `UPSTREAM_DNS`. Removing
  `keys_zone` sizing and loader parameters from the operator's vocabulary is a stated product goal,
  not a cosmetic choice.
- Sizes parse with units (`64GiB`, `2t`); durations likewise (`3560d`).
- Validation happens at startup, not first use.
- `slice_size` and the store format version are written to `CACHE_DATA_DIR/CONFIG`. A mismatch
  aborts startup unless `FORCE_CONFIG=true`. This guard is a feature; there is no "just warn" mode.
- The configuration reference in the docs is generated from the schema, never hand-maintained.

## Consequences

One definition per setting via `clap`'s derive with `env`, which is what makes generation possible.

`READAHEAD_SLICES` is not a lancache concept and has no counterpart to inherit. M0 makes its
meaning concrete and worth documenting plainly: per-connection memory is
`READAHEAD_SLICES * SLICE_SIZE`, which is the arithmetic an operator needs to size a box.

The index-memory finding (ADR 0003) adds an obligation here: `CACHE_MEM_SIZE` guidance must state
the per-entry index cost, as lancache's docs do for nginx, because our figure is about three times
theirs.

## What would overturn this

Migration feedback that reusing a monolithic variable name for a subtly different meaning is
causing silent misconfiguration. In that case, rename and document rather than preserving a
misleading compatibility.
