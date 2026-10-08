# gravityfile

[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Crates.io](https://img.shields.io/crates/v/gravityfile.svg)](https://crates.io/crates/gravityfile)

A Rust filesystem explorer and analyzer for inspecting disk usage, finding
duplicates, reviewing file ages, and performing explicit file operations.
Use the terminal interface interactively or the CLI's structured reports in
automation. Both `grav` and `gravityfile` run the same implementation.

<img src="assets/video.gif" alt="gravityfile terminal interface" style="width: 100%; max-width: 100%;"/>

## Start exploring

Build the current checkout with Rust 1.90 or newer:

```sh
cargo install --path . --locked
```

A published version can be installed with `cargo install gravityfile --locked`.
Published packages can differ from this checkout; the evidence linked below
identifies the exact source and binaries tested.

```sh
grav .                 # Open the explorer
grav --scan .          # Request a full recursive scan at startup
grav scan . --top 10   # Print a disk-usage summary
```

Startup scanning follows saved settings unless `--scan` explicitly requests it.
Tree, Miller-column, and treemap layouts share the scanned data. Duplicate and
age views provide additional ways to inspect it. Git annotations identify
modified, staged, and untracked files.

Text previews include syntax highlighting and hex views; archives have bounded
listings. Unix image previews query terminal capabilities with a bounded startup
read. Failed detection uses the ordinary binary preview; non-Unix hosts use
half-block image previews. See [preview and terminal limits](docs/RELIABILITY.md).

## Reports and automation

```sh
grav duplicates . --min-size 1MB --format json --envelope --strict
grav age . --stale 1y --format json --envelope --strict
grav export . --output scan.json --envelope --strict --max-entries 100000
```

Versioned envelopes carry report kind, scope, completeness status, and data.
`--strict` returns failure for incomplete results; consumers should check both
exit status and report status. `--max-entries` limits descendant entries and
fails if exhausted. Its default is unlimited, and it is not a total RAM or
filesystem-latency bound. Scans observe a changing filesystem; they are not
snapshots or authoritative deletion plans.

The [report contract](docs/REPORTS.md) defines native-path encoding, legacy
compatibility, cancellation, and partial-result behavior. Use `grav <command>
--help` for command-specific options.

## Files, recovery, and plugins

Copy/move operations use private staging and explicit conflict handling.
Deletion uses the system Trash. Undo has operation-specific limits; incomplete
undo is retained for review rather than automatically replayed.

```sh
grav recover . --json
```

Recovery inspection does not itself restore or delete files. Copy undo retains
destination contents in sibling recovery journals, including later edits; it
does not reclaim space. In the TUI, `:undo-review` inspects an incomplete undo,
`:undo-review 2` selects the second page, and `:undo-export review.json` exports
the retained record to a new file. Read the [recovery guide](docs/RECOVERY.md)
before restoring interrupted work. Bulk rename and archive publication are not
crash-atomic multi-file transactions.

Lua and Rhai plugins support lifecycle and action hooks through supervised
worker processes with deadlines and bounded IPC. They do not have OS-enforced
filesystem/network confinement or process memory limits. See the
[plugin API](docs/PLUGINS.md) and [execution boundary](docs/PLUGIN_PROCESSES.md).

## Navigation and shell integration

| Key | Action |
| --- | --- |
| `j` / `k` | Move down / up |
| `h` / `l` | Collapse / expand |
| `Enter` / `Backspace` | Enter directory / navigate back |
| `v` | Cycle Tree, Miller, and treemap layouts |
| `Tab` | Switch explorer, duplicates, age, and errors views |
| `Space` / `V` | Mark item / range selection |
| `y` / `x` / `p` | Copy / cut / paste |
| `d` / `D` | Delete through Trash |
| `Ctrl-z` | Undo |
| `/` / `:` | Search / command palette |
| `P` | Cycle preview mode |
| `?` / `q` | Help / quit |

To change your shell's directory when the explorer exits successfully, source
[scripts/grav.bash](scripts/grav.bash) or [scripts/grav.zsh](scripts/grav.zsh).
For Fish, install [scripts/grav.fish](scripts/grav.fish) in your functions
directory. These wrappers preserve complete Unix directory names, including
trailing newlines. [Shell qualification](docs/qualification/shell-matrix.json)
covers Bash, Zsh, and Fish on Linux with ordinary and non-Unicode filenames.

Text, Markdown editor mode, and bulk rename share editor settings and
`VISUAL`/`EDITOR` fallback. Executable names and configured arguments are passed
literally; shell command strings are not evaluated. GUI editors used for bulk
rename must wait for editing to finish. See [editor resolution](docs/RELIABILITY.md#editor-resolution).

## Qualification and development

The project has local macOS and Colima/Linux test, packaging, terminal, and
benchmark evidence. It is not fully enterprise-qualified: native Windows,
durable operation auditing, OS plugin confinement, complete multi-file crash
recovery, and ten-million-file filesystem/interactive qualification remain open.
Some tree operations remain synchronous, and total process memory is not capped.

[Project state](docs/PROJECT_STATE.md) links current results and the ordered
[enterprise delivery milestones](docs/PROJECT_STATE.md#enterprise-delivery-milestones).
[Benchmarks](docs/BENCHMARKS.md) distinguish real filesystem
workloads from in-memory measurements; results are not performance-leadership
guarantees. [Architecture](docs/ARCHITECTURE.md) describes the six library crates.

```sh
cargo test --workspace --all-features --locked
just release-check
```

The full local Unix gate also requires Rust 1.90, Python 3, `just`, `cargo-audit`,
`cargo-deny`, and `cargo-machete`. Its advisory check uses the existing cache;
refresh it explicitly when current advisory evidence is required. See
[release procedures](docs/RELEASING.md) and the
[Colima qualification recipe](scripts/qualification/linux/README.md).
Remote CI definitions do not prove that a remote run or publication succeeded.

## License

Licensed under either [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
