# Machine-readable reports

`grav scan|duplicates|age|export PATH --envelope` emits a versioned JSON document:

```json
{
  "schema_version": 1,
  "tool_version": "0.3.1",
  "kind": "duplicates",
  "root": "/data",
  "scope": { "root": "/data" },
  "parameters": { "min_size": 1048576, "max_groups": 20 },
  "status": {
    "state": "complete",
    "scan_warning_count": 0,
    "issue_count": 0,
    "issues": [],
    "issues_omitted": 0
  },
  "data": {}
}
```

The example abbreviates `scope`, `parameters`, and `data`. Scope contains the
effective scan configuration; parameters contains the effective analysis
configuration (including thresholds and display limits), or null for a scan. Data contains the scan tree, duplicate report, or age report.
Both `scan` and `export` use kind `scan`. `--envelope` implies JSON output.
Without this option the existing text/raw JSON shape is preserved, with an
additive `status` field on duplicate and age reports.

Consumers should require `schema_version == 1`, check `kind`, ignore unknown
object fields, and inspect `status.state` before treating absence of matches as
evidence. Breaking envelope changes require a new schema version. Data is the
library's serialized report format and may gain fields; consumers must not rely
on object key order, presentation whitespace, or exact human error messages.

## Scan entry limits

All four report commands accept `--max-entries N`, for example:

```sh
grav export /data --max-entries 100000 --envelope --output report.json
```

The limit counts descendant entries, excluding the root. Zero permits an empty
scope; omitting the option leaves traversal unlimited. The first excess entry
fails the command with a nonzero exit status before analysis or report output,
even without `--strict`. Existing export files remain unchanged. Successful
envelopes record the selected limit in `scope.max_entries`.

This bounds entries consumed by the scanner, not total memory or elapsed time:
walker prefetching, variable path sizes, and Git enrichment require separate
resource controls. The option applies to report subcommands; the interactive TUI
uses its separate saved `max_scan_entries` setting. `scan --max-depth` remains a display
setting and does not limit traversal.

## Completeness

- `complete`: the configured scope was evaluated without reported scan or
  analysis failures. Filters and display limits are intentional scope choices.
- `partial`: scan warnings or failed file reads may hide results.
- `cancelled`: a signal or library caller cancelled the analysis; any returned matches are
  only observations made before cancellation.
- `unknown`: legacy serialized reports did not include completeness metadata.
  Deserializing them never silently upgrades them to complete.

Duplicate reports count all candidate failures and include details for at most
100, sorted by path. Missing files, substituted nonregular files, read errors,
and changes detected between partial/full reads make the report partial. Full
hashing uses the observed length, checks metadata and Unix file identity/change
time around reads, and refuses substituted leaf symlinks on Unix. These checks
detect ordinary concurrent changes; they do not establish filesystem snapshot
isolation or prove that a file remains unchanged after analysis.

Group paths and equal-waste groups have deterministic ordering. `max_groups`
limits displayed groups after computing totals and does not make the report
partial. Complete means complete within the scan and analysis filters, not a
claim that every file on the device was inspected.

Age distributions retain observed values when incomplete. Stale-directory
recommendations are suppressed whenever warnings, cancellation, or a depth limit
mean unseen descendants could be recent. Quick listings explicitly record depth 1
and their actual root, even if supplied a recursive configuration.

## Exit behavior

By default partial reports are emitted with diagnostics on stderr and exit
successfully. `--strict` emits the report and then exits unsuccessfully if it is
incomplete. This allows automation to keep evidence without mistaking it for a
successful audit. Fatal scan/output errors fail without promising a report.

A downstream reader closing stdout early is successful in ordinary mode and
unsuccessful with `--strict`, where delivery of the report matters. Neither
case panics. These options require a noninteractive command.

```bash
grav duplicates /data --min-size 1MiB --envelope --strict > duplicates.json
grav export /data --envelope --strict -o scan.json
```

CLI observation commands use the signal handlers described below. Library
callers can inspect cancelled analysis reports through their cancellation tokens.
Scan cancellation returns `ScanError::Interrupted` rather than a truncated
successful scan.

## Configuration validation

