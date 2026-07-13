#!/usr/bin/env bash
# Shared machinery for cross-compiling rustyboi inside the USA-RedDragon/rust-cross
# container image. NOT executed directly — sourced by build-libretro.sh and
# build-native.sh, which supply their own output handling.
#
# The full target list lives here (TARGETS) so both scripts stay in lockstep.
# The core defines the libretro C ABI by hand (no bindgen), so cross-compiling
# needs only a linker — no per-target C sysroot.
#
# Contract for the caller:
#   - optionally define  usage()  (printed for -h/--help)
#   - call  rc_parse_args "$@"    -> fills the SELECTED array with target names
#   - call  rc_engine             -> resolves $ENGINE and checks the image
#   - for each selected target, look up its fields and call
#       rc_build <triple> <variant> <crt> <extra cargo args...>
#     then place target/<triple>/release/<artifact> wherever it wants.
#
# Fields per target:  name | rust-triple | os | variant
#   os      drives artifact naming in the caller (linux/darwin/ios/windows/android)
#   variant "musl" | "android" | "ios" | "" — selects the per-target linker tweak
# The <crt> arg to rc_build is "static" | "dynamic" | "" (crt-static preference —
# a musl *cdylib* needs "dynamic"; a fully static *binary* wants "static").

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/.."

IMAGE="${RUSTBOI_CROSS_IMAGE:-ghcr.io/usa-reddragon/rust-cross:1.94.1}"
CARGO_VOL="${RUSTBOI_CARGO_VOLUME:-rustyboi-xcross-cargo}"   # persists the crate cache
ANDROID_API="${ANDROID_API:-21}"
HOST_UIDGID="$(id -u):$(id -g)"
ENGINE=""

# The complete rustyboi cross-compilation matrix, shared by all build scripts.
TARGETS=(
    "linux-x86_64|x86_64-unknown-linux-gnu|linux|"
    "linux-i686|i686-unknown-linux-gnu|linux|"
    "linux-aarch64|aarch64-unknown-linux-gnu|linux|"
    "linux-armv7|armv7-unknown-linux-gnueabihf|linux|"
    "linux-riscv64|riscv64gc-unknown-linux-gnu|linux|"
    "linux-x86_64-musl|x86_64-unknown-linux-musl|linux|musl"
    "linux-i686-musl|i686-unknown-linux-musl|linux|musl"
    "linux-aarch64-musl|aarch64-unknown-linux-musl|linux|musl"
    "linux-armv7-musl|armv7-unknown-linux-musleabihf|linux|musl"
    "linux-riscv64-musl|riscv64gc-unknown-linux-musl|linux|musl"
    "android-arm64|aarch64-linux-android|android|android"
    "android-armv7|armv7-linux-androideabi|android|android"
    "android-x86_64|x86_64-linux-android|android|android"
    "android-x86|i686-linux-android|android|android"
    "macos-x86_64|x86_64-apple-darwin|darwin|"
    "macos-aarch64|aarch64-apple-darwin|darwin|"
    "ios-arm64|aarch64-apple-ios|ios|ios"
    "windows-x86_64|x86_64-pc-windows-gnullvm|windows|"
    "windows-arm64|aarch64-pc-windows-gnullvm|windows|"
    "windows-i686|i686-pc-windows-gnullvm|windows|"
)

# --- target-table helpers ---
field() { local IFS='|'; read -ra f <<<"$1"; echo "${f[$2]:-}"; }

rc_target_by_name() {
    local t
    for t in "${TARGETS[@]}"; do [ "$(field "$t" 0)" = "$1" ] && { echo "$t"; return 0; }; done
    return 1
}

rc_print_list() {
    printf '%-20s %s\n' "NAME" "RUST TRIPLE"
    local t
    for t in "${TARGETS[@]}"; do printf '%-20s %s\n' "$(field "$t" 0)" "$(field "$t" 1)"; done
}

