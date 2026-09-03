# syntax=docker/dockerfile:1

# cachic container image.
#
# Distroless base running as non-root: a filesystem with one executable, a CA bundle and the C
# runtime, and nothing else. No shell, no package manager, nothing for an attacker who reaches
# the container to pivot with.
#
# ## Why glibc rather than static musl
#
# The implementation plan specifies a static musl binary on distroless/static. foyer 0.22.4 does
# not compile for musl: `foyer-storage`'s device-capacity helper guards its macOS branch with
# `cfg!(target_os = "macos")` - a runtime boolean - rather than `#[cfg(...)]`, so the Darwin
# `ioctl` call is compiled on every unix target. It typechecks against glibc, where `libc::Ioctl`
# is `u64`, and fails against musl, where the request argument is `c_int`.
#
# The upstream fix is one line (`cfg!` to `#[cfg]`). Until it lands we build against glibc and use
# distroless/cc, which carries libc and libgcc. This also blocks the static musl release binaries
# in FR-73; see TASK-26.
#
# cargo-chef splits dependency compilation from application compilation, so editing our own source
# does not recompile the entire dependency graph. That matters because a release build with LTO
# is slow.

ARG RUST_VERSION=1.98

# --- plan -------------------------------------------------------------------------------------
FROM rust:${RUST_VERSION}-bookworm AS chef
RUN cargo install cargo-chef --locked
WORKDIR /build

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# --- build ------------------------------------------------------------------------------------
FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
# No --target: the release pipeline builds each architecture on a native runner, so the host is
# the target. Naming an explicit triple meant the arm64 runner tried to cross-compile for x86_64
# without that toolchain installed, which is how this was found.
# Dependencies only: this layer is cached until Cargo.lock changes.
RUN cargo chef cook --release --bin cachic --recipe-path recipe.json
COPY . .
RUN cargo build --release --bin cachic \
 && cp target/release/cachic /build/cachic \
 && strip /build/cachic

# --- runtime ----------------------------------------------------------------------------------
FROM gcr.io/distroless/cc-debian12:nonroot AS runtime

LABEL org.opencontainers.image.title="cachic" \
      org.opencontainers.image.description="HTTP caching proxy for game and OS update CDNs" \
      org.opencontainers.image.source="https://github.com/leftathome/cachic" \
      org.opencontainers.image.licenses="Apache-2.0"

COPY --from=builder /build/cachic /usr/local/bin/cachic

# distroless's nonroot user. Ports 80 and 443 need CAP_NET_BIND_SERVICE, which the compose and
# Helm examples grant; the defaults below are the unprivileged equivalents for a bare run.
USER nonroot:nonroot

# The data volume. Declared so an operator who forgets to mount one does not silently cache into
# the container's writable layer and lose everything on replacement.
VOLUME ["/data/cache"]
ENV CACHE_DATA_DIR=/data/cache

EXPOSE 80 443 9090

ENTRYPOINT ["/usr/local/bin/cachic"]