Scan builders and both scanning entry points reject malformed ignore patterns.
Mutating public ignore-pattern fields cannot reuse a cache compiled from older
patterns. `compile_patterns`, `effective_ignore_globset`, `build_ignore_globset`,
and `should_ignore` now return `Result`; callers must handle invalid scope.
Duplicate configuration builders and `DuplicateFinder::with_config` reject
invalid exclusion patterns and minimum sizes greater than maximum sizes.
`with_config` is fallible and requires error handling. These API changes prevent
an invalid exclusion from silently widening the audited scope.

## Interrupts and termination

For `scan`, `duplicates`, `age`, and `export`, Unix SIGINT/SIGTERM and Windows
Ctrl-C/Ctrl-Break request cancellation through a shared scan/analysis token.
Interrupted commands exit unsuccessfully even without `--strict`. Cancellation
during traversal discards the unfinished tree and returns an error without JSON.
Cancellation during duplicate or age analysis prevents new text or JSON report
output from starting. Text writes check the token, and JSON output checks it
before staging and at buffered write boundaries. Cancellation after text output
starts can leave an incomplete prefix. Library callers can still inspect returned cancelled reports.
Inspect status as well as the exit code when a report is available. A signal
arriving after computation can leave a complete report accompanied by an
unsuccessful exit status.

Export preserves the previous file when cancellation is observed during traversal
or staged serialization. It also checks cancellation immediately before atomic
replacement. A signal arriving after that check can still accompany publication
of a complete report and an unsuccessful exit status; cancellation and filesystem
replacement are not one atomic operation. Stdout copying checks cancellation
between writes and can leave a partial prefix if interrupted after publication
starts. Individual filesystem syscalls and output writes
are not preempted by the first signal. A second signal forces exit (130 for
interrupt, 143 for terminate/break), skipping cleanup; temporary export files
may remain. The graceful error exit currently uses code 1.

