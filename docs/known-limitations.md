# Known limitations

Things that are true today, why, and what would change them. Each is linked from wherever someone
would trip over it rather than only living here.

## No static musl binaries

**What.** Release binaries are dynamically linked against glibc, and the container image uses
`gcr.io/distroless/cc-debian12` rather than `distroless/static`. There is no `musl` target in the
release pipeline.

**Why.** `foyer` 0.22 does not compile for musl. `foyer-storage`'s device-capacity helper guards
its macOS branch with `cfg!(target_os = "macos")` — a *runtime* boolean — instead of
`#[cfg(target_os = "macos")]`. The Darwin `ioctl` call is therefore compiled on every unix target.
It typechecks against glibc, where `libc::Ioctl` is `u64`, and fails against musl, where the
request argument is `c_int`:

```
error[E0308]: mismatched types
  --> foyer-storage/src/io/device/utils.rs:50
   |
50 |  let res = unsafe { libc::ioctl(fd, DKIOCGETBLOCKCOUNT, &mut block_count) };
   |                     -----------     ^^^^^^^^^^^^^^^^^^ expected `i32`, found `u64`
```

**Consequences.**

- The image is ~13 MB compressed rather than ~3 MB. Still well inside NFR-8's 40 MB.
- The binary needs a glibc host. Alpine and other musl-based distributions cannot run it directly.
- FR-73's "static binaries for Linux" is unmet in the *static* sense.

**What would change it.** A one-line upstream fix — `cfg!` to `#[cfg]`. When it lands, add
`x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` back to the release matrix and switch
the runtime image to `distroless/static`. Both changes are already commented in the Dockerfile and
`.github/workflows/release.yml`.

**Workaround if you need musl today.** Run the container image, which carries its own libc.

## The release binaries need glibc 2.36 or newer

**What.** The `*-unknown-linux-gnu` tarballs run on Debian 12, Ubuntu 22.10 and anything newer.
They will not start on a host with an older glibc:

```
./cachic: /lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.38' not found
```

**Why.** A dynamically linked binary requires whatever glibc symbol versions it was linked
against. 0.1.0-rc1 was built on `ubuntu-latest` (glibc 2.39) and so demanded `GLIBC_2.38`, which
does not exist on Debian 12 (2.36) or Ubuntu 22.04 (2.35) — both ordinary homelab hosts, and both
still in support. The binary was unusable there and nothing said so.

**What changed.** Release binaries now build inside `rust:1.98-bookworm`, the same Debian 12 base
the container image uses, so the tarball and the image agree on their floor. The release job then
reads the highest `GLIBC_x.y` symbol out of the built binary and fails if it exceeds 2.36, because
a floor that is merely written down is a floor nobody notices breaking.

**Consequences.** Ubuntu 22.04 (glibc 2.35) is still below the floor.

**Workaround.** Run the container image, which carries its own libc, or build from source on the
host. Lowering the floor further means building on an older base than the image itself uses, which
would make the two artefacts disagree; see [No static musl binaries](#no-static-musl-binaries) for
why the static option is closed today.

## No macOS binaries

**What.** The release pipeline builds Linux amd64 and arm64 only.

**Why.** `foyer`'s `FsDeviceBuilder::with_direct` is `#[cfg(target_os = "linux")]`, since O_DIRECT
does not exist on macOS. cachic now compiles for macOS — the call is gated and buffered IO is used
— but shipping a binary implies a support commitment nothing else here backs. The PRD lists macOS
as a development platform, not a deployment target.

`cargo build` on macOS works, which is what a contributor needs.

## Stale-on-error is transient-only

`STALE_ON_ERROR` serves cached slices through an upstream 5xx, timeout or connection failure. It
does **not** serve them through a 404: a 404 means the object is gone, and serving a cached
remnant of a deleted object is staleness rather than resilience.

It also cannot serve a slice that was never cached. A request spanning cached and uncached regions
still fails during an outage, because inventing the missing bytes is worse than failing.

## Per-service limits apply to configured services only

`max_inflight` in the rules file bounds a named service. A service with no entry is bounded only
by `UPSTREAM_MAX_INFLIGHT`. That is deliberate — most of `cache-domains` needs no per-service
ceiling — but it means one unconfigured service can still consume the global budget.

## No import from an existing nginx cache

Migrating from `lancachenet/monolithic` starts with a cold cache. There is no import and none is
planned for 1.0; see the [migration guide](./migration-from-lancache.md).

## Not verified against real hardware

The parity benchmark against monolithic, a Kubernetes install, prefill runs and the 7-day soak
have not been carried out. See the [definition of done](./definition-of-done.md) and the
[release-candidate test plan](./rc-test-plan.md).
