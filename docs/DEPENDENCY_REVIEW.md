# Dependency refresh — October 1, 2026

Rechecked all 55 direct Cargo requirements, including incompatible upgrades:
all already match current stable release lines. Refreshed the lockfile:

- `luajit-src`: `210.7.3+1ee778a` → `210.7.4+c6ffc14`.
- `zerocopy` and `zerocopy-derive`: `0.8.58` → `0.8.59`.
- CI `taiki-e/install-action`: `2.87.21` → `2.87.22`, pinned to its resolved
  upstream commit. Other action pins and `cargo-about` 0.9.2 remain current.

The Rust 1.90 minimum and existing dependency policy are retained. `generic-array`
remains constrained to 0.14.7 by upstream `crypto-common`; the existing bincode
and smartstring unmaintained-dependency warnings remain. No new exception was added.

Validation passed: 812 macOS and 818 Linux tests (eight ignored on each), strict
workspace Clippy, Rust 1.90 all-target/all-feature checks, fresh advisory audit,
dependency policy, formatting and workflow lint.

[Verification results and upstream version evidence](qualification/dependency-refresh-2026-10-01.json)
record this refresh independently of earlier release qualification. Native Windows,
remote CI, signing, release packaging and publication were not rerun for this refresh.

## Previous refresh — September 26, 2026

All direct Cargo requirements were checked against current stable registry
releases, including incompatible upgrades. `jwalk` moves to 0.9, `dirs` to 7,
and Rhai to 1.26.1. The lockfile refresh changes 115 package-name entries,
including additions and removals. Scanner pruning now uses jwalk's renamed
`read_children` field. The declared Rust 1.90 minimum is retained and tested.

GitHub Actions were checked against upstream stable tags and pinned to their
resolved commits. Checkout moves to 7.0.1 and setup-python to 7.0.0; the installer
action moves to 2.87.21. Other action pins already matched their latest stable
releases. Weekly Dependabot checks now cover Cargo as well as Actions.
The [checkout migration](https://github.com/actions/checkout/releases/tag/v7.0.0)
restricts unsafe fork checkouts in privileged events; these workflows use ordinary
push/pull-request events. The [setup-python migration](https://github.com/actions/setup-python/releases/tag/v7.0.0)
removes `pip-install`, which these workflows do not use. Remote CI remains unrun.

## Verification and remaining constraints

- macOS: 782 Rust tests passed, eight ignored, including doctests; strict workspace
  Clippy and Rust 1.90 all-target/all-feature checks passed.
- Colima Linux on Rust 1.90: 786 Rust tests passed, eight ignored.
- Optimized macOS binaries: CLI report/recovery smoke checks and five terminal
  scenarios per executable passed. Scanner tests without default features passed.
- Python tooling: 92 passed, seven skipped. Formatting, actionlint, and diff
  whitespace checks passed. Fresh advisory checking and dependency policy passed.
- `generic-array` remains 0.14.7 because upstream `crypto-common` requires that
  exact version; an attempted 0.14.9 update demonstrated the resolver conflict.
- Existing unmaintained advisory exceptions remain for bincode through syntect
  and smartstring through Rhai. No new advisory exception was added.
- Upgraded Zstandard bindings require BSD-3-Clause; `deny.toml` allows it only for
  the two exact binding versions. Distribution notice obligations still apply.
- The Linux image intentionally retains the Rust 1.90 baseline. Native Windows,
  remote CI, signing and publication were not qualified by this update.

[Machine-readable versions, hashes, logs and failed initial checks](qualification/dependency-refresh.json)
preserve the scope of this evidence. Initial builds exposed the jwalk API change;
the initial license check exposed the new binding licenses. Both were addressed
before the passing checks.

## Primary-source practices applied to the project

Dependency updates need separate resolution, compatibility, behavior and policy
checks. Cargo's [resolver documentation](https://doc.rust-lang.org/cargo/reference/resolver.html)
explains that version constraints and Rust compatibility affect selection; a
lockfile refresh alone cannot establish runtime correctness. Keep CI on `--locked`
and retain a tested minimum-toolchain lane. Scoped license allowances follow
[cargo-deny's documented exception mechanism](https://embarkstudios.github.io/cargo-deny/checks/licenses/cfg.html).

For UI responsiveness, keep expensive operations outside event handling and
bound worker concurrency. [Tokio's blocking-task documentation](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)
states that started blocking tasks cannot be aborted; cancellation must be
cooperative or use an appropriate process boundary. The duplicate-mark
optimization now avoids cloning a whole duplicate group when toggling one file;
its regression passes in this refresh. Its [synthetic benchmark](benchmarks/macos-duplicate-mark-1m.json)
predates the dependency update and is not an end-to-end latency guarantee.

Filesystem safety remains a higher-priority enterprise gap than further UI
micro-optimization. Linux [openat2](https://man7.org/linux/man-pages/man2/openat2.2.html)
can restrict resolution relative to a directory descriptor across path components.
The project's leaf checks do not yet close ancestor substitution races. Separately,
[fsync documentation](https://man7.org/linux/man-pages/man2/fsync.2.html) distinguishes
file persistence from directory-entry persistence. Recovery qualification must
cover operation ordering and crash points, including each platform's behavior;
successful ordinary tests do not establish power-loss durability.

## Current release notice generation — October 7, 2026

[All five target notices](qualification/release-notices-2026-10-07.json) were
regenerated with cargo-about 0.9.2 against the current lockfile, manifest set and
configuration. Schema-2 provenance and body checksums passed; a local unsigned
macOS archive containing current binaries and notices passed contents validation.
Generation used pinned upstream texts after a network-restricted initial attempt
failed. The four existing canonical-MIT source-text exceptions remain explicit;
native vendored/SDK attribution and legal review remain open. Target notice
generation does not establish native compilation, execution or distribution
qualification for those targets.
