# Recording qualification commands

Use `run-check.py` for a command whose actual outcome must be recorded:

```sh
python3 scripts/qualification/run-check.py \
  --output /tmp/ops-check.json --log /tmp/ops-check.log \
  --input Cargo.lock --input crates/gravityfile-ops/src/copy.rs \
  --input-glob 'crates/gravityfile-ops/src/**/*.rs' \
  -- cargo test -p gravityfile-ops --locked --offline
```

Report and log paths must be new; their parent directories must exist. The runner
captures combined stdout/stderr and records the literal command, working
directory, timestamps, elapsed time, actual exit code, log SHA-256 and hashes of
the explicitly named inputs before and after execution. A pass requires exit
code zero and unchanged readable inputs. Missing executables are recorded as
failures. The JSON report is staged beside its destination and installed by an
atomic hard link that refuses an occupied name. A concurrent report is never
replaced. Filesystems without hard-link support fail publication and retain the
command log rather than falling back to an overwrite.

Run one check per invocation. Do not wrap multiple checks in a shell command
that discards earlier exit statuses. Shell expansion is not implicit. The runner
returns the child failure code, or nonzero for changed inputs/recording failure.

This is outcome recording, not test-result interpretation or attestation. Name
all relevant inputs for the scope being claimed; omitted files, environment,
toolchain and dependency state are not fingerprinted automatically. Changes
that are reverted between hash observations are not detected. Reports do not
prove test coverage, power-loss durability or authenticity. Concurrent writers
should use distinct artifact paths; conflicting log/report publication fails.
There is no process-tree timeout or interrupt-resumption facility. An absent
report after interruption/recording failure is incomplete evidence, never a pass.

Repeat `--input-glob` to select source files dynamically. Quote patterns so the
runner expands them before and after the command, rather than the shell expanding
them once. Each pattern must initially match at least one file. Added, removed,
renamed or changed selected files make a successful command's record fail.
Patterns follow Python glob semantics, including `**`; select source locations
and keep generated files and report/log outputs outside them. Pattern-selected
paths retain their absolute lexical names, so removal of a selected symlink is
detected even when its target remains. Explicit inputs retain their existing
canonical-path behavior. File bytes are fingerprinted, not permissions or link
metadata; identical-content replacements and changes reverted between snapshots
are not detected. This is not a filesystem snapshot or hostile-writer defense.

The focused regression suite is included in the release gate's Python discovery:

```sh
python3 -m unittest discover -s scripts -p test_qualification_runner.py
```

Other qualification tools include [Linux runtime checks](linux/README.md) and
`terminal-smoke.py` for Unix terminal lifecycle scenarios.

The Rust workflow's platform test matrix records shell/tooling tests, Rust tests,
binary builds, CLI smoke checks and Unix terminal checks with this runner. An
environment record identifies the actual checkout commit (which may be a PR
merge commit), GitHub run and attempt, platform, architecture and toolchain
versions. Only explicitly selected GitHub context variables are recorded.
Each command fingerprints the workspace manifest, lockfile, dependency policy
and justfile, plus patterns for Rust sources, crate manifests, test fixtures,
Python tooling, shell wrappers and workflow YAML. Before/after snapshots detect
new selected files as well as modifications and deletions. The checkout commit
identifies the source revision; unselected files and reverted in-job mutations
remain outside this check.

The matrix always attempts to upload `ci-qualification/` as
`qualification-<matrix-os>-<run-id>-<attempt>` with a requested 30-day retention.
After a failure, later commands remain skipped; a missing report means the
command was not recorded as complete. Abrupt runner loss can prevent upload.
Download the artifact from the workflow run and inspect command exit codes,
recording state and logs together. These artifacts are diagnostic evidence,
not signed attestations. Quality, MSRV and supply-chain jobs still use their
ordinary workflow logs. Local checks do not establish successful remote artifact
publication or native Windows behavior.
