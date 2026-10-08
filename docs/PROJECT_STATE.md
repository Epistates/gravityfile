# Project state and reliability contract

gravityfile is a library-first filesystem explorer: scanning produces a shared
tree, analysis identifies large, duplicated, or old data, and the CLI/TUI supports
inspection and explicit file operations. Six library crates and two executable
names share this implementation.

The project has substantial local correctness and release evidence, but is not
fully enterprise-qualified. Native Windows execution, OS plugin confinement,
durable operation auditing, multi-file crash recovery, and ten-million-file
filesystem/interactive qualification remain open. The `.strategy` documents are
aspirations, not an inventory of implemented capabilities. Trend history,
persistent incremental scans, and io_uring acceleration are not established.

This page summarizes current behavior. The [dated engineering record](qualification/engineering-history-2026-09-21.md)
preserves incremental changes and their original qualification limits. Individual
JSON artifacts identify the exact inputs tested; historical results do not imply
that later changes passed the same gate.

## Current qualification

The [October 7 coordinated-recovery qualification](qualification/coordinated-recovery-2026-10-07.json)
passed 854 macOS checkout tests, 854 packaged-source tests and 860 Colima Linux
Rust 1.90 tests (eight ignored per suite), plus 100 tooling tests (seven skips)
and 15 Linux shell-integration tests. Both platforms passed optimized CLI checks
and five terminal scenarios for each executable. macOS also passed strict lint,
Rust 1.90, documentation, dependency-policy gates and 14 packaged feature checks.
All recorded inputs stayed unchanged; all 143 shared input hashes matched across
runs. Compressed logs, binary hashes and actual command exit codes are retained.
The record includes failed initial Linux starts caused by a missing image and the
Linux fixture ordering failure resolved before the passing gate.

Bulk rename now prepares every intent and matching participant plan before source
mutation, records object/parent identities and a batch commit marker, and exposes
explicit coordinated restoration through `recover --rollback-rename`. Recovery
checks current positions and identities, refuses unrelated occupied originals,
locks all participants and resumes interrupted no-replace moves. Extraction now
records its publication plan and entry identities before destination mutation;
failed publication and `recover --retain-extraction` move owned output into
retained storage instead of deleting it. Subprocess tests cover 21 archive
publication boundaries, recovery before/after moves and completed checkpoints,
replacement data, unrelated children, same-object edits and cross-parent swaps.
These checks do not establish snapshot isolation, hostile-writer confinement,
atomic multi-file publication or power-loss durability. The isolated Windows
identity module cross-compiles; native Windows execution and directory persistence
remain unqualified. The [recovery guide](RECOVERY.md) contains the platform matrix.

[Release notices regenerated on October 7](qualification/release-notices-2026-10-07.json)
cover all five default-feature target graphs with schema-2 provenance and verified
body checksums. A local unsigned macOS archive passed contents validation using
the current binaries and generated notice. Four existing canonical-MIT source-text
exceptions remain visible; native vendored-code/SDK attribution and legal review
remain open. Target notice generation is not native execution qualification.
Remote CI, signing, notarization and publication remain unqualified.

The [October 5 gate](qualification/rename-progress-2026-10-05.json) retains the
historical rename-progress checkpoint. The [October 2](qualification/integrated-release-2026-10-02.json),
[October 1](qualification/integrated-release-2026-10-01.json) and
[September 30](qualification/archive-release-integrated.json) gates preserve prior
results. The current gate retains the 8 MiB diagnostic log cap and its concurrency/
partial-I/O regressions, recovery output handling, plugin queue cancellation and
serialization budgets. There is no aggregate log quota or durable audit retention.
The gate uses cached advisory data; the [dependency refresh](qualification/dependency-refresh-2026-10-01.json)
separately verified fresh advisories. The older September 27 intermittent plugin
protocol-test failure remains unexplained and did not recur in the passing runs.

Plugin-specific regression records retain the detailed
[queue-abort](qualification/plugin-queue-abort.json),
[encoding-budget](qualification/plugin-encoding-budget.json) and
[protocol refactor](qualification/plugin-protocol-module.json) checks.
Serialization remains nonpreemptible, and process supervision provides no OS
filesystem/network confinement.

