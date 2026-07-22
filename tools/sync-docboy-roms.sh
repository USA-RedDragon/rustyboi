#!/usr/bin/env bash
# Idempotent sync of the docboy test corpus into the gitignored
# gb-test-roms/docboy/ dir. Mirrors tools/run-suites.sh::sync_gbchwtests_roms:
# a shallow, pinned checkout copied into place, never vendored into git.
#
# The corpus (~250 MiB of ROMs + ~1,395 reference PNGs) is Docheinstein/docboy's
# own test suite. We pin an exact commit for reproducibility; re-running is a
# no-op once the pin is present (sentinel check).
#
# Fast path: set DOCBOY_CLONE=/path/to/an/existing/docboy/checkout to copy from
# a local clone (verified against the pin) instead of fetching over the network.
set -eu

PIN=bb4c525a68f099d31d1da0f0c7eac83f3efda5be
URL=https://github.com/Docheinstein/docboy

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/gb-test-roms/docboy"
SENTINEL="$DEST/.docboy-sha"

if [ -f "$SENTINEL" ] && [ "$(cat "$SENTINEL")" = "$PIN" ] \
    && [ -f "$DEST/tests/config/dmg.json" ]; then
    echo "docboy corpus already synced at $PIN"
    exit 0
fi

tmp=""
cleanup() { [ -n "$tmp" ] && rm -rf "$tmp"; }
trap cleanup EXIT

if [ -n "${DOCBOY_CLONE:-}" ] && [ -d "$DOCBOY_CLONE/tests/config" ]; then
    src="$DOCBOY_CLONE"
    got="$(git -C "$src" rev-parse HEAD 2>/dev/null || echo unknown)"
    if [ "$got" != "$PIN" ]; then
        echo "!!  DOCBOY_CLONE is at $got, not the pin $PIN" >&2
    fi
    echo "Copying docboy corpus from local clone $src"
else
    tmp="$(mktemp -d)"
    git -C "$tmp" init -q
    git -C "$tmp" remote add origin "$URL"
    # GitHub allows fetching a specific reachable SHA (allowReachableSHA1InWant).
    git -C "$tmp" fetch -q --depth 1 origin "$PIN"
    git -C "$tmp" checkout -q FETCH_HEAD
    src="$tmp"
    echo "Fetched docboy $PIN over the network"
fi

mkdir -p "$DEST/tests"
for sub in config results roms; do
    if [ ! -d "$src/tests/$sub" ]; then
        echo "!!  missing $src/tests/$sub" >&2
        exit 1
    fi
    rm -rf "$DEST/tests/$sub"
    cp -a "$src/tests/$sub" "$DEST/tests/$sub"
done

echo "$PIN" >"$SENTINEL"
roms=$(find "$DEST/tests/roms" -type f | wc -l)
pngs=$(find "$DEST/tests/results" -name '*.png' | wc -l)
echo "Synced docboy corpus ($PIN): $roms ROM files, $pngs reference PNGs -> $DEST"
