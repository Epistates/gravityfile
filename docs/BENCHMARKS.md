# Reproducible performance measurements

## Duplicate analysis phases

```bash
cargo test -p gravityfile --release --lib --locked --offline benchmark_duplicate_phases -- --ignored --nocapture
```

This manual benchmark creates 100K files by default, scans once, then measures
three analyses of the same tree. Set `GRAVITYFILE_DUPLICATE_PROFILE_FILES` to an
integer of at least 512 to change the workload. It checks complete status, file
counts, all duplicate totals, the 256 content groups, and displayed group size.
The fixture is removed before the `DUPLICATE_PHASE_BENCHMARK` record is emitted.
Debug builds are rejected.

The [100K macOS profile](benchmarks/macos-duplicate-phases-100k.json) measured
median analysis time of 2.871 seconds: collection 0.266 s, grouping 0.00155 s,
hashing/identity validation 2.596 s, and finalization 0.00689 s. Individual phase
medians need not sum to the median total. Hashing occupied 86–90% of each sample;
this phase includes file opening, metadata validation, reads and issue collection,
so these results do not isolate BLAKE3 computation from filesystem costs.

`DuplicateFinder::find_duplicates_with_timings` returns `(DuplicateReport,
DuplicateTimings)` with the same report semantics as `find_duplicates`. Timings
exclude scanning and serialization; parallel work is measured as elapsed wall
time, not summed CPU time. The ordinary API uses the same implementation and
discards the timing values. Clock-sampling overhead has not been isolated.
[Qualification](qualification/duplicate-phases.json) records tests, lint, MSRV,
source hashes and limitations. No RSS, cold-cache or cross-platform phase profile
is established by this run.

The profile motivated reusing one scratch buffer per candidate group. Previously
each file read initialized a 64 KiB stack buffer; the replacement allocates once
per group, sized to the observed bytes and capped at 64 KiB. Identity checks,
bounded reads and cancellation checks remain. A poisoned-buffer regression
compares exact hashes across short, empty and multi-chunk inputs.

The [100K buffer comparison](benchmarks/macos-hash-buffer-100k.json) preserves
the prior release test binary and records three samples each, baseline first.
Median analysis time was 2.618 seconds before and 2.493 after; median hashing
phase was 2.371 versus 2.248 seconds. Ranges overlap and the shared host was not
isolated, so the roughly 4.8% lower observed median is not a general performance
guarantee. Separate equivalent fixtures were used. No RSS or 1M-file optimization
comparison was performed. [Qualification](qualification/hash-buffer.json) retains
both runs plus macOS/Colima correctness checks and source hashes.

## Staged file copying

```bash
cargo test -p gravityfile-ops --release --locked --offline benchmark_staged_copy -- --ignored --nocapture
```

The manual benchmark creates a 64 MiB + 17 byte dense file and a sparse file
with two small populated regions and a trailing hole. It runs three staged
copies of each, preserving timestamps. Timing includes temporary-directory
creation, source validation, copying and exclusive publication. Each result's
length and every byte are compared with its source outside the timed section;
allocated bytes are recorded on Unix. The fixture is removed before emitting
the `COPY_BENCHMARK` JSON record. The test rejects debug builds.

These are warm-cache-possible, shared-host observations. They exclude fixture
creation, verification and removal, and do not measure destination durability
syncs, peak RSS, cold caches, concurrent copies or cross-filesystem moves. Dense
data uses a repeating nonzero pattern; sparse data is not an arbitrary extent
layout. The benchmark establishes a workload baseline, not superiority over
OS copying or the previous implementation.

The [macOS baseline](benchmarks/macos-staged-copy.json) records all six samples,
the test-binary and source hashes, and host/toolchain details. Median elapsed
time was 14.84 ms for dense data and 19.53 ms for sparse data. Dense destinations
allocated 67,112,960 bytes; sparse destinations allocated 524,288 bytes for the
same 67,108,881-byte logical length. These short buffered-I/O measurements are
not sustained storage throughput measurements.

## Duplicate-view rendering

Run the manual optimized widget benchmark:

```bash
cargo test -p gravityfile-tui --release --all-features --locked --offline duplicate_render_benchmark -- --ignored --nocapture
```

