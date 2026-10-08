# Releasing gravityfile

The workspace uses one version for every crate. A release tag must be the
workspace version prefixed with `v` (for example, workspace `0.3.1` is tagged
`v0.3.1`).

## Preflight

1. Move the completed changelog entries out of `Unreleased` into a dated
   version section.
2. Update `workspace.package.version` and every internal workspace dependency
   version in `Cargo.toml`.
3. Refresh `Cargo.lock`.
4. Run the full local gate:

   ```bash
   just release-check
   ```

   This checks formatting, strict Clippy, all-feature tests, the declared Rust
   1.90 MSRV, advisories, dependency licenses/sources, unused dependencies,
   package assembly, an optimized build, and both optimized CLI entry points
   against an isolated smoke-test fixture.
   Unsoundness advisories fail the local gate. The advisory check uses its cached
   database (`--no-fetch`); CI refreshes advisories through `cargo deny`.
   Inspect assembled package contents before publishing, including confirmation
   that local agent state and credentials are absent.

5. Generate and review dependency notices for the intended release targets:

   ```bash
   cargo install cargo-about --version 0.9.2 --locked --features cli
   python3 scripts/generate-notices.py --target aarch64-apple-darwin --output THIRD_PARTY_NOTICES.html
   ```

   Use the actual target triple for each artifact. Release CI runs this before
   packaging and includes `LICENSE-MIT`, `LICENSE-APACHE`, and
   `THIRD_PARTY_NOTICES.html` beside both binaries. The publication verifier
   requires all three documents, regular file types and bounded nonzero sizes.
   It also requires the notice provenance header to match the artifact's target,
   the checkout's `Cargo.lock`, `about.toml`, and root/crate manifests. Changing
   dependency features can leave the lockfile unchanged, so manifests are bound
   separately. Generation checks those inputs again before installing its output.
   Schema 2 also records a SHA-256 digest of the UTF-8 HTML body; the verifier
   streams and checks that body to reject truncation or alteration after generation.
   Older notices must be regenerated. These checks detect stale, misplaced or
   damaged generated files; they are not signatures or proof of license completeness.
   Document presence is not a review of the legal completeness of their contents.

   The generator pins [cargo-about](https://github.com/EmbarkStudios/cargo-about)
   0.9.2, uses the locked default-feature workspace graph (including build
   dependencies), escapes HTML, checks package attribution coverage, and excludes
   local source paths from output. `about.toml` pins license-file checksums;
   remote files use crate-recorded commits or explicit commit pins. Generation
   requires network access for missing upstream texts and fails on new unknown
   canonical-text fallbacks. Target-specific unused-clarification warnings are
   expected. Cargo-deny remains the dependency policy gate.

   Four version-scoped source-text limitations are visible in the generated
   notices: `opensesame 0.1.1`, `objc2 0.6.4`, `objc2-encode 4.1.0`, and
   `objc2-foundation 0.3.2` declare MIT but do not provide full text at the checked
   source locations. The output includes canonical MIT text and declared author
   metadata without inventing copyright statements. Resolve/review these gaps
   before claiming complete attribution coverage. Native vendored-code notices,
   SDK terms and non-Cargo components also require release review; automated
   crate-license collection alone does not establish those obligations.

6. Commit the release preparation and ensure the working tree is clean.

## crates.io

Publishing requires `cargo-workspaces` and a configured crates.io token:

```bash
just publish-dry-run
just publish
```

The workspace publisher handles dependency order and registry propagation.
Conceptually the order is:

1. `gravityfile-core`, `gravityfile-ops`, and `gravityfile-plugin`
2. `gravityfile-analyze` and `gravityfile-scan`
3. `gravityfile-tui`
4. `gravityfile`

Never reuse a version already published to crates.io.

## GitHub binaries

Create and push the matching tag:

```bash
just tag v0.3.1
```

The tag recipe refuses a mismatched version or dirty working tree. The release
workflow repeats formatting, lint, and tests before building Linux, macOS, and
Windows archives. After it completes:

1. Verify every target archive and checksum is attached.
2. Download at least one archive and verify it against `SHA256SUMS`.
3. Run `grav --version` and one scan from the extracted archive.
4. Confirm the generated release notes and changelog agree.

## Release artifact validation

Before uploading binary assets, the release job runs
`python3 scripts/verify-release.py release-artifacts final-release` (Python 3.11
or newer). It requires exactly one archive and its build-produced SHA-256 file
for each of the five declared targets. Missing or unexpected files, symlinks,
checksum mismatches, malformed checksum records, duplicate archive members,
unexpected paths, missing binaries, and non-regular binaries fail the gate.
Unix binaries must have executable permissions and no special permission bits.
Each binary must be nonempty and at most 512 MiB. Contents are streamed through
the archive decoder without extracting or executing them. TAR packages must end
with at least two zero blocks and at most 1 MiB of zero padding. Validation reads
gzip streams through their end, including concatenated members, so truncated or
corrupt compression trailers fail even when the supplied SHA-256 matches.

Before TAR extension payloads are processed, the verifier limits each archive to
32 physical headers, 64 KiB per extension, and 256 KiB of aggregate extension
payload. This includes nested PAX and GNU long-name/link headers, which are not
normally visible during member iteration. Ordinary PAX timestamps remain
supported. Unsupported physical member types, including old GNU sparse headers,
are rejected before processing. The guard uses Python's internal `_proc_member`
hook; regression tests must remain enabled when upgrading Python. This metadata
policy is not a total decoder memory or CPU limit. PAX GNU sparse variants 0.0,
0.1, and 1.0 are rejected before sparse-map parsing, including metadata in global
headers. [Sparse-format qualification](qualification/tar-sparse.json) covers these
dispatch paths with invalid map data to confirm early rejection.
[Metadata qualification](qualification/tar-metadata.json) includes
Python 3.13 and 3.14 host runs. A separate [Colima run](qualification/python312-linux.json)
passed all 56 Python tests on Linux ARM64 with Python 3.12.14; remote CI remains
unverified.
Release preflight, build, and publication jobs explicitly set up Python 3.12 before
running Python tooling, including the preflight regression suite.

ZIP initialization has a 1 MiB cumulative metadata-read budget, enforced before
the central directory is allocated. Seeking does not replenish the budget;
read-to-end calls use at most the remaining allowance plus one detection byte.
At most three ZIP members are permitted: the two binaries and an optional empty
root directory. Only stored and deflated compression are accepted, matching the
release packaging contract; LZMA and other decoder formats are rejected before
payload decoding. Directory entries with nonempty payloads are rejected.
[ZIP qualification](qualification/zip-metadata.json) covers synthetic archives
on Python 3.13/3.14 and the later Python 3.12 Colima run, not native Windows
`Compress-Archive` execution. The metadata
budget is a byte-input bound, not an exact process-memory or CPU-time guarantee.

Unix packaging sets `COPYFILE_DISABLE=1` to exclude macOS AppleDouble metadata
sidecars while preserving executable permissions. The native packaging regression
test includes resource-fork metadata on macOS and verifies the resulting archive.

Archives are copied into private staging before checksum and content validation.
Before copying, artifact discovery checks the exact two-level layout: five target
directories, each containing its archive and checksum as regular files. Unknown
entries, including empty directories, fail immediately without recursive descent.
Source and target directory symlinks are rejected. Discovery retains at most the
fixed expected names and stops at the first unexpected entry; it does not scan
arbitrary nested trees. [Layout qualification](qualification/artifact-layout.json)
covers these checks, without claiming protection against ancestor replacement
between filesystem calls.
Both checks inspect the staged bytes that become the final upload assets, so a
download-path replacement cannot separate the hashed data from inspected data.
Checksum reads are capped at 257 bytes, rejecting inputs larger than 256 bytes.
Each compressed archive is limited to 1,040 MiB (two maximum-sized binaries plus
16 MiB of container overhead). The staging copy checks the opened file type and
size and enforces the byte limit while reading, including growth after the size
check. Unix opens reject leaf symlinks and do not wait on a substituted FIFO.
Across the fixed five-target set, staged archive data is at most 5,200 MiB;
downloaded inputs already present on disk are outside this staging budget.
Only after every staged archive passes does the script write the combined
`SHA256SUMS` and rename staging to the final upload directory. Failures do not
leave a partial final directory. Existing output directories are rejected.
This does not provide isolation from a hostile process running as the same user,
ancestor-directory replacement protection, or an archive-decoder memory/CPU budget.

The gate's regression tests run in ordinary CI, release preflight, and
`just release-check`; run them locally with:

```bash
python3 -m unittest discover -s scripts -p 'test_*.py'
```

Checksums establish consistency with the build-produced digest, not independent
provenance or authenticity. This validation does not prove the target ABI,
code-signing identity, notarization status, or runtime behavior of each binary.
Those require platform-specific qualification and release provenance checks.

## Built-binary smoke checks

Run `python3 scripts/smoke-release.py` after building, or supply `--binary-dir`
for a target-specific build directory. The script reads the expected workspace
version from Cargo.toml, executes both binary names, and checks strict versioned
JSON from scan, export, duplicates, and age against a known temporary fixture.
It checks exact file/directory counts, duplicate paths, and wasted bytes.
Each report runs at the exact descendant-entry budget and must reject a smaller
budget without emitting a report. An over-budget export must preserve the
existing destination and leave no temporary output behind.
Recovery fixtures also verify complete/partial native-path JSON, retained valid
records beside malformed journals, locked rename-intent inspection, refusal of
payload restoration from copy-undo and committed overwrite journals, and explicit
copy-undo restoration without replaying rename intents. Fixtures use Unix bytes
or Windows UTF-16 units and handle Windows extended drive/UNC paths. Each
invocation has a 30-second timeout. Successful output records both binary hashes;
failures exit nonzero and the temporary fixture is cleaned up.

The local release gate runs this after its optimized build. The release workflow
runs it for each build not using `cross`, before signing and packaging. Cross-built
Linux ARM binaries still require separate runtime qualification. These checks do
not exercise the interactive terminal, OS signing policy, or hostile filesystem
races. Their hashes describe the pre-signing binaries, not the final signed assets.

For local Linux ARM64 qualification on an Apple Silicon host, use the
[Colima recipe](../scripts/qualification/linux/README.md). The recorded
[Linux qualification](qualification/linux-arm64.json) includes workspace tests,
optimized builds, and both CLI smoke runs on Rust 1.90 with source hashes and
compressed logs. It qualifies that local build, not independently cross-built
release assets or the complete release gate.

## macOS signing configuration

macOS release builds validate signing configuration before certificate import.
With all six Apple signing/notarization settings absent, builds use the documented
ad-hoc mode. If any setting is present, all six must be nonempty and certificate
import must succeed; import failure no longer silently falls back to ad-hoc signing.
The validation helper prints only the selected mode or missing setting names.
Identity and notarization credentials enter shell commands through quoted
environment variables rather than interpolation into executable shell text.
This policy has synthetic configuration tests, but no certificate import, signing,
notarization, or Gatekeeper acceptance was qualified by those tests.

Developer ID builds request JSON from `notarytool submit --wait` and require
`scripts/verify-notarization.py` to confirm exactly `Accepted` with a submission
UUID before packaging. The verifier caps input at 1 MiB and rejects malformed
JSON, duplicate fields, and missing or invalid result fields without printing
response contents. Submission responses stay outside the release asset directory.
The accepted status follows Apple's documented
[notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow).
The `spctl` invocation is explicitly diagnostic; it is not proof of acceptance
on a user's machine. [Synthetic verification evidence](qualification/notarization.json)
does not establish actual Apple submission or end-user Gatekeeper behavior.

## Native platform checks

Workflow actions are pinned to full commit hashes resolved from their official
repositories, with the previous tag/branch retained in comments for review.
[Resolution evidence](qualification/action-pins.json) records the pins. Weekly
GitHub Actions updates are configured in `.github/dependabot.yml`; review those
pull requests and require CI before merging. See [GitHub's Dependabot action-update
documentation](https://docs.github.com/en/code-security/how-tos/secure-your-supply-chain/secure-your-dependencies/auto-update-actions).
Pinning freezes action code, not the toolchains, tools, runner images, or external
packages installed by that code. It is not proof that each action has been audited.

The Rust test matrix configures Python 3.12 on Linux, macOS, and Windows and
runs the release-tooling regression suite on each native filesystem. This covers
platform-specific cache locking where supported; Unix-only permission/symlink
fixtures remain explicitly skipped on Windows. Each job also builds default-feature
CLI binaries and runs `smoke-release.py --binary-dir target/debug` against both
entry points, including native recovery-path JSON. Jobs have a 45-minute timeout.
These debug-build checks complement optimized release smoke tests; they do not
replace signing or interactive-terminal qualification. The expanded matrix has
been linted locally and its commands exercised on macOS; remote Windows/Linux
results for this wiring are not yet recorded.

## Scheduled supply-chain monitoring

The Rust workflow includes a daily supply-chain check at 07:23 UTC and supports
manual dispatch. Scheduled runs execute only the supply-chain job; pushes, pull
requests, and manual dispatch retain the full quality/test/MSRV jobs. The
supply-chain job refreshes advisories through `cargo deny check`, enforces
`deny.toml`, checks unused dependencies, and has a 15-minute timeout. Its checkout
does not retain repository credentials, and workflow permissions remain read-only.

The schedule takes effect only after the workflow exists on the default branch.
GitHub can delay scheduled runs, so this is periodic monitoring rather than a
real-time security guarantee; see [GitHub's schedule documentation](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#schedule).
Workflow failures remain visible through ordinary GitHub Actions results and
repository notification settings. The local configuration has been linted, but
no successful remote scheduled run is recorded yet.

## Package source portability

The packaged-workspace verifier preflights every archive member before writing
any extracted file. In addition to path traversal, links, duplicates, and
file/parent conflicts, it rejects case-folded NFC aliases at all path prefixes,
including implicit directories. It also rejects reserved device names (including
COM/LPT superscript forms), forbidden/control characters, leading/trailing
ASCII spaces, trailing periods, and surrogate characters. The Windows naming
checks follow [Microsoft's filename conventions](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file).

This is an intentionally conservative package policy: some rejected pairs could
coexist on a case-sensitive filesystem. Portable Unicode names and ordinary
internal spaces remain supported. These checks do not model every filesystem
alias, path-length rule, or extraction resource limit, and do not replace runtime
qualification on supported platforms.

After namespace validation, extraction reserves the package root with an exclusive
directory creation. Existing directories, files, and dangling symlinks are rejected
before extraction. A later extraction failure can leave a partial directory inside
the verifier's temporary workspace; the surrounding temporary-workspace cleanup
removes it.

The build-cache lease requires a regular lock file with one link. New locks use
mode 0600, and Unix opens use no-follow and nonblocking flags with a second type
check on the opened descriptor. The requested cache directory must be usable
before any previous content cache is retired. These checks cover leaf substitution
and invalid targets; they do not pin ancestor directories or provide equivalent
Windows protection against concurrent reparse-point changes.


## Package verification disk headroom

`verify-packaged-workspace.py` requires 4 GiB free on both the Cargo target and
temporary-source volumes before extracting packages or retiring a verification
cache. Insufficient space exits with a diagnostic before compilation. This is a
configurable minimum, not a build-size prediction, reservation, or guarantee
against concurrent disk consumption. Standalone verification accepts
`--min-free-bytes N`; zero disables the minimum. Successful output records the
chosen threshold. The standard release gate uses the default. Free disk space
when this check fails; overriding it does not resolve an actual capacity problem.


Package verification reads each compressed archive through a regular-file
descriptor and retains at most 64 MiB, rejecting an excess-byte probe before
parsing or extraction. Unix opens refuse leaf symlinks and avoid blocking on a
substituted FIFO. The digest and namespace parser use the same retained bytes.
The compressed-input bound alone does not constrain TAR metadata expansion,
filesystem read time, or concurrent edits within the read window.
[Qualification](qualification/package-input.json) includes all seven existing
package archives and synthetic boundary/special-file regressions.


Before TAR parsing, the verifier decompresses the complete gzip stream into
a spool (8 MiB in memory before spilling to temporary storage), rejects decoded
input above 256 MiB, and validates gzip trailers, including concatenated members.
Before extraction it allows at most 10,000 members, caps each regular file at
64 MiB, and rejects sparse files and directory payloads.
[Qualification](qualification/package-decoded.json) covers corruption, expansion,
member limits, sparse files, and existing package compatibility. TAR extension
metadata still parses before member checks; these limits do not establish total
RSS, CPU deadlines, or a bound on every parser allocation.


A physical TAR preflight now runs before Python's extension processing. It
checks checksums and payload offsets, requires two zero end blocks, rejects
nonzero trailing data, and allows at most 20,000 physical headers. Extension
records are limited to 64 KiB each, 4 MiB total, and 32 consecutive records.
Sparse formats and PAX size overrides are rejected before full parsing; the
conservative policy also rejects their reserved byte markers within PAX values.
Package paths are limited to 4,096 UTF-8 bytes and 128 components before
namespace normalization. [Qualification](qualification/package-header.json)
proves malformed fixtures fail before `tarfile.open` and checks all seven
existing archives. These policy bounds do not establish an exact process
memory ceiling or read/CPU deadline.