| Recent change | Evidence | Remaining qualification limit |
| --- | --- | --- |
| Coordinated rename recovery | [Identity, crash/resume and CLI gates](qualification/coordinated-recovery-2026-10-07.json) | Explicit recovery; path/ancestor races, identity reuse and power-loss qualification remain open |
| Archive extraction journals | [Publication/retention crash gates](qualification/coordinated-recovery-2026-10-07.json) | Retains owned output and decoded staging; no atomic multi-file publication or aggregate quota |
| Per-process diagnostic log cap | [Integrated concurrent-write/I/O-failure tests](qualification/integrated-release-2026-10-02.json) | 8 MiB per file; no aggregate quota, rotation or durable audit |
| Failed diagnostic output | [Closed-stderr CLI tests](qualification/diagnostic-output-failure.json) | Best-effort delivery; no native Windows qualification |
| Recovery acknowledgement errors | [Restore/flush tests](qualification/recovery-acknowledgement.json) | Diagnostic delivery still requires working stderr |
| Recovery output failures | [macOS/Linux CLI tests](qualification/recovery-broken-pipe.json) | Lost acknowledgement does not imply mutation rollback |
| Recovery issue categories | [API and CLI tests](qualification/recovery-issue-codes.json) | Additive JSON field; no safe-retry guarantee |
| Recovery lock classification | [Contention tests](qualification/recovery-lock-kind.json) | Native Windows unqualified |
| Extraction cancellation | [Staging/TUI tests](qualification/extraction-cancellation.json) | Publication completes or rolls back once begun; no hard deadline |
| Archive creation cancellation | [API/TUI boundary tests](qualification/archive-cancellation.json) | Cooperative; hard interruption deadlines remain open |
| Lost archive replies | [Completion-channel regression](qualification/archive-worker-loss.json) | Refresh and diagnostics, not crash recovery |
| TUI archive module | [Existing-suite verification](qualification/tui-archive-module.json) | Included in integrated gate above; no new performance claim |
| Exact ZIP symlink targets | [Whitespace round trips](qualification/zip-link-target.json) | Non-UTF-8 ZIP targets rejected; use TAR |
| Private archive output | [Five-format Unix tests](qualification/archive-private-output.json) | No custom Windows DACL or encryption |
| ZIP regular-file permissions | [Round-trip tests](qualification/zip-file-modes.json) | No directory modes, ownership, ACL or xattr preservation |
| ZIP source read bounds | [Mutation tests](qualification/zip-bounded-source.json) | Metadata checks cannot establish a content snapshot |
| ZIP source opening | [Replacement regressions](qualification/zip-source-open.json) | Unix leaf identity only; no ancestor or content snapshot protection |
| Archive selection cleanup | [Completion regression](qualification/archive-selection.json) | Removes quadratic comparisons; no latency benchmark claim |
| Native archive destination paths | [Linux destination regression](qualification/archive-native-parent.json) | No concurrent ancestor substitution protection |
| TUI published-archive warnings | [Refresh regression](qualification/archive-warning-refresh.json) | macOS suite; injected completion error |
| Archive publication persistence | [Fault injection](qualification/archive-parent-sync.json) | Unix directory sync; no real power-loss or native Windows qualification |
| Native TAR filenames | [Linux round trips](qualification/tar-native-names.json) | ZIP still requires UTF-8 names; no filesystem snapshot |
| Dependency and CI-action refresh | [Dependency review](DEPENDENCY_REVIEW.md) | Upstream version pins and documented advisory exceptions remain |
| Async plugin queue deadlines and dispatch cleanup | [Cross-platform tests](qualification/plugin-cross-platform.json), [deadline regression](qualification/plugin-queue-deadline.json), [cleanup regression](qualification/plugin-dispatch-cleanup.json) | No native Windows or OS confinement |
| Recovery sync attempts and rollback diagnostics | [Sync-pair tests](qualification/journal-sync-pair.json), [rollback regression](qualification/rollback-sync-diagnostic.json) | No power-loss durability qualification |
| Release notice target/input binding and body checksums | [Five regenerated targets and local archive validation](qualification/release-notices-2026-10-07.json) | Default-feature graphs; checksums are not signatures or legal review |
| License source attribution | [Current target notices](qualification/release-notices-2026-10-07.json) | Four canonical-MIT exceptions and native vendored/SDK attribution remain open |

[Earlier incremental notes](qualification/engineering-history-2026-09-29.md)
preserve the chronology and original test counts. Evidence applies to the inputs
recorded in each artifact, not to future changes.

## Current architecture

