#!/usr/bin/env bash
# Build the rustyboi libretro core (RetroArch) for every libretro target, inside
# the USA-RedDragon/rust-cross container image. Host-agnostic: needs only Docker
# or Podman — every target is cross-compiled in the container (see rust-cross.sh),
# then the cdylib is renamed into RetroArch's rules:
#   Linux/BSD   <corename>_libretro.so       (no `lib` prefix)
#   macOS       <corename>_libretro.dylib    (no `lib` prefix)
#   iOS         <corename>_libretro_ios.dylib
#   Windows     <corename>_libretro.dll
#   Android     <corename>_libretro_android.so   (mandatory `_android` suffix)
#
# Usage:
#   ./build-libretro.sh --list                     # the target table
#   ./build-libretro.sh linux-x86_64 windows-x86_64 ios-arm64
#   ./build-libretro.sh --all                      # every target
#   RUSTBOI_CROSS_IMAGE=... ./build-libretro.sh …   # override the image
#
# Output: target/libretro/<name>/<corename>_libretro[.so|.dylib|.dll]
#         (or _android.so), each with rustyboi_libretro.info copied alongside.

set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/rust-cross.sh"

CORENAME="rustyboi"
OUT="$PROJECT_ROOT/target/libretro"
INFO="$PROJECT_ROOT/rustyboi-libretro/rustyboi_libretro.info"

usage() { sed -n '2,20p' "$0"; }

cargo_artifact() {   # os -> the cdylib filename cargo emits
    case "$1" in
        windows)     echo "${CORENAME}_libretro.dll" ;;
        darwin|ios)  echo "lib${CORENAME}_libretro.dylib" ;;
        *)           echo "lib${CORENAME}_libretro.so" ;;
    esac
}
retroarch_name() {   # os -> the filename RetroArch expects
    case "$1" in
        windows) echo "${CORENAME}_libretro.dll" ;;
        darwin)  echo "${CORENAME}_libretro.dylib" ;;
        ios)     echo "${CORENAME}_libretro_ios.dylib" ;;
        android) echo "${CORENAME}_libretro_android.so" ;;
        *)       echo "${CORENAME}_libretro.so" ;;
    esac
}

rc_parse_args "$@"
rc_engine
echo "Image: $IMAGE"

built=() failed=()
for name in "${SELECTED[@]}"; do
    entry="$(rc_target_by_name "$name")"
    triple="$(field "$entry" 1)"; os="$(field "$entry" 2)"; variant="$(field "$entry" 3)"

    echo ""
    echo "==> $name  ($triple)"
    crt=""; [ "$variant" = musl ] && crt="dynamic"
    if ! rc_build "$triple" "$variant" "$crt" -p rustyboi-libretro; then
        failed+=("$name"); continue
    fi
    src="$PROJECT_ROOT/target/$triple/release/$(cargo_artifact "$os")"
    if [ ! -f "$src" ]; then
        echo "ERROR: expected artifact missing: $src"; failed+=("$name"); continue
    fi
    dir="$OUT/$name"
    mkdir -p "$dir"
    cp -f "$src" "$dir/$(retroarch_name "$os")"
    [ -f "$INFO" ] && cp -f "$INFO" "$dir/"
    echo "    -> $dir/$(retroarch_name "$os")"
    built+=("$name")
done

echo ""
echo "Done. Built: ${built[*]:-(none)}"
[ ${#failed[@]} -gt 0 ] && { echo "Failed: ${failed[*]}"; exit 1; }
echo "Cores are under $OUT/<name>/ with rustyboi_libretro.info alongside."
