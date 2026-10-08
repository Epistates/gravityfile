#!/bin/sh
# Run from the repository root with an already-running Colima profile.
set -eu
test -f scripts/test_verify_release.py
test -f Cargo.toml
image='python@sha256:392307d22300de8b5986851a12d9176dfc0fc073e65bf6523ebd7dcbeb23564e'
# Fetch and run together: the local image store has lost images between calls.
docker --context colima pull "$image"
exec docker --context colima run --rm --pull never \
  --network none --read-only --cap-drop ALL --security-opt no-new-privileges \
  --user 10001:10001 --memory 512m --cpus 2 --pids-limit 64 \
  --tmpfs /tmp:rw,nosuid,nodev,size=128m \
  --mount "type=bind,source=$PWD,target=/workspace,readonly" \
  --workdir /workspace --env PYTHONDONTWRITEBYTECODE=1 "$image" \
  sh -c 'python -VV && uname -a && python -m unittest discover -s scripts -p "test_*.py"'
