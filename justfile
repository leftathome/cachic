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

test:
    cargo nextest run --all-features

# Fall back for environments without cargo-nextest installed.
test-std:
    cargo test --all-features

bench:
    cargo bench

# M0 spike: prototype proxy over mockcdn (TASK-03).
spike *ARGS:
    cargo run --release --bin spike -- {{ARGS}}

# M0 measurements (TASK-04).
measure *ARGS:
    cargo run --release --bin measure -- {{ARGS}}

image:
    docker build -t cachic:dev .

chart:
    helm lint charts/cachic

check: lint test
