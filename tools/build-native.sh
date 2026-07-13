#!/usr/bin/env bash
# Build the native rustyboi desktop app (the `rustyboi` binary from
# rustyboi-platform) for every GUI-capable target, inside the
# USA-RedDragon/rust-cross container image. Host-agnostic: needs only Docker or
# Podman; the cross-compile machinery is shared with build-libretro.sh (rust-cross.sh).
#
# The app is a GPU GUI (egui/wgpu + winit + rodio/cpal), so it can't be fully
# static (wgpu loads GPU drivers at runtime; cpal links ALSA at build time) and
# only runs on desktop OSes. The target set is therefore the "plain" targets from
# the shared table (empty variant = gnu Linux / macOS / Windows); musl, Android
# and iOS are excluded (no GUI libs / app-bundle targets). Binaries are linked
# dynamically against the system GUI/audio libraries, as desktop apps are.
#
# Usage:
#   ./build-native.sh --list                    # the target table
#   ./build-native.sh linux-x86_64 windows-x86_64
#   ./build-native.sh --all                     # every GUI-capable target
#   RUSTBOI_CROSS_IMAGE=... ./build-native.sh …   # override the image
#
# Output: target/native/<name>/rustyboi[.exe]
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/rust-cross.sh"

BIN="rustyboi"
PKG="rustyboi-platform"
OUT="$PROJECT_ROOT/target/native"

# Filter the shared matrix down to GUI-capable targets: empty variant selects the
# gnu-Linux / macOS / Windows targets (musl/android/ios have a variant tag). All
# five gnu-Linux arches are covered — the image (Debian Trixie) carries the
# X11/Wayland/ALSA multiarch dev libs, riscv64 included.
_native=()
for _t in "${TARGETS[@]}"; do [ -z "$(field "$_t" 3)" ] && _native+=("$_t"); done
TARGETS=("${_native[@]}")

usage() { sed -n '2,22p' "$0"; }

bin_name() {   # os -> the binary filename cargo emits
    case "$1" in
        windows) echo "${BIN}.exe" ;;
        *)       echo "${BIN}" ;;
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
    # Dynamic link (crt "") — a GPU GUI app can't be fully static.
    if ! rc_build "$triple" "$variant" "" -p "$PKG" --bin "$BIN"; then
        failed+=("$name"); continue
    fi
    src="$PROJECT_ROOT/target/$triple/release/$(bin_name "$os")"
    if [ ! -f "$src" ]; then
        echo "ERROR: expected binary missing: $src"; failed+=("$name"); continue
    fi
    dir="$OUT/$name"
    mkdir -p "$dir"
    cp -f "$src" "$dir/$(bin_name "$os")"
    echo "    -> $dir/$(bin_name "$os")"
    built+=("$name")
done

echo ""
echo "Done. Built: ${built[*]:-(none)}"
[ ${#failed[@]} -gt 0 ] && { echo "Failed: ${failed[*]}"; exit 1; }
echo "Binaries are under $OUT/<name>/."
