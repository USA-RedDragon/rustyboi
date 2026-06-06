#!/usr/bin/env bash
# Canonical profile-guided-optimization (PGO) profile for the whole workspace.
#
# The profile is generated ONCE and consumed by every build script via
#   RUSTFLAGS="$(tools/pgo.sh flags) ${RUSTFLAGS:-}" cargo build ...
# It is NOT committed — it is a large, rustc/LLVM-version-specific binary blob
# that lives under target/ (already gitignored) and is trivially regenerated.
#
#   tools/pgo.sh gen --sweep DIR... [--sample N] [--frames N]  # gameplay profile
#   tools/pgo.sh gen --container --sweep DIR...        # profile for the cross
#                                                     #   image's rustc (so
#                                                     #   rc_build cross-compiles
#                                                     #   get PGO too)
#   tools/pgo.sh gen --suite                          # free-test-ROM fallback
#   tools/pgo.sh flags                                # RUSTFLAGS fragment (or empty)
#   tools/pgo.sh path                                 # print the profile path
#   tools/pgo.sh clean                                # remove it
#
# REPRESENTATIVENESS (this is the important part): `gen` drives each ROM with
# the gameplay MASHER (deterministic input, past the title screen into actual
# play) via `bench --drive` — PURE emulation, no per-frame framebuffer hashing.
# That distinction matters: profiling through the `sweep` binary (which hashes
# the whole framebuffer every frame) skews the profile toward hashing and makes
# the PGO build ~30% SLOWER; the pure-emulation drive gives +40-60%. It profiles
# a STRATIFIED SAMPLE (~50 ROMs, every Nth of the library you point at) as
# SEPARATE PROCESSES (no atomic-counter false-sharing, so it parallelizes and
# covers the sample in ~1 min). A ~20-60 game stratified set lands within ~1% of
# a whole-library profile (measured) — profiling all 1000+ is unnecessary, not
# harmful. Point --sweep at a diverse library (or set RB_PGO_ROMS); tune the
# sample size with --sample N. With no ROMs, `--suite` falls back to the free
# test-ROMs (a menu/torture profile, NOT gameplay-representative).
#
# SAFETY: `flags` verifies the profile is compatible with the ACTIVE rustc (a
# tiny preflight compile) before emitting -Cprofile-use, and emits nothing if
# the profile is missing or was built by a different toolchain. So wiring it
# into a build never breaks that build — it just silently forgoes PGO.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

PGO_DIR="$ROOT/target/pgo"
# Profiles are keyed by rustc version: a .profdata is only usable by the exact
# toolchain that produced it (the LLVM profile format is version-specific), so
# a host profile (e.g. 1.97) and a cross-container profile (e.g. 1.94) coexist
# without clobbering each other, and `flags` for a given toolchain only ever
# finds a matching profile. IR PGO is target-PORTABLE within one rustc version,
# so one host profile covers all same-version cross targets (arch-independent).
RUSTC_TAG="$(rustc --version 2>/dev/null | tr -c 'A-Za-z0-9._' '-' )"
PROFILE="$PGO_DIR/profile-$RUSTC_TAG.profdata"
RAW_DIR="$PGO_DIR/raw"
INSTR_TARGET="$PGO_DIR/instr"

find_profdata() {
    find "$(rustc --print sysroot)" -name 'llvm-profdata*' 2>/dev/null | head -1
}

cmd_path() { echo "$PROFILE"; }

cmd_clean() { rm -rf "$PGO_DIR"; echo "removed $PGO_DIR"; }