On Unix, file export syncs the staged file before replacement and its containing
directory after replacement. A directory-sync failure returns an error explicitly
stating that the export was already published and durability is uncertain; inspect
the destination before retrying. The new complete file remains visible, and the
previous file is not restored. On Unix, staging creation, replacement, cleanup,
and sync all use the same open parent-directory handle. Renaming that directory
or substituting its old path after opening cannot redirect these operations.
If the parent moves, the export follows the originally opened directory; the
requested pathname printed by the CLI may no longer locate it. Initial parent
resolution can still follow symlinks or racing ancestors. Writers with permission
to modify entries inside the opened directory can still interfere with staging
or replace the report; this is not an isolation boundary against such writers.
Non-Unix platforms currently sync the staged file only. This ordering follows
the distinction between file data and directory entries in the
[fsync contract](https://man7.org/linux/man-pages/man2/fsync.2.html); it does not
establish power-loss guarantees for every filesystem or storage device.

These handlers apply only to observation commands. TUI, recovery, and plugin
worker execution retain their separate lifecycle behavior. Signal listeners use
[Tokio's process-wide handling](https://docs.rs/tokio/latest/tokio/signal/unix/struct.Signal.html),
which does not restore Unix default dispositions when a listener is dropped.
`gravityfile::run` is therefore a process entry point, not an embeddable dispatcher.
Unix subprocess tests verify first/second signal behavior; Windows console
behavior still requires target-platform qualification.

## Consumer compatibility and numeric precision

Checked-in [legacy fixtures](../tests/fixtures/reports/README.md) freeze raw
scan and analysis field names and types independently of the current writer. Tests verify
that missing completeness stays `unknown`, additive fields are accepted, and
invalid types for known fields fail. These fixtures supplement the CLI envelope
integration tests. The scan fixture protects tree metadata, warning retention,
precision, and legacy defaults; it does not exhaust every node variant or provide
a complete scan-tree schema.

Byte counts remain JSON integer numbers, including values above 2^53 - 1.
Consumers must use a parser that preserves 64-bit integers; converting through
an IEEE-754 double can silently round these values. Time values preserve seconds
and nanoseconds in separate integer fields. The fixtures deliberately exercise
both large byte counts and nonzero nanoseconds.

## Age bucket configuration

Custom age buckets must have nonblank, unique names and distinct maximum ages.
At least one bucket must exist, and one must use `Duration::MAX` so every file is
represented. Input order is normalized by ascending maximum age. A boundary age
belongs to the first bucket whose maximum is greater than or equal to that age.
Both the builder and `AgeAnalyzer::with_config` reject invalid configurations;
`with_config` now returns `Result<AgeAnalyzer, String>`. Callers migrating from
the previous infallible constructor must handle that result and add a catch-all
bucket if they previously supplied only finite ranges.

Stale-directory selection retains at most `max_stale_dirs` candidates while
traversing the tree. Results are ordered by descending size, then ascending path
for ties. A zero limit retains no candidates. This bounds recommendation storage;
it does not bound the source scan tree or traversal stack.

Duplicate analysis bypasses partial filtering for size groups wholly covered by
the configured head/tail ranges. Those files use the normal full-hash path once,
with the same pre/post-read metadata and identity checks. Larger files still use
partial hashes only to select candidates for full hashing. Report hashes remain
canonical full-content BLAKE3 hashes in both cases.

## Human-readable terminal text

CLI report and progress lines display control characters as visible escapes,
including embedded newlines, escape/bell characters, and Unicode direction
controls. Ordinary Unicode text is preserved. This prevents scanned filenames
from introducing terminal controls or extra report lines through these outputs.
JSON is serialized normally; decoding it recovers the original string values.
Applications displaying decoded JSON must apply their own terminal escaping.

Explicit CLI path fields, including report roots, duplicate and stale-directory
paths, progress, export destinations and recovery listings, also escape literal
backslashes and native non-UTF-8 bytes. For example, an invalid byte appears as
`\xff`, while a literal backslash followed by `xff` appears as `\\xff`.
These labels are for display; use the JSON path representation for automation.
Argument-parser diagnostics and paths embedded inside third-party error messages
have separate formatting and are not covered by this path-display guarantee.

## Stale-directory recommendation availability

Age reports include `stale_directory_status`: `evaluated`,
`suppressed_incomplete_scan`, `suppressed_filtered_scan`, or `unknown` for legacy
reports. Warnings/cancellation suppress recommendations, as do depth limits,
hidden-file exclusion, and nonempty ignore-pattern lists. Those filters may hide
recent descendants even when the report is complete for its configured scope.
Distributions still describe observed files. CLI/TUI labels distinguish
unavailable recommendations from an evaluated empty result.

`evaluated` remains relative to observed filesystem scope, not snapshot or backup
certification. Filesystem-boundary exclusions, unfollowed symlinks, and concurrent
mutation are not proof that all reachable content is old. Consumers loading old
reports should require `evaluated` before interpreting recommendations as having
passed these scope checks.


JSON reports sent to stdout finish serialization before publishing bytes.
Small reports use an in-memory spool; above 1 MiB the spool uses temporary
storage. Encoding or staging errors return failure without a partial document
on stdout. Once copying starts, a broken pipe or process interruption can still
leave incomplete output; consumers must validate JSON and check exit status.
Large reports therefore require temporary disk space as well as their eventual
output storage. The spool threshold is not a total-report or exact RAM limit.


## Native path values

Report paths remain JSON strings when they are valid Unicode. Previously
unrepresentable non-Unicode paths now use one of these tagged objects:

```json
{"platform":"unix","bytes":[47,100,97,116,97,47,255]}
```

```json
{"platform":"windows","wide":[67,58,92,55296]}
```

Unix `bytes` preserves the full path bytes; Windows `wide` preserves UTF-16
units, including unpaired surrogates. This applies to envelope roots, scan scope,
scan statistics and warnings, duplicate paths, age-report paths, and analysis
issues. Serialized library `ScanError` paths use the same encoding in permission,
not-found, I/O and not-a-directory errors; variant names and source-message
strings are unchanged. Existing Unicode-only report shapes and tuple layouts
are unchanged.
Consumers must handle the string/object union explicitly and must not replace
native data with a lossy display string. Rust deserialization rejects native
objects for another platform instead of constructing a different host path.
These are report representations, not permission to operate on the paths.

[Qualification](qualification/native-reports.json) exercises both CLI names and
all four commands with native Linux roots and filenames, and preserves fixed
legacy fixtures. Native Windows runtime verification remains outstanding.

Age-bucket `largest_files` entries are ordered by size descending, then native
path ascending and modification timestamp ascending. Equal-size results therefore
have stable ordering for identical observations on the same platform.

`ScanError` and `ScanWarning` human-readable formatting escapes control and
Unicode directional characters. Explicit error paths use native filename
escaping. Original I/O sources and caller-supplied stored warning messages are
retained; error-chain renderers and JSON consumers must escape those independently.
Permission and broken-link warning constructors generate escaped path labels.

Both executable names print handled runtime failures as one escaped `Error:`
line containing the cause chain and exit with status 1. This replaces automatic
color-eyre debug/backtrace output for returned errors. `gravityfile::run()` still
returns the original report for callers that need detailed inspection. Panic
hooks, argument-parser diagnostics and third-party direct writes remain separate
output paths.

Argument-parser errors retain exit status 2. When an argument contains control or
directional characters, the executable escapes the entire parser diagnostic to
one line, including layout newlines. Ordinary parser errors, help and version
retain Clap's normal formatting. The library `run()` now returns Clap reports;
`main_entry()` handles their output streams and exit codes for executables.

Text scan summaries traverse displayed nodes iteratively, retaining one child
iterator per ancestor. `--max-depth` and `--top` preserve their display semantics,
including omission counts after each directory's displayed children. A 5,000-level
in-memory tree is exercised on a 128 KiB thread stack. This removes recursion
from summary rendering; indentation still grows with depth and total output can
be large. This does not establish a filesystem-depth or output-size budget.

CLI scan envelopes and both legacy and versioned JSON exports encode tree nodes
iteratively. They emit compact JSON plus a final newline, preserving the same
field names, values, omissions and nesting as the Serde representation. Metadata
and native paths retain their existing Serde codecs. Compact output avoids
indentation growth with nesting; consumers must parse JSON rather than depend on
whitespace. The encoder keeps one child iterator per ancestor and uses the same
staging/publication path as other reports. It does not create a second full tree
or full JSON value in memory.

Compatibility tests compare legacy/versioned output with Serde for a fixed
fixture, additional node variants and native Unix names. A 10,000-level in-memory
tree is staged on a 128 KiB thread stack. Direct library Serde serialization and
deserialization remain recursive, and downstream JSON parsers may reject deeply
nested documents. This change does not provide a total memory, output-size or
filesystem-depth bound. [Qualification](qualification/tree-json.json)

Text scan-summary rendering checks cancellation before node output and while
unwinding ancestor summaries. Cancellation after a written line suppresses
remaining descendants and omission lines and returns an interruption error.
Already written text remains partial. These checks do not interrupt an OS write
that is already blocked. The existing second-signal forced-exit behavior remains
separate.

JSON staging and copying use a cancellation-aware writer beneath the staging
buffer. Cancellation before stdout publication leaves stdout empty; cancellation
during file staging discards the temporary file and preserves the previous
export. Tests inject cancellation after a 2 MiB serialized value and during
stdout copying. The writer returns a non-retryable I/O error for cancellation,
avoiding `write_all` retries of `Interrupted`. Arbitrary serializer computation
that performs no writes and blocked I/O are not
preempted by this mechanism.

Duplicate and age text reports share the cancellation-aware output writer with
JSON publication. Their extracted renderers preserve escaped path diagnostics
and return output failures; tests use real analysis reports and cancel during
the first written line. These checks do not abort an already-blocked write or
undo bytes delivered before cancellation.

Tree statistics resolve equal largest-file sizes and equal oldest/newest
modification times by the smallest native path. This makes representative-file
selection independent of arrival order for the same observed entries. Full and
quick scans share this rule. Native path ordering is platform-specific; missing
modification times remain excluded from oldest/newest selection, and filesystem
changes can still alter results between scans.

On Unix, duplicate analysis chooses the smallest native path among eligible
observed aliases of each device/inode pair. This preserves one candidate per
physical identity while removing traversal-order dependence from the displayed
representative. Excluded paths cannot replace an eligible representative. Group
paths remain sorted before group ranking and truncation. The inode index stores
a candidate position in addition to each identity, increasing retained metadata;
existing memory benchmarks predate this change. Concurrent mutation, inode reuse,
and whether deleting an alias would reclaim blocks remain separate limitations.

Analysis diagnostics retain at most 100 distinct paths in native path order.
Each retained message is capped at 8 KiB with UTF-8-safe truncation and an explicit
marker. If a path has multiple failures, the lexically smallest bounded message
is retained, making selection independent of arrival order. Counts include all
observed failures, including repeated paths and details not retained. Failures
for paths already beyond the retained set are counted without formatting their
messages. This bounds retained message text, not native path storage, an error
object's preexisting allocations, or work inside a custom error formatter.
