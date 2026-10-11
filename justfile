set shell := ["bash", "-euo", "pipefail", "-c"]

msrv := `sed -n 's/^rust-version = "\([^"]*\)"/\1/p' Cargo.toml | head -1`

default:
    @just --list

# Run the explorer on PATH
run PATH=".":
    cargo run --bin grav -- {{PATH}}

build:
    cargo build --locked

release:
    cargo build --release --locked

test:
    cargo nextest run --workspace --all-features --locked
    cargo test --workspace --all-features --locked --doc

fmt:
    cargo fmt --all

lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
    cargo clippy --workspace --all-targets --no-default-features --locked -- -D warnings

doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked

msrv:
    cargo +{{msrv}} check --workspace --all-targets --all-features --locked

# Review CLI output snapshots after an intended change
snapshots:
    cargo insta test --test snapshots --review

# Fuzz TARGET (snapshot or units) for SECONDS; needs nightly and cargo-fuzz
fuzz TARGET="snapshot" SECONDS="60":
    mkdir -p fuzz/corpus/{{TARGET}} fuzz/seeds/{{TARGET}}
    cd fuzz && cargo +nightly fuzz run {{TARGET}} corpus/{{TARGET}} seeds/{{TARGET}} -- -max_total_time={{SECONDS}}

supply-chain:
    cargo deny check
    cargo machete

coverage:
    cargo llvm-cov nextest --workspace --all-features --html

semver:
    cargo semver-checks --workspace

# Unix PTY smoke test of terminal setup and restoration
smoke: release
    python3 scripts/terminal-smoke.py target/release/grav

# Everything CI runs, locally
ci: lint test doc msrv supply-chain smoke

# Check ncdu dump compatibility against ncdu itself (needs ncdu and jq)
ncdu-interop: build
    scripts/ncdu-interop.sh target/debug/grav