cmd_gen() {
    local frames=1200 sample="${RB_PGO_SAMPLE:-50}" sweep_dirs=() use_suite="" in_container=""
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --sweep) shift; while [ "$#" -gt 0 ] && [ "${1#--}" = "$1" ]; do sweep_dirs+=("$1"); shift; done ;;
            --frames) frames="$2"; shift 2 ;;
            --sample) sample="$2"; shift 2 ;;
            --suite) use_suite=1; shift ;;
            --container) in_container=1; shift ;;
            *) echo "gen: unknown arg $1" >&2; exit 2 ;;
        esac
    done
    if [ "${#sweep_dirs[@]}" -eq 0 ] && [ -n "${RB_PGO_ROMS:-}" ]; then
        # shellcheck disable=SC2206
        sweep_dirs=(${RB_PGO_ROMS})
    fi
    if [ "${#sweep_dirs[@]}" -eq 0 ] && [ -z "$use_suite" ]; then
        echo "gen: no ROMs. Point --sweep at a diverse ROM library (or set" >&2
        echo "     RB_PGO_ROMS=\"dirA dirB\"); or --suite for the free test-ROM" >&2
        echo "     fallback (a menu/torture profile, not gameplay-representative)." >&2
        exit 2
    fi

    # --container: re-run gen INSIDE the rust-cross image, so the profile is
    # produced by (and keyed to) the container's rustc — the one rc_build's
    # cross-compiles use. Mounts each ROM dir read-only at /roms/N; the profile
    # lands in the mounted target/pgo and persists on the host.
    if [ -n "$in_container" ]; then
        [ "${#sweep_dirs[@]}" -gt 0 ] || { echo "gen --container needs --sweep ROMs" >&2; exit 2; }
        # shellcheck source=tools/rust-cross.sh
        source "$ROOT/tools/rust-cross.sh"; rc_engine
        local mounts=() incroms=() i=0 d
        for d in "${sweep_dirs[@]}"; do
            mounts+=(-v "$(cd "$d" && pwd)":/roms/$i:ro); incroms+=("/roms/$i"); i=$(( i + 1 ))
        done
        echo "==> [container $IMAGE] generating PGO profile"
        local incmd="set -e
            find \"\$(rustc --print sysroot)\" -name 'llvm-profdata*' | grep -q . || rustup component add llvm-tools
            tools/pgo.sh gen --sweep ${incroms[*]} --sample $sample --frames $frames
            chown -R $HOST_UIDGID target/pgo"
        "$ENGINE" run --rm -v "$PROJECT_ROOT":/project -w /project \
            -v "$CARGO_VOL":/usr/local/cargo/registry "${mounts[@]}" \
            "$IMAGE" sh -c "$incmd"
        return
    fi

    local profdata; profdata="$(find_profdata)"
    [ -n "$profdata" ] || { echo "llvm-profdata not found; rustup component add llvm-tools" >&2; exit 1; }

    rm -rf "$RAW_DIR" "$PROFILE"; mkdir -p "$RAW_DIR"
    echo "==> instrumented build (test-runner)"
    RUSTFLAGS="-Cprofile-generate=$RAW_DIR" \
        cargo build --release -p rustyboi-test-runner --target-dir "$INSTR_TARGET"
    local bench="$INSTR_TARGET/release/bench"

    if [ "${#sweep_dirs[@]}" -gt 0 ]; then
        # Enumerate the library, then STRATIFY-SAMPLE to ~$sample ROMs (every
        # Nth, alphabetical) — a diverse gameplay set within ~1% of the whole
        # library, so profiling all 1000+ is unnecessary (see the sweep
        # campaign). Drive each through `bench --drive` (masher gameplay input,
        # PURE emulation — no framebuffer hashing to skew the profile), as
        # SEPARATE PROCESSES: each gets its own counters, so there is no atomic
        # false-sharing and it parallelizes fully.
        local all=() rom
        while IFS= read -r rom; do all+=("$rom"); done < <(
            find "${sweep_dirs[@]}" -type f \( -iname '*.zip' -o -iname '*.gb' -o -iname '*.gbc' \) | sort
        )
        local n="${#all[@]}" stride=1 picked=()
        [ "$n" -gt "$sample" ] && stride=$(( n / sample ))
        local i=0
        while [ "$i" -lt "$n" ]; do picked+=("${all[$i]}"); i=$(( i + stride )); done
        echo "==> profiling workload: $n ROMs -> ${#picked[@]} sampled, bench --drive (gameplay, parallel)"
        printf '%s\0' "${picked[@]}" | xargs -0 -P "$(nproc 2>/dev/null || echo 4)" -I {} \
            sh -c 'LLVM_PROFILE_FILE="'"$RAW_DIR"'/p-%p-%m.profraw" "'"$bench"'" "$1" '"$frames"' --drive >/dev/null 2>&1' _ {}
    else
        echo "==> profiling workload: test suites (--suite; not gameplay-representative)"
        local s
        for s in acid2 cgb_acid_hell scribbltests mealybug blargg; do
            RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 RB_JOBS=1 \
                RB_BIN="$INSTR_TARGET/release/rustyboi-test-runner" \
                "$ROOT/tools/run-suites.sh" "$s" >/dev/null 2>&1 || true
        done
    fi

    if ! ls "$RAW_DIR"/*.profraw >/dev/null 2>&1; then
        echo "no .profraw produced (workload ran nothing); aborting" >&2
        exit 1
    fi
    echo "==> merging profile ($(ls "$RAW_DIR"/*.profraw | wc -l) raw)"
    "$profdata" merge -o "$PROFILE" "$RAW_DIR"/*.profraw
    echo "profile: $PROFILE ($(du -h "$PROFILE" | cut -f1))"
}

# Emit `-Cprofile-use=<profile>` iff the profile exists AND the active rustc
# accepts it. rustc only WARNS on a bad/incompatible profile (exit 0, then
# ignores it), so an incompatible profile never breaks a build — but we detect
# it here to avoid the noise and the false impression that PGO is applied. A
# probe compile against the profile is clean for a good profile; a bad one
# prints "invalid instrumentation profile / bad magic / unsupported version"
# (file-level errors), which is distinct from the per-function "no data"
# warnings a valid-but-partial profile would emit.
cmd_flags() {
    [ -z "${RB_NO_PGO:-}" ] || return 0   # explicit opt-out
    [ -f "$PROFILE" ] || { echo "pgo: no profile ($PROFILE); build without PGO (tools/pgo.sh gen)" >&2; return 0; }
    local tmp err; tmp="$(mktemp -d)"
    err="$(printf 'pub fn f(){}\n' | rustc --crate-type lib -Cprofile-use="$PROFILE" \
        --emit=obj -o "$tmp/probe.o" - 2>&1 || true)"
    rm -rf "$tmp"
    if echo "$err" | grep -qiE 'invalid instrumentation profile|bad magic|truncated profile|unsupported.*version|malformed'; then
        echo "pgo: profile incompatible with active rustc (regenerate: tools/pgo.sh gen); building without PGO" >&2
    else
        echo "-Cprofile-use=$PROFILE"
    fi
}

case "${1:-}" in
    gen)    shift; cmd_gen "$@" ;;
    flags)  cmd_flags ;;
    path)   cmd_path ;;
    clean)  cmd_clean ;;
    *) echo "usage: tools/pgo.sh {gen [--sweep DIR...] [--frames N] | flags | path | clean}" >&2; exit 2 ;;
esac