# Parse CLI args into the global SELECTED array (--all / --list / --help / names).
rc_parse_args() {
    SELECTED=()
    local arg t
    for arg in "$@"; do
        case "$arg" in
            --all)  for t in "${TARGETS[@]}"; do SELECTED+=("$(field "$t" 0)"); done ;;
            --list) rc_print_list; exit 0 ;;
            -h|--help) if declare -F usage >/dev/null; then usage; else rc_print_list; fi; exit 0 ;;
            --*)    echo "Unknown option: $arg"; echo "Try --help or --list."; exit 1 ;;
            *)      if rc_target_by_name "$arg" >/dev/null; then SELECTED+=("$arg")
                    else echo "Unknown target: $arg"; echo "Run '$0 --list' for valid names."; exit 1; fi ;;
        esac
    done
    if [ ${#SELECTED[@]} -eq 0 ]; then
        echo "No targets given."; echo "Run '$0 --list', or pass targets / --all."; exit 1
    fi
}

# Resolve the container engine and check the image is available.
rc_engine() {
    ENGINE="${RUSTBOI_CONTAINER_ENGINE:-}"
    if [ -z "$ENGINE" ]; then
        if command -v docker >/dev/null 2>&1; then ENGINE=docker
        elif command -v podman >/dev/null 2>&1; then ENGINE=podman
        else echo "ERROR: need Docker or Podman (set RUSTBOI_CONTAINER_ENGINE)."; exit 1; fi
    fi
    $ENGINE image inspect "$IMAGE" >/dev/null 2>&1 || \
        echo "note: image $IMAGE not present locally; $ENGINE will try to pull it."
}

# Android NDK clang wrapper prefix (armv7 -> armv7a is the quirk).
_rc_android_clang_prefix() {
    case "$1" in
        aarch64-linux-android)    echo "aarch64-linux-android" ;;
        armv7-linux-androideabi)  echo "armv7a-linux-androideabi" ;;
        x86_64-linux-android)     echo "x86_64-linux-android" ;;
        i686-linux-android)       echo "i686-linux-android" ;;
    esac
}

# Cross-compile in the container. crt = "static" | "dynamic" | "". Args after crt
# are appended verbatim to `cargo build --target <triple> --release`. Artifacts
# land in the mounted target/<triple>/release, chowned back to the host user.
rc_build() {   # triple variant crt <extra cargo args...>
    local triple="$1" variant="$2" crt="$3"; shift 3

    # Compose RUSTFLAGS: crt-static preference + riscv64-musl's ld.lld (its
    # musl-cross-make binutils `ld` is too old for Rust's RISC-V ISA attributes).
    local flags=""
    case "$crt" in
        static)  flags="-C target-feature=+crt-static" ;;
        dynamic) flags="-C target-feature=-crt-static" ;;
    esac
    if [ "$triple" = riscv64gc-unknown-linux-musl ]; then
        flags="$flags -C link-arg=-B\$(rustc --print sysroot)/lib/rustlib/\$(rustc --print host-tuple)/bin/gcc-ld -C link-arg=-fuse-ld=lld"
    fi
    # rust-lld mislinks i686 *executables* as elf64 (`cc -m32` finds the 32-bit
    # objects, but lld stays 64-bit); GNU ld handles the -m32 emulation correctly.
    if [ "$triple" = i686-unknown-linux-gnu ]; then
        flags="$flags -C link-arg=-fuse-ld=bfd"
    fi

    # Per-target env (linker / deployment target).
    local pre=""
    case "$variant" in
        android)
            local pfx lvar
            pfx="$(_rc_android_clang_prefix "$triple")"
            lvar="CARGO_TARGET_$(echo "$triple" | tr 'a-z-' 'A-Z_')_LINKER"
            pre="export $lvar=\"\$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin/${pfx}${ANDROID_API}-clang\";" ;;
        ios)
            # Modern LC_BUILD_VERSION (platform iOS), not the legacy min-10.0 cmd.
            pre='export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-13.0}";' ;;
    esac
    [ -n "$flags" ] && pre="$pre export RUSTFLAGS=\"$flags \${RUSTFLAGS:-}\";"

    local incmd="set -e; $pre
        cargo build --target $triple --release $*
        chown -R $HOST_UIDGID target"
    $ENGINE run --rm \
        -v "$PROJECT_ROOT":/project -w /project \
        -v "$CARGO_VOL":/usr/local/cargo/registry \
        "$IMAGE" sh -c "$incmd"
}
