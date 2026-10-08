# Filesystem recovery

Copy and move overwrites now create a private `.gravityfile-recovery-*` directory
beside the destination. Before moving source or destination paths, the operation
writes and syncs a versioned manifest recording the destination filename and,
for moves, the original source path. Native path encoding preserves non-UTF-8
Unix filenames and Windows UTF-16 rather than converting them lossily.

New journal directories explicitly request Unix `0700` permissions when created,
so staged content and path metadata are not exposed through a permissive umask.
Older retained journals are not automatically changed. Windows uses inherited
parent ACLs; a custom owner-only DACL is not currently installed. Process-exit
tests verify that retained Unix journals keep their restrictive directory mode.

On Unix, directory syncing opens with directory-only, no-follow, nonblocking
flags. A replacement regular file, symlink, or FIFO is rejected rather than
followed or opened with a potentially blocking read. This checks the final path
component at open time; it does not pin ancestors throughout the transaction.
Windows directory syncing remains unimplemented.

The directory can contain:

| Entry | Meaning |
| --- | --- |
| `manifest.json` | Versioned intent and whether publication was recorded |
| `lock` | Exclusive lock held by a live operation or recovery action |
| `payload` | Staged new content; for a move this can be the only remaining source copy |
| `original` | Saved pre-overwrite destination or retained copy-undo item |

Publication saves the original, installs the payload with a no-replace rename,
then records publication before deleting the old backup. These are separate
steps. An error or process exit can leave a journal between them. Dropping a
journal releases its lock but **never deletes its data**. Successful publication
cleans up the old backup and journal; cleanup failures leave a discoverable
record and a log warning.

Overwrite moves check cancellation again after staging and before publication.
If cancelled there, rollback attempts to restore the source without replacement;
an occupied source leaves the payload in the journal for explicit recovery.
Rollback proceeds even though cancellation was requested. Once publication has
started, its completion and recovery steps are not interrupted by cancellation.
A successfully rolled-back cancellation retains the journal for inspection even
when neither payload nor backup remains.

## Copy undo retention

Copy undo moves each destination into `original` in a sibling recovery journal
instead of permanently deleting it. Progress and completion use
`OperationType::Retain`, and the TUI identifies retained data rather than deletion.
The manifest has purpose `copy_undo`;
older manifests without that field default to `overwrite`. The manifest is
written and synced before the no-replace rename, and the affected directories
are synced after it. Neither completion, cancellation, process exit, nor journal
Drop automatically removes retained data. Once an item's rename starts, its
sync completes without a cancellation interruption; cancellation stops later
items. A sync failure reports failure with the journal path even if the rename
already happened.

This preserves later edits, added directory children, and symlinks themselves.
A relative symlink may not resolve inside the journal; restoration returns it
to its original location without rewriting its target. Undo does not verify
original object identity: a replacement at the recorded destination can also
be moved into the journal. Concurrent ancestor replacement remains outside the
safety contract.

Use `grav recover <destination-parent>` and `--restore-original` as described
below to inspect and restore retained copies. Restoration refuses an occupied
destination. Copy undo does **not reclaim disk space**, and journals have no
automatic expiry or quota. Review their data before explicitly removing them.
A missing destination reports failure rather than creating a recovery record.
Tests cover real copy/edit/undo/restore, competing restoration targets, dangling
symlinks, legacy manifests, and process exit before and after the retention
rename. These are not power-loss or cross-platform qualification.

## Inspect and recover

List records directly within the affected destination directory:

```bash
grav recover /data
```

The listing shows the journal purpose, destination, recorded move source, publication flag, and
whether original/payload/destination paths exist. Valid records remain visible
when a neighbor is active, malformed, or inaccessible. Each inspection failure
is printed with its path on stderr, and the command exits unsuccessfully with
an incomplete-listing summary. It never reports "No recovery records" when
inspection failed. Discovery is nonrecursive and does not change journal data.

Library callers can use `discover_recovery_report` for valid records and all
path-specific issues. Failure to open the parent directory returns an error
outright. The original `discover_recovery` API remains scoped to overwrite/copy-undo
journals and returns an error if any matching entry failed. Results are observations: inspection releases each
journal lock, so later recovery reopens and validates it under a new lock.
Discovery collects paths and results in memory under the shared entry/input
budgets documented below; these are not a precise resident-memory ceiling.

