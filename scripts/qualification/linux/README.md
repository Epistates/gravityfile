# Linux qualification with Colima

Run from the repository root on an ARM64 host. The image pins the Rust 1.90
baseline by digest. Docker uses Colima's existing profile; start it with
`colima start` if needed. These commands do not change application containers.

```sh
docker --context colima buildx build \
  --output type=docker,dest=/tmp/gravityfile-linux-qualification.tar \
  -t gravityfile-linux-qualification:msrv scripts/qualification/linux
docker --context colima load --input /tmp/gravityfile-linux-qualification.tar

docker --context colima run --rm --name gravityfile-linux-qualification \
  --mount "type=bind,source=$PWD,target=/workspace,readonly" \
  --mount type=volume,source=gravityfile-linux-cargo,target=/home/qualification/.cargo \
  --mount type=volume,source=gravityfile-linux-target,target=/build-target \
  gravityfile-linux-qualification:msrv cargo fetch --locked

docker --context colima run --rm --network none --name gravityfile-linux-qualification \
  --mount "type=bind,source=$PWD,target=/workspace,readonly" \
  --mount type=volume,source=gravityfile-linux-cargo,target=/home/qualification/.cargo \
  --mount type=volume,source=gravityfile-linux-target,target=/build-target \
  gravityfile-linux-qualification:msrv sh scripts/qualification/linux/check.sh
```

The explicit archive import avoids relying on BuildKit's image-store export.
During local qualification the image disappeared between invocations; the cause
was not established. If this occurs, repeat the archive import immediately before
the run. Do not prune unrelated images or change existing application containers.
Source is mounted read-only. UID 10001 runs tests; build outputs and dependencies
live in dedicated Docker volumes. Dependencies are fetched separately so the
test/build/smoke phase can run offline. Retain these volumes for incremental runs;
use fresh volume names to qualify an uncached build. Avoid concurrent runs using
the same container name or volumes. Keep source unchanged while checks run.

The check stops on the first failure. It first requires Bash, Zsh, and Fish and
runs all 15 shell-wrapper cases without missing-shell skips. It then runs
all-feature workspace tests, builds optimized default-feature binaries, and
exercises both binary names with release smoke fixtures and five Unix PTY
scenarios (silent/responding graphics query, editor success/failure handoff, and
initialization output failure). The image provides Python 3.11 for these checks;
it does not run the Python 3.12 release packaging gate.
The release-tooling regression suite has a separate Python 3.12 recipe:

```sh
sh scripts/qualification/linux/python-check.sh
```

This uses a digest-pinned official Python image, UID 10001, read-only source and
root filesystem, no networking during tests, a 512 MiB memory limit, and 128 MiB
temporary storage. Only image retrieval needs network access. It runs the Python
regression suite, including native Linux TAR fixtures, without compiling Rust or
executing the complete packaged-workspace build. Keep source unchanged during
the run. [Recorded evidence](../../../docs/qualification/python312-linux.json)
contains the Python version, image digest, input hashes, and test log.

This is Linux container runtime and scoped PTY evidence, not Windows, arbitrary
terminal/editor compatibility,
host-mounted filesystem mutation, power-loss, signing, registry publication,
or complete release qualification. Package repositories remain mutable despite
the pinned base image; retain the resulting image ID with qualification evidence.
