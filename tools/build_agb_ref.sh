#!/bin/bash
# Build the Gambatte-AGB bootstrap reference generator against the repo's
# prebuilt libgambatte.a. The library lives in the main checkout's gambatte-core
# (read-only reference); we only link it, never modify it.
set -e
GAMBATTE=/home/reddragon/projects/rustyboi/gambatte-core
LIBA="$GAMBATTE/libgambatte/libgambatte.a"
INC="$GAMBATTE/libgambatte/include"
OUT="$(dirname "$0")/gambatte_agb_ref"
SRC="$(dirname "$0")/gambatte_agb_ref.cpp"

if [ ! -f "$LIBA" ]; then
  echo "libgambatte.a not found at $LIBA" >&2
  exit 1
fi

g++ -O2 -fno-exceptions -fno-rtti -DHAVE_STDINT_H \
    -I"$INC" "$SRC" "$LIBA" -lz -lm -o "$OUT"
echo "built $OUT"