For automation, use the inspection-only JSON format:

```bash
grav recover /data --json > recovery.json
```

Schema version 1 has `kind: "recovery"`, purpose `recovery_inspection_only`, a
native `parent` path, `status` (`state: complete|partial` and `issue_count`),
`records`, `rename_intents`, and `issues`. Each ordinary recovery record includes its journal purpose, transaction,
destination, optional move source, and the same availability/publication flags
as human output. Each issue has a native path, diagnostic `message`, portable
`error_kind`, and optional `raw_os_error`. The additive `error_kind` field may be
absent in older schema-version-1 reports; treat absence as unclassified. Consumers
must ignore unknown fields and handle unfamiliar category values conservatively.

The defined categories are `not_found`, `permission_denied`, `already_exists`,
`invalid_data`, `invalid_input`, `would_block`, `interrupted`, `timed_out`,
`unexpected_eof`, `unsupported`, and `other`. An active lock is `would_block`;
malformed manifests are `invalid_data`. Unmapped I/O kinds use `other` while
retaining their diagnostic and OS code. Categories describe the inspection error,
not whether a restore or retry is safe. Messages and OS error numbers remain
platform dependent. [API and CLI qualification](qualification/recovery-issue-codes.json)
verifies classification independently of message wording.

Every path is an object with a lossy `display` string plus `platform: unix` and
`bytes`, or `platform: windows` and `wide` (UTF-16 code units). The native array
is authoritative. Display strings are not safe restoration inputs. Serialization
streams the existing report; it does not build a second complete JSON value.
Discovery collects records in memory, with shared budgets of 10,000 directory
entries and 64 MiB of control-file input per pass. Entries unrelated to recovery
also count; rename staging directories share the same budgets as their parent.
Each control file still has a 1 MiB limit. A read may consume one additional byte
to detect overflow. Malformed input consumes the byte budget too.

When a budget prevents further inspection, the report preserves inspected records
and includes an explicit issue, producing partial JSON and an unsuccessful exit.
It does not count the uninspected remainder. Directory enumeration order determines
which entries fit; sorting orders only the collected subset. The library's
`inspect_recovery` can inspect an individual overwrite/copy-undo journal without
enumerating its parent. Oversized bulk-rename scopes still need manual inspection;
discovery does not yet offer pagination. These are enumeration/input budgets,
not a precise resident-memory cap or a deadline for slow filesystem calls.

Partial JSON is emitted before the command exits unsuccessfully, and diagnostic
text goes to stderr. Failure to open the parent produces an error without a JSON
report. A complete report means discovery observed no errors, not that later
restoration is guaranteed safe. Records can change after inspection. JSON is
not a replay format and cannot be combined with either restore option. The
existing `--envelope` and `--strict` flags remain unsupported for recovery;
`--json` has its own versioned schema and always fails incomplete discovery.

If the saved original exists, publication was not committed, and its destination
is unoccupied:

```bash
grav recover /data/.gravityfile-recovery-ABC123 --restore-original
```

Restoration uses an exclusive rename while holding the journal lock. It refuses
to overwrite any file, directory, or dangling symlink that now occupies the
destination. If new content already occupies that path, review it and move it to
a chosen safe location before restoring the old content.

To recover an unpublished overwrite payload, supply the destination explicitly:

```bash
grav recover /data/.gravityfile-recovery-ABC123 --restore-payload-to /data/recovered-item
```

Payload restoration rejects committed overwrite journals and copy-undo journals,
even if a file named `payload` exists there. Such a file is unexpected in those
states and requires manual inspection. Rejection preserves the payload, saved
original, manifest, and destination. Copy-undo data remains restorable with
`--restore-original`.

The target must be unoccupied and outside the transaction directory. Its parent
must already exist. The recorded move-source path is informational; recovery
never trusts it as an implicit write target. Cross-filesystem payload recovery
uses the same staged-copy/source-removal behavior as normal moves. If source
removal fails, the completed recovered copy is retained and the error reports it.

The two restore options are mutually exclusive. Recovery leaves the manifest
and any other data in place for review. There is no automatic journal purge or
startup replay. Once both original and payload are accounted for, the empty
control directory can be removed using normal file-management tools.

## Interruption states

- **Payload staged, original still at destination:** preserve the destination;
  a move's source may be recoverable from `payload` at an explicitly chosen path.
