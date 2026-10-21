#!/usr/bin/env bash
# Check grav's ncdu support against ncdu itself: grav must read ncdu's dump of
# a tree with the same total as its own scan, and ncdu must load grav's dump
# and export it again unchanged in total.
#
# Usage: scripts/ncdu-interop.sh path/to/grav   (needs ncdu and jq)
set -euo pipefail

grav=${1:?usage: $0 path/to/grav}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

tree=$work/tree
mkdir -p "$tree/a/b" "$tree/empty"
head -c 50000 /dev/urandom > "$tree/a/data"
ln "$tree/a/data" "$tree/a/b/hard-link"
ln -s data "$tree/a/symlink"
printf 'contents' > "$tree/$(printf 'not-utf8-\377')"
printf 'contents' > "$tree/quote\"back\\slash"

total() { "$grav" scan "$1" -f json | jq -r '.data.total.size'; }

expected=$(total "$tree")

ncdu -x -o "$work/ncdu.json" "$tree"
from_ncdu=$(total "$work/ncdu.json")

"$grav" export "$tree" -f ncdu -o "$work/grav.json"
ncdu -f "$work/grav.json" -o "$work/round-trip.json"
round_trip=$(total "$work/round-trip.json")

echo "grav scan: $expected; ncdu's dump: $from_ncdu; grav's dump via ncdu: $round_trip"
test "$from_ncdu" = "$expected"
test "$round_trip" = "$expected"
