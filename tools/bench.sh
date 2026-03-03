#!/usr/bin/env bash
# Perf benchmark across representative retail ROMs (loads zips in-memory).
set -euo pipefail
BIN="${BIN:-./target/release/bench}"
FRAMES="${FRAMES:-5000}"
GBC=/home/reddragon/Downloads/gb/GBC
GB=/home/reddragon/Downloads/gb/GB
ROMS=(
  "$GBC/Harry Potter and The Chamber of Secrets (UE) (M10) [C][!].zip"
  "$GBC/Legend of Zelda, The - Oracle of Ages (U) [C][!].zip"
  "$GBC/Mario Tennis (U) [C][!].zip"
  "$GB/Super Mario Land (W) (V1.1) [!].zip"
  "$GB/Kirby's Dream Land (UE) [!].zip"
)
for r in "${ROMS[@]}"; do
  "$BIN" "$r" "$FRAMES" 2>/dev/null
done
