#!/usr/bin/env bash
# Merge every .profraw in the instrumented target dir and export ONE lcov,
# mapped against every instrumented executable: the unit-test binaries AND the
# release test-runner that ran the ROM suites.
#
# Why not `cargo llvm-cov report`: it only maps artifacts recorded by its own
# cargo invocations. The release runner (built by raw `cargo build` under
# show-env, then de-atomized by tools/deatomize-coverage.py) is never
# discovered, so suite coverage silently reads 0% — verified empirically; nor
# do `cargo llvm-cov run` registration or disabling strip change that. So this
# drives llvm-profdata/llvm-cov directly.
#
# Usage: CARGO_TARGET_DIR=<instrumented tree> coverage-lcov.sh <out.lcov> [-p crate]...
#   The -p list must match the earlier `cargo test` so the same test
#   executables resolve (cargo test --no-run is a cached no-op here).

set -euo pipefail

TD="${CARGO_TARGET_DIR:?set CARGO_TARGET_DIR to the instrumented target dir}"
OUT="${1:?usage: coverage-lcov.sh <output.lcov> [-p crate]...}"
shift

LLVM_BIN="$(rustc --print target-libdir)/../bin"
[ -x "$LLVM_BIN/llvm-profdata" ] \
    || { echo "llvm-profdata not found (rustup component add llvm-tools-preview)" >&2; exit 1; }

"$LLVM_BIN/llvm-profdata" merge -sparse -o "$TD/rustyboi.profdata" "$TD"/*.profraw

# Every test executable cargo knows for the requested packages -- already built
# under the coverage env, so this is a no-op build that just prints artifacts.
objs="$(cargo test --no-run --message-format=json "$@" 2>/dev/null \
    | python3 -c '
import json, sys
for line in sys.stdin:
    try:
        m = json.loads(line)
    except ValueError:
        continue
    e = m.get("executable")
    if e and m.get("profile", {}).get("test"):
        print(e)')"

runner="$TD/release/rustyboi-test-runner"
[ -x "$runner" ] || { echo "instrumented runner not found at $runner" >&2; exit 1; }

args=""
for o in $objs; do args="$args -object $o"; done
# shellcheck disable=SC2086  # word-splitting of -object pairs is intended
"$LLVM_BIN/llvm-cov" export -format=lcov -instr-profile="$TD/rustyboi.profdata" \
    "$runner" $args \
    --ignore-filename-regex='(\.cargo/registry|\.rustup/|/rustc/)' > "$OUT"

echo "wrote $OUT ($(grep -c '^SF:' "$OUT") source files)"
