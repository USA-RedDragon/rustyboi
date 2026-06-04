#!/usr/bin/env bash
# Fetch the public No-Intro Game Boy + Game Boy Color DATs (ClrMamePro format)
# from libretro-database into a directory (default ./nointro-dats). These map a
# ROM's CRC32 to its canonical No-Intro name; `sweep run --names <dat>...` uses
# them to label the gallery and comparison reports. The DATs are public (not a
# secret) and are NOT committed — fetch them on demand.
#
#   ./tools/fetch-nointro-dats.sh [OUT_DIR]
#   ./target/release/sweep run --roms LIB/GB LIB/GBC --out out \
#       --names OUT_DIR/gb.dat OUT_DIR/gbc.dat
set -euo pipefail
OUT="${1:-nointro-dats}"
BASE="https://raw.githubusercontent.com/libretro/libretro-database/master/metadat/no-intro"
mkdir -p "$OUT"
curl -fsSL "$BASE/Nintendo%20-%20Game%20Boy.dat" -o "$OUT/gb.dat"
curl -fsSL "$BASE/Nintendo%20-%20Game%20Boy%20Color.dat" -o "$OUT/gbc.dat"
echo "fetched No-Intro DATs -> $OUT/{gb,gbc}.dat"