| Area | Present behavior | Remaining boundary |
| --- | --- | --- |
| Core tree | Exact native filenames; deterministic size/name ordering; iterative counts, sorting, cloning and destruction; [local-metadata debug summaries](qualification/node-debug.json) | Whole-tree memory/copy cost; recursive Serde; `FileNode` Drop changes field moves ([migration](../crates/gravityfile-core/README.md)) |
| Scanning | Parallel traversal, full/quick listings, optional entry limits, symlink/filesystem scope, hard-link accounting and Git annotations | Defaults allow unlimited entries; limits do not bound prefetch, Git work, RAM or filesystem-call latency; scans are not snapshots |
| Analysis | Size/partial/full BLAKE3 duplicate checks with identity validation; bounded diagnostics; iterative age traversal, bounded top results and binary bucket lookup | No hostile-mutation snapshot; candidate memory grows with input; HDD/network-storage performance unqualified |
| Operations | Conflict mediation, staged publication, overwrite/copy-undo recovery, explicit recovery CLI and limited undo | Bulk-rename outcome replay and archive journals incomplete; ancestor races, durable audit trail and filesystem durability matrix open |
| Plugins | Supervised Lua/Rhai workers, deadlines, bounded IPC and captured logs | No OS-enforced filesystem/network capabilities or process memory ceiling |
| TUI | Tree/Miller/treemap views, bounded/coalesced previews; iterative lookup/sort; owned input reader joined for editor handoff | Some work remains synchronous; interactive latency/protocol qualification and other recursive consumers open |
| CLI/reports | Scan/duplicate/age/export commands, strict completeness, versioned envelopes, native path encoding, staged iterative tree JSON, escaped diagnostics and iterative text summaries | Windows signals, exhaustive scan/envelope fixtures, direct library Serde recursion, panic and third-party direct-output boundaries open |
| Delivery | Local macOS release gate, Colima runtime checks, CI definitions, archive validation, isolated packaged-source testing | Remote target results, native Windows, signing/publication and distribution provenance not established |

## Implemented contracts

[Detailed reliability contracts](RELIABILITY.md) cover staging, recovery,
archives, export, settings, undo, progress, selection, and filename handling.
The key boundaries are:

- Copy/move publication does not overwrite an occupied destination silently.
  Overwrite recovery is explicit and journaled. Bulk rename and archive
  publication are not crash-atomic multi-file transactions.
- Archive paths, entry sizes, counts, and decoded data are checked. XZ decoder
  memory and ZIP metadata reads have budgets; total archive-process memory and
  CPU time are not comprehensively bounded.
- Strict CLI reports distinguish complete, partial, cancelled, and legacy
  unknown results. Scans remain observations, not authoritative deletion plans.
- Incomplete undo retains its original record in memory for review and blocks
  automatic replay; this is not a durable audit trail or a resumable protocol.
  Undo cancellation preserves completed-item accounting. Creation undo rejects
  later content and changed types. Copy undo retains destinations and later edits
  in persistent sibling recovery journals for explicit restoration; it does not
  reclaim disk space. Original-file identity protection, concurrent ancestor
  protection, and retrying failed/remaining undo are unfinished.
- The Errors view retains the latest operation's diagnostics across rescans.
  Selection and single-file actions follow the active view. Diagnostics and undo
  history are session-local; there is no durable enterprise audit trail.
- Plugin workers have process supervision, bounded IPC, and deadlines. They
  do not have OS-enforced filesystem/network capabilities or memory confinement.
- Settings and exports use temporary-file replacement. Both sync their containing
  directory on Unix and identify post-install sync failures explicitly. Recursive
  ancestor durability, permission/ACL retention, and concurrent settings-writer
  coordination remain open. Export sync ordering and failure behavior are tested;
  real power-loss durability remains unqualified. Unix export staging, publication,
  and cleanup use the same open parent handle, resisting later ancestor substitution;
  initial resolution and hostile writers inside that directory remain open.

## Verification evidence

