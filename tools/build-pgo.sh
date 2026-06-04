#!/usr/bin/env bash
# Build the native runnable binaries (test-runner + desktop app) with PGO:
# same source, byte-identical emulation, ~40-60% faster codegen (the branch-
# dense per-dot PPU dispatch is PGO's sweet spot).
#
# Profile generation is delegated to tools/pgo.sh (the canonical, shared
# profile every build script uses); this script just generates it (if needed)
# and does the -Cprofile-use build into target/pgo-use.
#
# Usage:
#   ./tools/build-pgo.sh --sweep DIR...    # BEST: gameplay-representative
#                                          #   (masher drives ROMs into play)
#   RB_PGO_ROMS="dirA dirB" ./tools/build-pgo.sh   # same, via env
#   ./tools/build-pgo.sh                   # fallback: test-suite workload
#
# See tools/pgo.sh for the representativeness notes (a ~20-60 game gameplay set
# ~= the whole library). The profile is NOT committed — it lives under target/.
#
# Output: target/pgo-use/release/{rustyboi,bench,rustyboi-test-runner}
# Requires: rustup component add llvm-tools
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "==> Generating PGO profile (tools/pgo.sh gen)"
tools/pgo.sh gen "$@"

FLAGS="$(tools/pgo.sh flags)"
[ -n "$FLAGS" ] || { echo "PGO profile unusable; aborting" >&2; exit 1; }

echo "==> Optimized build (-Cprofile-use) into target/pgo-use"
RUSTFLAGS="$FLAGS ${RUSTFLAGS:-}" \
    cargo build --release -p rustyboi-test-runner -p rustyboi-platform --target-dir target/pgo-use

echo
echo "PGO binaries under target/pgo-use/release/:"
ls -1 target/pgo-use/release/ | grep -E '^(rustyboi|bench|rustyboi-test-runner)$' | sed 's/^/  /'
echo "Benchmark with: BIN=./target/pgo-use/release/bench ./tools/bench.sh"
