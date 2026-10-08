# Reports and automation

`grav scan`, `duplicates` and `age` print text by default and JSON with
`-f json`. `grav export` always writes JSON. Every JSON document has the same
envelope, so a consumer can tell what it is looking at before reading the data.

## Envelope

```json
{
  "schema_version": 2,
  "tool_version": "0.3.1",
  "kind": "scan",
  "root": "/data",
  "scope": { "root": "/data", "apparent_size": false, "max_entries": null, "...": "..." },
  "parameters": { "depth": 1, "limit": 20 },
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

| Field | Meaning |
|---|---|
| `schema_version` | Incremented on any incompatible change to the envelope or a `data` shape. Require the version you understand. |
| `kind` | `scan` (from `scan` and `export`), `duplicates` or `age`. |
| `root` | The canonical directory that was scanned. |
| `scope` | The effective scan configuration: size measure, hard links, symlinks, filesystems, hidden files, excludes, entry limit. |
| `parameters` | The command's own options: `depth`/`limit` for `scan`, the analysis configuration for `duplicates` and `age`, `null` for `export`. |
| `status` | Whether the results cover everything in scope. See [Completeness](#completeness). |
| `data` | The command's results, below. |

Consumers should ignore unknown fields and must not depend on key order or
whitespace. Human-readable messages are not part of the contract.

## Data

**`scan`** lists the largest entries down to `--depth`, at most `--limit` per
directory, in depth-first order:

```json
{
  "total": { "size": 2046296064, "files": 300020, "directories": 21656, "symlinks": 0 },
  "entries": [
    { "path": "node_modules", "depth": 1, "kind": "directory", "size": 1200000000,
      "files": 180000, "directories": 9000,
      "modified": { "secs_since_epoch": 1791467275, "nanos_since_epoch": 843020891 },
      "omitted": { "entries": 312, "size": 41000000 } },
    { "path": "node_modules/typescript", "depth": 2, "kind": "directory", "...": "..." }
  ],
  "omitted": { "entries": 4, "size": 422000 }
}
```

`path` is relative to the root. `kind` is `directory`, `file`, `symlink` or
`other` (FIFOs, sockets, devices). Directories carry `files` and `directories`
counts for their whole subtree; symlinks carry `target`; `git` appears with
`--git`. `omitted` summarizes children beyond the limit, on the directory whose
children were listed, or at the top level for the root.

**`export`** contains the complete tree: every node with `name`, `kind`, `size`,
`blocks`, `timestamps`, `inode` (for multiply linked files), `git_status` and
`children`, plus `stats` and `warnings`. It is the snapshot format.

**`duplicates`** contains groups of identical files (`size`, `wasted_bytes`,
`paths`), sorted by wasted space, with totals computed before `--limit`
truncation and `groups_omitted`.

**`age`** contains the age distribution (`buckets`), `stale_directories`, and
`stale_directory_status`.

## Sizes

Sizes are bytes. By default they are disk usage (allocated blocks), and
directories include their own allocation, so the root total matches `du -s`.
`-A` switches to apparent sizes (file lengths). A file with several hard links
counts once unless `-l` is given; with `-L`, an object reached through several
symlinks also counts once.

Byte counts are JSON integers and can exceed 2^53. Parse them with a 64-bit
integer type, not an IEEE double. Times are `{secs_since_epoch, nanos_since_epoch}`.

## Paths

Paths are JSON strings when they are valid Unicode. Other paths use a tagged
object that preserves them exactly:

```json
{"platform":"unix","bytes":[47,100,97,116,97,47,255]}
{"platform":"windows","wide":[67,58,92,55296]}
```

Handle both forms. Do not substitute a lossy display string for the native
value.

In text output, control characters, Unicode direction marks and invalid bytes
in names are shown as visible escapes (`\n`, `\u{202e}`, `\xff`), and literal
backslashes in paths are doubled, so a file name can neither inject terminal
sequences nor forge extra lines. Use JSON when the exact name matters.

## Completeness

`status.state` is one of:

- `complete`: everything in scope was read and analyzed. Excludes, `--no-hidden`
  and display limits are deliberate scope choices and do not make a report partial.
- `partial`: some entries could not be read (`scan_warning_count`) or some files
  could not be analyzed (`issue_count`, with up to 100 `issues`). Sizes and
  counts are lower bounds.
- `cancelled`: the run was interrupted.
- `unknown`: a report written before completeness was recorded.

Stale-directory recommendations are only produced when nothing could hide a
recent file: `stale_directory_status` is `evaluated`, or else
`suppressed_incomplete_scan` / `suppressed_filtered_scan`.

A partial report is printed with a warning on stderr. Check `status`, not just
the exit status, before treating an empty result as evidence.

## Exit status

| Status | Meaning |
|---|---|
| 0 | Success, including partial results without `--strict`, and a reader closing the pipe early (`grav scan \| head`). |
| 1 | The command failed. No report is promised. |
| 2 | Invalid arguments. |
| 3 | `--strict` and the results are incomplete. The report is still written. |
| 130, 143 | Interrupted by SIGINT or SIGTERM (Ctrl-C or Ctrl-Break on Windows). A second signal exits immediately. |

With `--strict`, a reader closing the pipe early is a failure (status 1),
because the report was not fully delivered.

## Output delivery

JSON is serialized completely before any of it is written (in memory up to
1 MiB, then in a temporary file), so a serialization error or an interruption
before output begins leaves stdout empty. `export -o FILE` writes a private
temporary file beside `FILE`, syncs it, and renames it into place; the previous
file survives any failure up to that rename. A symlink at `FILE` is replaced,
not followed. On Unix the containing directory is synced afterwards; if that
fails the error says the export was published but durability is uncertain.

`--max-entries N` fails the command before any output if the scan reaches more
than `N` entries below the root, and leaves an existing export untouched. It
bounds the scanner's work, not total memory.

## Compatibility fixtures

[`tests/fixtures/reports`](../tests/fixtures/reports/README.md) holds
hand-written schema 1 documents. Tests check that they still load, that fields
dropped in schema 2 (node ids, content hashes, access and creation times) are
ignored, that unknown fields are accepted, and that malformed known fields are
rejected.
