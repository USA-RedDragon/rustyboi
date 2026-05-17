#!/usr/bin/env bash
# Build the rustyboi libretro core (RetroArch) for every libretro target, inside
# the USA-RedDragon/rust-cross container image — it bakes in all the cross
# toolchains (gnu + musl linkers, llvm-mingw, osxcross, the Android NDK).
#
# Host-agnostic: the only requirement on the machine running this is a working
# Docker (or Podman) engine. Every target is cross-compiled *in the container*
# via `cargo build --target <triple>`, then the artifact is renamed into
# RetroArch's rules:
#   Linux   <corename>_libretro.so       (no `lib` prefix)
#   macOS       <corename>_libretro.dylib    (no `lib` prefix)
#   Windows     <corename>_libretro.dll
#   Android     <corename>_libretro_android.so   (mandatory `_android` suffix)
#
# The core defines the libretro C ABI by hand (rustyboi-libretro-sys) — no
# bindgen — so cross-compiling needs only a linker, no per-target C sysroot.
# The only per-target tweaks the script applies:
#   - musl targets    -> RUSTFLAGS=-C target-feature=-crt-static (musl defaults
#                        to a static crt, which can't produce a cdylib).
#   - riscv64 musl    -> Rust's bundled ld.lld (the image's musl-cross-make
#                        binutils `ld` is too old for Rust's RISC-V ISA attrs).
#   - android targets -> the NDK clang linker (the image has the NDK but not
#                        cargo-ndk).
#
# Usage:
#   ./build-libretro.sh --list                     # the target table
#   ./build-libretro.sh linux-x86_64 windows-x86_64 android-arm64
#   ./build-libretro.sh --all                      # every target
#   RUSTBOI_CROSS_IMAGE=... ./build-libretro.sh …   # override the image
#
# Output: target/libretro/<name>/<corename>_libretro[.so|.dylib|.dll]
#         (or _android.so), each with rustyboi_libretro.info copied alongside.

set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "$0")" && pwd)/.."
cd "$PROJECT_ROOT"

CORENAME="rustyboi"
IMAGE="${RUSTBOI_CROSS_IMAGE:-ghcr.io/usa-reddragon/rust-cross:1.94.1}"
CARGO_VOL="${RUSTBOI_CARGO_VOLUME:-rustyboi-xcross-cargo}"   # persists the crate cache
ANDROID_API="${ANDROID_API:-21}"
OUT="$PROJECT_ROOT/target/libretro"
INFO="$PROJECT_ROOT/rustyboi-libretro/rustyboi_libretro.info"
HOST_UIDGID="$(id -u):$(id -g)"

# ---------- Target table: name | rust-triple | os | variant ----------
#   os      drives artifact naming (linux/darwin/windows/android)
#   variant "musl" | "android" | "" — selects the per-target linker tweak
TARGETS=(
    "linux-x86_64|x86_64-unknown-linux-gnu|linux|"
    "linux-aarch64|aarch64-unknown-linux-gnu|linux|"
    "linux-armv7|armv7-unknown-linux-gnueabihf|linux|"
    "linux-x86_64-musl|x86_64-unknown-linux-musl|linux|musl"
    "linux-aarch64-musl|aarch64-unknown-linux-musl|linux|musl"
    "linux-armv7-musl|armv7-unknown-linux-musleabihf|linux|musl"
    "linux-riscv64-musl|riscv64gc-unknown-linux-musl|linux|musl"
    "android-arm64|aarch64-linux-android|android|android"
    "android-armv7|armv7-linux-androideabi|android|android"
    "android-x86_64|x86_64-linux-android|android|android"
    "android-x86|i686-linux-android|android|android"
    "macos-x86_64|x86_64-apple-darwin|darwin|"
    "macos-aarch64|aarch64-apple-darwin|darwin|"
    "windows-x86_64|x86_64-pc-windows-gnullvm|windows|"
    "windows-arm64|aarch64-pc-windows-gnullvm|windows|"
)

field() { local IFS='|'; read -ra f <<<"$1"; echo "${f[$2]:-}"; }
target_by_name() {
    for t in "${TARGETS[@]}"; do [ "$(field "$t" 0)" = "$1" ] && { echo "$t"; return 0; }; done
    return 1
}
print_list() {
    printf '%-20s %s\n' "NAME" "RUST TRIPLE"
    for t in "${TARGETS[@]}"; do printf '%-20s %s\n' "$(field "$t" 0)" "$(field "$t" 1)"; done
}

# ---------- Parse args ----------
SELECTED=()
for arg in "$@"; do
    case "$arg" in
        --all)  for t in "${TARGETS[@]}"; do SELECTED+=("$(field "$t" 0)"); done ;;
        --list) print_list; exit 0 ;;
        -h|--help) sed -n '2,36p' "$0"; exit 0 ;;
        --*)    echo "Unknown option: $arg"; echo "Try --help or --list."; exit 1 ;;
        *)      if target_by_name "$arg" >/dev/null; then SELECTED+=("$arg")
                else echo "Unknown target: $arg"; echo "Run '$0 --list' for valid names."; exit 1; fi ;;
    esac
