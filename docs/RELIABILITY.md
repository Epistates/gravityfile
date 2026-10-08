# Reliability contracts and limits

These describe implemented behavior and its known limits. For qualification
results and remaining acceptance criteria, see [project state](PROJECT_STATE.md).
A local test result does not establish guarantees on an untested platform or
filesystem.

## File operation guarantees and limits

- Copies reserve a private staging directory on the destination filesystem.
  Copy/archive staging and overwrite journals explicitly request Unix `0700`
  at creation; tempfile directory defaults alone do not provide that guarantee.
  Windows continues to inherit parent ACLs and needs separate DACL qualification.
  TAR/ZIP extraction skips archive root-directory headers and rejects root
  entries of other types, preventing TAR metadata from weakening the host-owned
  staging-root permissions. Tests include permissive and inaccessible TAR root
  modes followed by normal payload content.
  TAR hard links resolve relative to the archive root and must reference an
  already extracted regular file inside staging. Tests cover nested link entries,
  rejection of cwd-file imports, symlink targets, and parent traversal. Forward
  hard links and hard links to symlinks are not supported by this extraction path.
  TAR preflight uses parser-effective sizes, including sparse logical extents,
  against per-entry and remaining total budgets. Oversized sparse entries are
  rejected before creating output paths; bounded sparse contents still extract.
  FIFO and device headers are explicitly rejected rather than materialized as
  ordinary files by the underlying library.
  Archive inputs are opened once and validated as regular files before staging;
  decoders retain that handle across pathname replacement. Unix opens are
  nonblocking to reject FIFOs without waiting for writers, while symlinks to
  regular inputs remain supported. Tests cover missing/directory/FIFO inputs
  and pathname replacement after opening. This does not isolate concurrent
  modifications to the opened file's contents.
  TAR extraction drains the selected decoder after its end marker before
  publication, allowing at most 1 MiB of decoded zero padding. Tests reject
  truncated gzip/bzip2/xz trailers, a corrupt gzip checksum, hidden decoded
  trailing content, and excessive padding. Multi-member gzip/bzip2/xz decoding
  covers splits within TAR headers, payloads, and padding, and rejects hidden
  decoded content or truncated trailers in later members. Extraction does not
  yet impose a CPU-time limit or compressed-member-count budget.
  XZ filter/dictionary allocation has a 256 MiB decoder-memory limit. A fixed
  header requesting a 4 GiB dictionary is rejected before publication without
  constructing a large encoder in the test. This limit covers codec memory,
  not total process RSS or concurrently running extractions.
  ZIP preflight fails on entry-read errors, checks count before entry traversal,
  and validates individual/remaining sizes and compression ratios before decoding.
  A crafted zero-compressed/nonempty entry is rejected before materialization.
  ZIP metadata parsing now has a cumulative 64 MiB read budget. Seeks and
  parser retries do not replenish it; exhaustion rejects the archive before
  materializing output. The budget ends after metadata parsing, so it does not
  cap compressed payload reads. Tests exercise rejection, successful extraction
  of a payload larger than the metadata budget, and repeated reads/seeks.
  This bounds parser input work, not total allocation: the ZIP library can
  reserve memory from advertised entry counts before further reader I/O.
  An independent allocator/process memory limit remains unfinished.
  TAR parser advancement has a cumulative 64 MiB metadata-read budget and a
  1 MiB budget per returned entry. These include GNU/PAX extension bodies,
  sparse maps, headers, and any skipped padding/body bytes; ordinary unpacked
  payload reads use the separate extraction limits. Budgets reject oversized
  extensions before the parser finishes buffering them, including compressed
  inputs. Tests use streaming 1 GiB advertised bodies, cumulative exhaustion,
  payload exemption, and public gzip extraction cleanup. These are reader-work
  limits, not exact allocation/RSS or decompression CPU deadlines. Legitimate
  archives with unusually large metadata can also be rejected.
  Copying or serialization failure does not expose partial output at the final
  path. Cancellation is checked before copying and before copy publication.
- Publishing a new copy, moving, and renaming use atomic no-replace operations
  on supported Unix filesystems; Windows uses `MoveFileExW` without replacement or copy flags.
  Unsupported Unix filesystems return an error. An existence check followed by
  ordinary rename is insufficient to protect against another writer.
- Overwrites use a staged payload and a backup of the original destination.
  The backup and install are separate renames, **not a single atomic transaction**.
  A crash can leave staging or backup paths in a persistent overwrite journal,
  and a rollback failure reports the recovery location. [Explicit recovery](RECOVERY.md)
  inspects these paths and restores them without overwriting existing data.
  There is no automatic replay or full power-loss durability guarantee.
- Cross-filesystem moves first publish a complete copy, then remove the source.
  If removal fails, the complete destination is retained and its path reported.
  The source can be partially removed, so deleting the destination on this error
  would lose data. Move undo uses this same implementation.
- Conflict waits respond to cancellation even when their result channel is full
  or the UI retains an unanswered request. Regular-file copies check cancellation
  around reads/writes of at most 256 KiB. Blocking filesystem syscalls cannot be
  interrupted mid-call.
- Copy size estimation checks cancellation before fetching each directory entry
  and after the fetch, including failed entries, so a wide directory does not
  have to finish enumeration first. Partial estimates account only for entries
  already inspected. Directory timestamp finalization also checks cancellation
  between directories. These checks do not preempt a blocked directory read or
  metadata call; estimates continue to omit inaccessible entries.
- Directory copies restore source permissions after copying descendants, from
  children to parents, before publication. This is independent of timestamp
  preservation; when requested, directory timestamps use metadata captured
  before enumeration. Unix regression coverage includes a private `0700` root,
  a non-writable `0500` child and a `0600` file. Permission failures abort staged
  publication. This preserves permission bits, not ownership, ACLs or effective
  access under a different user/group; Windows permission semantics remain
  unqualified. Concurrent source or destination ancestor changes are not isolated.
- Failed new/overwrite copies explicitly attempt staging cleanup. If it fails,
  the error preserves the original failure and reports the cleanup error and
  staging path. Cleanup can be partial; no destructor retries it silently after
  that report. A retained `.gravityfile-stage-*` directory is temporary copy
  residue for manual inspection, not a replayable recovery journal and not
  included in recovery discovery. No automatic permission relaxation is used.
  After successful publication, failure to remove empty staging is logged as a
  cleanup warning rather than reporting that the completed copy failed.
- On Unix, a repeated source directory device/inode makes copying fail instead
  of silently skipping that subtree. This avoids reporting an incomplete copy
  as successful, including the copy phase preceding cross-filesystem source
  removal. Directory aliases/cycles are rejected, not reproduced. The regression
  injects an already-observed identity; it does not qualify real bind mounts,
  directory hard links, or concurrent inode reuse.
- Regular-file copies open a validated source handle and create their staged
  output exclusively. Unix uses no-follow/nonblocking opens and checks device,
  inode, size, modification time and change time before/after reading. Other
  platforms check regular-file type, size and modification time. Short/growing
  inputs fail rather than publishing a partial copy; permissions and optional
  timestamps are applied through the destination handle. This is not a snapshot
  or protection against ancestor replacement, inode reuse or changes invisible
  to these metadata checks. Windows reparse-point races remain unqualified.
  On Unix, all-zero chunks are skipped by seeking in the exclusively created
  output, and the final length is explicitly set, including trailing holes.
  This can reduce allocated space on filesystems supporting holes; it does not
  preserve exact source extents and still reads every source byte. Tests cover
  empty, all-zero and populated sparse files with trailing holes. Small-file or
  filesystem allocation policies can still allocate the full apparent size.
  Bounded copying replaces OS copy acceleration: clone/reflink behavior,
  extended metadata and large-file throughput remain unqualified. Windows does
  not use zero-chunk seeking; copies may require the full apparent size in space.
- Copy/move completion explicitly reports cancellation, including conflict
  aborts. `is_success()` is false for cancelled completion; the TUI labels it
  cancelled while retaining undo records for completed items. Aborted items are
  not counted as deliberate skips. Cancellation does not undo completed work.
  Progress updates are best-effort: a full queue drops the new update rather
  than suspending filesystem work. Dropped receivers cancel work through both
  executor and TUI adapters, awaiting cooperative cleanup instead of detaching
  blocking operations. Completion delivery still requires the receiver to drain
  its queue; cancellation cannot interrupt a blocking filesystem syscall.