- **Original saved, destination absent:** `--restore-original` restores it
  exclusively; `payload` remains available for separate recovery.
- **Payload installed, original saved:** the destination holds the new data;
  recovery refuses to overwrite it. Review both before choosing what to retain.
- **Publication recorded, cleanup interrupted:** new data is installed. The old
  backup may be complete, partially deleted, or absent. The publication flag
  does not prove the backup is recoverable. `--restore-original` refuses these
  committed records; review and recover their remaining data manually.

The manifest's publication flag is a recorded milestone, not an oracle: a crash
after installation but before recording it leaves `published = false` alongside
an occupied destination. Recovery therefore checks actual paths and refuses
ambiguous overwrites instead of guessing.

## Guarantees and limits

Overwrite publication, rollback and retained copy undo attempt to sync both the
journal and its parent directory, even if the first sync fails. Errors identify
every failed path and preserve the first I/O error kind. The
[sync-pair regression](qualification/journal-sync-pair.json) covers success, either
single failure and both failures; macOS and Colima operations suites passed.

If overwrite publication fails and attempts to move the original back, its error
reports both the rollback result and directory synchronization result. A sync
failure is explicitly marked as uncertain durability, even when the move back
succeeded. Inspect the destination before retrying; the journal is retained.
The [rollback sync regression](qualification/rollback-sync-diagnostic.json)
injects this failure and checks restored bytes and the inspectable manifest.

After a successful recovery move, both the journal and destination-parent
directory syncs are attempted, even if the first fails. A sync failure reports
that recovery **already restored the destination**; inspect that destination
before retrying. Library callers can downcast the inner `io::Error` to
`RecoveryDurabilityError` to obtain the installed destination and all failed
directory syncs with their underlying I/O errors. The return remains an error
because persistence is uncertain. This does not roll back the restored data or
claim recursive payload durability. Windows directory sync remains a no-op.

Fault-injection tests cover failure of either sync and both syncs, for original
and payload recovery. They verify that data is installed, the old staging path
is absent, the manifest remains inspectable, and both directories were attempted.

Tests exit a real child process without running destructors at the payload,
backup, and publication boundaries. They verify discovery after lock release,
preservation of both versions, and refusal to clobber recreated destinations.
Additional tests cover live locks, malformed/oversized manifests, path traversal,
native filename roundtrips, substituted control-file symlinks on Unix, and the
CLI recovery workflow.

On Unix, opening a journal control file also compares the opened device/inode
with the preceding metadata observation. A regular-file replacement during that
interval fails inspection before the descriptor is used for reading or locking.
This applies to overwrite manifests, locks, and rename intents. The regression
keeps both inodes alive and tests read-only and writable opens without timing
assumptions. This check does not prevent inode reuse, in-place content changes,
replacement after opening, or ancestor-directory races; Windows currently checks
the file type without this identity comparison.

Manifests are file-synced before mutation; journal and destination directories
are also synced on Unix. Windows directory synchronization is not implemented.
Copied payload contents are not recursively file-synced, and remote filesystems,
devices, and operating systems may have different persistence guarantees. These
tests establish recovery from **process interruption on the tested filesystem**,
not full power-loss durability across all supported platforms.

The journal covers copy/move overwrites. It does not yet cover bulk rename,
archive publication, trash, or persistent undo. It is not a tamper-proof audit
log and does not provide snapshot isolation against hostile concurrent changes
to parent directories. Lock support is required; unsupported locking fails
before overwrite mutations begin. Locks coordinate cooperating processes, not
hostile same-user filesystem modifications.

The locking implementation uses the standard library's
[exclusive file lock](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock),
available within the project's Rust 1.90 minimum.

