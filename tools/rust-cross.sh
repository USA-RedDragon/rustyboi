#!/usr/bin/env bash
# Shared machinery for cross-compiling rustyboi inside the USA-RedDragon/rust-cross
# container image. NOT executed directly — sourced by the `libretro`/`native`
# Makefile recipes and ios/Makefile, which supply their own output handling.
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

    # gnu-linux targets: link against an OLD glibc (2.17, ~CentOS 7 / 2013) via
    # cargo-zigbuild, so the RELEASE binaries run on essentially any still-used
    # distro instead of only the image's Trixie-era glibc. Without this, Rust
    # std's pidfd_spawnp floors them at GLIBC_2.39 (ubuntu 24.04 / Debian 13 only).
    # zig is the linker here, so the lld/bfd tweaks below don't apply. Needs zig +
    # cargo-zigbuild in the image; if absent, falls back to a plain build (2.39)
    # with a note, so the build still succeeds.
    local zig_target=""
    if [ -z "$variant" ]; then
        case "$triple" in
            # riscv64 didn't exist in glibc until 2.27
            riscv64gc-*-linux-gnu)         zig_target="$triple.2.27" ;;
            *-linux-gnu|*-linux-gnueabihf) zig_target="$triple.2.17" ;;
        esac
    fi

    local cflags_pre=""
    if [ -n "$zig_target" ] && [[ "$triple" == riscv64gc-*-linux-gnu ]]; then
        local shim=/tmp/rb-zig-libc-shim
        local cvar="CFLAGS_$(echo "$triple" | tr '-' '_')"
        cflags_pre="mkdir -p $shim/gnu && : > $shim/gnu/stubs-lp64d.h; export $cvar=\"-I$shim \${$cvar:-}\";"
    fi

    # Compose RUSTFLAGS: crt-static preference + riscv64-musl's ld.lld (its
    # musl-cross-make binutils `ld` is too old for Rust's RISC-V ISA attributes).
    local flags=""
    case "$crt" in
        static)  flags="-C target-feature=+crt-static" ;;
        dynamic) flags="-C target-feature=-crt-static" ;;
    esac
    case "$triple" in
        *-pc-windows-gnullvm) flags="$flags -C target-feature=+crt-static" ;;
    esac
    if [ "$triple" = riscv64gc-unknown-linux-musl ]; then
        flags="$flags -C link-arg=-B\$(rustc --print sysroot)/lib/rustlib/\$(rustc --print host-tuple)/bin/gcc-ld -C link-arg=-fuse-ld=lld"
    fi
    # rust-lld mislinks i686 *executables* as elf64 (`cc -m32` finds the 32-bit
    # objects, but lld stays 64-bit); GNU ld handles the -m32 emulation correctly.
    # (Only the non-zig path needs this; zig's linker handles -m32 itself.)
    if [ -z "$zig_target" ] && [ "$triple" = i686-unknown-linux-gnu ]; then
        flags="$flags -C link-arg=-fuse-ld=bfd"
    fi

    # PGO: the shared profile lives under target/ (mounted at /project). Apply
    # it best-effort. IR PGO is target-portable, so a profile collected on the
    # container's x86_64 host helps every cross target; rustc only WARNS on an
    # incompatible profile (never breaks the build), and `make -s pgo-flags`
    # (run in-container below) suppresses it if the container toolchain rejects
    # it. RB_NO_PGO=1 opts out. Requires GNU Make >= 3.82 in the image.
    # Profiles are version-keyed (profile-<rustc>.profdata); the in-container
    # pgo-flags picks the one matching the CONTAINER's rustc, or emits nothing
    # (host and container toolchains often differ). Attempt whenever any profile
    # exists on the host tree (mounted at /project).
    local pgo_pre=""
    if [ -z "${RB_NO_PGO:-}" ] && ls "$PROJECT_ROOT"/target/pgo/profile-*.profdata >/dev/null 2>&1; then
        pgo_pre='PGO="$(make -s pgo-flags 2>/dev/null || true)";'
        flags="$flags \$PGO"
    fi

    # Per-target env (linker / deployment target). Seed with pgo_pre so $PGO is
    # set before the RUSTFLAGS export that references it, then the C-header shim.
    local pre="$pgo_pre$cflags_pre"
    case "$variant" in
        android)
            local pfx lvar
            pfx="$(_rc_android_clang_prefix "$triple")"
            lvar="CARGO_TARGET_$(echo "$triple" | tr 'a-z-' 'A-Z_')_LINKER"
            pre="$pre export $lvar=\"\$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin/${pfx}${ANDROID_API}-clang\";" ;;
        ios)
            # Modern LC_BUILD_VERSION (platform iOS), not the legacy min-10.0 cmd.
            pre="$pre"' export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-14.0}";' ;;
    esac
    [ -n "$flags" ] && pre="$pre export RUSTFLAGS=\"$flags \${RUSTFLAGS:-}\";"

    # Chown only what this build wrote back to the host. rc_emit_* set RC_CHOWN
    # to the per-target dir (target/cross/<name>) so parallel `make -j` builds
    # don't each chown -R the whole shared target/ tree. Default: the whole tree.
    # gnu-linux → cargo-zigbuild against glibc 2.17 (artifacts still land in the
    # plain target/<triple>/release, so callers are unchanged); everything else →
    # cargo build with the toolchain the image sets up per target.
    local build_cmd="cargo build --target $triple --release $*"
    if [ -n "$zig_target" ]; then
        build_cmd="if command -v cargo-zigbuild >/dev/null 2>&1 && command -v zig >/dev/null 2>&1; then
            cargo zigbuild --target $zig_target --release $*
        else
            echo 'rc_build: zig/cargo-zigbuild absent — plain build (glibc floors at ~2.39; add them to the image for portable gnu binaries)' >&2
            cargo build --target $triple --release $*
        fi"
    fi

    local chown_path="${RC_CHOWN:-target}"
    local incmd="set -e; $pre
        $build_cmd
        chown -R $HOST_UIDGID $chown_path"
    local cname="rcb-$$-$RANDOM"
    trap '"$ENGINE" kill "'"$cname"'" >/dev/null 2>&1 || true; exit 130' INT TERM
    $ENGINE run --rm --name "$cname" \
        -v "$PROJECT_ROOT":/project -w /project \
        -v "$CARGO_VOL":/usr/local/cargo/registry \
        "$IMAGE" sh -c "$incmd"
    local rc=$?
    trap - INT TERM
    return $rc
}