Historical qualification correction: the original widget benchmark constructed
paths using the temporary directory spelling, while the app canonicalized its
root. A later scope-count assertion reproduced zero matches on macOS's
`/var`/`/private/var` alias. Earlier artifacts did not assert matched population;
their timings do not establish rendering of the intended million in-scope file
rows and must not serve as a qualified performance comparison. Raw artifacts
remain retained below.

The historical [million-path artifact](benchmarks/macos-duplicate-render-1m.json) records
the exact test-binary hash, toolchain, platform, and all 31 cached-frame samples.
Both scenarios expand every group and select the final file in a 120×40 view.

| Synthetic report | First frame | Cached median | Cached p95 |
| --- | ---: | ---: | ---: |
| 1 group, 1M paths | 98.49 ms | 0.074 ms | 0.103 ms |
| 1,000 groups, 1M paths | 96.86 ms | 0.089 ms | 0.102 ms |

The [prepared-count artifact](benchmarks/macos-duplicate-render-prepared-1m.json)
measures the updated lifecycle: prepare counts before the first draw, install
those counts, then render. Production preparation runs on the analysis worker;
the benchmark times the same preparation function separately on its test thread.

| Synthetic report | Preparation | UI installation | First frame | Cached median |
| --- | ---: | ---: | ---: | ---: |
| 1 group, 1M paths | 96.47 ms | 0.0013 ms | 0.515 ms | 0.079 ms |
| 1,000 groups, 1M paths | 93.04 ms | 0.0126 ms | 0.139 ms | 0.088 ms |

The work moves off the UI thread; it has not disappeared. These measurements
render the complete App widget into a newly allocated in-memory buffer. They
exclude terminal output, report construction, analysis, and filesystem scanning.
Peak RSS was not measured. These historical results come from an optimized test
harness, not an end-to-end terminal session; scheduling and terminal latency
remain unqualified.

The [corrected scope benchmark](benchmarks/macos-duplicate-scope-1m.json) uses
canonical app-root paths, padded sorted names, and assertions for path ordering
and populated/empty directory counts. Worker preparation now validates ordering
while counting paths. Directory changes reuse that metadata and perform two
binary searches per sorted group; unsorted reports retain linear filtering.

| Million-path layout | Worker preparation | Scope-count median | Scope-count p95 |
| --- | ---: | ---: | ---: |
| One group | 154.90 ms | 0.003625 ms | 0.004 ms |
| 1,000 groups | 156.30 ms | 2.082 ms | 2.424 ms |

Each scope series contains 31 alternating populated and empty scopes. These
times cover only count-cache refresh, excluding navigation, rendering and
terminal I/O. The artifact separately records widget timings. UI work still
scales with group count; unseeded reports check ordering on the UI thread, and
unsorted groups scan paths. No hard latency or memory ceiling is established.
[Qualification](qualification/duplicate-scope.json) retains the failed fixture
attempt, corrected run and macOS/Colima tests.

## Filesystem scanning and analysis

Build the executable under test, then run the standard-library-only Python
harness. It creates its own deterministic fixture and removes it afterward.

```bash
cargo build --release --locked
python3 scripts/benchmark.py --files 100000 --runs 3 --output /tmp/gravityfile-100k.json
```

Use `--fixture-parent` to select the storage being tested. Record the device and
filesystem type separately; the artifact records the platform, CPU count,
filesystem block sizes where available, binary SHA-256, arguments, raw timing
samples, and peak child RSS on Unix. Unsupported resource measurements are
`null`, not zero. Each command must complete successfully and report exactly the
fixture's file count before its measurements are accepted.

The fixture contains 1,000 files per directory, 256 distinct contents, and
256-byte files by default. Allocated storage may be much larger than logical
content: 100,000 files on a filesystem with 4 KiB allocation units can consume
approximately 400 MB plus metadata. Increase to 1M/10M entries only with adequate
storage and memory; those runs are not implied by a 100K result.

Measurements cover complete scan, duplicates, and age CLI commands. Analysis
measurements include their preceding scan. Filesystem caches are warm because
the fixture was just written; the harness never clears system caches. It does
not measure cold-cache performance, interactive TUI latency, hostile concurrent
writers, or network storage reliability.

