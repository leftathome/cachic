# TASK-07: Configuration surface

## Context
Milestone: M1 | Requirements: FR-10, FR-60, ADR 0005

12-factor config is a goal in its own right (G3), and the store config guard (FR-10) has to exist
before the store writes its first byte, or early cache directories become unopenable.

## Implementation Plan
### Phase 1: Schema
- [ ] `clap` derive with `env` so each setting is defined once
- [ ] Size parsing with units (`64GiB`, `2t`, `1m`), duration parsing (`3560d`)
- [ ] Reuse monolithic's env names where the meaning matches; document every divergence
- [ ] Precedence: env > file > defaults
- [ ] Store write-path tuning: flusher count and flush buffer pool size. foyer's defaults drop
      10% of a 10 Gbit fill; these must be settable and must default well above foyer's (TASK-11)

### Phase 2: Rules file
- [ ] Optional TOML for per-service rules
- [ ] `serde` model with clear errors that name the offending key and line

### Phase 3: Validation and guard
- [ ] Validate at startup, not first use (bad size, unwritable dir, contradictory limits)
- [ ] Write `slice_size` + store format version to `CACHE_DATA_DIR/CONFIG`
- [ ] Abort on mismatch unless `FORCE_CONFIG=true`

### Phase 4: Reference
- [ ] Generate the config reference from the schema for the docs site

## Technical Decisions
- Env is the primary surface; the file exists for per-service rules that do not fit env vars.
- The config guard is a safety feature, not a nuisance. Do not add a "just warn" mode.

## Dependencies
- Requires: TASK-01, ADR 0005
- Blocks: TASK-11 (store), TASK-08 (services)

## Completion Checklist
- [ ] Unit tests for unit parsing and precedence
- [ ] Guard tested: mismatched slice_size aborts, `FORCE_CONFIG` overrides
- [ ] Generated reference matches the code
