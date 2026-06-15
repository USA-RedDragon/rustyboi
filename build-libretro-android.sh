#!/usr/bin/env bash
# Cross-compile the rustyboi libretro core for Android (RetroArch).
#
# Produces, per ABI, a `librustyboi_libretro_android.so` — note the mandatory
# `_android` suffix: RetroArch's Android core loader only recognises cores whose
# filename ends in `_libretro_android.so` and ignores a plain `_libretro.so`.
#
# Prerequisites:
#   - cargo install cargo-ndk
#   - rustup target add aarch64-linux-android armv7-linux-androideabi \
#                       x86_64-linux-android i686-linux-android
#   - Android NDK (resolved from ANDROID_NDK_ROOT/ANDROID_NDK_HOME, NDK_HOME,
#     or $ANDROID_HOME/ndk/*). Tested with r26 (26.1.10909125).
#   - A host libclang for bindgen. libclang 22 mis-parses a libretro struct, so
#     this script prefers libclang 21 if present; override with LIBCLANG_PATH.
#
# Usage:
#   ./build-libretro-android.sh                 # arm64-v8a (most phones), release
#   ./build-libretro-android.sh --all           # all four ABIs
#   ./build-libretro-android.sh arm64-v8a x86_64
#   API=24 ./build-libretro-android.sh --all    # override min API (default 21)
#
# Output: target/libretro-android/<abi>/librustyboi_libretro_android.so
#         plus rustyboi_libretro.info copied alongside.

set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$PROJECT_ROOT"

API="${API:-21}"
ALL_ABIS=(arm64-v8a armeabi-v7a x86_64 x86)

# ---------- Parse ABIs ----------
ABIS=()
for arg in "$@"; do
    case "$arg" in
        --all) ABIS=("${ALL_ABIS[@]}") ;;
        -h|--help) sed -n '2,28p' "$0"; exit 0 ;;
        arm64-v8a|armeabi-v7a|x86_64|x86) ABIS+=("$arg") ;;
        *) echo "Unknown argument: $arg"; echo "Usage: $0 [--all] [arm64-v8a armeabi-v7a x86_64 x86]"; exit 1 ;;
    esac
done
[ ${#ABIS[@]} -eq 0 ] && ABIS=(arm64-v8a)

# ABI -> clang target triple (for bindgen --target; correct layouts on 32-bit).
abi_triple() {
    case "$1" in
        arm64-v8a)    echo "aarch64-linux-android${API}" ;;
        armeabi-v7a)  echo "armv7a-linux-androideabi${API}" ;;
        x86_64)       echo "x86_64-linux-android${API}" ;;
        x86)          echo "i686-linux-android${API}" ;;
    esac
}

# ---------- Preflight ----------
command -v cargo-ndk >/dev/null 2>&1 || { echo "ERROR: cargo-ndk not installed. Run: cargo install cargo-ndk"; exit 1; }

NDK_ROOT="${ANDROID_NDK_ROOT:-${ANDROID_NDK_HOME:-${NDK_HOME:-}}}"
ANDROID_HOME="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
if [ -z "$NDK_ROOT" ] && [ -n "$ANDROID_HOME" ] && [ -d "$ANDROID_HOME/ndk" ]; then
    NDK_ROOT=$(ls -d "$ANDROID_HOME/ndk/"*/ 2>/dev/null | sort -V | tail -1 || true)
fi
NDK_ROOT="${NDK_ROOT%/}"
if [ -z "$NDK_ROOT" ] || [ ! -d "$NDK_ROOT" ]; then
    echo "ERROR: Could not locate Android NDK."
    echo "  Set ANDROID_NDK_ROOT/ANDROID_NDK_HOME, or install: sdkmanager 'ndk;26.1.10909125'"
    exit 1
fi
export ANDROID_NDK_ROOT="$NDK_ROOT" ANDROID_NDK_HOME="$NDK_ROOT"

SYSROOT="$NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/sysroot"
[ -d "$SYSROOT" ] || { echo "ERROR: NDK sysroot not found at $SYSROOT"; exit 1; }

# bindgen runs with the *host* libclang. Prefer 21 (22 mis-parses retro_game_info).
if [ -z "${LIBCLANG_PATH:-}" ]; then
    for c in /usr/lib/llvm21/lib /usr/lib/llvm-21/lib /usr/lib64/llvm21/lib; do
        [ -d "$c" ] && { LIBCLANG_PATH="$c"; break; }
    done
fi
export LIBCLANG_PATH="${LIBCLANG_PATH:-}"
[ -n "$LIBCLANG_PATH" ] && echo "Using libclang: $LIBCLANG_PATH" \
    || echo "WARN: LIBCLANG_PATH unset; relying on system libclang (must be <= 21)."

OUT="$PROJECT_ROOT/target/libretro-android"
INFO="$PROJECT_ROOT/rustyboi-libretro/rustyboi_libretro.info"
echo "Using NDK: $NDK_ROOT   API: $API   ABIs: ${ABIS[*]}"

# ---------- Build per ABI ----------
# One cargo-ndk invocation per ABI so bindgen gets the correct per-ABI --target
# (a single BINDGEN_EXTRA_CLANG_ARGS can't vary across ABIs, and a wrong target
# corrupts pointer/size_t layouts on the 32-bit ABIs).
for abi in "${ABIS[@]}"; do
    triple="$(abi_triple "$abi")"
    echo ""
    echo "==> $abi ($triple)"
    BINDGEN_EXTRA_CLANG_ARGS="--target=$triple --sysroot=$SYSROOT" \
        cargo ndk -t "$abi" -P "$API" -o "$OUT" build -p rustyboi-libretro --release

    src="$OUT/$abi/librustyboi_libretro.so"
    # RetroArch Android cores have NO `lib` prefix: `<name>_libretro_android.so`.
    dst="$OUT/$abi/rustyboi_libretro_android.so"
    [ -f "$src" ] || { echo "ERROR: expected artifact missing: $src"; exit 1; }
    mv -f "$src" "$dst"
    # Drop the stray core-lib cdylib cargo-ndk also copies; RetroArch only needs the core.
    rm -f "$OUT/$abi/librustyboi_core_lib.so" "$OUT/$abi/librustyboi_libretro_android.so"
    [ -f "$INFO" ] && cp -f "$INFO" "$OUT/$abi/"
    echo "    -> $dst"
done

echo ""
echo "Done. RetroArch Android keeps cores in an app-private dir you can't push"
echo "into directly, so install via the menu instead:"
echo "  1. adb push $OUT/arm64-v8a/rustyboi_libretro_android.so /sdcard/Download/"
echo "  2. In RetroArch: Main Menu -> Load Core -> Install or Restore a Core"
echo "     -> Downloads -> rustyboi_libretro_android.so  (RetroArch copies it in)"
echo "  3. Load Core -> Rustyboi, then Load Content with a .gb/.gbc ROM."
echo "  (Confirm the real cores path under Settings -> Directory -> Cores.)"