| Historical and focused evidence | Verified scope | Limits |
| --- | --- | --- |
| [Integrated depth/report release](qualification/integrated-tree-release.json) | 777 macOS checkout + 777 packaged-source + 781 Colima tests, seven ignored each; 87 Python passed/seven skipped; 15 Linux shell cases; 14 package feature checks; lint/MSRV/docs/policy and CLI/terminal smoke; 134 unchanged inputs on both platforms | Includes Git depth/cancellation, local debug summaries, iterative CLI tree JSON and CI recording; Windows, remote CI, publication and broader interactive qualification remain |
| [Editor configuration](qualification/editor-config.json) | Unified editor resolution and literal arguments; 256 macOS TUI tests; both binaries pass five PTY scenarios, including editor handoff | Builds on [input ownership](qualification/input-owner.json); included in integrated reporting gate; arbitrary editor applications remain unqualified |
| [Linux shell matrix](qualification/shell-matrix.json) | All 15 Bash/Zsh/Fish cases pass with no skips, including native bytes; 735 Linux tests and both CLI/terminal suites pass | Recorded shell versions only; remote CI and native Windows unqualified; [handoff durability limits](qualification/shell-handoff.json) remain |
| [Deterministic statistics](qualification/stats-order.json), [aliases](qualification/alias-order.json), [diagnostics](qualification/issue-message.json) | Core/scan/analyzer checks and Clippy/MSRV; stable representatives and first-100 diagnostics with 8 KiB messages | Included in integrated macOS/package/Linux gate; 100K comparison below predates message changes; scans are not snapshots |
| [Iterative tree JSON](qualification/tree-json.json), [report cancellation](qualification/text-report.json) | 58 macOS/59 Colima root tests, Clippy/MSRV; 10,000-level staged tree output on a 128 KiB stack; schema comparison and cancellation preservation; included in integrated gate | Compact whitespace replaces pretty tree JSON; blocked I/O, partial delivered output and direct library Serde recursion remain |
| [Directory copy lifecycle](qualification/directory-release.json) | Explicit failure on repeated identities, preserved directory modes, per-entry cancellation, cleanup diagnostics; integrated on macOS/Colima | Actual mount aliases, inode reuse, ownership/ACLs and Windows semantics unqualified; residue can require manual cleanup |
| [Copy safety and sparse output](qualification/sparse-copy.json), [handle validation](qualification/copy-handles.json) | Cancellable handle copying and Unix zero-chunk holes; macOS/Colima operations tests; 64 MiB sparse fixture copies use 512 KiB or zero data blocks | Included in integrated gate; exact extents, Windows sparse output, OS acceleration, extended metadata and broader throughput qualification remain open |
| [Recovery safety](qualification/recovery-durability.json), [control identity](qualification/recovery-identity.json) | Post-move sync errors identify restored data; both syncs attempted; Unix control-file replacement detected; macOS ops/CLI tests, Clippy and MSRV passed | Included in integrated gate; Windows identity/directory-sync checks, ancestor races and power-loss qualification remain open |
| [Scan compatibility fixture](qualification/scan-fixture.json) | Nine macOS compatibility tests and strict Clippy; handwritten tree protects precision, warnings and legacy defaults | Included in integrated gate; native encodings, node variants and envelope migrations not exhausted |
| [Worker lifecycle](qualification/plugin-startup.json), [cancellation ownership](qualification/plugin-cancel-owner.json) | Startup failures terminate/reap workers; descendant-pipe cleanup, owned cancellation, response/log enforcement; included in the latest macOS/package/Colima gate | Termination latency and OS memory/capability confinement remain open |
| [Git annotation depth](qualification/git-depth.json), [compaction cancellation](qualification/git-compaction.json) | 80 macOS and 82 Colima scanner tests; strict feature/no-feature Clippy and MSRV checks; 4,000-level in-memory annotation on a 128 KiB stack; interrupted compaction clears partial annotations; included in integrated gate | Filesystem depth and overall memory/time bounds unqualified; allocation release and libgit2 calls have no hard cancellation deadline |
| [CI qualification artifacts](qualification/ci-evidence.json), [source membership checks](qualification/input-patterns.json) | Platform matrix records command outcomes, source-pattern hashes, logs, checkout/toolchain context and requests 30-day artifact retention; actionlint and 91 Python tests pass (seven skipped), including ten recorder tests | Source-pattern change postdates integrated gate; remote execution/upload and native Windows publication unverified; unselected/reverted changes are not detected; records are not attestations |
| [Fresh advisory audit](qualification/fresh-advisories.json) | RustSec revision `bd8037e5cbb8d8cc687c68cdd42ca53d742503fb`: 1,256 advisories, 442 locked dependencies | Two allowed unmaintained warnings: bincode via syntect; smartstring via rhai; point-in-time coverage only |

Run the local integrated gate with `just release-check`. It checks formatting,
Python verifiers, workspace/feature Clippy, tests, Rust 1.90, documentation,
dependency policy, packaged source and optimized CLI behavior. Its audit uses
`--no-fetch`; refresh the database explicitly when current advisory evidence is
needed. [Release procedures](RELEASING.md) document distribution prerequisites.

