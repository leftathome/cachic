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
# cargo-auditable embeds the exact dependency graph into the binary. Without it the published SBOM
# describes the Debian base layer and nothing else - rc3's listed twelve packages, none of them a
# Rust crate, so scanning it said nothing about the 232 crates that actually make up cachic.
RUN cargo install cargo-chef cargo-auditable --locked
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
# `cargo auditable` writes the dependency list into a .dep-v0 section, which syft, trivy and
# cargo-audit all read. --keep-section pins that against a binutils that would discard a
# non-allocated section, which would silently empty the SBOM again.
RUN cargo auditable build --release --bin cachic \
 && cp target/release/cachic /build/cachic \
 && strip --keep-section=.dep-v0 /build/cachic \
 && objdump -h /build/cachic | grep -q '\.dep-v0' \
    || { echo "the dependency section did not survive the build; the SBOM would be empty"; exit 1; }

# --- runtime ----------------------------------------------------------------------------------
FROM gcr.io/distroless/cc-debian12:nonroot AS runtime

# Static identity. Version, revision and build time come from the release workflow, because a
# Dockerfile cannot know them and an image that cannot be traced to a commit is not much of a
# supply-chain artefact.
ARG VERSION=dev
ARG REVISION=unknown
ARG CREATED=""
LABEL org.opencontainers.image.title="cachic" \
      org.opencontainers.image.description="HTTP caching proxy for game-distribution and OS-update CDN traffic; a drop-in replacement for the nginx engine in lancachenet/monolithic" \
      org.opencontainers.image.source="https://github.com/leftathome/cachic" \
      org.opencontainers.image.url="https://github.com/leftathome/cachic" \
      org.opencontainers.image.documentation="https://leftathome.github.io/cachic/" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.vendor="leftathome" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}" \
      org.opencontainers.image.created="${CREATED}"

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