- Archive extraction has path/expansion checks, private decoded staging and a
  synced publication journal. Pending/completed records bracket directory and
  leaf publication; a whole-extraction marker follows successful synchronization.
  Failed publication retains owned output instead of unlinking it and exposes
  `ArchiveError::ExtractionRecovery`. Explicit `--retain-extraction` preflights
  identities and moves owned output into retained storage, leaving preexisting
  directories, unrelated children and replacements intact. Recovery resumes after
  interrupted moves and re-syncs before acknowledging its final outcome. Symlink
  objects move without following their targets; same-object edits remain retained.
  The TUI warns, retains selection and refreshes without replaying extraction.
  [The recovery contract](RECOVERY.md#archive-extraction-journals) documents bounds,
  retained-data handling and the platform matrix. Multiple-entry publication is
  not crash-atomic; path checks leave ancestor and check-to-move races. Native
  Windows execution and power-loss durability are not established. Older
  [rollback](qualification/extraction-rollback.json) and
  [identity](qualification/extraction-identity.json) records document the earlier
  in-process removal implementation rather than qualifying this protocol.
- These are user-level filesystem operations, not snapshot isolation. Parent
  paths and source contents can change concurrently. Protection against hostile
  mutation of ancestor directories requires operations relative to held directory
  handles and file-identity validation. Private staging alone does not provide it.
- Archive creation syncs completed file contents before no-replace publication,
  then syncs the destination parent on Unix. A post-publication sync error returns
  `ArchiveError::PublishedDurability` with the destination and underlying I/O
  error; the complete archive remains visible and must be inspected before retry.
  The TUI refreshes after this published-state warning, retains source selection,
  and does not interrupt another active mutation. Its warning remains visible
  after the refresh ([TUI regression](qualification/archive-warning-refresh.json)).
  Exhaustive library matches must handle this new variant. Windows directory
  sync is a no-op. This does not establish power-loss durability or protect
  against ancestor-directory substitution. [Fault-injection qualification](qualification/archive-parent-sync.json)
  covers all five formats and successful extraction after a reported sync failure.
- The TUI appends default archive extensions to native paths without lossy display
  conversion. A [Linux regression](qualification/archive-native-parent.json) places
  distinct native and replacement-character directories side by side and checks
  that only the intended directory receives the archive.
- ZIP regular-file inputs are opened with no-follow and nonblocking flags on
  Unix, then checked against the inspected device/inode and regular-file type.
  [Fault-injection tests](qualification/zip-source-open.json) replace an inspected
  input with a symlink, another file, a directory and a FIFO. These checks do not
  provide ancestor confinement, a stable content snapshot, or equivalent Windows
  identity guarantees; TAR traversal has separate library-owned open behavior.
- ZIP reads stop at the inspected file length and reject short reads or detectable
  size, modification-time, and Unix inode/change-time changes before or after
  copying. [Mutation tests](qualification/zip-bounded-source.json) inject growth
  and truncation while bytes are written and verify the output bound. This is
  not a snapshot: same-size edits that evade available metadata can go undetected.
- ZIP creation preserves ordinary Unix regular-file permission bits instead of
  defaulting every entry to `0644`; private and executable modes survive
  extraction ([round-trip tests](qualification/zip-file-modes.json)). Special bits
  are removed. This does not preserve directory modes, ownership, ACLs or xattrs,
  or guarantee equivalent access under a different owner/group.
- ZIP and TAR container files are created exclusively with Unix mode `0600`
  (subject to umask) and retain owner-only access when published. Change their
  permissions explicitly for sharing. Entry permissions do not protect bytes
  inside an archive. [Five-format tests](qualification/archive-private-output.json)
  verify output permissions and extraction on macOS and Linux. Windows retains
  inherited ACLs; this does not provide encryption or an audited ACL policy.
- ZIP symlink extraction preserves UTF-8 target text exactly, including leading
  and trailing whitespace. Invalid UTF-8, empty and NUL-containing targets are
  rejected rather than normalized into different names. [Round-trip tests](qualification/zip-link-target.json)
  cover spaces, tabs and newlines; non-UTF-8 native link targets require TAR.
- If an archive worker closes its completion channel without a result, the TUI
  releases its task slot and reports an unknown outcome requiring destination
  inspection. It refreshes the view, deferring behind active mutations, and keeps
  source selection. [Channel-loss tests](qualification/archive-worker-loss.json)
  cover idle and active-operation paths. This is not crash recovery or replay.
- Archive creation supports `create_archive_cancellable` with a cancellation token.
  It checks cancellation at admission, source validation, ZIP traversal, output
  writes/seeks/flushes and before publication. A cancelled creation returns
  `ArchiveError::Cancelled` and removes private staging. After publication it
  retains the archive and reports success or a durability error. Exhaustive
  library matches must handle the new variant. In the TUI, Esc or normal quit
  requests cancellation and waits for completion; selection is retained. Forced
  exit or dropping the application also requests cancellation. Active foreground
  mutations retain their existing cancellation priority.
  [Creation qualification](qualification/archive-cancellation.json) covers the
  admission/output/publication boundaries on macOS and Linux. Filesystem calls,
  encoder internals and TAR traversal have no hard interruption deadline.
- Extraction supports `extract_archive_cancellable`. Reads, ZIP entry processing
  and merge preflight observe cancellation. Decoded staging is removed and no
  destination entries are installed when cancellation is observed before commit.
  Once destination mutation starts, the existing completion/rollback path runs
  without cancellation. The TUI uses the same cancel/quit and drop guard for
  extraction as creation. [Extraction qualification](qualification/extraction-cancellation.json)
  covers staged cancellation with missing/existing destinations across all formats
  and queued TUI cancellation. Filesystem/decoder calls, sorting and cleanup have
  no hard interruption deadline; this does not make a merge crash-atomic.
- Archive source basenames must be unique. ZIP creation rejects non-UTF-8
  filenames and link targets rather than applying lossy replacement. TAR retains
  native Unix bytes in top-level and nested names and symlink targets;
  [Linux round-trip tests](qualification/tar-native-names.json) cover TAR, gzip,
  bzip2 and XZ creation followed by extraction. ZIP links use the format's explicit symlink entries
  ([writer API](https://docs.rs/zip/latest/zip/write/struct.ZipWriter.html#method.add_symlink)).
  Case-folding and Unicode-normalization collisions on a different extraction
  filesystem remain subject to extraction conflict checks.
- Undo has a bounded in-memory history; it is neither a backup nor a persistent
  recovery mechanism. Metadata preservation does not promise ACL, xattr, sparse
  extent, or hardlink-topology preservation.
- Bulk rename supports staged swaps/cycles and attempts no-replace rollback.
  Preflight resolves existing parent directories while preserving leaf symlinks,
  validates no-op entries, and rejects duplicate sources/destinations or entries
  beneath another source. This prevents staging a parent out from beneath the
  same batch's remaining operations; parents must already exist.
  Payloads are staged inside reserved `.gravityfile-rename-*` directories, one
  per source parent, with Unix `0700` permissions. Staging names do not embed
  source names or depend on probing predictable sibling candidates. Cleanup
  uses empty-only directory removal, never recursive deletion: failed rollback,
  unexpected entries, or unwinding retain payloads. Tests verify permissions,
  directory reuse, cleanup after commit/cancellation, and payload retention
  during an injected unwind. Windows staging inherits parent ACLs.
  A failed restaging step is excluded from source restoration so an occupied
  staging path cannot be mistaken for the restaged payload. Missing payloads
  produce explicit rollback errors while other entries continue restoring.
  Tests cover occupied staging, missing payloads, and a partially installed
  cycle. Every changed item receives a synced native-path intent and object
  identity before any source mutation. Matching plans across participant staging
  directories record parent identities and the coordinator; a batch commit marker
  follows successful publication and synchronization. Immutable progress remains
  historical inspection evidence. Explicit `--rollback-rename` validates and locks
  all participants, refuses replacements/ambiguous objects/occupied original paths,
  restages installed objects and restores sources using no-replace moves. Recovery
  checkpoints are resumable across process exit, including after a move but before
  its sync, and journals remain after explicit restoration. Source hard-link aliases
  are rejected before mutation. Path-based object checks cannot close concurrent
  ancestor/check-to-move races; this is not a crash-atomic transaction or a content
  snapshot. See [bulk rename recovery](RECOVERY.md#bulk-rename-intent-records).
  The TUI runs bulk rename on a blocking worker and continues rendering while
  waiting. Its running state defers commands until the result arrives; cancel or
  quit requests cooperative cancellation and waits for rollback before returning
  to normal mode. `rename_paths_with_cancellation` checks between preflight,
  staging, and installation steps. Final installation commits the batch;
  cancellation received afterwards does not undo it. Rollback ignores cancellation
  and reports blocked restoration paths. Tests cover each partial phase, the
  commit boundary, blocked rollback, and the TUI cancellation/result lifecycle.
  Success clears marks, failure preserves them, and either outcome refreshes the view. Worker
  join errors are reported in unwinding builds; release panics still abort.
  Editor handoff owns a private temporary directory and deterministic source
  ordering. The line-based format rejects non-UTF-8/control-character source
  names, malformed edits, and lists over 1 MiB. Unix readback rejects symlinks
  and FIFOs; atomic editor file replacement remains supported. Editor failures
  stop processing. Temporary storage is cleaned on return/drop, and original
  marks remain available for retry; invalid edited text is not retained.

## CLI contract

`export -o PATH` serializes to a private temporary file in PATH's parent, flushes
and syncs the file, then replaces PATH. The final symlink is replaced rather than
followed. Errors before replacement preserve an existing report. Replacement
does not preserve the old file's ownership, ACLs, or permissions. On Unix,
staging, replacement, cleanup and directory sync use one open parent handle;
later parent substitution cannot redirect them. A sync error after replacement
reports that the completed export was already published. Initial parent
resolution, hostile writers inside that directory and non-Unix directory sync
remain outside this guarantee; see [report semantics](REPORTS.md). Power-loss
behavior across filesystems/devices remains unqualified.

JSON export uses buffered serialization instead of allocating a second complete
JSON string. Unix log files are created with mode 0600. Help, version, and command
syntax errors are handled before creating a log file.

Each process's diagnostic log has an 8 MiB byte cap, including a single
truncation marker. All logging threads share the same budget. Once full, further
diagnostics are discarded; a write failure also disables further writes to that
sink. A final record can be cut mid-message or mid-UTF-8 sequence. This is a bound
on one file, not on the number of invocations or their aggregate disk usage.
There is no automatic retention, rotation, or cross-process quota. These
temporary diagnostic files are not durable operation audit records.

Size arguments retain historical binary multipliers for KB/MB/GB and also accept
KiB/MiB/GiB. Integer arguments are checked without floating-point conversion;
fractional and scientific notation use floating point and truncate fractional
bytes. Repeated suffixes and overflow are errors. Age thresholds accept h, d, w,
m/mo, and y; an unqualified number means days, months mean 30.44 days, and years
mean 365.25 days.

## Settings and scan lifecycle

Settings saves serialize into a private sibling file, sync its contents, then
atomically replace the destination and sync its containing directory on Unix.
Tests cover replacement, failed publication
cleanup, and Unix leaf-symlink replacement with private (0600) permissions.
Bookmark save errors are visible. Saves do not preserve existing permissions or
ACLs or coordinate multiple writers; loading still
falls back to settings with automatic scanning disabled for unreadable or invalid settings, with a visible startup
error. Missing settings use defaults without an error. Reads require a regular
UTF-8 file and enforce a 1 MiB cap before and after reading; saves enforce the
same cap. Unix nonblocking opens reject FIFOs without waiting for a writer.
Settings symlinks to regular files are supported on load, but saves replace the
link itself. Image-preview detection shares the app's loaded configuration.
The settings dialog applies edits only after saving succeeds. Failure retains
the dirty dialog for retry and preserves active settings; unchanged dialogs
close without writing. Regression tests cover failure followed by retry.
If directory sync fails after replacement, the error explicitly says the settings
were installed but durability is uncertain. The dialog retains its edits and old
active settings for retry; the on-disk document may already contain those edits.
The sync helper requires a directory and uses a nonblocking Unix open. Parent
symlinks remain supported; ancestor substitution is not prevented. New ancestor
directory entries are not recursively synced, and Windows directory durability
and power-loss behavior remain unqualified.

Scan completion preserves active dialogs and their edits, including on scan
failure. Successful scans no longer erase unrelated startup/settings errors.
Escape in normal mode dismisses an error before changing selection or cancelling
work; regression tests cover these state transitions.
Final warning merge uses borrowed hash keys rather than repeatedly scanning the
accumulated list. Warning identity includes path, kind, and message so distinct
failures on one path remain visible. Tests cover stable order and repeated
10,000-warning batches; this is not a full large-tree UI latency measurement.
Duplicate row layout visits group metadata and materializes only the visible
row coordinates. Tests compare it with a flattened reference across selections
and terminal heights and verify a million-row group produces just 24 visible
coordinates. Group membership, per-group visible-path counts, and scoped wasted
bytes are cached by immutable report identity plus view/scan roots. Repeated
rendering and navigation reuse that cache; rebuilding scans paths once. Weak
report ownership avoids retaining old reports. Tests cover reuse, directory and
report replacement, and scope accounting. Group-reference lists and row layout
still scale with group count. An optimized full-widget benchmark with 1M
synthetic paths originally measured first frames at 97–98 ms. Initial root counts
now run on the analysis worker with cancellation checks, and are installed with
report identity and scope before rendering. The updated benchmark measures
93–96 ms preparation separately, 0.001–0.013 ms installation, 0.139–0.515 ms
first frames, and 0.079–0.088 ms cached medians. [Raw measurements](benchmarks/macos-duplicate-render-prepared-1m.json)
exclude terminal I/O and report construction. Scope changes still rebuild counts
on the UI thread; directory-change and end-to-end terminal latency remain
unfinished qualification work. Regression tests verify seeded cache reuse,
changed-scope invalidation, and cancelled preparation. The Unix strict-output
test now closes its output peer before launch, eliminating a parent/child race.

## Executor result delivery

The generic operation executor uses one forwarding loop for copy, move, rename,
and create results. It observes downstream closure while waiting for a worker
result, then drops the worker receiver. Closure detection therefore does not
require another progress event. Completion counts and diagnostics pass through
unchanged; raw copy/move undo metadata remains available only through the raw
operation APIs. Worker cancellation remains cooperative and cannot interrupt an
already active filesystem call. The TUI's separate create/rename adapters also
observe UI receiver closure while idle. Creation undo records require positive
successful mutation counts, so zero-work completions cannot claim ownership of
an item for later deletion.

## Creation lifecycle

File and directory creation share one worker implementation. Filename validation,
metadata inspection, parent creation, and exclusive final creation run off the
async runtime thread. Closed receivers prevent queued work from beginning; the
worker checks again before creating parents and before creating the final item.
Progress does not block on a full queue. Active filesystem calls are awaited and
cannot be interrupted. Parents already created are left in place after
abandonment. Final `create_new`/`create_dir` results determine success, so a
competing writer's destination is never adopted into the operation's undo record.
Parent-directory identity/race protection and crash-atomic parent-chain creation
are not provided.

## Create and enter

The TUI's create-and-enter command validates names through the shared validator
and performs metadata checks and leaf-directory creation on a blocking worker.
It accepts an existing real directory, rejects files and symlinks, and records
creation undo only when its own exclusive create succeeds. Navigation waits for
completion; failures preserve the current view. If the user navigated elsewhere
while work ran, completion records any creation but does not override that view.
A closed receiver is checked before work and before creation. Active calls and
concurrent path replacement are not isolated; the parent must already exist.

Create-and-enter has a dedicated result channel, independent of scans and other
file operations. Starting another scan or operation cannot discard its creation
result. Completion retains creation undo but leaves later navigation and active
operation receivers intact; it does not restart their scans. A second
create-and-enter request is rejected while one is pending. Ordinary quit requests
wait for this worker, as for archives; force quit can still discard session state.
Tests exercise a later scan and a concurrent file creation, including both undo
records, overlap rejection, and ordinary quit handling.

## Navigation history

The CLI and TUI share `gravityfile_core::escape_terminal_text` for visible escaping
of control characters and Unicode direction-formatting characters. The TUI's
left/right fitting helpers escape before measuring or truncating, covering tree
and Miller names and dialogs that use those helpers. Miller preview filename
headings also escape explicitly. Ordinary Unicode remains unchanged; native paths,
selection identity, and operation inputs are not rewritten. Escaped display strings
are not a reversible or unique filename encoding, and truncated labels can still
be ambiguous. Text preview loading escapes controls and directional formatting
after tab expansion in both plain and syntax-highlighted spans. Highlighting
failures render the entire preview as escaped plain text. Miller directory/archive preview
names, archive link targets, and preview error messages also escape at display
time. Other direct-rendered diagnostics and editable input remain separate
surfaces requiring review. Caller-supplied prebuilt styled preview lines and image
protocol output are outside the text-loader escaping boundary.

Automatic and forced text previews share a reader capped at 10 MiB plus one
overflow-detection byte, independent of the initial metadata size. Oversized,
invalid UTF-8, and failed reads return errors instead of presenting a silently
partial text preview. A bounded input buffer supplies the total line count and
the first 500 displayed lines; CRLF removes one carriage return, preserving any
additional carriage returns as visible escapes. Unix text opens are nonblocking
and verify that the opened descriptor is a regular file, so a substituted FIFO
does not wait for a writer. Symlinks to regular files remain supported. These
checks do not provide a filesystem snapshot, a deadline for regular-file reads,
or a CPU/memory cap on highlighting and expanded display spans. Automatic binary
detection reads at most 1,024 bytes, rewinds the same inspected handle, and passes
it to hex rendering. Forced hex mode obtains length from its checked handle too.
Hex reads retain at most 8,000 bytes (500 rows), handle short reads through EOF
or the budget, and avoid narrowing large file lengths before applying the cap.
A pathname replacement cannot redirect an already opened binary preview, but
in-place modifications can still change its data. Archive and image paths require
their own qualification.

Image previews now use the same checked regular-file open. Encoded input is
buffered with a 32 MiB limit enforced during reading, plus one overflow-detection
byte. Format detection, dimension inspection, and decoding use that buffered
input and a single decoder, so reopening a changed pathname cannot bypass the
40-million-pixel check. Inspection failures stop decoding. Direct decoder use
preserves the image library's default output-allocation accounting. Decoder
initialization may still allocate before dimensions are available, and temporary
allocation limits are decoder-dependent. This is not an exact process-memory,
CPU, protocol-encoding, or filesystem-read deadline guarantee; concurrent in-place
writes may affect bytes while buffering. [Image input qualification](qualification/image-input.json)
includes a header-only pixel bomb and byte-limit boundaries, plus visible image
rendering and the TUI regression suite.

Directory previews use a bounded max heap to select the first 500 entries in
directories-first, name order, retaining at most 501 candidates during selection.
They enumerate the entire directory to compute its observed total and show that
total separately from visible rows. Enumeration and file-type errors fail the
preview instead of silently omitting entries or treating unknown types as files.
Regressions compare the selected prefix across forward and reverse 100,000-entry
streams. Memory retained for selection is bounded by entry count; enumeration
time, filesystem-call latency, filename byte lengths, and concurrent changes are
not bounded by this mechanism. Names remain lossy display strings, not native-path
identities or an authoritative inventory. `PreviewContent::Directory` now carries
`total_entries`; downstream constructors and exhaustive field patterns must account
for this field.

Archive previews retain at most 500 display entries and inspect up to 10,000
entries for metadata totals. ZIP entry-opening failures, including those beyond
the displayed prefix, fail the preview. ZIP and TAR entry-limit exhaustion also
fail explicitly; they do not return a zero size or partial sum as a complete
inventory. Size accumulation rejects overflow. Displayed TAR paths and link names
must parse successfully. These are inventories rather than extraction-safety
approvals: ZIP payload CRCs, hostile decoder resource usage, and concurrent
archive modifications are not fully qualified by preview success.

TAR-family previews open and check a regular file once, then enforce a 100 MiB
source-read budget and a 128 MiB decoded-read budget. Each budget may read one
extra byte to detect overflow and returns an error rather than a synthetic EOF.
XZ preview decoding also has a 256 MiB decoder-memory limit, with a 4 GiB
dictionary-request regression fixture. Plain TAR, gzip, bzip2, and XZ fixtures
verify ordinary inventories under these limits. TAR previews drain the decoder
after the TAR end marker, checking compression trailers and allowing at most
1 MiB of trailing zero padding. Gzip, bzip2 and XZ decoders process concatenated
members; tests accept boundaries inside headers/payloads/padding and reject
truncated or corrupt trailers and hidden later data. The same source/decoded
budgets apply throughout draining. These checks do not establish a total-process
memory or CPU bound, nor protect against concurrent changes to the input file.

ZIP previews use the checked regular-file opener and enforce a cumulative 64 MiB
read budget throughout central-directory parsing and entry inspection. Seeks do
not replenish it. Exhaustion remains an explicit budget error even if the ZIP
parser retries records or returns a different fallback result. The budget is
separate from the 100 MiB archive-file size check and may reject metadata-heavy
archives below that size. One additional byte can be read to distinguish exact
EOF from overflow. This bounds parser I/O, not all parser allocations, CPU time,
payload integrity, or concurrent changes to the opened file.

The application's preview scheduler permits one active load and one replaceable
pending request. Rapid selection or mode changes replace the pending request
instead of starting more blocking decoders. Completion starts only the latest
pending load and discards the old result even when its path/mode match a forced
reload. Clearing the preview drops pending work but retains observation of the
active worker, preventing view changes from bypassing the concurrency bound.
A worker ending without a result advances pending work or displays an error.
Regressions exercise 1,000 coalesced requests, same-path reloads, clear/re-enter,
and channel loss. The active decoder is not forcibly cancelled: a stalled load
can delay the latest preview, and runtime shutdown may still wait for blocking
work. This is a per-application scheduling bound, not a deadline or an OS resource
sandbox; callers of the standalone loader manage their own concurrency.

Syntax highlighting accepts at most 16 KiB per displayed line and 256 KiB across
the displayed preview. These byte budgets are checked before syntax detection,
including first-line regex matching. Larger inputs remain fully represented in
the bounded text preview as escaped plain text, with a plain-text header label.
Parser failures also fall back as a whole, avoiding incorrect state after a
skipped line. The budgets limit parser input, not worst-case regex execution time
or the memory required for escaped display spans; a hard timeout still requires
stronger isolation.

The line-oriented bulk rename editor rejects controls and directional formatting
in both original and edited filenames. Its rejected names remain accessible through
other supported path APIs; the editor does not silently rewrite them.

Directory tab labels truncate by terminal display width using the shared Unicode
text helper. They no longer slice a UTF-8 string at an arbitrary byte offset.
Both shared truncation directions now keep whole grapheme clusters and measure
their display widths, including emoji variation selectors. Regression coverage
checks ASCII, accented characters, CJK, combining marks, emoji sequences, flags,
keycaps, both truncation boundaries, and zero-width budgets. This prevents the
tab-label panic and splitting combined display characters; terminal-specific
emoji-width agreement remains platform qualification work.

Rename/create text input now keeps its cursor at grapheme boundaries, advances
by UTF-8 byte lengths after insertion, and moves/deletes whole graphemes. Edits
that join neighboring clusters snap the cursor to the new boundary. Ctrl-W
removes trailing whitespace and the preceding word without slicing inside a
multibyte whitespace character. The input modal measures visible columns and
highlights the current grapheme using the byte cursor. Filename validation uses
the shared operation validator. Regression coverage includes incremental Unicode
typing, movement, deletion, joined flags, Unicode whitespace, narrow rendering,
and highlighting after an accented character. The command palette reuses this
editor, retaining its empty-Backspace cancellation and clear-on-submit behavior.
Both widgets share display-width clipping and cursor highlighting; externally
supplied byte cursors are clamped to a preceding grapheme boundary before slicing.
Command regression coverage exercises Unicode paths, joined emoji deletion,
submission, and every byte cursor across narrow widths. Search keeps its existing
character-index cursor API but moves and deletes whole graphemes, recomputing a
valid boundary when edits join clusters. Its overlay converts the cursor to a
byte offset and uses the same width-clipped renderer. Search regressions cover
incremental Unicode input, movement in both directions, both deletion keys, and
flag clusters joined by deletion.

Glob search compiles its existing `*`/`?` language once per query into the regex
engine instead of recursively exploring wildcard splits for each path. Matching
remains anchored, lowercase-based, and newline-aware; other pattern characters
remain literal. Repeated adjacent stars are collapsed. Regex/glob compilation
errors are displayed separately from valid searches with no matches and cleared
when the query or mode changes. Regression coverage includes a repeated-wildcard
non-match that would require exponential recursive work, Unicode/newline/literal
matching, and recovery from invalid regex syntax.

The interactive app enables background query execution before entering its event
loop. Each search state permits one running query and one coalesced pending
query; edits, mode changes, index replacement during active search, clearing,
and dismissal invalidate old output. The worker shares an immutable path index
through `Arc`, checks cancellation between candidates and fuzzy highlighting
results, and returns at most 1,000 results through a capacity-one channel. Matcher
initialization and pattern compilation also run in the worker. The UI polls
completion on its existing background-work timer, shows `Searching…`, and ignores
Enter until the current query finishes. Worker startup/disconnection errors are
visible. Dropping search state requests cancellation without blocking shutdown.
Standalone `SearchState` retains synchronous execution unless background mode is
explicitly enabled, preserving the algorithm benchmark and existing API behavior.

Cancellation cannot interrupt one matcher/regex call or pattern compilation.
Result rendering remains on the UI thread. Index memory, thread startup cost,
physical terminal latency, and shutdown timing are not bounded by this change. Regressions exercise completed stale results,
coalesced edits, replacement indexes, mode/error recovery, clear/dismiss/drop
cancellation, pending feedback, and Enter while pending.

App construction uses the quick listing's canonical root consistently for scan
and view state, tree navigation, the initial tab, and plugin read scope. It does
not independently resolve the path a second time. Failed initial listings retain
the supplied path and error. Regression coverage creates relative, dot-component,
and Unix symlink aliases and verifies background search finds the same file from
each. This does not make later path-based operations immune to concurrent alias
replacement or change all subsequent navigation argument handling.

Search index construction first resolves the selected view root within the scan
tree, then visits only that subtree. Previously, a nested view was rejected at
the scan root and produced an empty index. Traversal now uses a stack of child
iterators and one mutable path, preserving preorder without recursive calls or
a directory-width-sized stack of allocated paths. Regression coverage checks
root/nested scopes, Unicode names, sibling exclusion, missing/outside roots, and
actual search activation in a nested directory.

Interactive index construction runs on a separate worker holding the scan tree
through `Arc`, with one running index build and one coalesced replacement. Query
edits wait for the latest index and do not restart indexing. Scope replacement,
dismissal, and state drop request cancellation; traversal checks cancellation
between nodes. Completed obsolete indexes are rejected. Index worker failures
remain visible across query edits until a new index is requested. The previous
installed path index is released by the next index worker, reducing large frees
on activation. Regression coverage blocks an index build while replacing scopes,
checks latest-query execution, dismissal, and worker-disconnection recovery.

Subtree lookup checks cancellation before starting and between child entries,
so a wide directory does not require finishing its entire child-list search after
cancellation. A deterministic regression interrupts a 1,000-child lookup after
ten checks; nested search activation also exercises background indexing and query
execution together. Individual path operations and allocator calls remain
uninterruptible. Index storage is still proportional to path count and total path bytes. Discarding
an already-completed obsolete index may free its storage on the UI thread. There
is no measured end-to-end large-tree activation or cancellation-latency guarantee.

Search retains at most 1,000 results during collection. Fuzzy matching scores all
candidates into a bounded heap of score/index pairs, then clones paths and
computes highlighting only for the winners. Ranking remains descending by score,
with original input order breaking ties. Glob and regex results share a fixed
score, so collection stops at the existing 1,000-result limit without scanning
or materializing later matches. A reference-sort regression over 1,800 candidates
checks ranking, late high-scoring matches, highlighting, and equal-score order.
The source path index and individual path/query sizes remain outside this bound.

Back navigation checks whether a saved destination is present in the current
tree. If a bookmark or other navigation replaced that tree, Back prepares a
quick listing before consuming history. A failed listing preserves both history
stacks, the current tree, and the view, and reports the error. Successful root
replacement cancels prior background work and clears old analysis data. Parent
navigation beyond the scan root likewise appends forward history only after a
successful listing and adopts its resolved root. Regressions cover cross-root
bookmark/Back sequences and an unavailable history destination in both layouts.
Reloading uses quick listings rather than restoring a full historical scan;
filesystem changes can therefore affect restored selection and contents.

Bookmark jumps resolve saved paths and report unset, inaccessible, and non-directory
destinations. A destination absent from the loaded tree gets a prepared quick
listing before navigation state changes, even when it lies under the old scan
root. Failed preparation preserves the previous tree and navigation history.
Successful tree replacement cancels old background work, adopts the listing's
resolved root, and invalidates analysis state. Jumping to the current resolved
directory does not append history. Regressions cover invalid bookmarks, missing
descendants in a partial tree, dot components, and Unix directory aliases. Path
resolution/listing remain synchronous and do not prevent concurrent filesystem
replacement between calls; saved bookmark spellings are not rewritten.

The `:cd` command navigates the loaded tree. A leading `/` is relative to the
scan root; `/` and `~` retain their existing return-to-root behavior, and the
standalone `..` retains history-back behavior. Other paths normalize `.` and
parent components lexically within the scan root. This is virtual-tree navigation,
not filesystem canonicalization or shell path resolution. Missing/unloaded paths,
non-directory targets, unsupported prefixes, and attempts to leave the scan root
produce visible errors without changing navigation history. Resolving to the
current directory is a no-op. A regression covers compound paths, Unicode and
repeated spaces, file/missing targets, root escape, and unavailable trees.

Path/name arguments to navigation, rename, creation, create-and-enter, extraction,
and compression preserve their internal whitespace exactly. The parser strips
surrounding command whitespace and separators after the command word, but does
not collapse spaces, tabs, or Unicode whitespace inside the argument. Quotes
remain literal; this is not shell parsing. Filename validation still applies
before mutation. Parser tests cover the command aliases, and a filesystem
regression distinguishes directories and filenames containing one versus two
spaces to verify that the submitted path is the path acted upon.

Unknown palette commands, unsupported theme/layout/sort options, excess option
arguments, missing compression names, and malformed bookmark keys produce a
visible error. Invalid sort input no longer cycles sorting, and a multi-character
bookmark argument cannot silently target its first character. Empty commands and
documented no-argument actions retain their behavior. Parser and application
regressions verify that invalid commands preserve sorting, bookmarks, navigation,
and the inactive worker state.

Create-and-enter, `cd`, bookmark jumps, and search-result navigation record the
origin cursor from the active Tree or Miller layout and clear forward history
when starting a new navigation branch. `cd` also resets the Miller cursor and
refreshes both layout caches for the destination. Regression coverage exercises
these entry points in both layouts, including file and directory search results.

## New filename validation

UTF-8 create/rename inputs use an application limit of 255 bytes and reject
control characters, separators, and selected problematic edge whitespace/dots.
This conservative byte budget is not a universal filesystem component limit.
On Windows, additional character and device-name restrictions apply, including
COM/LPT names using superscript ¹, ², and ³ and reserved basenames followed by
extensions, as specified in [Microsoft's naming rules](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file).
The Windows-specific validator is exercised by host tests without applying those
restrictions to Unix. Actual Windows filesystem behavior remains a target-CI
qualification requirement.

## Single-item rename lifecycle

After submitting a rename, file creation, or directory creation, the TUI waits
in a dedicated operation mode until completion. Refresh, undo, navigation, and
quit input cannot replace the result receiver during this interval. Escape and
quit explain that filesystem work is still pending; these APIs do not expose
token cancellation. The submit handler preserves the worker's mode, and terminal
completion restores normal input while recording successful creation/rename undo
or displaying failure details. Regression coverage submits each operation through
the input dialog, exercises intervening keys, and checks both success and existing
destination conflicts. A worker channel failure also releases this mode with an
unknown-outcome error.

Rename progress is best-effort, while completion remains reliable. Receiver
closure is checked before dispatch and again on the worker before filesystem
work and before the rename syscall. Active calls are awaited and cannot be
interrupted. Source/destination metadata checks run on the blocking worker;
non-NotFound errors are reported rather than treated as absent paths. Same-name
requests validate source existence and return zero mutations on success. The
final no-replace syscall still determines whether the rename won a concurrent
destination race. Source identity and parent-path replacement races remain
outside this guarantee.

## Deletion delivery and reporting

UI deletions use system trash. Progress snapshots are best-effort; a full queue
does not stall deletion. A closed receiver stops subsequent items, with another
check before queued blocking work starts. An already active trash/filesystem
call is awaited and cannot be interrupted. Empty batches emit completion.
Accumulated selected size saturates on overflow and is not a measurement of
reclaimed disk space. Trash completion messages report moved size without
claiming space was freed. Completion carries the actual mode, and the internal
permanent-deletion path is labeled accordingly; the UI still only initiates trash
operations. Per-path failures are logged and forwarded to the latest-operation
Errors view, surviving rescan refresh. Deletion clears an older operation summary
so its result is visible, and later successful completion clears old diagnostics.
Persistent deletion journals and durable diagnostic history remain unfinished.

## Undo execution and safety

Undo history has a maximum operation count; a zero limit disables retention.
Recording still allocates an operation ID when retention is disabled. The limit
does not bound the number of paths inside one operation or its memory usage.

Undoing creation now removes only an empty item of the expected type. Directory
undo uses nonrecursive `remove_dir`, preserving contents added after creation.
File undo rejects nonempty files, and both reject replacement symlinks or changed
file types. Copy/create undo failures now retain path-specific errors, while
unsupported deletion undo reports failure rather than a successful completion
with an error message. Tests cover empty-item success, later contents, changed
types, symlink preservation, missing paths, and unsupported operations.
These checks do not establish original file identity or prevent a concurrent
writer/replacement between metadata inspection and removal. Copy undo instead
moves destinations into persistent sibling recovery journals, preserving later
edits and added children without recursive deletion. It retains symlinks as links
and supports explicit no-replace restoration through `grav recover`. Successful
undo does not reclaim disk space; retained data has no automatic expiry or quota.
The journal is written before mutation and remains on failure or process exit.
Original object identity and concurrent ancestor protection remain unfinished;
a replacement at a recorded destination may be retained too. Safe retry is not
provided. See [copy undo retention](RECOVERY.md#copy-undo-retention).

Undo now exposes `execute_undo_with_cancellation`; the original API delegates
with a fresh token. Progress updates are best-effort, and a closed result receiver
cancels further work. A supervisor awaits the worker after receiver closure;
active blocking filesystem calls cannot be interrupted. Move undo passes the
token through the existing staged-move implementation, and rename undo uses the
cancellable transactional rename with full native paths instead of reconstructing
a lossy filename in the current parent. The UI supplies its cancellation token
and keeps the result adapter alive through cancellation to receive partial
completion. Tests cover pre-cancellation across operation types, a closed
receiver, progress backpressure, UI completion delivery, and original-path rename
restoration. Arbitrary-byte filename coverage is Linux-only; APFS tests use
Unicode names. Completed changes are not rolled back across multiple undo items,
and failed/remaining undo records still need a retry protocol.

The TUI retains an original record while undo runs. Failed, cancelled, skipped,
or diagnostic-bearing completion returns it to the bounded undo log marked
`review_required`; unexpected worker closure does the same. Successful completion
releases the pending record. Review-required entries preserve the original ID,
timestamp, operation paths, and description, but `UndoLog::pop` will not replay
them or skip past them. The public undo executor also refuses such an entry.
Older serialized entries default to ordinary replay eligibility when the new
field is absent. Partial-undo regression coverage verifies that a successful
removal followed by failure retains the record and cannot delete new data at the
already-completed path on another Undo request.

The TUI releases a pending undo record only when reported success exactly matches
the original top-level item count and completion has no cancellation, failures,
skips, or diagnostics. Under-counted or over-counted success is retained for
review even if the worker supplied no errors, and the status is unsuccessful.
`UndoableOperation::item_count` supplies the same count used by original-path
review pagination. A regression covers zero, partial, exact, and excessive
success counts for a two-item record. This validates reported accounting; it does
not independently prove that the corresponding filesystem effects occurred.

Review records also retain `last_attempt` completion evidence: receipt time,
reported counts/cancellation/bytes, and original path-specific errors. The TUI
copies this evidence before replacing transient diagnostics; later operations do
not erase it. A missing completion remains `None`, exported as JSON `null`, rather
than invented zero counts. Review rows summarize reported counts; the full export
includes all diagnostics with native path encoding. These aggregate counts do not
identify each successful path or prove rollback after an error. Receipt time is
not a filesystem commit timestamp. Error retention adds memory proportional to
the retained diagnostics and remains subject to the history/session limits.
Tests cover partial completion, replacing live diagnostics, missing completion,
and non-UTF-8 diagnostic paths in exported evidence.

This is conservative in-memory retention, not resumable undo or a durable audit
journal. Records remain subject to history capacity and session lifetime. There
is no per-item success map or safe retry/dismissal flow yet; even
cancellation before mutation requires review. Holding the original
record also duplicates operation-path memory while the worker owns its input.

The `:undo-review` command opens a bounded snapshot in Errors & Warnings. It
shows original paths and intended reverse actions, including both locations for
moves and renames, with each item's outcome explicitly marked unverified. It
does not execute filesystem changes or consume history. Existing operation and
scan diagnostics remain available after the review rows; a new operation clears
the old review snapshot so fresh errors take precedence. Each page materializes
at most 1,000 original paths. `:undo-review` opens page one; `:undo-review 2`
opens page two. The status reports current/total pages and total retained paths.
Whole operations before a requested page are skipped by their path counts;
indexed iterators skip directly inside path vectors, avoiding earlier-path
formatting and cloning. Invalid, zero, or out-of-range pages preserve the current
snapshot. Pages are rebuilt from current retained history rather than a durable
snapshot, so history eviction or new records can shift page boundaries. TUI file export is available through `:undo-export <new-file.json>`. Regressions check move/rename context, diagnostic preservation,
no mutation, the 1,000-row boundary, final-page contents, and invalid page inputs.

The `:undo-review errors [page]` variant displays retained attempt diagnostics,
independently of original-path pagination. Each page holds at most 1,000 errors,
with original native paths and messages prefixed by the undo record ID. Live
operation and scan diagnostics remain after the review rows. Zero retained errors
reports that no diagnostics are available and directs users to original records
and completion status; it does not claim success. Invalid pages preserve the
current snapshot. A 1,002-error regression checks page boundaries, exact final
messages, preservation of live errors, switching to original paths, and empty
history. Error records can describe operations that partially changed files; they
are evidence for inspection, not a list of safe retry targets.

`UndoLog::write_review_json` streams all retained review-required entries to a
caller-provided writer using schema version 1. The separate review-only format
preserves entry IDs, timestamps, descriptions, original operation kinds, and
path roles. Unix paths include authoritative byte arrays; Windows paths include
UTF-16 code units. Display strings are informational and may be lossy. The writer
does not clone the whole history or assemble a full JSON value; per-path display
conversion and Windows code-unit collection may allocate. This does not change
the existing `UndoEntry` serialization format. There is no importer or replay API
for review documents, and no per-item outcome is inferred from the original log.

Writer errors propagate without consuming history and may leave partial output.
Callers using a raw writer must separately stage, sync, and publish. The
`UndoLog::export_review_json` file API provides private same-directory staging,
streamed serialization, explicit flush and file sync, then atomic no-replace
publication. Unix files are created with mode 0600. Existing files and symlink
destinations are preserved; serialization and publication failures clean staging.
Tests inject partial serialization failure and a competing destination writer,
and check symlink preservation and Unix permissions. The API is synchronous and
should run on a worker in interactive callers. Parent-directory fsync, protection
against concurrent ancestor replacement and automatic persistence are not
provided by this change. Serialization tests cover all six
operation kinds, move/rename direction, non-UTF-8 Unix path bytes on this host,
and writer failure. Windows UTF-16 tests are present but not host-executed.

The command `:undo-export <new-file.json>` snapshots all retained incomplete
records and exports them on a blocking worker through an independent completion
channel. Relative destinations resolve beneath the current view; absolute paths
are used as supplied, and internal spaces are preserved. History now stores
immutable entries behind `Arc`; review snapshots share those entries rather than
cloning path vectors, and remain valid after live history changes. One export may
run at a time. Ordinary Quit waits for completion; explicit force quit retains
its existing immediate-exit behavior. Completion reports the destination or a
failure without consuming live history. The command refuses empty review history
and never overwrites an existing file. Tests cover snapshot identity/lifetime,
exact destination spelling, concurrent-export rejection, ordinary quit, successful
JSON publication, and destination conflict. This is user-requested export, not
automatic crash persistence or an undo-replay/import protocol.

The TUI enters a dedicated undo-in-progress mode before accepting more input.
It blocks repeat undo, refresh, navigation, and other operations from replacing
the result receiver. Escape and quit requests cancel cooperatively and remain
in this mode until completion is processed; they do not discard pending results
or exit during active filesystem work. A regression exercises repeated undo,
refresh input, cancellation, and the transition back to normal mode. This
ownership guarantee does not add durable recovery or restore popped undo entries.

## Progress and operation diagnostics

Selection totals in the header and deletion confirmation, stale-directory totals
in the age view, and `AgeReport::total_stale_size` saturate at `u64::MAX`.
Extreme or overlapping entries therefore cannot wrap the displayed total or
panic on arithmetic overflow. Regression coverage renders oversized selections
and age reports and checks the public aggregate. Saturation does not deduplicate
overlapping paths or establish physical reclaimable space.

Archive success requests a refresh without replacing an occupied scan/operation
result channel. If another worker is active, the request remains pending until
its completion releases the channel or starts a fresh scan. Multiple archive
refresh requests coalesce, and a new scan consumes the request. Active operation
status messages remain intact. Regression coverage completes both compression
and extraction while file creation is pending and checks that creation finishes
with its undo record. An unexpected worker-channel failure still reports an
unknown outcome rather than automatically retrying filesystem work.

Completion success requires no cancellation, zero failed items, and an empty
error list. If a producer supplies diagnostics with a zero failure count, both
the library summary and TUI show their count and report an unsuccessful outcome.
Diagnostics still appear in the errors view, and undo information for completed
items is retained. Tests cover cancellation/failure/diagnostic combinations and
the TUI's handling of inconsistent counts; this does not validate the truth of
every producer-supplied counter or undo record.

Operation progress percentages are bounded to 0–100, including when processed
bytes or files exceed stale totals. Byte totals take precedence, file totals are
the fallback, and unknown totals display zero. Counter increments saturate at
their integer limits rather than wrapping or panicking. The TUI progress bar
uses this shared calculation. Percentages remain estimates; only the terminal
result establishes completion and success. Tests cover unknown totals, normal
ratios, stale totals, and maximum counter values.

Copy, move, undo, and single-entry update modes display an immediate pending
modal even before their first worker progress update. It shows the operation
message or an explicit waiting message without a fabricated percentage. The
measured-progress modal replaces it when an update arrives. Rendering coverage
checks each mode with and without a pending message, tiny and offset rectangles,
and the transition to worker progress.

Filesystem operations share one background-slot handoff: cancel the previous
token, release its receiver, create a fresh token, and clear scan/analysis status
before installing the operation receiver. Paste, undo, rename, creation,
create-and-enter, approved deletion, and bulk rename use this handoff. Existing
tree data remains visible. Deletion transfers ownership only after plugin veto
checks succeed. Regression coverage checks cancellation, receiver release, fresh
tokens, retained trees, and operation completion for six entry points. Cancelling
the old token is cooperative and does not synchronously join an active syscall.

The main event loop uses Tokio's randomized select polling order, allowing ready
background results to compete with sustained terminal input. Each input dispatch
drains at most 32 additional events. Redraw timers skip missed ticks instead of
replaying accumulated frames after a stall. This avoids fixed input priority and
catch-up redraw bursts; it does not establish a deterministic latency bound.
Physical-terminal latency under sustained input remains a qualification gap.

If a background result channel closes without completion, the TUI cancels its
token, clears stale progress and success messages, and releases operation modes
including copy, move, undo, deletion, bulk rename, and conflict resolution.
It reports an unknown outcome and asks the user to inspect affected paths before
retrying. The existing tree, diagnostics, and retained undo history remain; no
success, rollback, or automatic retry is inferred. Unrelated input modes remain
open. A closed-channel regression covers these states. This recovery path cannot
prove that detached filesystem work has stopped or reconstruct lost results.

Copy and move workers share the TUI's active cancellation token. Starting a
paste cancels the previous scan. Escape and quit requests during a paste signal
cancellation and retain the operation mode and result receiver until completion;
aborting a conflict follows the same lifecycle. A conflict already queued when
cancellation occurs is answered with Abort instead of reopening a modal. The
active paste kind is stored independently of the clipboard, so clearing the cut
clipboard cannot turn a move's conflict-resume state into copy mode. Tests cover
copy/move cancellation before work and at a conflict, source/destination
preservation, final cancellation summaries, and resuming a skipped move conflict.

Copy, move, and undo progress now reserve channel capacity before constructing a
snapshot. Full or closed queues do not clone error lists or paths for discarded
updates; a closed queue still cancels the operation. Tests make snapshot
construction fail if attempted on a full/closed queue and verify 10,000 dropped
updates construct no extra snapshots, then resume delivery when space returns.
This removes discarded-snapshot allocation work; successfully queued snapshots
still clone retained diagnostics. `OperationProgress::add_error` now keeps at most
1,000 records. Each inserted record has an 8 KiB combined native-path/message
payload limit. Oversized details become an explicit omission record with their
represented count and a size-limit explanation; paths are never truncated. Up to
1,000 size-compliant errors retain full detail; beyond that, the first 999
remain and the last record explicitly counts omitted diagnostics. Its path is
empty and `omitted_count` is nonzero. Ordinary diagnostics have a zero omission
count, including older deserialized records that lack the field. Progress totals,
completion summaries, and UI counters sum represented diagnostics with saturating
arithmetic rather than treating the omission record as one error. Failed-item
counts remain independent and unchanged. Completion preserves the omission
record, and undo-review JSON carries its structured count and message.

This bounds retained record count and logical path/message payload bytes through
the progress accumulator, not allocator capacities, pre-insertion allocations,
manually constructed public vectors, scan warnings, or every
operation-specific error source. Omitted path details cannot be reconstructed
from the summary and are not silently described as retained. Regressions cover
100,000 accumulated errors, exact cap boundaries and retained-prefix order,
legacy deserialization, 1,100 real failed undo items, and exported omissions.

The TUI now forwards final diagnostics from copy, move, rename, create, and undo
completion instead of discarding them in adapters. The latest operation's retained
path/message list, including any omission summary, appears first in the Errors view, separately from scan warnings,
and survives automatic rescanning. Completion with diagnostics opens that view;
the next operation completion replaces the previous operation list (including
clearing it on success without diagnostics). Navigation and counts combine both
lists, while operation diagnostics never offer the broken-symlink delete action.
An end-to-end undo regression checks message forwarding, rescan retention,
rendered path/reason, mixed-list selection, and preservation of the user's data.
This remains session-local latest-operation history, not a persistent audit log.

## Selection and rename inputs

Single-file action targets and selected-item details now follow the active view.
Previously, duplicate, age, and diagnostic views could reuse a hidden Explorer
cursor for rename/copy/cut actions. Expanded duplicate-file selections, stale
directories, and the combined diagnostic list now resolve their own paths;
collapsed duplicate groups and invalid selections have no single-file target.
Missing diagnostic paths do not borrow another file's metadata. Regression tests
cover clipboard targets, rename-dialog context, details, collapsed headers, and
invalid selections in both Explorer layouts. Tree-node lookup now follows path
components and searches only each level's children, without allocating a path
for every node in unrelated subtrees. Wide individual directories still require
linear child lookup; no constant-time lookup or latency claim is implied.

The single-item rename dialog no longer pre-fills replacement characters for a
non-UTF-8 filename. It preserves the original native path as the operation
context, explains the encoding limitation, and starts with an empty field that
requires an explicit new UTF-8 name. Empty submission fails before dispatching
filesystem work. Valid Unicode names, including a literal replacement character,
retain their exact prefill. Filesystem roots cannot enter rename mode. Tests
exercise dialog state and submission boundaries without requiring APFS to create
invalid-UTF-8 names. Arbitrary-byte destination entry is not supported by this
text dialog; native-path source handling remains intact.

Packaged-source verification isolates Cargo build output by a digest of all package
archive hashes, the generated workspace manifest, and the copied lockfile. Cargo
archives normalize source timestamps; a shared target directory previously reused
an older analysis dependency across changed package contents. The integrated gate
caught this when the packaged overflow regression linked the old non-saturating
implementation while checkout tests passed. Content-specific cache directories
prevent that cross-input reuse. Repeated identical package inputs may reuse their
cache. The verifier now holds an exclusive OS file lock across cache selection,
retirement, and all Cargo checks/tests. A competing verifier fails with a retry
message instead of pruning a live build. Only obsolete 64-character lowercase
hash directories in the dedicated cache root are retired; unrelated entries and
symlinks are preserved, and active/root/lock symlinks are rejected. The current
content cache is reused, limiting managed retention to one snapshot. This bounds
snapshot count, not bytes: one build can still consume gigabytes. Tests cover
retirement, lock contention, failure cleanup, symlink preservation, and process
exit without context cleanup. Windows locking code is present but not executed
on this host. Legacy non-content cache directories are not pruned automatically.

Recovery discovery retains valid records alongside per-path inspection failures.
The CLI prints accessible journals even when a neighbor is active or malformed,
reports each issue, and exits unsuccessfully for incomplete discovery. It reports
absence only when both records and issues are empty. The strict legacy library
API remains available; `discover_recovery_report` exposes partial observations.
Tests cover active locks, malformed neighbors, valid retained content, empty and
unopenable parents, deterministic ordering, and CLI output/exit behavior.
Enumeration remains nonrecursive and memory usage grows with matching entries.

`grav recover <parent> --json` emits a versioned inspection document with native
paths, complete/partial status, valid records, and per-path issues. It emits
partial data before returning failure; diagnostics remain on stderr. The flag
conflicts with restore actions. Root-open failure produces no JSON document.
The streaming library writer shares the undo-review native-path encoder and
propagates write errors without consuming report data. Tests exercise invalid
Unix path bytes (Windows surrogate tests are conditional and not run on this
host), writer failure, complete empty reports, partial reports with and without
valid records, and CLI restore-option conflicts. This is not an import/replay
format or a new filesystem snapshot guarantee.

Unix recovery directory sync requires a directory at open time and rejects leaf
symlinks, using nonblocking opens to avoid waiting on a replacement FIFO. The
regression test bounds response time and verifies ordinary files remain intact;
the operations suite includes existing publication and process-exit recovery
tests. This does not establish descriptor-relative ancestor safety or Windows
directory durability.

Recovery discovery also reads `.gravityfile-rename-*` intent files and exposes
them separately in JSON `rename_intents`. It bounds each control-file read at
1 MiB, validates schema/purpose/native encoding and the declared staging path,
and rejects symlink control files. Malformed entries do not hide valid neighbors.
The CLI labels all rename outcomes unverified and never restores from intent
records. Inspection does not probe recorded source/destination paths. New
batches hold an exclusive lock per staging directory through mutation and
cleanup; discovery acquires it before reading intents and reports active locks
as issues. New records require their lock file. Legacy records can be inspected
without coordination and explicitly report `locked_during_inspection: false`.
Advisory locking does not prevent noncooperating filesystem mutation. Existing strict `discover_recovery` remains scoped to overwrite
and copy-undo journals; the report API includes rename intents. Process-exit
fixtures, native-byte paths, invalid/oversized records, symlinks, and CLI read-only
behavior are covered on this host. Aggregate discovery memory remains unbounded.


### Bounded scan warning detail

Full scans and quick listings accumulate at most 999 detailed warnings, each with
at most 8 KiB of combined native-path and message bytes. Excess or oversized
warnings contribute to one final `WarningKind::Omitted { count }` record with an
empty path and an explicit explanation. Paths are not truncated. Observed counts
use saturating arithmetic; `FileTree::warning_count()`, CLI scan status/summaries,
and emitted scan progress count represented warnings rather than retained rows.
Any retained omission keeps `has_warnings()` true and the scan report incomplete.
The TUI does not offer broken-link deletion for omission records and labels its
retained rows separately from the represented warning total.

`WarningKind::Omitted` is a new public/serialized variant: consumers matching the
enum or parsing serialized warnings must support it. Earlier warning variants
retain their representations. This bounds each scanner's logical retained detail,
not the scan tree itself, allocator capacity, pre-insertion allocation, manually
constructed `FileTree` values, or manually supplied incremental/final warning batches merged by the TUI.
The current background scanner sends warnings in the final result, with no
producer for incremental warning events.
No omitted path details can be recovered from the summary. Qualification includes
100,000 synthetic warnings and full/quick scans of 1,100 real broken symlinks.


CLI warning-count qualification exercises 1,100 real broken symlinks through
`scan`, `duplicates`, `age`, and `export`, using both binary names in strict
mode. Every JSON envelope must remain partial with `scan_warning_count: 1100`,
and every command must exit unsuccessfully after emitting the report. This
caught an analysis-status consumer that still counted the 1,000 retained rows;
analysis status now uses `FileTree::warning_count()` as scan/export already did.


The Errors view renders undo-review, operation, and scan warnings in the same
order used by selection and keyboard actions. Undo-review rows have an explicit
label and contribute to tab/header counts. A rendered-buffer regression covers
all three sources together, verifies the selected broken-link deletion hint, and
checks that Delete confirms only that displayed scan path when no paths were
previously marked. This fixes an omitted undo-review rendering prefix that shifted
visible rows relative to action selection. [Evidence](qualification/warning-rows.json)
retains the failing reproduction and passing TUI qualification.


Scan tree assembly checks cancellation before creating directory nodes, between
children, and around each directory's sibling sort. Interruption returns
`ScanError::Interrupted` rather than a partial success tree. Each directory sorts
only its immediate children; completed descendants are already sorted, avoiding
repeated recursive sorting at every ancestor. Ordering remains descending size
with a name tie-break. [Qualification](qualification/tree-cancellation.json)
checks deterministic mid-assembly interruption and nested ordering/aggregation.
Cancellation still has no hard latency bound during individual filesystem calls,
one sibling sort, or cleanup of collected/partially built data. Recursive depth
and complete scan-tree memory remain separate limits.


Optional scan-time hashing checks cancellation before opening each input. Unix
opens use `O_NONBLOCK`, and the opened descriptor must be a regular file before
reading. A file substituted with a FIFO therefore fails with a scan warning
rather than waiting for a writer. Symlinks resolving to regular files remain
supported. [Qualification](qualification/hash-open.json) covers those behaviors
and cancellation before a missing-path open. This is not a snapshot guarantee:
traversal/open identity changes, concurrent in-place writes, growing inputs, and
blocking regular-file reads or metadata calls remain possible.


Scan-time hashing reads no more than the opened file's initial length plus one
byte for growth detection, without buffered read-ahead beyond that boundary.
Premature EOF or extra data returns a warning and no stored hash. The opened
handle's length and modification time are checked again before accepting the
digest. [Qualification](qualification/hash-length.json) verifies byte boundaries,
empty inputs, short reads, and cancellation between reads. This is best-effort
consistency: same-length edits with unchanged/restored timestamps, unavailable
modification times, traversal/open identity changes, and blocking filesystem
calls remain possible. The bound scales with observed file size and is not a
fixed scan-wide byte or time budget.


Before scan-time hashing, metadata from the opened handle is compared with the
traversal observation: length, available modification time, and Unix device/inode
identity must match. A mismatch yields a read warning and no content hash attached
to the observed node. [Qualification](qualification/hash-identity.json) includes
a Unix file replacement with equal size and restored modification timestamp,
which must still fail the identity check. Non-Unix identity extraction currently
returns no identity, so those platforms receive only length/mtime comparison.
This does not detect every same-inode edit, restored timestamp, inode reuse, or
mutation after the final check; it is not an authoritative filesystem snapshot.


Scan-owned Git enrichment checks cancellation before repository work/status
queries, between returned status entries, and before annotating each tree node.
The scanner's final cancellation check rejects the scan rather than returning
partially applied Git annotations. Existing public Git helpers remain non-cancellable. [Qualification](qualification/git-cancellation.json)
includes deterministic interruption of a 1,000-node annotation and reuse of a
cache after pre-cancellation. These checkpoints do not interrupt the underlying
libgit2 discovery/status calls or filesystem canonicalization, nor bound cache
memory, recursive depth, or cleanup latency.


Git status queries use literal subtree pathspecs, so brackets and wildcard
characters in directory names do not become matching patterns. Repository-root
queries use no pathspec restriction. Failed canonicalization or relative-path
conversion stops initialization rather than widening to the whole repository.
On Unix, the Git cache constructs paths from raw status bytes instead of skipping
non-UTF-8 entries. [Qualification](qualification/git-pathspec.json) includes
synthetic repositories with pattern-like and shared-prefix siblings on macOS and
Linux, plus invalid UTF-8 directory/file names on Linux. Tree display names remain
lossy, so native cache lookup support does not establish lossless annotation for
every file-tree node. libgit2 query latency and cache resource use remain separate limits.


Public `apply_git_status` refreshes annotations on reused trees, including clearing
previous statuses when the current cache has no entry. A committed file no longer
remains marked staged merely because the entire repository became clean. Fresh
scanner-owned trees retain the clean-cache fast path without an extra node walk.
[Qualification](qualification/git-refresh.json) exercises untracked, staged, and
committed states on one tree in a temporary repository. Callers should inspect cache initialization errors or tree warnings to distinguish
a failed refresh from absent/clean annotations.


Git caches expose the most recent initialization error separately from ordinary
absence of a repository. Status-query failures, non-not-found discovery failures,
and subtree normalization failures are retained. Enrichment adds a bounded scan
warning on failure, so the filesystem tree remains available but reports are
partial. Public refresh preserves prior annotations when lookup fails rather
than clearing them as if clean; the warning identifies their unverified state.
[Qualification](qualification/git-failures.json) uses a real corrupted Git index,
checks retained annotations, and verifies the cached error clears after repair.
Not-found discovery remains normal absence. Queries have no deadline, and this
classification does not prove coverage of every libgit2 or filesystem failure.


Git enrichment retains ignored directory prefixes within the literal selected
subtree instead of requesting every ignored descendant from libgit2. Explicit
statuses and clean tracked exceptions take precedence over inherited ignored
status. A regression fixture with 200 generated files requires at most three
explicit cache entries; mixed tracked/ignored fixtures cover clean and modified
tracked files and final tree annotations. Canonical lookups use path components
and perform no existence check, so a removed descendant can still inherit the
cached annotation. This is observational state, not a filesystem snapshot.

The earlier [ignored-file qualification](qualification/git-ignored.json) used
recursive enumeration, which stalled qualification on a checkout with large
ignored build caches. The replacement disables recursive ignored enumeration;
libgit2 can still inspect directories containing tracked entries. Query memory
and latency depend on indexed files, changed/untracked paths, and ignored
prefixes. Clean indexed entries are collected temporarily before retaining only
exceptions beneath ignored prefixes. There is still no Git query deadline or
total memory ceiling. [Current qualification](qualification/git-prefix-release.json)
records the replacement and its integration checks.

Clean tracked exception compaction checks cancellation between paths and after
the final path, including an empty cache. Cancellation discards explicit statuses,
ignored prefixes, and tracked exceptions together, preventing partially compacted
annotations from being reused. [Scoped qualification](qualification/git-compaction.json)
covers cancellation before, during, and after candidate processing on macOS and
Colima. Releasing candidate allocations still takes time, and compaction briefly
holds both candidate storage and the retained set; no hard latency or memory
bound is implied.

Git annotation walks nodes iteratively, retaining traversal order and per-node
cancellation checks. Ordinary single-component names reuse one path buffer with
heap frames proportional to tree depth; unusual names in publicly constructed
trees retain parent paths to preserve `Path::join` behavior. A 4,000-level
in-memory tree is annotated and dropped on a 128 KiB thread stack on macOS and
Colima. [Qualification](qualification/git-depth.json) does not establish equivalent
filesystem depth support, a total memory/time ceiling, or recursive serialization
and debug safety.


Full scans stamp `scan_duration` and `scanned_at` after Git enrichment and the
final cancellation check, so completion metrics include enrichment and warning
handling. These values may be larger/later than prior versions that stamped them
before Git processing. Git-cache reinitialization drops its previous hash table,
including on pre-cancellation, instead of retaining capacity from a larger prior
subtree. Cancellation while copying statuses drops that table as well.
[Qualification](qualification/scan-finalization.json) checks release of a
100,000-entry reservation; it does not measure OS RSS reclamation or bound the
memory needed by an active Git query. Dropped allocations may remain in the
process allocator for reuse.


On non-Unix systems, Git status paths that cannot decode as UTF-8 invalidate
the status cache and retain an initialization error. Enrichment reports that
failure through the bounded scan-warning path and preserves prior annotations
on a failed refresh. This avoids silently publishing an incomplete status map.
Unix status-cache paths continue to preserve native bytes. The
[decoder qualification](qualification/git-decode.json) covers valid Unicode,
invalid bytes, partial-cache disposal, and diagnostics on macOS and Colima
Linux; it does not establish native Windows behavior or bound Git query
memory and duration.


`ScanConfig::max_entries` optionally limits descendant entries consumed by
recursive scans and `quick_list`. The root is excluded; name/depth-filtered
entries do not count, while metadata failures and unsupported file types do.
The first excess entry returns `ScanError::EntryLimitExceeded { limit }` before
its metadata processing, hashing, or retention. No truncated tree is returned.
Zero allows an empty scope; absent/null preserves the unlimited legacy default.
The [qualification](qualification/entry-budget.json) covers exact boundaries,
empty roots, nested descendants, ignored/hidden entries, depth limits, and JSON
compatibility. CLI report commands expose `--max-entries`; the TUI uses its saved `max_scan_entries` setting. The new public error variant
requires exhaustive-match consumers to update.

This count budget is not a process memory ceiling. Jwalk may prefetch directory
batches before consumption, entry byte sizes vary, and Git enrichment has its
own unbounded query/cache behavior. Iterator errors do not consume the entry
budget. Tree recursion, filesystem-call time, and process RSS remain separate
qualification gaps.


All CLI report commands (`scan`, `duplicates`, `age`, `export`) pass the optional
`--max-entries` budget into the scanner. It defaults to unlimited and is recorded
in successful envelope scope. Budget exhaustion aborts before analysis and
output, regardless of strict mode, preserving existing export files.
[CLI qualification](qualification/cli-budget.json) executes both binary names
with exact-boundary and exceeded limits on all four commands, rejects invalid
values, and checks export preservation. These are macOS process tests; this
change has not refreshed Linux CLI, Windows, or packaged release evidence.


The TUI saved `max_scan_entries` setting feeds both quick-list configuration
and background recursive scans, including navigation listings and rescans.
Absent values preserve unlimited behavior. Budget failure appears through the
existing scan-error path, clears scan progress, and does not start analysis;
failed rescans retain the previous listing. Startup listing failure leaves no
partial tree. [Qualification](qualification/tui-budget.json) uses real filesystem
fixtures, two background rescans, startup/quick-list checks, and TOML save/load
round trips. These are macOS application-state tests, not interactive terminal
or native Windows qualification. There is no numeric settings-dialog editor.
Invalid/unreadable settings still fall back to defaults with a visible warning;
this setting is not an enforced administrative resource ceiling.


[Colima budget qualification](qualification/linux-budgets.json) refreshes the
Linux ARM64 workspace tests and optimized CLI runtime after the scan, CLI, and
TUI budget changes. The release smoke now requires all four report commands to
accept the exact fixture budget and reject an exceeded budget without output.
It also checks preservation of an existing export. Both binary hashes and
unchanged source hashes are recorded. This is not archive packaging, signing,
Windows, interactive-terminal, or process-resource ceiling qualification.


The [release retry after disk recovery](qualification/macos-retry-release.json)
passed the full local gate: 689 checkout tests and 689 extracted-package tests,
four ignored in each run, all 14 package feature checks, lint/MSRV/docs, cached
dependency policy, optimized builds, and both CLI smoke suites. Python tooling
passed all 68 tests on host Python 3.14, Python 3.13, and Colima Python 3.12.
This supersedes the disk-failed attempt for current local release qualification;
its failure record remains historical evidence. Source and binary hashes are
recorded. Publication, native Windows, signing, and scale/resource guarantees
remain outside this result.


Unreadable or invalid TUI settings now disable `scan_on_startup` in the fallback
settings rather than starting recursive work with an unknown saved budget.
Explicit startup overrides still take precedence. Missing settings remain a
successful default load, preserving first-run behavior. The fallback value is
retained in the settings model, so saving unrelated edits does not silently
re-enable automatic scans. Quick browsing and manual scans remain available;
this is not a mandatory resource ceiling.
[Qualification](qualification/settings-fallback.json) tests all three startup
override states, successful defaults, and existing missing-file behavior.


Physical-size scanning retains hard-linked device/inode identities for the
scan lifetime. Link counts are mutable hints and no longer drive eviction.
When following symlinks, single-link file identities are also retained because
directory aliases may expose the same physical file repeatedly. Previously
seen identities remain suppressed even if their link count drops to one.
Apparent-size scans continue counting each path.
[Qualification](qualification/inode-alias.json) covers real directory aliases
with and without hard links, plus changing link-count sequences.

The tradeoff is greater tracking memory: all observed hard-linked identities,
or all observed file identities when following symlinks in physical-size mode.
Configured entry budgets limit consumed entries but are not exact memory caps.
Inode reuse and concurrent mutation remain outside snapshot guarantees; which
alias receives the physical bytes depends on first encounter.


Quick listings now deduplicate physical hard-link sizes within their visible
child set using the same scan-lifetime inode tracker. Both root totals and
statistics agree with summed child sizes; apparent-size mode still counts each
path. Filtering an earlier alias does not suppress the remaining visible file.
[Qualification](qualification/quick-inode.json) checks real hard links, both size
modes, filtered scope, and the TUI regression suite. Independent quick listings
do not share inode state; nested directory sizes remain unknown until scanning.
Physical attribution still follows first encounter and is not a snapshot or a
guarantee of bytes reclaimable by deleting one hard-link path.


Quick listings honor `follow_symlinks` by reading target metadata in follow
mode, while retaining link nodes in no-follow mode. Target metadata failures
produce bounded warnings and omit the unresolved entry. Physical-size mode
tracks single-link identities when following aliases; apparent sizes count
each path. `cross_filesystems` is applied against root device identity before
retention, including resolved targets. Followed directories remain shallow
placeholders, consistent with the recorded depth-one scope.
[Qualification](qualification/quick-follow.json) covers relative file/directory
links, broken targets, physical/apparent totals, and a real /dev filesystem
boundary on macOS and Colima Linux.


Scanned nodes now retain non-Unicode filename components separately from display
labels. Recursive and quick scans populate `native_name`, and `file_name()`
provides the exact component to path-building consumers. Analysis traversal,
Git annotation, TUI tree/Miller/treemap paths, search, selection, and path lookup
use the native component. This prevents distinct native entries with identical
lossy labels from resolving to the same reconstructed path.
[Qualification](qualification/native-name.json) includes real invalid-UTF-8
Linux filenames, node JSON round trips, and TUI selection/search collisions.

The public field is additive for serialized Unicode-only nodes but requires
Rust struct-literal consumers to update. Optional storage increases node size.
Display labels may remain identical, and full `PathBuf` fields elsewhere in
reports still have separate non-Unicode serialization limitations. No native
Windows or large-scale memory claim follows from these tests.


Explorer tree/Miller/treemap labels, selected-file information, search labels,
and CLI scan node labels now use native-aware filename escaping. Invalid bytes
appear as hexadecimal escapes; literal backslashes are doubled, so escape-like
filenames do not look identical to encoded bytes or controls. Native components
remain unchanged for filesystem operations.
[Qualification](qualification/filename-display.json) checks rendered colliding
labels as well as selection/search identity and ordinary Unicode. This is not
a portable path encoding, and truncated columns can still hide distinctions.
Other full-path diagnostic and report views require separate display adoption.


Optional native filename storage now uses a boxed `OsString`, keeping the
common Unicode-only node compact. The measured ARM64 macOS `FileNode` size
fell from 240 to 224 bytes, reducing inline node storage by 16 bytes per node.
Non-Unicode names incur an additional allocation.
[Qualification](qualification/native-storage.json) records the layout probe
and full workspace regressions, including native-name serialization and lookup.
This is a target/toolchain-specific layout result, not a process RSS benchmark.
The public Rust field type changes, while Box serialization remains transparent.


CLI JSON stdout publication now stages the complete serialized document before
copying it to the output stream. A late serialization failure cannot publish
a prefix, including failures in full-path fields that cannot encode native
paths. [Qualification](qualification/json-staging.json) tests failure after a
2 MiB element, large successful output, and sink error propagation. Staging
spills above 1 MiB; it does not bound total report bytes, reserve disk space, or
make stdout copying atomic against interruption.


Scan/analysis report path fields now serialize Unicode paths as legacy strings
and non-Unicode paths as tagged Unix bytes or Windows UTF-16 units. Adapters
cover scope/root paths, tree statistics, warnings, duplicate groups, age paths,
and analysis issues. Optional tuples retain their array/null layout.
[Qualification](qualification/native-reports.json) verifies legacy compatibility,
native round trips, foreign/malformed rejection, and real Linux CLI execution
for both binaries and all four report commands. This supersedes the earlier
full-path JSON limitation for these report fields. Native Windows execution
and general untrusted-deserialization resource limits remain unverified.


Filename ordering now uses compatibility labels followed by exact native
components as a tiebreak. Full/quick scans, core size sorting, and TUI sorting
share this comparison; equal size/time/count values also use deterministic
name ties. Treemap rendering and selected-path lookup use the same size/name
order. [Qualification](qualification/native-order.json) reverses input order
for colliding native labels across all eight explorer sort modes and checks
that each treemap rectangle targets its selected native path. This does not
make concurrently changing metadata or physical-byte attribution deterministic.

## Terminal graphics startup

Unix graphics detection runs synchronously before the input event loop, with a
250 ms read deadline and an 8 KiB reply budget. No background stdin reader or
independent raw-mode guard survives a timeout. Reading stops at the terminal
status reply, preserving trailing keys. Keys typed during probing can still be
consumed; this is not an input replay mechanism. Query writes have no separate
output deadline. A failed query falls back to ordinary binary previews. Non-Unix
hosts use half-block image previews without querying stdin; native Windows
graphics protocol detection remains unqualified.

Run `python3 scripts/qualification/terminal-smoke.py target/release/grav` (and
`target/release/gravityfile`) on Unix to exercise silent and simulated responding terminals, directory
navigation, quit, terminal settings restoration and alternate-screen restoration.
Each command waits for the command prompt before sending its body. The event
loop receives input through a Tokio channel and drains at most 32 additional
ready events without installing a no-op waker. The test uses disposable files
and checks they remain intact.
It does not qualify graphics rendering, resize, mouse input or interactive latency.

The local release gate, Colima qualification script, native Unix CI tests and
native Unix release builds invoke both terminal probes. Workflow wiring does not
establish a successful remote run. Cross-built binaries and Windows are excluded
from these Unix PTY probes.

The probe requires image previews enabled in existing user settings and fails
if it does not observe the query; it does not silently skip this regression.

Terminal cleanup is armed before initialization, so errors after enabling raw
mode still attempt restoration. Raw mode, mouse capture, alternate screen and
cursor restoration are attempted independently, retaining the first cleanup
error. An application error takes precedence over cleanup errors. Drop provides
best-effort cleanup during unwinding, supplementing Ratatui's panic hook; process
abort, SIGKILL and a disconnected terminal cannot guarantee restoration.
The terminal smoke gate also forces initialization output failure and verifies
that the process reports failure while restoring terminal settings. TUI broken
pipes are errors; the CLI's non-strict pipeline early-close behavior is unchanged.

A single owned input thread polls at 50 ms intervals and feeds a 64-event channel,
with one additional pending event under backpressure. Shutdown interrupts queue
backpressure and joins the thread before an external editor starts; a fresh
reader starts after terminal reinitialization. Browser events already in that
channel are discarded on handoff. This is an event-count bound, not a byte bound
on terminal-parser buffers or event payloads, and the poll interval is not an
OS scheduling deadline. The handoff does not promise to discard every byte
already in the terminal driver's or Crossterm's internal buffers.

The PTY gate uses a disposable editor to verify canonical/echo mode, exclusive
receipt of a line of input, and browser resumption after editor exit statuses 0
and 7. It leaves the rename list unchanged and verifies fixture contents. This
does not qualify arbitrary editors, job-control suspension or crash recovery.

## Shell directory handoff

`--cwd-file` writes exact native path bytes on Unix, without a trailing delimiter.
It writes and syncs a private sibling file before replacing the destination;
a failed staging or replacement leaves the previous destination intact. An
existing destination symlink is replaced instead of followed. Unix handoff files
are private (0600); prior permissions/ACLs are not retained. This is an ephemeral
shell handoff, not a directory-synced crash-durability guarantee or protection
against concurrent ancestor replacement.

`--print-cwd` writes native path bytes followed by one newline and returns output
errors instead of panicking. These are machine path outputs: controls and path
newlines are preserved, not escaped. On non-Unix hosts, paths that cannot be
represented as Unicode are rejected rather than silently changed.

Bash/Zsh wrappers preserve trailing newlines via a command-substitution sentinel;
Fish reads the entire file with NUL-delimited input. Wrappers stop on failed
temporary-file creation and change directory only after a successful host exit.
The Python gate tests installed shells with disposable fixtures, skips absent
shells explicitly, and reserves non-Unicode filename cases for Linux because
the macOS filesystem rejects them. The Linux qualification image and Linux CI
job install all three shells; `GRAVITYFILE_REQUIRE_SHELLS=1` makes an absent
shell fail the tests. [Linux shell qualification](qualification/shell-matrix.json)
covers all 15 cases on Bash 5.2.15, Zsh 5.9 and Fish 3.6.0. This does not prove
compatibility with every shell version or user startup configuration.

## Editor resolution

Text editing, Markdown configured with `md = "editor"`, and bulk rename share
one editor resolver: explicit `editor.editor`, configured `editor.editor_kind`,
nonempty `VISUAL`, nonempty `EDITOR`, then `vim`. Configured `editor.args` are
passed as separate literal arguments before the absolute file operand. The
`openers.text` custom executable setting is honored instead of being ignored.
Relative file operands become absolute to avoid leading-dash option ambiguity;
native path bytes remain intact.

Executable settings and environment values are executable names/paths, not shell
command strings. Use the argument list for flags; quoting, variable expansion,
pipes and shell substitution are not evaluated. Paths to executables containing
spaces remain one executable path. An explicitly empty editor is an error. GUI
editors used for bulk rename must be configured to wait for editing to finish
(for example through their documented wait argument); generic command launching
does not detect application-specific detached-process completion.
