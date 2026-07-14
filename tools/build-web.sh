#!/usr/bin/env bash
# Build the rustyboi web (WASM) frontend for Firefox, optimized.
#
# wasm-pack bundles an ancient wasm-opt that cannot validate the post-MVP wasm
# features LLVM emits (bulk-memory memory.copy/fill, sign-ext, ...), failing even
# with --enable-* flags. So wasm-opt is disabled in rustyboi-web/Cargo.toml and
# we run a current binaryen wasm-opt here with `-all` (enable all features —
# required even on the latest binaryen).
#
# Requires binaryen:  sudo pacman -S binaryen
#   (or your distro's package / https://github.com/WebAssembly/binaryen/releases)
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

OUT=www/pkg
WASM="rustyboi-web/$OUT/rustyboi_web_bg.wasm"

# `build-web.sh profiling` builds optimized wasm that KEEPS the name section so a
# browser profiler shows real function names. wasm-pack `--profiling` = release
# opt + debug info retained; `wasm-opt -g` preserves those names through -O3.
# (bundled wasm-opt is disabled for the profiling profile too, in Cargo.toml.)
MODE="${1:-release}"
if [ "$MODE" = "profiling" ]; then
    PACK_PROFILE="--profiling"; OPT_FLAGS="-O3 -g -all"
    # The release profile has `strip = true`, which drops the wasm name section;
    # --profiling still builds on release, so override strip + add line tables so
    # a browser profiler resolves real Rust function names (incl. rustyboi-core).
    export CARGO_PROFILE_RELEASE_STRIP=false
    export CARGO_PROFILE_RELEASE_DEBUG=1
    echo "==> PROFILING build (optimized + symbols)"
else
    PACK_PROFILE="--release"; OPT_FLAGS="-O3 -all"
fi

echo "==> wasm-pack build ($PACK_PROFILE; bundled wasm-opt disabled)"
# wasm-pack 0.13.1 re-reads a pre-existing pkg/package.json and dies on its own
# `"files": [...]` array ("invalid type: sequence, expected a string"). Wipe the
# out-dir first so it always generates fresh.
rm -rf "rustyboi-web/$OUT"
# PGO best-effort (host rustc == the one wasm-pack drives; IR profile is
# target-portable, empty/incompatible => no-op). RB_NO_PGO=1 opts out.
PGO_FLAGS="$("$(dirname "${BASH_SOURCE[0]}")/pgo.sh" flags 2>/dev/null || true)"
RUSTFLAGS="$PGO_FLAGS ${RUSTFLAGS:-}" \
    wasm-pack build rustyboi-web --target web --out-dir "$OUT" "$PACK_PROFILE"

if command -v wasm-opt >/dev/null 2>&1; then
    before=$(stat -c%s "$WASM")
    echo "==> wasm-opt $OPT_FLAGS ($(wasm-opt --version))"
    wasm-opt $OPT_FLAGS "$WASM" -o "$WASM.tmp"
    mv "$WASM.tmp" "$WASM"
    echo "    $before -> $(stat -c%s "$WASM") bytes"
else
    echo "WARNING: wasm-opt not found — shipping UN-optimized wasm."
    echo "  Install binaryen:  sudo pacman -S binaryen"
fi

# Content-hash the final wasm into the service worker's cache id: changing bytes
# here are how the browser detects a new version and shows the OPT-IN update
# prompt (see www/sw.js, which importScripts this). Generated + gitignored.
sw_id=$(sha256sum "$WASM" | cut -c1-12)
printf 'self.BUILD_ID = "%s";\n' "$sw_id" > "rustyboi-web/www/sw-version.js"
echo "==> sw-version.js BUILD_ID=$sw_id"

echo
# One wasm module serves BOTH threads (`--target web` emits a single module):
#  - the worker (www/worker.js) uses the `Emulator` export to run the Session;
#  - the main thread (www/index.html) uses the `WebApp` export to render the
#    egui UI over the game with wgpu's WebGL2 backend, plus `WebAudio`.
# The wasm is larger than the old 2D-canvas build (it now bundles egui + wgpu +
# naga); that is expected. `wasm-opt -O3 -all` above trims it.
echo "Serve for Firefox:  (cd rustyboi-web/www && python3 -m http.server 8080)"
echo "Then open http://localhost:8080/ — use the File menu (top bar) to load a ROM."
