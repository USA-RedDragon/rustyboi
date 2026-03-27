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

echo "==> wasm-pack build (release; bundled wasm-opt disabled)"
wasm-pack build rustyboi-web --target web --out-dir "$OUT" --release

if command -v wasm-opt >/dev/null 2>&1; then
    before=$(stat -c%s "$WASM")
    echo "==> wasm-opt -O3 -all ($(wasm-opt --version))"
    wasm-opt -O3 -all "$WASM" -o "$WASM.tmp"
    mv "$WASM.tmp" "$WASM"
    echo "    $before -> $(stat -c%s "$WASM") bytes"
else
    echo "WARNING: wasm-opt not found — shipping UN-optimized wasm."
    echo "  Install binaryen:  sudo pacman -S binaryen"
fi

echo
echo "Serve for Firefox:  (cd rustyboi-web/www && python3 -m http.server 8080)"
echo "Then open http://localhost:8080/ and click \"Load ROM…\""
