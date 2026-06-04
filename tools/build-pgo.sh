#!/usr/bin/env bash
# Profile-guided-optimization build: same source, byte-identical emulation,
# substantially faster codegen (+40-60% measured on the bench basket — the
# branch-dense per-dot PPU dispatch is exactly PGO's sweet spot).
#
# Three phases:
#   1. instrumented build  (-Cprofile-generate) into target/pgo-gen
#   2. profile workload    (tools/bench.sh against the instrumented bench, or
#                           the ROMs you pass as arguments)
#   3. optimized build     (-Cprofile-use)      into target/pgo-use
#
# Usage:
#   ./tools/build-pgo.sh --sweep DIR...        # BEST: gameplay-representative —
#                                              #   profiles the whole library via
#                                              #   the sweep masher (real input,
#                                              #   past menus into gameplay)
#   ./tools/build-pgo.sh --list roms.txt       # one ROM path per line (no input:
#                                              #   menus/attract only)
#   ./tools/build-pgo.sh rom1.zip rom2.gb ...  # explicit workload ROMs
#   ./tools/build-pgo.sh                       # fallback: tools/bench.sh ROM set
#
# REPRESENTATIVENESS MATTERS: profile on a broad, diverse set (15+ games across
# DMG/CGB, mappers, halt-heavy vs busy-wait, sprite/window/HDMA-heavy titles),
# NOT just the games you benchmark. Measured on this codebase: a 19-game
# diverse profile transferred +35-53% to six held-out games never profiled,
# while a 5-game profile overfit those five by ~2-5% relative. The emulation
# is byte-identical either way (full suite board verified against PGO
# binaries); only speed distribution across titles changes.
#
# Output binaries: target/pgo-use/release/{rustyboi,bench,rustyboi-test-runner}
#
# Requires: rustup component add llvm-tools
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SYSROOT="$(rustc --print sysroot)"
PROFDATA="$(find "$SYSROOT" -name llvm-profdata | head -1)"
if [ -z "$PROFDATA" ]; then
    echo "llvm-profdata not found; run: rustup component add llvm-tools" >&2
    exit 1
fi

PGO_DIR="$ROOT/target/pgo-profiles"
rm -rf "$PGO_DIR"
mkdir -p "$PGO_DIR"

echo "==> Phase 1: instrumented build"
RUSTFLAGS="-Cprofile-generate=$PGO_DIR" \
    cargo build --release -p rustyboi-test-runner --bin bench --bin sweep --target-dir target/pgo-gen

echo "==> Phase 2: profiling workload"
if [ "${1:-}" = "--sweep" ]; then
    shift
    [ "$#" -gt 0 ] || { echo "--sweep needs at least one ROM directory" >&2; exit 1; }
    # The sweep masher drives every ROM past its title screen into gameplay
    # (deterministic seeded input), so the profile weights real workloads —
    # sprites in flight, window splits, HDMA streams — not menu idles.
    #
    # --jobs 1 is REQUIRED: -Cprofile-generate counters are atomic, and running
    # the instrumented binary multi-threaded makes every increment ping-pong the
    # counter cache line across cores (false sharing) — a ~100x slowdown that
    # never finishes. Single-threaded the instrumented build runs near native
    # (~600 fps), so a few dozen ROMs profile in ~1-2 minutes.
    ./target/pgo-gen/release/sweep run --roms "$@" \
        --out "$PGO_DIR/sweep-out" --frames 1800 --warmup 600 --no-screens \
        --jobs 1 2> /dev/null
elif [ "${1:-}" = "--list" ]; then
    LIST="${2:?--list needs a file}"
    while IFS= read -r rom; do
        [ -n "$rom" ] || continue
        echo "    $rom"
        ./target/pgo-gen/release/bench "$rom" 800 > /dev/null
    done < "$LIST"
elif [ "$#" -gt 0 ]; then
    for rom in "$@"; do
        ./target/pgo-gen/release/bench "$rom" 800
    done
else
    echo "    (fallback: bench-basket workload — pass --list <file> with a"
    echo "     broad, diverse ROM set for profiles that generalize; see header)"
    BIN=./target/pgo-gen/release/bench FRAMES=800 ./tools/bench.sh
fi

echo "==> Merging profiles"
"$PROFDATA" merge -o "$PGO_DIR/merged.profdata" "$PGO_DIR"/*.profraw

echo "==> Phase 3: optimized build"
RUSTFLAGS="-Cprofile-use=$PGO_DIR/merged.profdata" \
    cargo build --release -p rustyboi-test-runner -p rustyboi-platform --target-dir target/pgo-use

echo
echo "PGO binaries under target/pgo-use/release/:"
ls -1 target/pgo-use/release/ | grep -E '^(rustyboi|bench|rustyboi-test-runner)$' | sed 's/^/  /'
echo "Benchmark with: BIN=./target/pgo-use/release/bench ./tools/bench.sh"