# --- Makefile fan-out support (make libretro/native TARGETS=...) --------------
# The Makefile's libretro-%/native-% pattern rules call the rc_emit_* helpers
# below, one per target name. This keeps the target matrix a single source of
# truth here (Make asks for the name list via rc_names/rc_names_native) and the
# per-OS artifact naming in bash, while Make owns the DAG, per-target
# success/failure, and `-j` parallelism.

# Print all target names (rc_names) / just the desktop-capable ones (rc_names_
# native = gnu/darwin/windows + musl; android/ios have no desktop GUI target).
rc_names()        { local t; for t in "${TARGETS[@]}"; do field "$t" 0; done; }
rc_names_native() { local t; for t in "${TARGETS[@]}"; do case "$(field "$t" 3)" in ""|musl) field "$t" 0 ;; esac; done; return 0; }
# Test runners: same set as native (everything with a run host: non-android/ios).
rc_names_runner() { rc_names_native; }

# Warm the shared cargo registry ONCE before the parallel per-target builds, so
# they only READ it — concurrent cold downloads into $CARGO_VOL could race (the
# .package-cache lock is per-container here, not shared across containers).
rc_fetch() {
    rc_engine
    local cname="rcf-$$-$RANDOM"
    trap '"$ENGINE" kill "'"$cname"'" >/dev/null 2>&1 || true; exit 130' INT TERM
    $ENGINE run --rm --name "$cname" \
        -v "$PROJECT_ROOT":/project -w /project \
        -v "$CARGO_VOL":/usr/local/cargo/registry \
        "$IMAGE" sh -c "cargo fetch --locked || cargo fetch"
    local rc=$?
    trap - INT TERM
    return $rc
}

rc_run() {
    rc_engine
    local cname="rcr-$$-$RANDOM"
    trap '"$ENGINE" kill "'"$cname"'" >/dev/null 2>&1 || true; exit 130' INT TERM
    "$ENGINE" run --rm --name "$cname" \
        -v "$PROJECT_ROOT":/project -w /project \
        -v "$CARGO_VOL":/usr/local/cargo/registry \
        "$IMAGE" sh -c "{ $*; }; rc=\$?; chown -R $HOST_UIDGID /project 2>/dev/null || true; exit \$rc"
    local rc=$?
    trap - INT TERM
    return $rc
}

