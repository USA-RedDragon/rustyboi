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
#   ./tools/build-pgo.sh                 # workload = tools/bench.sh ROM set
#   ./tools/build-pgo.sh rom1.zip rom2.gb ...  # custom profiling workload
#
# Output binaries (byte-identical emulation, verified against the full suite
# board): target/pgo-use/release/{rustyboi,bench,rustyboi-test-runner}
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
    cargo build --release -p rustyboi-test-runner --bin bench --target-dir target/pgo-gen

echo "==> Phase 2: profiling workload"
if [ "$#" -gt 0 ]; then
    for rom in "$@"; do
        ./target/pgo-gen/release/bench "$rom" 800
    done
else
    # The standard bench basket (see tools/bench.sh for the ROM paths).
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