Use the [qualification recorder](../scripts/qualification/README.md) for new
command evidence. It records actual exit codes and log/input hashes; unchanged
named inputs and a zero exit are required for a pass. Its six focused tests and Python-suite run
are recorded in [publication evidence](qualification/qualification-publication.json).
Report installation refuses concurrent replacement; unsupported hard-link
filesystems fail publication while retaining the command log.
This reduces manual status-reporting mistakes without establishing coverage or
fingerprinting omitted inputs.

Packaged-source tests use extracted archives and local patches for internal
crates. They do not prove registry publication/installability. Fourteen feature
checks cover each package's library/binaries with defaults enabled and disabled;
they do not exhaust every feature combination. CI wiring is not evidence that a
remote run succeeded. The [historical record](qualification/engineering-history-2026-09-21.md)
retains archive validation, recovery, platform and workflow evidence in detail.

## Measured scale

[Benchmark methodology](BENCHMARKS.md) and linked artifacts retain workloads,
binary hashes, samples and exclusions. Measurements are host observations, not
performance leadership claims.

| Workload | Observation | Limit |
| --- | --- | --- |
| [64 MiB staged copies](qualification/copy-bench.json) | Three samples each: dense median 14.84 ms, sparse 19.53 ms; sparse output allocated 512 KiB; every byte verified | Buffered macOS baseline only; no sustained throughput, RSS, cold-cache or OS-copy comparison |
| [100K full JSON export](qualification/export-scale.json), [1M continuation](qualification/export-scale-1m.json) | Three samples at each size; export medians 0.443 / 9.045 s; maximum child RSS 65.64 / 514.72 MiB; every expected file/directory and allocated-size aggregate checked | Warm APFS stdout-export baseline; harness JSON-validation memory excluded; no 10M export or cold-cache claim |
| [Current 1M filesystem files](qualification/export-scale-1m.json), [historical run](qualification/validated-1m.json) | Current medians: scan 9.07 s, duplicates 85.53 s, age 7.84 s; maximum child RSS approximately 548/1,108/541 MiB | Three warm-cache samples, tiny-file macOS fixture; phase costs and host contention not isolated; no cold-cache/platform guarantee |
| [100K duplicate phase profile](qualification/duplicate-phases.json) | Median analysis 2.871 s; hashing/identity checks 2.596 s, collection 0.266 s, grouping/finalization under 0.01 s each; 62 analyzer tests and Clippy/MSRV pass | Postdates integrated gate; one scan followed by three analyses; no RSS or CPU-vs-filesystem attribution; manual benchmark adds an ignored root test |
| [Hash buffer reuse](qualification/hash-buffer.json) | One bounded scratch buffer per group; exact-hash, mutation and cancellation checks on macOS/Colima; 100K median 2.618 s before / 2.493 s after | Postdates integrated gate; sequential warm-cache sample ranges overlap; no general speedup or total memory guarantee |
| [100K siblings, deterministic representatives](qualification/determinism-benchmark.json) | Duplicate median 5.724 / 4.955 s before/after; RSS ranges 105.64–125.98 / 109.03–110.55 MiB | Two changes together; shared-host warm caches; ranges overlap; no isolated overhead or improvement established |
| [In-memory age analysis](qualification/age-scale.json) | 1M-file medians: 75.27 ms with five buckets, 102.11 ms with 1,001 | Excludes scan, construction, serialization, destruction and terminal output; no RSS measurement |
| [10M-file in-memory lifecycle](qualification/tree-lifecycle-scale.json) | Median clone 275.92 ms, finalize 229.60 ms; maximum process RSS 4.35 GiB with source and clone | Short names/fixed metadata; no filesystem scanning, serialization or terminal-scale claim |
| [Corrected 1M-path duplicate scope benchmark](qualification/duplicate-scope.json) | Scope-count refresh medians 0.003625 ms for one group and 2.082 ms for 1,000 groups; ordering prepared off-thread; 259 macOS and 259 Colima TUI tests pass | Earlier widget fixture did not assert in-scope population; current timings exclude navigation/terminal I/O; unsorted/unseeded reports retain linear work; postdates integrated gate |

Entry limits and bounded preview/diagnostic collectors do not establish a total
process memory ceiling. Duplicate scope counts use binary searches for prepared
sorted groups, but group iteration and unsorted fallback remain on the UI thread;
lookup searches siblings linearly. Clone and sort operations remain
synchronous. Ten-million-file filesystem scanning and interactive response are
not established by the in-memory lifecycle benchmark.