Add `--include-export` to measure full versioned tree JSON to stdout as a fourth
workload. The harness captures each fixture file's allocated size (`st_blocks *
512` on POSIX, logical size elsewhere), then verifies exported identities,
directory membership and size aggregates. This measures scanning, staging and
JSON output to a temporary file; it does not measure durable named-file
publication. The harness materializes JSON for validation outside child timing
and RSS measurements, so reserve memory for that additional work.

The [100K export baseline](benchmarks/macos-export-scale-100k.json) records three
samples per command on an internal APFS SSD (Mac16,6, 128 GiB RAM, 16 logical
CPUs). Full export median was approximately 0.443 seconds; maximum child RSS was
68,829,184 bytes (65.64 MiB). The fixture was removed after validation. This is a
warm-cache shared-host baseline, not a comparison with a prior writer.
[Qualification](qualification/export-scale.json) retains host
details, input hashes, logs and the rejected initial validator attempt.

The [million-file continuation](benchmarks/macos-export-scale-1m.json) uses the
same executable hash and workload parameters, increasing only the file count.
Each command passed three samples and all corresponding semantic checks:

| Command | Median seconds | Maximum child RSS (MiB, rounded) |
| --- | ---: | ---: |
| Scan | 9.070 | 548 |
| Duplicates, including scan | 85.525 | 1,108 |
| Age, including scan | 7.836 | 541 |
| Full JSON export, including scan | 9.045 | 515 |

Fixture creation took 74.89 seconds and is excluded from these timings. The
million files were removed before the artifact was published. Export validates
every expected filename, parent directory and allocated-size aggregate. The
harness's full JSON decoding is outside child time/RSS measurement. Results
remain warm-cache, shallow-tree, shared-host observations; the spread in raw
samples and superlinear wall-time growth from 100K do not identify a specific
bottleneck. Phase profiling is needed before attributing the cost to traversal,
hashing, allocation or host contention. These results do not establish cold-cache,
10M-file, Linux/Windows, or interactive performance.
[Qualification](qualification/export-scale-1m.json) retains command results and
source/binary provenance.

Compare medians and raw samples on the same machine/storage/workload. Retain
artifacts with the binary hash; version text alone cannot identify an uncommitted
build. No fixed performance claim or regression percentage is enforced until
multiple runs on stable benchmark hardware establish normal variability.

## Initial 100K-file measurement

On the recorded 16-logical-CPU macOS arm64 host, three warm-cache runs gave:

| Command | Median elapsed | Maximum observed peak RSS |
| --- | ---: | ---: |
| Scan | 0.238 s | 58.5 MiB |
| Duplicates, including scan | 3.604 s | 119.0 MiB |
| Age, including scan | 0.262 s | 55.7 MiB |

The [raw artifact](benchmarks/macos-100k.json) identifies the exact measured binary.
These are local measurements of the stated fixture, not service-level objectives
or comparisons with competing products. They do not include later additive
envelope metadata changes.

A file-level parallel hashing experiment in the same Rayon pool increased the
duplicate-command median to 10.846 s and maximum peak RSS to 125.3 MiB on the same
fixture. The implementation was reverted; the [experimental artifact](benchmarks/macos-100k-parallel.json)
is retained to explain why the current implementation parallelizes size groups
but keeps reads within each group sequential. This result does not establish the
best policy for large files, HDDs, or network filesystems.

## One-million-file qualification

A current release build on the local Apple M4 Max host (128 GiB RAM, internal
APFS SSD, 4 KiB allocation blocks) completed three repetitions of the default
one-million-file fixture. All commands reported the expected file count; both
JSON analysis reports declared complete status. The fixture took 61.87 seconds
to create and was removed after measurements.

| Command | Median elapsed | Maximum observed peak RSS |
| --- | ---: | ---: |
| Scan | 12.084 s | 486.2 MiB |
| Duplicates, including scan | 264.047 s | 854.0 MiB |
| Age, including scan | 12.630 s | 489.3 MiB |

The [raw one-million-file artifact](benchmarks/macos-1m.json) records all samples
and the exact binary hash. This verifies completion and memory use for this
warm-cache workload, but exposes substantial duplicate-analysis latency. It does
not establish interactive responsiveness, a ten-million-file capacity, or
cross-platform performance. The machine was not dedicated benchmark hardware;
background system activity was not controlled.

The older 100K artifact uses a different binary, so its timing ratio must not be
interpreted as a clean scaling measurement. Optimization should retain file
identity/change checks and be evaluated against the same workload and binary
provenance.

A [fresh 100K comparison](benchmarks/macos-100k-current.json), run immediately
afterward with the identical binary hash, gave:

| Command | 100K median | 100K maximum peak RSS | 1M / 100K elapsed ratio |
| --- | ---: | ---: | ---: |
| Scan | 0.276 s | 52.9 MiB | 43.7× |
| Duplicates | 3.486 s | 114.7 MiB | 75.7× |
| Age | 0.269 s | 57.7 MiB | 47.0× |

The nonlinear increase is observed evidence, not proof of an algorithmic
complexity class. File count, directory count, filesystem metadata working set,
and uncontrolled host activity all vary or can affect the measurement. The
one-million-file duplicate samples used about 83–89 seconds of kernel CPU time
and roughly 5 seconds of user CPU time, with elapsed time substantially greater
than their sum. Profile metadata calls and storage/cache behavior before
attributing the delay to hashing or adding more worker threads. The scan slowdown
also needs investigation independently of duplicate hashing.

## Avoiding redundant complete reads

When head/tail partial ranges cover an entire size group, duplicate analysis now
uses the validated full-hash path directly. The
[100K artifact after this change](benchmarks/macos-100k-single-hash.json) records
three runs with a duplicate-command median of 1.823 seconds and maximum peak RSS
103.9 MiB. The preceding current-build baseline was 3.486 seconds / 114.7 MiB,
an observed elapsed reduction of 47.7%. Scan and age medians were 0.261 and
0.228 seconds respectively; background activity was still uncontrolled.

This removes a known redundant read without relaxing metadata checks. It is a
local 100K result, not a measured one-million-file improvement or an explanation
of the nonlinear scaling observed above. The retained one-million-file artifact
predates this optimization. Repeat that workload and profile the remaining
metadata/storage costs before extending the claim.

## In-memory search at 100K and 1M paths

[Raw measurements](benchmarks/macos-search-1m.json) record three samples per case
on an Apple M4 Max with 128 GiB RAM, using an optimized build. The fixture contains
synthetic `src/module-N/report-NNNNNNN.rs` paths. Times include query compilation,
matching, ranking, and result materialization; they exclude filesystem scanning,
terminal rendering, and peak RSS measurement.

| Query case | 100K paths, median ms | 1M paths, median ms |
| --- | ---: | ---: |
| Fuzzy, many matches | 8.06 | 76.55 |
| Glob, many matches | 0.16 | 0.14 |
| Regex, many matches | 0.13 | 0.15 |
| Glob, no matches | 3.77 | 35.21 |
| Regex, no matches | 0.54 | 5.61 |

Broad glob and regex cases stop after collecting 1,000 results; these times are
not full-index traversal costs. Fuzzy search scores the full index, retaining
only 1,000 candidates. The index construction took about 5.38 ms at 100K paths
and 50.79 ms at 1M paths in this run. Input timing, allocator/RSS behavior, colder
caches, varied path distributions, and slower machines are unqualified.

These measurements predate background query execution and demonstrate why
bounded result memory alone was insufficient. The interactive app now runs
queries in a cancellable background worker with stale-result rejection. This
benchmark continues to measure the synchronous matching engine; it does not
measure the revised app's input-to-display latency. Interactive path-index
construction now also runs in a worker and requires separate latency/RSS
qualification; the fixture construction times above are not app activation times.

Reproduce the ignored benchmark with:

```sh
cargo test -p gravityfile-tui --release --locked --offline search_scale_benchmark -- --ignored --nocapture
```

## Background index and query execution

[Raw measurements](benchmarks/macos-background-search-1m.json) cover three optimized
samples per size on the same M4 Max host. A synthetic flat `FileNode` tree feeds
the actual app search-index collector and the fuzzy query `rpt`. Every sample
asserts 1,000 matching results. Fixture construction is excluded; subsequent
samples include retiring the prior index on the index worker.

| Files | Median activation ms | Median query submission ms | Maximum single poll ms | Median completion ms |
| ---: | ---: | ---: | ---: | ---: |
| 100,000 | 0.308 | 0.000375 | 0.131 | 32.29 |
| 1,000,000 | 0.108 | 0.000500 | 0.043 | 275.70 |

Activation schedules indexing; completion includes indexing, query execution,
and test polling. The benchmark polls every 1 ms; the interactive app uses a
50 ms background timer. These are caller-side scheduling observations, not
physical input-to-display latency. No terminal rendering, filesystem scan, RSS,
rapid cancellation, or varied/deep path distributions are measured. Host load is
uncontrolled. Do not compare completion directly with the earlier query-only
benchmark, which excludes index construction and uses a different path shape.

The initial fixture used a noncanonical macOS temporary path and returned no
results because its view root differed from the canonical quick-list root. The
recorded successful fixture canonicalizes input. A subsequent constructor fix
adopts the quick-list root for app state; regression tests cover relative paths,
dot components, and Unix directory symlinks. The retained measurements predate
that constructor fix.

```sh
cargo test -p gravityfile-tui --release --locked --offline background_search_scale_benchmark -- --ignored --nocapture
```

## In-memory age analysis

```bash
cargo test -p gravityfile-analyze --release --test analyze_tests --locked --offline age_analysis_scale_benchmark -- --ignored --nocapture
```

The [age-analysis artifact](benchmarks/macos-age-analysis.json) records three
samples each for 100K and 1M flat-tree files with five and 1,001 buckets. Every
sample checks totals and per-bucket accounting against a linear reference over
the fixture's 2,000 distinct ages. At 1M files, observed median analysis times
were 75.27 ms (five buckets) and 102.11 ms (1,001 buckets). Fixture creation,
filesystem scanning, serialization, report destruction and terminal output are
excluded. This is a current measurement, not a measured speedup or RSS bound.

## In-memory tree lifecycle

```bash
cargo test -p gravityfile-core --release --lib --locked --offline tree_lifecycle_scale_benchmark -- --ignored --nocapture
```

The [lifecycle artifact](benchmarks/macos-tree-lifecycle.json) includes three
process repetitions, each measuring 100K, 1M and 10M synthetic files. At 10M,
median clone time was 275.92 ms and finalization 229.60 ms. Median cloned/source
tree destruction took 53.58/94.74 ms. Maximum process peak RSS was 4.35 GiB,
including simultaneous source and clone ownership. RSS is process-wide across
all fixture sizes, not isolated per stage. Short inline filenames, fixed
metadata and 1,000 files per directory limit how broadly these results apply.
No filesystem scanning, JSON serialization or terminal work is measured here.

## Deterministic representative selection: 100K comparison

[Before](benchmarks/macos-determinism-before.json) and
[after](benchmarks/macos-determinism-after.json) each contain three warm-cache
samples for 100,000 independent 256-byte files in one child directory, with
256 distinct contents. The baseline hash matches the integrated reporting
release; the rebuilt binary includes deterministic tied statistics and hard-link
representatives. All 18 command samples passed the harness's correctness checks.

| Command | Median before / after | Peak RSS range before / after |
| --- | --- | --- |
| Scan | 0.428 / 0.383 s | 76.75–77.95 / 76.72–78.36 MiB |
| Duplicates | 5.724 / 4.955 s | 105.64–125.98 / 109.03–110.55 MiB |
| Age | 0.393 / 0.381 s | 76.97–78.56 / 76.92–78.59 MiB |

This shared-host, sequential comparison does not isolate either change. RSS
ranges overlap; no fixed memory overhead or improvement is established. The
fixture measures identity-map overhead without hard-link aliases. Larger scales,
cold caches and other platforms remain unmeasured for these changes.
[Qualification record](qualification/determinism-benchmark.json) retains hashes,
logs and limits. Both disposable fixtures were removed after measurement.