# Build ONE libretro core for <name> into an ISOLATED target dir
# (target/cross/<name>, so parallel builds don't contend on cargo's per-target-
# dir build lock), then copy/rename into target/libretro/<name>/ by RetroArch's
# rules with rustyboi_libretro.info alongside.
rc_emit_libretro() {   # name
    local name="$1" entry triple os variant crt tdir cargo_art ra_name src dir info
    entry="$(rc_target_by_name "$name")" || { echo "Unknown target: $name" >&2; return 1; }
    triple="$(field "$entry" 1)"; os="$(field "$entry" 2)"; variant="$(field "$entry" 3)"
    rc_engine
    echo "==> libretro $name  ($triple)"
    crt=""; if [ "$variant" = musl ]; then crt="dynamic"; fi
    tdir="target/cross/$name"
    mkdir -p "$PROJECT_ROOT/$tdir"
    RC_CHOWN="$tdir" rc_build "$triple" "$variant" "$crt" -p rustyboi-libretro --target-dir "$tdir"
    case "$os" in
        windows)    cargo_art="rustyboi_libretro.dll" ;;
        darwin|ios) cargo_art="librustyboi_libretro.dylib" ;;
        *)          cargo_art="librustyboi_libretro.so" ;;
    esac
    case "$os" in
        windows) ra_name="rustyboi_libretro.dll" ;;
        darwin)  ra_name="rustyboi_libretro.dylib" ;;
        ios)     ra_name="rustyboi_libretro_ios.dylib" ;;
        android) ra_name="rustyboi_libretro_android.so" ;;
        *)       ra_name="rustyboi_libretro.so" ;;
    esac
    src="$PROJECT_ROOT/$tdir/$triple/release/$cargo_art"
    [ -f "$src" ] || { echo "ERROR: expected artifact missing: $src" >&2; return 1; }
    dir="$PROJECT_ROOT/target/libretro/$name"; mkdir -p "$dir"; cp -f "$src" "$dir/$ra_name"
    info="$PROJECT_ROOT/rustyboi-libretro/rustyboi_libretro.info"
    if [ -f "$info" ]; then cp -f "$info" "$dir/"; fi
    echo "    -> $dir/$ra_name"
}

# Build ONE desktop rustyboi binary for <name> into target/native/<name>/.
# Desktop targets: gnu/darwin/windows (empty variant) + musl. musl builds as
# DYNAMIC musl (crt-static off) so winit/wgpu can dlopen X11/Wayland/GPU at
# runtime — a static musl binary can't dlopen. android/ios have no desktop GUI.
rc_emit_native() {   # name
    local name="$1" entry triple os variant crt tdir art src dir
    entry="$(rc_target_by_name "$name")" || { echo "Unknown target: $name" >&2; return 1; }
    variant="$(field "$entry" 3)"
    crt=""
    case "$variant" in
        "") ;;
        musl) crt="dynamic" ;;
        *) echo "native: $name has no desktop GUI (variant=$variant)" >&2; return 1 ;;
    esac
    triple="$(field "$entry" 1)"; os="$(field "$entry" 2)"
    rc_engine
    echo "==> native $name  ($triple)"
    tdir="target/cross/$name"
    mkdir -p "$PROJECT_ROOT/$tdir"
    RC_CHOWN="$tdir" rc_build "$triple" "$variant" "$crt" -p rustyboi-platform --bin rustyboi --target-dir "$tdir"
    case "$os" in windows) art="rustyboi.exe" ;; *) art="rustyboi" ;; esac
    src="$PROJECT_ROOT/$tdir/$triple/release/$art"
    [ -f "$src" ] || { echo "ERROR: expected binary missing: $src" >&2; return 1; }
    dir="$PROJECT_ROOT/target/native/$name"; mkdir -p "$dir"; cp -f "$src" "$dir/$art"
    echo "    -> $dir/$art"
}

rc_emit_runner() {   # name
    local name="$1" entry triple os variant crt tdir art src dir
    entry="$(rc_target_by_name "$name")" || { echo "Unknown target: $name" >&2; return 1; }
    variant="$(field "$entry" 3)"
    crt=""
    case "$variant" in
        "") ;;
        musl) crt="dynamic" ;;
        *) echo "runner: $name has no test host (variant=$variant)" >&2; return 1 ;;
    esac
    triple="$(field "$entry" 1)"; os="$(field "$entry" 2)"
    rc_engine
    echo "==> runner $name  ($triple)"
    tdir="target/cross/$name"
    mkdir -p "$PROJECT_ROOT/$tdir"
    RC_CHOWN="$tdir" rc_build "$triple" "$variant" "$crt" -p rustyboi-test-runner --bin rustyboi-test-runner --target-dir "$tdir"
    case "$os" in windows) art="rustyboi-test-runner.exe" ;; *) art="rustyboi-test-runner" ;; esac
    src="$PROJECT_ROOT/$tdir/$triple/release/$art"
    [ -f "$src" ] || { echo "ERROR: expected runner missing: $src" >&2; return 1; }
    dir="$PROJECT_ROOT/target/runner/$name"; mkdir -p "$dir"; cp -f "$src" "$dir/$art"
    echo "    -> $dir/$art"
}