Recovery lock guards explicitly call
[`File::unlock`](https://doc.rust-lang.org/std/fs/struct.File.html#method.unlock)
at the end of ownership, before closing the descriptor. A duplicated descriptor
must not extend a completed operation's lock lifetime. A Unix regression test
keeps a duplicate open and verifies that recovery becomes available after the
owner finishes. Abrupt process-exit recovery remains covered separately.

## Bulk rename intent records

Bulk rename reserves private `.gravityfile-rename-*` directories beside its
sources, reusing one directory per source parent. Before any source is staged,
it prepares all `item-N.json` with schema version 1, purpose
`bulk_rename_inspection_only`, and native `source`, `staged`, and `destination`
paths. The path encoding matches recovery JSON. The intent file is flushed and
synced, followed by the staging and parent directories, before the source
rename. Unix intent files request `0600`; staging directories request `0700`.

Records explicitly describe outcomes as unverified. Records for already
installed entries remain until the entire batch commits or rollback succeeds.
The affected source/target/staging directories are synced before normal cleanup
removes known intent files and empty staging directories. A sync failure after
installation reports failure and retains the intents even though the renames
may already be complete. Failed rollback and unwinding retain intent records
and remaining payloads. Cleanup never recursively deletes payloads.

Legacy per-item records remain inspection-only. New batches also prepare
`plan.batch` in every participant directory before mutation. Plans contain all
native path mappings, source object identities, parent identities and the same
coordinator/participant list. `committed.batch` records a successful whole-batch
publication after directory synchronization. Object identities use Unix
(device, inode, kind) or Windows (volume, 128-bit file ID, kind). They distinguish
replacement objects; they do not snapshot content or prevent identity reuse.

Explicit recovery of a new, uncommitted batch is available with:

```sh
grav recover /parent/.gravityfile-rename-TRANSACTION --rollback-rename
```

Recovery locks every participant in sorted order, validates matching plans and
item intents, checks parent identities, and locates each expected object uniquely
among its source, staging and destination paths. It refuses active journals,
committed batches, missing/ambiguous objects, unknown occupied original paths
and occupied staging targets before moving anything. Installed objects return
to staging first; staged objects then return to their original paths. Every move
uses no-replace publication and synced immutable pending/completion checkpoints.
An interrupted recovery can be retried after inspection; it validates actual
positions and re-syncs directories even when a prior move completed before its
checkpoint. A repeated completed restore makes no further data moves. Journals
remain as evidence. An error after recovery starts explicitly warns that the
result may be partial; a failed acknowledgement never undoes filesystem changes.

A batch is limited to 10,000 changed items, an 8 MiB plan and a 64 MiB combined
plan/intent read budget. Hard-link aliases among source items are rejected before
mutation because their recovery identities are ambiguous. These bounds do not
establish an aggregate journal quota or a large-batch latency guarantee.

`grav recover <parent>` now discovers these directories. Human output labels
rename intents as unverified; JSON places them in `rename_intents`, separate
from overwrite/copy-undo `records`. Each has native transaction, intent, source,
staged, and destination paths plus an explicit unverified outcome. Inspection
validates schema/purpose and the staging filename/path association, reads at
most 1 MiB per intent, and rejects symlink control files. Invalid intents produce
per-path issues while valid neighbors remain visible. Empty staging directories
without readable intents produce an issue rather than false absence.

New staging directories hold an exclusive OS lock through mutation, rollback,
and intent cleanup. Inspection acquires that lock for its directory read; active
batches produce an issue instead of exposing partially written intents. The
lock is released on process exit. New records have `lock_required: true`; a
missing lock file is an error, not an unlocked fallback.

Older records without this requirement remain inspectable without a lock and
are explicitly labeled `locked_during_inspection: false` in JSON and human
output. New coordinated observations report `true`. The label applies only
during inspection, not afterwards, and does not establish item identity or
outcomes. Legacy unlocked records can reflect an active writer. Locks are
advisory; noncooperating filesystem changes are not prevented. One lock handle
is retained per source parent for the duration of a batch.

New intents also declare `journal_version: 1`. They retain immutable progress
events named `item-N.00.event`, `item-N.01.event`, and so on. Each event is created
without replacement, written with Unix `0600` permissions, synced, and followed
by staging-directory sync on Unix. Before recording a completed move, both the
source and destination directories are synced. Pending installation and rollback
phases are persisted before the corresponding rename. The initial synced intent
represents `stage_pending`. This ordering follows the principle of persisting
intent before mutation and completion after synchronization; see
[SQLite's atomic-commit discussion](https://sqlite.org/atomiccommit.html).
This implementation does not inherit SQLite's transaction guarantees.

JSON inspection adds `last_recorded_phase`, and human output labels it as
historical. Its vocabulary is:

| Phase | Recorded fact |
| --- | --- |
| `stage_pending` | Intent preceded source staging; staging may or may not have happened |
| `staged` | Source-to-staging rename and directory syncs completed |
| `install_pending` | Installation intent was recorded; the rename may or may not have happened |
| `installed` | Staging-to-destination rename and directory syncs completed |
| `restage_pending` | Rollback restaging intent was recorded; the rename may or may not have happened |
| `restaged` | Destination-to-staging rename and directory syncs completed |
| `restore_pending` | Source restoration intent was recorded; the rename may or may not have happened |
| `restored` | Staging-to-source rename and directory syncs completed |

These facts do not establish current location, content, object identity, or a
whole-batch outcome. Even an `installed` item can be part of an incomplete batch.
Inspection does not probe its data paths. Legacy records yield `null`; older
JSON reports omit the field. Invalid event chains also yield `null` and an issue,
keeping a valid intent visible in a partial report. Consumers must handle missing,
null and unfamiliar phase values conservatively. The existing `outcome` remains
explicitly unverified. `--rollback-rename` requires and validates the additional
coordinated plan; historical phases alone never authorize restoration.

Each item has at most seven events. Inspection checks contiguous sequence numbers,
filename/item binding, schema version and permitted transitions before reporting
a phase. Each event read is capped at 1 KiB and consumes the shared discovery
entry/input budgets. Truncated, oversized, unsupported, orphaned or symlink events
produce issues without modifying records or payloads. Enumeration limits can
omit events, so a phase in a partial report is not a complete history.

A failed progress write stops further unrecorded moves, including rollback moves
that cannot first record their intent. Data and mappings remain for manual review;
rollback errors identify retained paths. A failed final completion record leaves
an already installed batch in place and reports that installation completed with
uncertain progress recording. Never retry the entire batch blindly. Successful
commit or rollback removes known event/intent files and only empty staging
directories. Interrupted cleanup can leave an incomplete history or orphan events.
Process-exit tests cover all eight phases; fault tests cover required directory
syncs, occupied event files, invalid histories and completed-installation errors.
They do not qualify power loss, ancestor replacement, hostile same-directory
writers, Windows directory durability or recovery against hostile concurrent writers.

Inspection never probes or mutates recorded source and destination paths. The
original/payload restore flags do not execute rename intents; coordinated rename
restoration requires `--rollback-rename`. Live bulk rename and explicit recovery
check object and parent identities before and after moves. These path checks
are not atomic with rename: an ancestor or leaf can change between verification
and mutation. A post-move mismatch reports uncertainty and retains data. Journals
are not a security boundary against a hostile writer with the same access.
Process-exit tests bypass destructors; they do not establish power-loss durability
or an atomic multi-file transaction. The release profile aborts on panic; unwind
tests only establish retention for unwind builds.

## Archive extraction journals

Extraction decodes into a private sibling `.gravityfile-extract-*` directory,
completes destination preflight, and writes a synced publication plan in a
`.gravityfile-extraction-journal-*` directory before changing destination entries.
The plan retains native paths and object/parent identities. New directories are
prepared inside the journal and moved without replacement; regular files are
published as hard links to decoded staging, and symlink objects are moved without
following their targets. Pending and completed records bracket publication;
`committed.archive` follows successful publication and directory synchronization.
Normal success removes known control files, empty journal directories and decoded
staging. Interrupted cleanup can leave a committed or incomplete journal requiring
inspection; committed extraction is never automatically undone.

Failed publication attempts to move every owned published object into
`retained-N` entries inside the journal. It never recursively removes output.
Successful in-process retention records `rolled-back.archive`; any uncertain
rollback reports `ArchiveError::ExtractionRecovery`, preserves decoded staging
and exposes the journal path. The TUI refreshes and retains the current selection.
Preflight/decode failures before publication clean staging without changing output.

`grav recover <parent> --json` adds `archive_records` with native transaction,
destination and staging paths, an entry count, and a historical status:
`interrupted_or_prepared`, `committed`, `rolled_back` or `retained`. Status is an
observation of markers, not proof of current data. Malformed or active journals
produce issues without suppressing valid neighboring records. Consumers must
accept missing arrays in older reports and unfamiliar future statuses.

To retain output from an interrupted, uncommitted extraction:

```sh
grav recover /parent/.gravityfile-extraction-journal-TRANSACTION --retain-extraction
```

The command preflights all planned objects and their current identities before
moving anything. Replacements, missing objects, occupied retention targets and
unrelated children in created directories stop recovery. Owned leaves then move
to retained storage; owned directories move only when empty. Preexisting
directories and unrelated files remain in place. Same-object content edits are
retained as they exist at recovery time. The command preserves decoded staging
and all recovery records, records immutable retention checkpoints, and supports
resumption after process death before or after a move and after completion.
Retry re-syncs retained files and directories before its final marker. An error
can leave a partial retention result and explicitly instructs inspection.

Plans are limited to 32 MiB and 100,001 entries including the destination root;
discovery also applies its shared input/entry budgets. These limits do not cap
retained payload bytes, repeated extraction journals or total process memory.
There is no automatic expiry or deletion of recovery payloads. Review retained
content and free space before manual cleanup. Path identity checks still leave
check-to-move and ancestor races; no snapshot, hostile-writer or power-loss
qualification is claimed.

## Filesystem durability qualification

| Platform/filesystem scope | Implemented mechanism | Evidence and limit |
| --- | --- | --- |
| macOS local filesystem | File sync, directory sync, native no-replace moves, device/inode checks | Process-exit and injected-error tests; no power-cut/storage-controller qualification |
| Colima Linux test filesystem | File/directory sync, native no-replace moves, device/inode checks | Native Rust process-exit tests; source bind mount and container volumes are not a production filesystem matrix |
| Windows | Handle-derived volume and 128-bit file IDs with reparse-point opens; no-replace moves | Identity module cross-compiles; native execution and directory persistence remain unqualified |
| Network, removable and unsupported filesystems | Errors are retained; no-replace publication must be supported | No durability or compatibility qualification; inspect partial outcomes before retrying |

The ordering is informed by [SQLite's commit protocol](https://www.sqlite.org/atomiccommit.html)
and the distinction between file and directory persistence in
[Linux fsync documentation](https://www.man7.org/linux/man-pages/man2/fsync.2.html).
Windows identity follows [Microsoft's FILE_ID_INFO contract](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_id_info).
These references do not transfer their guarantees to gravityfile.

## Lock failure classification

Inspection and recovery preserve the distinction exposed by Rust's
[`TryLockError`](https://doc.rust-lang.org/std/fs/enum.TryLockError.html).
An already-held journal lock returns `io::ErrorKind::WouldBlock` with an active
record diagnostic. Other lock failures retain their original I/O kind, OS error
code and message instead of becoming a generic `Other` error. This applies to
ordinary journals and locked bulk-rename staging directories. Discovery still
retains these failures as issues and never treats a busy journal as absent.

[Qualification](qualification/recovery-lock-kind.json) covers real lock contention
on macOS/Linux, continued discovery beside active and malformed journals, and
injected OS-error preservation. No automatic retry or change to lock ownership
is introduced. Native Windows execution remains unqualified.

## Output failure and exit status

Recovery inspection reports an unsuccessful exit if JSON cannot be written or
flushed, including a closed output pipe. A missing or truncated report must not
be treated as successful inspection. This applies to both complete and partial
discovery results. Recovery does not use the ordinary observation commands'
non-strict broken-pipe success policy. Scan/duplicates/age/export retain that
policy, and their strict mode still treats output failure as unsuccessful.

[Closed-output CLI tests](qualification/recovery-broken-pipe.json) exercise recovery
with complete and partial discovery and the ordinary/strict scan policies. Output
failure does not imply that a recovery mutation was rolled back; inspect the
filesystem before retrying a restore whose acknowledgement was lost.

After a successful original/payload restore, the CLI explicitly writes and flushes
its acknowledgement. Failure returns an unsuccessful exit with “Recovery completed”
and an instruction to inspect the restored destination before retrying. The
underlying output error remains in the error chain. The restored data and retained
journal are not rolled back to compensate for output failure.
[Qualification](qualification/recovery-acknowledgement.json) exercises both restore
commands against a closed stdout and injects a flush failure after an accepted write.

Progress and inspection diagnostics on stderr are best-effort. A closed or failing
stderr does not panic and does not replace the operation's result: partial recovery
still exits unsuccessfully and its JSON remains partial; a successful observation
can still return a complete report. Required stdout report/acknowledgement failures
remain errors. [Closed-stderr qualification](qualification/diagnostic-output-failure.json)
tests both executable names and preserves the inspected manifest. No mechanism
can guarantee diagnostic delivery when stderr is unavailable.
