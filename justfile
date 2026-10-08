# justfile for gravityfile - File system analyzer with TUI

set shell := ["bash", "-uc"]

# Default recipe - show available commands
default:
    @just --list

# ─────────────────────────────────────────────────────────────────────────────
# Development
# ─────────────────────────────────────────────────────────────────────────────

# Build the project in debug mode
build:
    cargo build --locked

# Build the project in release mode with optimizations
release:
    cargo build --release --locked

# Run the TUI with a path (default: current directory)
run PATH=".":
    cargo run -- {{PATH}}

# Run the release build
run-release PATH=".":
    ./target/release/gravityfile {{PATH}}

# Run all tests
test:
    cargo test --workspace --all-features --locked

# Run tests with output shown
test-verbose:
    cargo test --workspace --all-features --locked -- --nocapture

# Check code without building
check:
    cargo check --workspace --all-features --locked

# Run clippy for linting
lint:
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# Format code with rustfmt
fmt:
    cargo fmt --all

# Check if code is formatted
fmt-check:
    cargo fmt --all -- --check

# Clean build artifacts
clean:
    cargo clean

# ─────────────────────────────────────────────────────────────────────────────
# Installation
# ─────────────────────────────────────────────────────────────────────────────

# Install the binaries to ~/.cargo/bin
install:
    @echo "Installing gravityfile..."
    cargo install --path . --force
    @echo "✅ Installation complete! Run 'gravityfile --help' or 'grav --help' to get started."

# Uninstall the binaries
uninstall:
    cargo uninstall gravityfile

# ─────────────────────────────────────────────────────────────────────────────
# CI/CD
# ─────────────────────────────────────────────────────────────────────────────

# Full CI check: format, lint, test, build
ci: fmt-check lint test release
    @echo "✅ All CI checks passed!"

# Package all workspace crates (dry-run)
package:
    cargo package --workspace --allow-dirty --no-verify --locked --offline

# Build and test the extracted package contents with local internal patches.
verify-packages: package
    python3 scripts/verify-packaged-workspace.py

# Publish all crates to crates.io (in dependency order)
# Requires: cargo install cargo-workspaces
publish:
    cargo workspaces publish --publish-as-is --no-remove-dev-deps --publish-interval 30 --allow-branch main --locked

# Dry-run publish (verify all crates without uploading)
# Requires: cargo install cargo-workspaces
publish-dry-run:
    cargo workspaces publish --dry-run --publish-as-is --no-remove-dev-deps --allow-dirty --allow-branch main --locked

# List files that would be included in each crate's package
publish-list:
    @echo "Files to be published in each crate:"
    @for crate in gravityfile-core gravityfile-scan gravityfile-analyze gravityfile-ops gravityfile-plugin gravityfile-tui gravityfile; do \
        echo "\n=== $crate ===" && cargo package -p $crate --list 2>/dev/null || echo "(requires deps published first)"; \
    done

# Install cargo-workspaces for publishing
install-cargo-workspaces:
    cargo install cargo-workspaces

# ─────────────────────────────────────────────────────────────────────────────
# Release Management
# ─────────────────────────────────────────────────────────────────────────────

# Full local release gate.
# Requires the declared MSRV toolchain plus cargo-audit, cargo-deny, and cargo-machete.
release-check: fmt-check
    python3 -m unittest discover -s scripts -p "test_*.py"
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
    cargo clippy -p gravityfile-scan --all-targets --no-default-features --locked -- -D warnings
    cargo test --workspace --all-features --locked
    cargo +1.90 check --workspace --all-targets --all-features --locked
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked
    cargo audit --no-fetch --deny unsound
    cargo deny check bans licenses sources
    cargo machete
    cargo package --workspace --allow-dirty --no-verify --locked --offline
    python3 scripts/verify-packaged-workspace.py
    cargo build --release --locked
    python3 scripts/smoke-release.py
    python3 scripts/qualification/terminal-smoke.py target/release/grav
    python3 scripts/qualification/terminal-smoke.py target/release/gravityfile

# Show current version
version:
    @grep '^version' Cargo.toml | head -1 | cut -d'"' -f2

# Tag and push a release (usage: just tag v0.3.1)
tag VERSION:
    @test "{{VERSION}}" = "v$(awk -F '\"' '/^version = / { print $2; exit }' Cargo.toml)" || (echo "Tag must match workspace version" >&2; exit 1)
    @test -z "$(git status --porcelain)" || (echo "Working tree must be clean before tagging" >&2; exit 1)
    @echo "Creating tag {{VERSION}}..."
    git tag {{VERSION}}
    git push origin {{VERSION}}
    @echo "✅ Tag {{VERSION}} pushed! GitHub Actions will build the release."

# Create a release commit and tag
release-tag VERSION MESSAGE="Release":
    @test "{{VERSION}}" = "v$(awk -F '\"' '/^version = / { print $2; exit }' Cargo.toml)" || (echo "Tag must match workspace version" >&2; exit 1)
    just release-check
    git add -A
    git commit -m "{{MESSAGE}} {{VERSION}}"
    git tag {{VERSION}}
    git push origin main --tags
    @echo "✅ Released {{VERSION}}!"

# ─────────────────────────────────────────────────────────────────────────────
# Cross-compilation
# ─────────────────────────────────────────────────────────────────────────────

# Build for all supported targets (requires cross)
build-all:
    @echo "Building for all targets..."
    cargo build --release --target x86_64-unknown-linux-gnu
    cargo build --release --target x86_64-apple-darwin
    cargo build --release --target aarch64-apple-darwin
    @echo "Note: Use 'cross' for aarch64-unknown-linux-gnu and Windows targets"

# ─────────────────────────────────────────────────────────────────────────────
# Documentation & Info
# ─────────────────────────────────────────────────────────────────────────────

# Update dependencies
update:
    cargo update

# Show outdated dependencies (requires cargo-outdated)
outdated:
    cargo outdated -R

# Watch and rebuild on file changes (requires cargo-watch)
watch:
    cargo watch -x check -x 'test --workspace'

# Generate and open documentation
doc:
    cargo doc --workspace --open

# Show project statistics
stats:
    @echo "Lines of Rust code:"
    @find crates src -name "*.rs" -exec wc -l {} + | tail -1
    @echo "\nWorkspace crates:"
    @cargo metadata --format-version 1 | jq -r '.workspace_members[]' | cut -d' ' -f1
    @echo "\nDirect dependencies:"
    @cargo tree --workspace --depth 1

# Create a release build and show binary size
release-info: release
    @echo "Release binaries:"
    @ls -lh target/release/gravityfile target/release/grav 2>/dev/null | awk '{print $5, $9}'

# ─────────────────────────────────────────────────────────────────────────────
# CLI Commands Demo
# ─────────────────────────────────────────────────────────────────────────────

# Run disk usage scan on a path
scan PATH=".":
    cargo run -- scan {{PATH}}

# Find duplicate files
duplicates PATH=".":
    cargo run -- duplicates {{PATH}}

# Analyze file ages
age PATH=".":
    cargo run -- age {{PATH}}

# Export scan to JSON
export PATH="." OUTPUT="scan.json":
    cargo run -- export {{PATH}} -o {{OUTPUT}}
