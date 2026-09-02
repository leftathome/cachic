# 0009. Performance floor standard

- **Status**: Accepted
- **Date**: 2026-09-01
- **Context**: G2, NFR-1; owner's standard

## Context

The project replaces a working piece of software. `lancachenet/monolithic` is not slow, and an
operator's reason to switch has to survive the question "is it at least as fast?". A replacement
that is easier to operate but measurably slower is not a replacement.

## Decision

**The floor standard is: as good and fast as nginx, but easier to configure and operate. The ideal
is to be provably faster.**

Enforced mechanically rather than remembered, by `crates/cachic/tests/perf_gate.rs`:

- **Floor - hard build failure.** Below this the project has lost its reason to exist, so it stops
  the build rather than filing a regression.
- **Target - loud warning.** Above the floor but below what the hardware should reach. Something
  regressed; it is not yet fatal.

Current development hardware cannot demonstrate much beyond 2.5 Gbit/s, so that is the target.

## Consequences

Three design choices, each for a reason:

**Best of N rounds, not the mean.** Between-run variance on the development host is about 34%
(2.74-3.68 Gbps) while within-run variance is under 2%. That spread is contention from other work
on the machine. Throughput noise is one-sided - interference only ever makes you slower - so the
best round is the closest estimate of the machine's actual capability. A mean would encode whatever
else was running and make the gate flaky, and a flaky gate gets deleted.

**Release builds only.** Debug measures 2.16 Gbps against release's 2.57 on the same box. A gate
enforced against unoptimised code would warn on every single run, and a warning that always fires
is not a warning. In debug the test measures and reports without enforcing, and points at
`just perf`.

**Thresholds are environment-overridable.** Shared CI runners are slower and noisier than a
benchmark host. CI sets a deliberately low backstop that only catches catastrophic regressions; a
dedicated host enforces the real numbers. The override exists so that nobody is ever tempted to
delete the assertion to get a build through - the failure message says so explicitly.

**The floor number is currently provisional.** The floor *should* be nginx's throughput on the same
hardware in the same run. That comparison is TASK-25, which runs monolithic against the same data
volume in alternating runs. Until then the constant is a backstop chosen to sit below the noise
band. TASK-25 carries an explicit step to replace it, and this ADR is not fully honoured until it
does.

The gate measures warm cache-hit throughput, which is the number the floor standard is about:
what a LAN client sees pulling cached content. Upstream fill rate is a separate path with its own
much lower bar (200 Mbit/s; see `docs/benchmarks/m0/README.md`).

## What would overturn this

Nothing overturns the standard. The specific numbers should move: the target rises when better
hardware is available, and the floor becomes the measured nginx figure at TASK-25. If cachic turns
out to be reliably faster than nginx, the floor should be raised to cachic's own prior figure, so
the gate protects the gain rather than the original bar.