done
[ ${#SELECTED[@]} -eq 0 ] && { echo "No targets given."; echo "Run '$0 --list', or pass targets / --all."; exit 1; }

# ---------- Container engine ----------
ENGINE="${RUSTBOI_CONTAINER_ENGINE:-}"
if [ -z "$ENGINE" ]; then
    if command -v docker >/dev/null 2>&1; then ENGINE=docker
    elif command -v podman >/dev/null 2>&1; then ENGINE=podman
    else echo "ERROR: need Docker or Podman (set RUSTBOI_CONTAINER_ENGINE)."; exit 1; fi
fi
$ENGINE image inspect "$IMAGE" >/dev/null 2>&1 || \
    echo "note: image $IMAGE not present locally; $ENGINE will try to pull it."

# ---------- Artifact naming ----------
cargo_artifact() {   # os -> the filename cargo emits
    case "$1" in
        windows) echo "${CORENAME}_libretro.dll" ;;
        darwin)  echo "lib${CORENAME}_libretro.dylib" ;;
        *)       echo "lib${CORENAME}_libretro.so" ;;
    esac
}
retroarch_name() {   # os -> the filename RetroArch expects
    case "$1" in
        windows) echo "${CORENAME}_libretro.dll" ;;
        darwin)  echo "${CORENAME}_libretro.dylib" ;;
        android) echo "${CORENAME}_libretro_android.so" ;;
        *)       echo "${CORENAME}_libretro.so" ;;
    esac
}

# Android NDK clang wrapper prefix (armv7 -> armv7a is the quirk).
android_clang_prefix() {
    case "$1" in
        aarch64-linux-android)    echo "aarch64-linux-android" ;;
        armv7-linux-androideabi)  echo "armv7a-linux-androideabi" ;;
        x86_64-linux-android)     echo "x86_64-linux-android" ;;
        i686-linux-android)       echo "i686-linux-android" ;;
    esac
}

# ---------- Build one target in the container ----------
build_target() {
    local triple="$1" os="$2" variant="$3" name="$4"
    local pre=""   # shell run inside the container before `cargo build`
    case "$variant" in
        musl)
            pre='export RUSTFLAGS="-C target-feature=-crt-static ${RUSTFLAGS:-}";'
            # riscv64's musl-cross-make binutils `ld` is too old for the RISC-V
            # ISA attributes Rust's std emits — link with Rust's bundled ld.lld.
            [ "$triple" = riscv64gc-unknown-linux-musl ] && \
                pre='export RUSTFLAGS="-C target-feature=-crt-static -C link-arg=-B$(rustc --print sysroot)/lib/rustlib/$(rustc --print host-tuple)/bin/gcc-ld -C link-arg=-fuse-ld=lld ${RUSTFLAGS:-}";'
            ;;
        android)
            local pfx lvar
            pfx="$(android_clang_prefix "$triple")"
            lvar="CARGO_TARGET_$(echo "$triple" | tr 'a-z-' 'A-Z_')_LINKER"
            pre="export $lvar=\"\$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin/${pfx}${ANDROID_API}-clang\";"
            ;;
    esac

    local incmd="set -e; $pre
        cargo build --target $triple --release -p rustyboi-libretro
        chown -R $HOST_UIDGID target/$triple"

    echo "==> $name  ($triple, in $IMAGE)"
    $ENGINE run --rm \
        -v "$PROJECT_ROOT":/project -w /project \
        -v "$CARGO_VOL":/usr/local/cargo/registry \
        "$IMAGE" sh -c "$incmd" || return 1

    local src="$PROJECT_ROOT/target/$triple/release/$(cargo_artifact "$os")"
    [ -f "$src" ] || { echo "ERROR: expected artifact missing: $src"; return 1; }
    local dir="$OUT/$name"
    mkdir -p "$dir"
    cp -f "$src" "$dir/$(retroarch_name "$os")"
    [ -f "$INFO" ] && cp -f "$INFO" "$dir/"
    echo "    -> $dir/$(retroarch_name "$os")"
}

# ---------- Run ----------
echo "Image: $IMAGE"
built=() failed=()
for name in "${SELECTED[@]}"; do
    entry="$(target_by_name "$name")"
    triple="$(field "$entry" 1)"; os="$(field "$entry" 2)"; variant="$(field "$entry" 3)"
    echo ""
    if build_target "$triple" "$os" "$variant" "$name"; then built+=("$name"); else
        failed+=("$name")
    fi
done

echo ""
echo "Done. Built: ${built[*]:-(none)}"
[ ${#failed[@]} -gt 0 ] && { echo "Failed: ${failed[*]}"; exit 1; }
echo "Cores are under $OUT/<name>/ with rustyboi_libretro.info alongside."