## Enterprise delivery milestones

The full enterprise goal remains open. An integrated local release gate closes
the current implementation checkpoint; it does not close the milestones below.
Work proceeds in this order, with independently reviewable changes and evidence
for each milestone. Do not substitute a growing list of scoped regression fixes
for completion of these requirements. A discovered data-loss or security defect
can interrupt the order; record its impact and resume the interrupted milestone.

| Order | Milestone | Present state | Evidence required to close |
| --- | --- | --- | --- |
| 1 | Recoverable filesystem mutations | Overwrite/copy-undo journals exist; rename has coordinated identity-checked restoration; archives have publication journals and data-retaining recovery | Validated directory handles through publication/rollback; leaf-substitution and identity-reuse limits; power-loss and native Windows qualification; retained-data quotas and measured large-batch latency |
| 2 | Enforced plugin capabilities | Worker lifecycle/deadlines/IPC are bounded; VM permissions are not an OS security boundary | Platform-specific filesystem/network policy and memory/process limits installed before plugin execution; adversarial real-worker tests; unsupported enforcement fails explicitly when confinement is required |
| 3 | Measured scale and responsiveness | Warm 1M filesystem and 10M in-memory baselines exist; whole-tree memory and synchronous TUI work remain | Reproducible 100K/1M/10M real filesystem runs with cold/warm state, peak RSS and phase costs; input-to-frame latency distributions under scan/hash/sort load; architecture changes justified by those measurements |
| 4 | Durable operational accountability | Session diagnostics and recovery records exist; one diagnostic file is bounded | Versioned per-operation intent/outcome records with native paths and transaction IDs; explicit incomplete/uncertain outcomes after restart; concurrent writers, storage failures, retention and quota tests; privacy and deployment policy documented |
| 5 | Qualified delivery and compatibility | Local macOS/Colima gates and CI definitions exist; native Windows and publication remain unverified | Successful native macOS/Linux/Windows CI artifacts for the same revision; report migration/signal fixtures; reviewed target notices; signed/notarized distribution checks and installation smoke tests; published support and filesystem matrices |

Milestone 1 now has executable recovery protocols for new rename and extraction
journals. Legacy rename intents remain inspection-only. New recovery refuses
replaced/ambiguous objects, preserves uncertain data and resumes across process
death before/after moves and checkpoints. Plans, per-item transitions, published
outcomes and explicit actions share native-path/object-identity evidence.
Milestone 1 remains open for atomic parent anchoring, hostile concurrent mutation
limits, identity reuse, power-loss qualification and native Windows behavior.
The next boundary is holding and validating directory handles through publication
and rollback so later ancestor substitution cannot redirect those actions.
Initial path resolution and leaf substitution inside an open directory need
separate fixtures and stated limits; identity checks are not content snapshots.
Validate actual storage-crash durability separately from injected errors and
process termination. Large-batch latency and aggregate retained-data quotas also
need measured deployment limits before an enterprise completion claim.

Milestone 2 needs a platform design before implementation; a Linux-only backend
does not qualify the cross-platform product. Linux Landlock provides restrictions
whose supported rights depend on the runtime ABI; inherited open descriptors and
other documented limits need explicit treatment. See the primary
[kernel documentation](https://www.kernel.org/doc/html/latest/userspace-api/landlock.html).
Choose and qualify macOS/Windows backends independently. Test denied reads,
writes, connections, descriptor inheritance, descendant escape and allocation
exhaustion as well as permitted behavior. Retain the trusted-plugin deployment
boundary until that evidence exists.

For milestone 3, retain fixture generators, distributions, machine/filesystem
details, binary/input hashes, raw samples and validation of scan/analysis results.
Set workload-specific latency and memory budgets before comparing implementations;
report failures against them. Do not infer a filesystem or interactive guarantee
from an in-memory tree benchmark. Streaming summaries, an arena tree or an index
are candidate changes only after the measured bottleneck warrants them.

Milestones 4 and 5 also require deployment decisions and external evidence.
Audit retention must never automatically delete recovery payloads. Signing,
publishing and release credentials are separate from local implementation and
validation. Record unavailable external evidence as open rather than treating
workflow definitions as successful executions. This evidence-oriented development
approach is informed by [NIST SSDF](https://csrc.nist.gov/pubs/sp/800/218/final);
it is a project delivery plan, not a certification or compliance claim.
