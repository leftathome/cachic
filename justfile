# cachic dev loop. CI calls these same recipes (TASK-02).
#
# Under WSL2, set CARGO_TARGET_DIR to a native Linux path before using these; building on a
# /mnt/c DrvFs path is several times slower. See README.

default:
    @just --list

fmt:
    cargo fmt --all

lint:
    cargo fmt --all -- --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo deny check
    typos

test:
    cargo nextest run --all-features

# Fall back for environments without cargo-nextest installed.
test-std:
    cargo test --all-features

bench:
    cargo bench

# Soak test. Verifies every read against the generator; NFR-7 allows zero corrupt bytes.
#   just soak 3600
soak SECONDS="60":
    cargo run --release --example soak -- --seconds {{SECONDS}}

# Build the documentation site.
book:
    mdbook build
    python3 scripts/check-book-links.py

# Serve the documentation site locally.
book-serve:
    mdbook serve --open

# Regenerate the configuration reference from the clap definitions (TASK-07).
config-reference:
    cargo run --quiet --example config-reference > docs/configuration.md

# Fuzz one target for a while. Needs nightly: cargo-fuzz uses libFuzzer.
#   just fuzz range_header 300
fuzz TARGET SECONDS="60":
    cargo +nightly fuzz run {{TARGET}} -- -max_total_time={{SECONDS}}

# Performance gate. Release only: debug builds measure ~20% low and the thresholds assume
# optimised code. Floor is a hard failure, target is a loud warning.
# Override per host with CACHIC_PERF_FLOOR_GBPS / CACHIC_PERF_TARGET_GBPS.
perf:
    cargo nextest run --release --test perf_gate --no-capture

# M0 spike: prototype proxy over mockcdn (TASK-03).
spike *ARGS:
    cargo run --release --bin spike -- {{ARGS}}

# M0 measurements (TASK-04).
measure *ARGS:
    cargo run --release --example measure -- {{ARGS}}

# foyer ingest-rate probe (TASK-04 follow-up).
foyerprobe:
    cargo run --release --example foyerprobe

image:
    docker build -t cachic:dev -f Dockerfile .

# Run the built image and check it answers. Podman's CNI networking is unreliable under WSL2,
# hence --network=host.
image-smoke: image
    docker run --rm -d --name cachic-smoke --network=host -e HTTP_PORT=18081 -e ADMIN_PORT=19091 -e CACHE_DATA_DIR=/tmp/cachic -e CACHE_DISK_SIZE=1g -e CACHE_MEM_SIZE=64m cachic:dev
    sleep 8
    curl -sf http://127.0.0.1:19091/readyz
    docker rm -f cachic-smoke

chart:
    helm lint charts/cachic
    helm template cachic charts/cachic > /dev/null
    helm template cachic charts/cachic --set admin.token=x --set metrics.serviceMonitor.enabled=true --set serviceRules.steam.keep_query=true > /dev/null

check: lint test perf
