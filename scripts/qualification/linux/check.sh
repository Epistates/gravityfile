#!/bin/sh
set -eu
rustc --version
cargo --version
uname -a
bash --version
zsh --version
fish --version
GRAVITYFILE_REQUIRE_SHELLS=1 python3 -m unittest discover -s scripts -p test_shell_integration.py
cargo test --workspace --all-features --locked --offline
cargo build --release --locked --offline
python3 scripts/smoke-release.py --binary-dir "$CARGO_TARGET_DIR/release"
python3 scripts/qualification/terminal-smoke.py "$CARGO_TARGET_DIR/release/grav"
python3 scripts/qualification/terminal-smoke.py "$CARGO_TARGET_DIR/release/gravityfile"
