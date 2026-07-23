#!/usr/bin/env bash
# Run rustyboi's Game Boy test suites as a regression gate.
#
# This is the single entrypoint shared by CI (.github/workflows/test.yaml) and
# developers: whatever CI runs, you can reproduce locally with the same command.
#
# Usage:
#   tools/run-suites.sh setup            # fetch/extract the ROM set (idempotent)
#   tools/run-suites.sh build            # cargo build --release the test runner
#   tools/run-suites.sh list             # print the known suites + thresholds
#   tools/run-suites.sh report           # markdown progress table (all suites)
#   tools/run-suites.sh <suite>          # run one suite, gate against its floor
#   tools/run-suites.sh all              # run every suite, gate each
#   tools/run-suites.sh <suite> [<suite>...]   # run several
#
# `setup` and `build` run automatically before a suite if their outputs are
# missing, so `tools/run-suites.sh mealybug` works from a clean checkout.
# Skip them explicitly with RB_SKIP_SETUP=1 / RB_SKIP_BUILD=1 (CI splits the
# steps for caching and passes these).
#
# Regression gate: each suite has a floor (minimum passing cases) baked into the
# threshold() table below, measured on a known-good build. A suite passes CI when
# `passed >= floor` -- future fixes only ever raise the count, so the gate flags
# regressions without needing a manifest edit for every improvement. The
# `gambatte` suite instead asserts `failed <= 16` (its known real-silicon floor;
# see the manifest header and rustyboi-test-runner/suites/gambatte.manifest).
#
# Env knobs:
#   RB_MODE     hardware modes            (default: per-suite, see suite_mode)
#   RB_JOBS     parallel case workers     (default: nproc-derived)
#   RB_ROMS     ROM dir                   (default: gb-test-roms)
#   RB_BIN      runner binary             (default: target/release/rustyboi-test-runner)
#   RB_RUN_PREFIX  launcher prepended to every runner invocation (default: none =
#               run RB_BIN directly). Set it to run a non-native build under a VM;
#               the wasm CI target uses "wasmtime run --dir=. --dir=/tmp --" to run
#               a wasm32-wasip1 RB_BIN under wasmtime (pair with RB_JOBS=1 -- plain
#               wasip1 has no threads, and the runner only stays sequential there).
#   RB_PGO      1 = build the runner profile-guided (instrumented build, short
#               suite workload, -Cprofile-use rebuild). ~30-40% faster suite
#               runs (the branch-dense per-dot dispatch is PGO's sweet spot),
#               byte-identical results, at the cost of building twice. Needs
#               `rustup component add llvm-tools` + the ROM set already fetched;
#               falls back to a plain build (with a warning) if either is
#               missing.
#   CSP_VERSION c-sp gameboy-test-roms release tag (default: v7.0)
#   GAMBATTE_CORE_REF gambatte-core commit for the .bin/.dump oracles

set -euo pipefail

# --- repo root (this script lives in <root>/tools) ---------------------------
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# --- pinned sources ----------------------------------------------------------
CSP_VERSION="${CSP_VERSION:-v7.0}"
CSP_URL="https://github.com/c-sp/game-boy-test-roms/releases/download/${CSP_VERSION}/game-boy-test-roms-${CSP_VERSION}.zip"
GAMBATTE_CORE_REF="${GAMBATTE_CORE_REF:-5a41a68c25402421fb1983ddadc9faf2418ddb0f}"
GAMBATTE_CORE_URL="https://github.com/pokemon-speedrunning/gambatte-core.git"
# The sgb suite's ROM (cpp/sgb-ext-test.gb) is not in the c-sp set; it lives in
# the GBEmulatorShootout repo.
SHOOTOUT_REF="${SHOOTOUT_REF:-7c6ee9ba380fab277f30784c5de7d35b21a4b679}"
SHOOTOUT_URL="https://github.com/gbdev/GBEmulatorShootout.git"
# MagenTests: prebuilt .gbc release assets (alloncm, tag 0.5.0, 2025-03-22).
MAGENTESTS_VERSION="${MAGENTESTS_VERSION:-0.5.0}"
MAGENTESTS_URL="https://github.com/alloncm/MagenTests/releases/download/${MAGENTESTS_VERSION}"
# little-things-gb release ROMs (nitro2k01) + the repo-hosted reference images
# (real-SGB windesync capture, BGB double-halt captures) pinned to a commit.
LTG_URL="https://github.com/nitro2k01/little-things-gb/releases/download"
LTG_RAW_REF="${LTG_RAW_REF:-ce015ca2949b4db2babd03fe387b8b1999a8f60a}"
LTG_RAW_URL="https://raw.githubusercontent.com/nitro2k01/little-things-gb/${LTG_RAW_REF}"
# sketchtests: prebuilt release zip (Ashiepaws, v0.2-alpha, 2020-08-29).
SKETCHTESTS_VERSION="${SKETCHTESTS_VERSION:-v0.2-alpha}"
SKETCHTESTS_URL="https://github.com/Ashiepaws/sketchtests/releases/download/${SKETCHTESTS_VERSION}/sketchtests-${SKETCHTESTS_VERSION}.zip"
# AntonioND/gbc-hw-tests: real-silicon SRAM-capture suite (prebuilt .gbc ROMs
# + real_*.sav oracles captured on GB/GBP/GBC/GBA-SP, committed in-repo).
GBCHW_REF="${GBCHW_REF:-631e60000c885154a8526df0b148847f9c34ce42}"
GBCHW_URL="https://github.com/AntonioND/gbc-hw-tests.git"

# --- config ------------------------------------------------------------------
ROMS="${RB_ROMS:-gb-test-roms}"
# Empty = per-suite auto (suite_mode). An explicit RB_MODE is honoured verbatim.
MODE="${RB_MODE:-}"
SUITES_DIR="rustyboi-test-runner/suites"
# RB_TARGET cross-compiles to a Rust target triple (built + run under its
# artifact dir); empty = the native host build. On a foreign arch the built
# binary runs under qemu-user binfmt (register it with
# `docker run --privileged --rm tonistiigi/binfmt --install all`). On Windows
# (Git Bash) the binary carries a .exe suffix.
TARGET="${RB_TARGET:-}"
case "$(uname -s 2>/dev/null || echo)" in
    MINGW*|MSYS*|CYGWIN*) EXE=".exe" ;;
    *)                    EXE="" ;;
esac
if [ -n "$TARGET" ]; then
    BIN="${RB_BIN:-target/$TARGET/release/rustyboi-test-runner$EXE}"
else
    BIN="${RB_BIN:-target/release/rustyboi-test-runner$EXE}"
fi
# RB_RUN_PREFIX prepends a launcher to every runner invocation so the suites can
# run under a non-native VM without touching the call sites. The wasm target
# uses it: RB_RUN_PREFIX="wasmtime run --dir=. --dir=/tmp --" runs the
# wasm32-wasip1 build under wasmtime -- the same wasm ISA the browser ships, so
# it validates rustyboi-core's wasm codegen. The `--dir` preopens map the repo
# (ROMs + manifests, resolved relative to CWD) and /tmp (the --json scratch
# files), matching wasmtime's capability sandbox. Empty = native, run BIN
# directly. Force --jobs 1 (RB_JOBS=1) with it: plain wasip1 has no threads and
# the runner only stays single-threaded (never spawns) at jobs<=1.
RUN_PREFIX="${RB_RUN_PREFIX:-}"
# Invoke the runner: launcher (if any) + BIN + args. Word-splitting the prefix is
# intentional -- it is a fixed command, never user ROM data.
run_bin() {
    if [ -n "$RUN_PREFIX" ]; then
        $RUN_PREFIX "$BIN" "$@"
    else
        "$BIN" "$@"
    fi
}
# A .wasm module isn't chmod +x, so gate on presence (-f) not executability (-x)
# when running through a launcher.
bin_ready() { if [ -n "$RUN_PREFIX" ]; then [ -f "$BIN" ]; else [ -x "$BIN" ]; fi; }
# Default jobs: leave a core free, floor of 1. `nproc` is absent on some macOS
# shells, so fall back to sysctl then 4.
if [ -z "${RB_JOBS:-}" ]; then
    if command -v nproc >/dev/null 2>&1; then
        RB_JOBS="$(nproc)"
    elif command -v sysctl >/dev/null 2>&1; then
        RB_JOBS="$(sysctl -n hw.ncpu 2>/dev/null || echo 4)"
    else
        RB_JOBS=4
    fi
    RB_JOBS=$(( RB_JOBS > 1 ? RB_JOBS - 1 : 1 ))
fi
JOBS="$RB_JOBS"

# --- threshold table ---------------------------------------------------------
# threshold <suite> -> "<min-passed> <frames-override-or-'-'>", or "" if unknown.
# Floors auto-ratchet up to the measured counts on every report-update (the
# pre-commit hook); hand-edits are only needed to LOWER a floor. `>=` gate.
# gambatte is special-cased in run_suite (assert failed<=16, not passed>=).
# A plain `case` (not an associative array) keeps this portable to bash 3.2,
# which is the /bin/bash that GitHub's macOS runners ship.
threshold() {
    case "$1" in
        rustyboi)           echo "55 20" ;;  # first-party RGBDS ROMs (test-roms/); png ROMs settle in <20 frames (mooneye ROMs run to their own done-marker budget, so the SGB ROMs' ~50-frame waits are unaffected)
        acid2)              echo "3 -" ;;
        cgb_acid_hell)      echo "1 -" ;;
        mealybug)           echo "51 -" ;;
        mooneye)            echo "193 -" ;;
        mooneye_wilbertpol) echo "194 -" ;;
        age)                echo "56 -" ;;
        gbmicrotest)        echo "509 -" ;;
        samesuite_apu)      echo "70 -" ;;
        samesuite_nonapu)   echo "6 -" ;;
        samesuite_sgb)      echo "2 -" ;;
        sgb)                echo "1 -" ;;
        blargg)             echo "15 4000" ;;
        blargg_singles)     echo "51 2000" ;;
        scribbltests)       echo "10 -" ;;
        turtle_tests)       echo "4 -" ;;
        little_things_gb)   echo "4 -" ;;
        bully)              echo "2 -" ;;
        strikethrough)      echo "2 -" ;;
        daid)               echo "8 -" ;;
        rtc3test)           echo "6 -" ;;
        mbc3_tester)        echo "2 -" ;;
        cpp)                echo "3 -" ;;
        magentests)         echo "11 -" ;;
        little_things_extra) echo "4 -" ;;
        sketchtests)        echo "6 120" ;;
        gbc_hw_tests)       echo "338 800" ;;  # real-silicon SRAM captures (CGB+DMG+AGB columns); the sram path runs a FLAT budget with no done-marker, so the handful of ROMs that need longer carry a per-test frames= token instead of inflating the whole suite
        gambatte)           echo "5248 -" ;;  # gated on failed<=9 (GAMBATTE_MAX_FAIL)
        *)                  echo "" ;;
    esac
}
GAMBATTE_MAX_FAIL=9

# Deterministic suite order for `all`. The hardware-graded gate + README Total.
ORDER="rustyboi acid2 cgb_acid_hell mealybug mooneye mooneye_wilbertpol age gbmicrotest \
samesuite_apu samesuite_nonapu samesuite_sgb sgb blargg blargg_singles \
scribbltests turtle_tests little_things_gb bully strikethrough daid rtc3test \
mbc3_tester cpp magentests little_things_extra sketchtests gbc_hw_tests gambatte"

# --- helpers -----------------------------------------------------------------
log()  { printf '%s\n' "==> $*"; }
warn() { printf '%s\n' "!!  $*" >&2; }
die()  { printf '%s\n' "!!  $*" >&2; exit 1; }

usage() { sed -n '2,37p' "$0"; }

# python3 on Linux/macOS; `python` on Windows (Git Bash ships no `python3`).
PY="$(command -v python3 || command -v python || true)"
[ -n "$PY" ] || die "python3 (or python) is required but was not found on PATH"

# json_field <file> <key>  -> integer value (no jq dependency; python is already
# a prerequisite here, for zip extraction).
json_field() {
    "$PY" -c "import json,sys;print(json.load(open(sys.argv[1]))[sys.argv[2]])" "$1" "$2"
}

# suite_mode <manifest>  -> the --mode set to run this suite with.
#
# An explicit RB_MODE wins verbatim. Otherwise: dmg,cgb (the historical
# default) plus `agb` IFF the manifest actually declares literal agb rows.
# Without this, agb-mode rows parse fine, look covered, and never execute --
# `rustyboi.manifest`'s 4 licensee_boot_div.agbcompat_* rows were dark.
#
# Scoped per suite rather than a global `dmg,cgb,agb` on purpose: in
# `cases_for_rom`, enabling agb ALSO clones every CGB case into an AGB twin
# graded against the same CGB references. That path fires for `gambatte`-graded
# rows (mode `auto`), so a global agb would silently add thousands of twin
# cases to the gambatte suite and move its fail ceiling. Keying off the literal
# mode column keeps agb confined to the suites that actually ask for it.
#
# (A pure "union of modes present" default cannot work: gambatte's 3524 rows
# all carry mode `auto`, so their real modes never appear in the column.)
suite_mode() {
    if [ -n "$MODE" ]; then printf '%s' "$MODE"; return; fi
    if awk -F'|' '/^[^#]/ && $2=="agb" {found=1; exit} END {exit !found}' "$1"; then
        printf 'dmg,cgb,agb'
    else
        printf 'dmg,cgb'
    fi
}

# manifest_path <suite>  -> the manifest file backing a suite. All suites live
# in the tracked $SUITES_DIR.
manifest_path() {
    printf '%s' "$SUITES_DIR/$1.manifest"
}

# --- setup: fetch ROMs (idempotent) ------------------------------------------
setup() {
    if [ -f "$ROMS/.rb-setup-complete" ]; then
        log "c-sp ROM set present ($ROMS); verifying non-c-sp sources (rm $ROMS/.rb-setup-complete to re-provision)"
    else
        log "Fetching c-sp gameboy-test-roms $CSP_VERSION"
        mkdir -p "$ROMS"
        local zip="$ROMS/game-boy-test-roms-${CSP_VERSION}.zip"
        curl -fsSL -o "$zip" "$CSP_URL"
        # The archive is flat (suite dirs at its root) -> extract straight into $ROMS.
        # Extract with python3 (already required for JSON parsing) so this works
        # identically on Linux, macOS and Windows (Git Bash), where `unzip` may be
        # absent and GNU `tar` cannot read zips.
        "$PY" -c "import sys,zipfile;zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])" "$zip" "$ROMS"
        rm -f "$zip"
        log "Syncing gambatte-core oracles (.bin/.dump) @ ${GAMBATTE_CORE_REF:0:12}"
        sync_gambatte_oracles
        touch "$ROMS/.rb-setup-complete"
    fi
    # sgb/daid/cpp ROMs are not in the c-sp set. sync_shootout_roms is file-
    # guarded (no-ops when present) and cheap, so run it unconditionally -- this
    # is how a newly added shootout-sourced suite lands on an already-provisioned
    # ROM tree or a restored CI cache WITHOUT re-downloading the whole c-sp set.
    # The same self-guarding applies to every non-c-sp fetch below: the
    # .rb-setup-complete sentinel gates ONLY the original c-sp+gambatte fetch.
    log "Sourcing cpp + daid ROMs (not in the c-sp set) @ ${SHOOTOUT_REF:0:12}"
    sync_shootout_roms
    log "Sourcing MagenTests ${MAGENTESTS_VERSION} release ROMs"
    sync_magentests_roms
    log "Sourcing little-things-gb release ROMs + reference captures @ ${LTG_RAW_REF:0:12}"
    sync_little_things_extra
    log "Sourcing sketchtests ${SKETCHTESTS_VERSION} release ROMs"
    sync_sketchtests_roms
    log "Sourcing gbc-hw-tests ROMs + real-device .sav oracles @ ${GBCHW_REF:0:12}"
    sync_gbchwtests_roms
    log "Building first-party test ROMs (test-roms/, RGBDS)"
    build_test_roms
    log "ROM setup complete"
}

# Assemble the first-party test ROMs (test-roms/) with RGBDS into their
# gitignored build/ dir. The committed rustyboi.manifest references these built
# ROMs, so they must exist before the suite runs. rgbds is a documented dev/CI
# dependency; without it the rustyboi suite has no ROMs and misses its floor.
build_test_roms() {
    if command -v rgbasm >/dev/null 2>&1; then
        make -C "$ROOT/test-roms" roms || warn "test-roms RGBDS build failed"
    else
        warn "rgbds (rgbasm) not found; skipping first-party test-roms build"
    fi
}

# MagenTests (alloncm): eight prebuilt .gbc assets on the pinned release tag.
# oam_internal_priority is fetched but NOT manifested (its only oracle is a
# SameBoy screenshot; see suites/magentests.manifest header).
sync_magentests_roms() {
    local mt="$ROMS/magentests" f
    [ -f "$mt/bg_oam_priority.gbc" ] && [ -f "$mt/ppu_disabled_state.gbc" ] && return 0
    mkdir -p "$mt"
    for f in bg_oam_priority hblank_vram_dma key0_lock_after_boot \
             mbc_oob_sram_mbc1 mbc_oob_sram_mbc3 mbc_oob_sram_mbc5 \
             oam_internal_priority ppu_disabled_state; do
        curl -fsSL -o "$mt/$f.gbc" "$MAGENTESTS_URL/$f.gbc"
    done
    log "Sourced MagenTests ROMs into $mt"
}

# little-things-gb (nitro2k01) release ROMs the c-sp set does not carry
# (windesync-validate, double-halt-cancel) plus their reference images from
# the repo at a pinned commit: the real-SGB logic-analyzer windesync capture
# and the 2x-scale BGB double-halt captures (downscaled into suites/refs/ by
# gen_manifests.py). Lands in the same little-things-gb/ dir as the c-sp
# firstwhite/tellinglys files -- purely additive.
sync_little_things_extra() {
    local ltg="$ROMS/little-things-gb"
    [ -f "$ltg/windesync-validate.gb" ] && [ -f "$ltg/double-halt-cancel.gb" ] && return 0
    mkdir -p "$ltg"
    curl -fsSL -o "$ltg/windesync-validate.gb" \
        "$LTG_URL/Win-desync-v1.0/windesync-validate.gb"
    curl -fsSL -o "$ltg/double-halt-cancel.gb" \
        "$LTG_URL/Double-halt-cancel-v1.0/double-halt-cancel.gb"
    curl -fsSL -o "$ltg/double-halt-cancel-gbconly.gb" \
        "$LTG_URL/Double-halt-cancel-v1.0/double-halt-cancel-gbconly.gb"
    curl -fsSL -o "$ltg/windesync-reference-sgb.png" \
        "$LTG_RAW_URL/windesync-validate/images/windesync-reference-sgb.png"
    curl -fsSL -o "$ltg/double-halt-cancel-bgb-dmg-2x.png" \
        "$LTG_RAW_URL/double-halt-cancel/images/bgb-dmg.png"
    curl -fsSL -o "$ltg/double-halt-cancel-bgb-gbc-2x.png" \
        "$LTG_RAW_URL/double-halt-cancel/images/bgb-gbc.png"
    log "Sourced little-things-gb release ROMs into $ltg"
}

# sketchtests (Ashiepaws): prebuilt release zip; flatten the three ROMs into
# $ROMS/sketchtests/ (zip layout: cpu/instr/daa.gb, interrupts/..., other/...).
sync_sketchtests_roms() {
    local sk="$ROMS/sketchtests"
    [ -f "$sk/daa.gb" ] && [ -f "$sk/model_detector.gb" ] && return 0
    mkdir -p "$sk"
    local zip="$sk/sketchtests.zip"
    curl -fsSL -o "$zip" "$SKETCHTESTS_URL"
    "$PY" - "$zip" "$sk" <<'PY'
import sys, zipfile
zip_path, dest = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(zip_path) as z:
    for name in z.namelist():
        if name.endswith(".gb"):
            base = name.rsplit("/", 1)[-1]
            with open(f"{dest}/{base}", "wb") as f:
                f.write(z.read(name))
PY
    rm -f "$zip"
    log "Sourced sketchtests ROMs into $sk"
}

# AntonioND/gbc-hw-tests: the repo commits both the prebuilt .gbc test ROMs and
# the real-hardware SRAM captures (real_gb / real_gbp / real_gbc / real_gba_sp
# .sav, one per device class) they are graded against. Shallow single-commit
# checkout at the pinned ref; copy only the ROMs + .sav oracles + the rgblink
# .sym symbol tables (~17 MB), preserving the repo's category/test dir layout
# under $ROMS/gbc-hw-tests/. The 180 .sym files are what turn RB_SRAM_TRACE's
# writing-PC attribution into a symbol name; the runner picks one up whenever it
# sits beside the ROM, and silently degrades to bare PCs when it does not.
sync_gbchwtests_roms() {
    local hw="$ROMS/gbc-hw-tests"
    [ -f "$hw/cpu/halt_bug_test/halt_bug_test.gbc" ] \
        && [ -f "$hw/timers/div_reset_65k/real_gbc.sav" ] \
        && [ -f "$hw/cpu/halt_bug_test/halt_bug_test.sym" ] && return 0
    local tmp
    tmp="$(mktemp -d)"
    git -C "$tmp" init -q
    git -C "$tmp" remote add origin "$GBCHW_URL"
    git -C "$tmp" fetch -q --depth 1 origin "$GBCHW_REF"
    git -C "$tmp" checkout -q FETCH_HEAD
    local n=0 rel
    while IFS= read -r -d '' f; do
        rel="${f#"$tmp"/}"
        mkdir -p "$hw/$(dirname "$rel")"
        cp -p "$f" "$hw/$rel"
        n=$((n + 1))
    done < <(find "$tmp" -path "$tmp/.git" -prune -o \
        \( -name '*.gbc' -o -name 'real_*.sav' -o -name '*.sym' \) -print0)
    rm -rf "$tmp"
    log "Sourced $n gbc-hw-tests files into $hw"
}

# The cpp/ and daid/ suite ROMs (sgb-ext-test, the MBC3/RTC edge tests, and the
# daid PPU/STOP/speed-switch screen tests) are absent from the c-sp set; they
# live in GBEmulatorShootout/testroms/{cpp,daid}. Pull just those dirs with the
# same shallow / blobless / sparse checkout used for the gambatte oracles.
sync_shootout_roms() {
    [ -f "$ROMS/cpp/sgb-ext-test.gb" ] && [ -f "$ROMS/daid/ppu_scanline_bgp.gb" ] && return 0
    local tmp
    tmp="$(mktemp -d)"
    git -C "$tmp" init -q
    git -C "$tmp" remote add origin "$SHOOTOUT_URL"
    git -C "$tmp" config core.sparseCheckout true
    git -C "$tmp" sparse-checkout init --cone >/dev/null 2>&1 || true
    git -C "$tmp" sparse-checkout set testroms/cpp testroms/daid >/dev/null 2>&1 || true
    git -C "$tmp" fetch -q --depth 1 --filter=blob:none origin "$SHOOTOUT_REF"
    git -C "$tmp" checkout -q FETCH_HEAD
    mkdir -p "$ROMS/cpp" "$ROMS/daid"
    cp -p "$tmp/testroms/cpp/"*.gb "$tmp/testroms/cpp/"*.png "$ROMS/cpp/" 2>/dev/null \
        || warn "cpp ROMs missing in shootout checkout"
    cp -p "$tmp/testroms/daid/"*.gb "$tmp/testroms/daid/"*.gbc "$tmp/testroms/daid/"*.png "$ROMS/daid/" 2>/dev/null \
        || warn "daid ROMs missing in shootout checkout"
    rm -rf "$tmp"
    log "Sourced cpp + daid ROMs into $ROMS"
}

# The c-sp set ships gambatte ROMs but none of the 32 dumper oracle files
# (*_dmg08.bin / *_cgb.bin / *.dump). Those live in gambatte-core/test/hwtests
# as siblings of the ROMs; copy them in, preserving the subdir layout. We do NOT
# regenerate manifests -- the committed manifests are the tested source of truth.
sync_gambatte_oracles() {
    local gam="$ROMS/gambatte"
    [ -d "$gam" ] || { warn "no $gam dir; skipping oracle sync"; return 0; }
    local tmp
    tmp="$(mktemp -d)"
    # Shallow, single-commit, blobless, sparse checkout of just test/hwtests.
    git -C "$tmp" init -q
    git -C "$tmp" remote add origin "$GAMBATTE_CORE_URL"
    git -C "$tmp" config core.sparseCheckout true
    git -C "$tmp" sparse-checkout init --cone >/dev/null 2>&1 || true
    git -C "$tmp" sparse-checkout set test/hwtests >/dev/null 2>&1 || true
    git -C "$tmp" fetch -q --depth 1 --filter=blob:none origin "$GAMBATTE_CORE_REF"
    git -C "$tmp" checkout -q FETCH_HEAD
    local hw="$tmp/test/hwtests" n=0 rel
    while IFS= read -r -d '' f; do
        rel="${f#"$hw"/}"
        mkdir -p "$gam/$(dirname "$rel")"
        cp -p "$f" "$gam/$rel"
        n=$((n + 1))
    done < <(find "$hw" \( -name '*.bin' -o -name '*.dump' \) -print0)
    rm -rf "$tmp"
    log "Synced $n gambatte oracle files into $gam"
}

# --- build -------------------------------------------------------------------
build() {
    # Scope to -p rustyboi-test-runner: the workspace default-members include
    # rustyboi-platform (rodio -> cpal -> alsa-sys), which a bare `cargo build`
    # would pull in and fail to cross-compile. The runner itself is std-only.
    if [ -n "${RB_PGO:-}" ] && build_pgo; then
        return
    fi
    log "Building release test runner${TARGET:+ for $TARGET}"
    if [ -n "$TARGET" ]; then
        cargo build --release -p rustyboi-test-runner --target "$TARGET"
    else
        cargo build --release -p rustyboi-test-runner
    fi
}

# Profile-guided runner build (RB_PGO=1): instrumented build -> short mixed
# DMG/CGB suite workload -> -Cprofile-use rebuild into the normal $BIN path.
# Returns nonzero (falling back to the plain build) when llvm-profdata or the
# ROM set is missing. Byte-identity of PGO runners was verified against the
# full suite board (identical pass counts + FAILED-line sets).
build_pgo() {
    local profdata
    profdata="$(find "$(rustc --print sysroot)" -name "llvm-profdata$EXE" 2>/dev/null | head -1)"
    if [ -z "$profdata" ]; then
        log "RB_PGO: llvm-profdata not found (rustup component add llvm-tools); plain build"
        return 1
    fi
    if [ ! -f "$ROMS/.rb-setup-complete" ]; then
        log "RB_PGO: ROM set not fetched yet; plain build"
        return 1
    fi
    local pgo_dir="$ROOT/target/pgo-suite-profiles"
    local target_args=()
    [ -n "$TARGET" ] && target_args=(--target "$TARGET")
    rm -rf "$pgo_dir"
    mkdir -p "$pgo_dir"
    log "RB_PGO: instrumented build"
    RUSTFLAGS="-Cprofile-generate=$pgo_dir" \
        cargo build --release -p rustyboi-test-runner "${target_args[@]}"
    # RB_JOBS=1 during profiling: the runner parallelizes cases with scoped
    # threads, and -Cprofile-generate counters are atomic — multi-threaded the
    # instrumented binary false-shares counter cache lines across cores (~100x
    # slowdown). Single-threaded the instrumented runner is near native, and
    # this workload is only four small suites.
    log "RB_PGO: profiling workload (acid2 cgb_acid_hell scribbltests mealybug)"
    local s
    for s in acid2 cgb_acid_hell scribbltests mealybug; do
        RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 RB_JOBS=1 "$0" "$s" >/dev/null 2>&1 || true
    done
    if ! ls "$pgo_dir"/*.profraw >/dev/null 2>&1; then
        log "RB_PGO: workload produced no profiles; plain build"
        return 1
    fi
    "$profdata" merge -o "$pgo_dir/merged.profdata" "$pgo_dir"/*.profraw
    log "RB_PGO: optimized build"
    RUSTFLAGS="-Cprofile-use=$pgo_dir/merged.profdata" \
        cargo build --release -p rustyboi-test-runner "${target_args[@]}"
}

# --- ROM-presence preflight ---------------------------------------------------
# A manifest whose ROM FILES are absent still parses into cases, and the runner
# then grades every one of them as a failure: the JSON comes back passed=0
# total=N, which is byte-identical to a total regression. Nothing in the JSON
# distinguishes the two -- in particular `skipped_roms` does NOT, because only
# the name-discovery path ever sets it; run_manifest (which every suite here
# uses) leaves it 0 whether the ROMs exist or not. So "can this suite be
# measured at all" has to be answered here, before any count is believed.
#
# Field 4 of every manifest row is the ROM path, relative to $ROOT. Echoes the
# manifest-referenced ROMs that are not on disk.
manifest_missing_roms() {
    awk -F'|' '!/^#/ && NF >= 4 && $4 != "" { print $4 }' "$1" | sort -u |
    while IFS= read -r rom; do
        if [ ! -f "$ROOT/$rom" ]; then printf '%s\n' "$rom"; fi
    done
}

# Echo the suites (of those named in $@, default $ORDER) that cannot be measured
# right now: manifest absent, or any of its ROMs absent. Diagnostics go to
# stderr so callers can capture the suite list on stdout. ~0.5s for all 28.
unmeasurable_suites() {
    local suites="${*:-$ORDER}" suite manifest missing count
    for suite in $suites; do
        manifest="$(manifest_path "$suite")"
        if [ ! -f "$manifest" ]; then
            warn "suite '$suite' cannot be measured: manifest not found ($manifest)"
            printf '%s\n' "$suite"
            continue
        fi
        missing="$(manifest_missing_roms "$manifest")"
        [ -n "$missing" ] || continue
        count="$(printf '%s\n' "$missing" | wc -l | tr -d ' ')"
        warn "suite '$suite' cannot be measured: $count ROM file(s) missing, e.g. $(printf '%s\n' "$missing" | head -n 3 | tr '\n' ' ')"
        printf '%s\n' "$suite"
    done
}

# --- run one suite + gate ----------------------------------------------------
# echoes a one-line result; returns 0 pass / 1 regression / 2 config error.
run_suite() {
    local suite="$1"
    local row
    row="$(threshold "$suite")"
    [ -n "$row" ] || { warn "unknown suite: $suite (see 'list')"; return 2; }
    local manifest
    manifest="$(manifest_path "$suite")"
    [ -f "$manifest" ] || { warn "missing manifest: $manifest"; return 2; }
    # Absent ROMs are a config error (2), not a regression (1): grading them
    # would print a confident "FAIL ... REGRESSION" for a suite that never ran.
    [ -z "$(unmeasurable_suites "$suite")" ] || return 2

    local floor frames smode
    floor="$(printf '%s' "$row" | cut -d' ' -f1)"
    frames="$(printf '%s' "$row" | cut -d' ' -f2)"
    smode="$(suite_mode "$manifest")"

    local out
    out="$(mktemp)"

    # Only two suites (blargg, blargg_singles) override the global frame budget;
    # branch rather than use an array so this is safe under `set -u` on bash 3.2
    # (macOS /bin/bash), where expanding an empty "${arr[@]}" is an error.
    if [ "${RB_SHARDS:-1}" -gt 1 ]; then
        # Shard the suite across RB_SHARDS single-threaded PROCESSES instead of
        # in-process threads. Coverage-instrumented builds share one counter
        # array per process, so threads false-share it catastrophically;
        # separate processes get private counters (merged via %p profraws).
        log "Running suite '$suite' (mode=$smode shards=$RB_SHARDS jobs=1 ${frames#-} frames)"
        local k
        for k in $(seq 1 "$RB_SHARDS"); do
            if [ "$frames" != "-" ]; then
                run_bin --manifest "$manifest" --mode "$smode" --jobs 1 \
                    --shard "$k/$RB_SHARDS" --frames "$frames" \
                    --json "$out.$k" >/dev/null 2>&1 &
            else
                run_bin --manifest "$manifest" --mode "$smode" --jobs 1 \
                    --shard "$k/$RB_SHARDS" --json "$out.$k" >/dev/null 2>&1 &
            fi
        done
        wait
        "$PY" - "$out" "$RB_SHARDS" <<'PY'
import json, sys
out, n = sys.argv[1], int(sys.argv[2])
tot = {"passed": 0, "failed": 0, "total": 0}
for k in range(1, n + 1):
    d = json.load(open(f"{out}.{k}"))
    for key in tot:
        tot[key] += d[key]
json.dump(tot, open(out, "w"))
PY
        rm -f "$out".*
    elif [ "$frames" != "-" ]; then
        log "Running suite '$suite' (mode=$smode jobs=$JOBS ${frames#-} frames)"
        run_bin --manifest "$manifest" --mode "$smode" --jobs "$JOBS" \
            --frames "$frames" --json "$out" || true
    else
        log "Running suite '$suite' (mode=$smode jobs=$JOBS ${frames#-} frames)"
        run_bin --manifest "$manifest" --mode "$smode" --jobs "$JOBS" \
            --json "$out" || true   # runner exits 1 on any fail; we gate on counts
    fi

    local passed total failed
    passed="$(json_field "$out" passed)"
    total="$(json_field "$out" total)"
    failed="$(json_field "$out" failed)"
    rm -f "$out"

    if [ "$suite" = "gambatte" ]; then
        if [ "$failed" -le "$GAMBATTE_MAX_FAIL" ]; then
            printf 'PASS  %-20s passed=%s/%s failed=%s (floor: failed<=%s)\n' \
                "$suite" "$passed" "$total" "$failed" "$GAMBATTE_MAX_FAIL"
            return 0
        fi
        printf 'FAIL  %-20s passed=%s/%s failed=%s (floor: failed<=%s) REGRESSION\n' \
            "$suite" "$passed" "$total" "$failed" "$GAMBATTE_MAX_FAIL"
        return 1
    fi

    if [ "$passed" -ge "$floor" ]; then
        printf 'PASS  %-20s passed=%s/%s (floor: passed>=%s)\n' \
            "$suite" "$passed" "$total" "$floor"
        return 0
    fi
    printf 'FAIL  %-20s passed=%s/%s (floor: passed>=%s) REGRESSION\n' \
        "$suite" "$passed" "$total" "$floor"
    return 1
}

list() {
    printf '%-26s %-14s %s\n' "SUITE" "FLOOR" "FRAMES"
    local suite row floor frames
    for suite in $ORDER; do
        row="$(threshold "$suite")"
        floor="$(printf '%s' "$row" | cut -d' ' -f1)"
        frames="$(printf '%s' "$row" | cut -d' ' -f2)"
        [ "$frames" = "-" ] && frames="(default)"
        if [ "$suite" = "gambatte" ]; then floor="failed<=$GAMBATTE_MAX_FAIL"; fi
        printf '%-26s %-14s %s\n' "$suite" "$floor" "$frames"
    done
}

update_readme_report() {
    local readme="$ROOT/README.md" tmp out
    grep -q '<!-- SUITE-PROGRESS:START -->' "$readme" \
        && grep -q '<!-- SUITE-PROGRESS:END -->' "$readme" \
        || die "SUITE-PROGRESS markers not found in $readme"

    tmp="$(mktemp)"
    # No table -> no rewrite and no ratchet. Leaving the previous (measured)
    # counts in place keeps README stale-but-true; overwriting them with counts
    # we could not measure makes it confidently false, and the ratchet would
    # then bake those numbers into threshold() as well.
    if ! generate_report > "$tmp"; then
        rm -f "$tmp"
        warn "README suite table left UNCHANGED (nothing was overwritten)"
        return 1
    fi

    out="$(mktemp)"
    awk -v report="$tmp" '
        /<!-- SUITE-PROGRESS:START -->/ {
            print
            while ((getline line < report) > 0) print line
            close(report)
            skip = 1
            next
        }
        /<!-- SUITE-PROGRESS:END -->/ { skip = 0 }
        !skip
    ' "$readme" > "$out"

    mv "$out" "$readme"
    ratchet_thresholds "$tmp"
    rm -f "$tmp"
    # Stage the regenerated table so a commit that changes it is one-shot
    # (pre-commit aborts only on UNSTAGED hook modifications).
    git -C "$ROOT" add "$readme" 2>/dev/null || true
}

# Ratchet the per-suite pass floors in threshold() (and GAMBATTE_MAX_FAIL) up to
# the counts just measured into the report table, so every landed improvement
# becomes the new regression floor. Monotonic: pass floors only rise, the
# gambatte fail ceiling only falls -- a regression can never relax a floor here
# (run_suite still gates on it at test time). Stages its own edit like the table.
ratchet_thresholds() {
    local table="$1"
    "$PY" - "$table" "$ROOT/tools/run-suites.sh" <<'PY'
import os, re, sys
table, script = sys.argv[1], sys.argv[2]
counts = {}
for ln in open(table):
    m = re.match(r'\| (\w+) \| (\d+) \| (\d+) \|', ln)
    if m:
        counts[m.group(1)] = (int(m.group(2)), int(m.group(3)))

def bump(m):
    n = int(m.group('n'))
    if m.group('suite') in counts:
        n = max(n, counts[m.group('suite')][0])
    return f'{m.group("pre")}{n}{m.group("f")}'

s = open(script).read()
s = re.sub(r'(?P<pre>(?P<suite>\w+)\)\s*echo ")(?P<n>\d+)(?P<f> \S+" ;;)', bump, s)
if 'gambatte' in counts:
    p, t = counts['gambatte']
    s = re.sub(r'(GAMBATTE_MAX_FAIL=)(\d+)',
               lambda m: m.group(1) + str(min(int(m.group(2)), t - p)), s, count=1)
# atomic replace via a new inode: bash keeps reading its original open fd, so
# rewriting the script it is currently executing is safe (new content next run).
tmp = script + ".ratchet.tmp"
with open(tmp, "w") as f:
    f.write(s)
os.chmod(tmp, os.stat(script).st_mode)
os.replace(tmp, script)
PY
    git -C "$ROOT" add "$ROOT/tools/run-suites.sh" 2>/dev/null || true
}

# Emit a GitHub-flavored markdown progress table (one row per suite, this
# platform's pass counts). Consumed by the README auto-update in CI.
generate_report() {
    # Preflight FIRST, before a single row is emitted: a table is only worth
    # publishing if every row in it was actually measured. A suite whose ROMs
    # are missing would otherwise contribute a confident 0 (and drag the Total
    # down with it) -- that is exactly how a committed `| rustyboi | 0 | 25 |`
    # regression reached README. Emit nothing rather than something wrong.
    local blocked
    blocked="$(unmeasurable_suites)"
    if [ -n "$blocked" ]; then
        warn "refusing to generate the progress table: $(printf '%s\n' "$blocked" | tr '\n' ' ')cannot be measured"
        warn "provision the ROM set first ($0 setup), or build the first-party ROMs (make -C test-roms roms)"
        return 3
    fi
    printf '| Suite | Passing | Total |\n'
    printf '| :--- | ---: | ---: |\n'
    local suite row frames out passed total smode tp=0 tt=0
    for suite in $ORDER; do
        row="$(threshold "$suite")"
        frames="$(printf '%s' "$row" | cut -d' ' -f2)"
        # Same per-suite mode set the gate uses, so the table and the floors
        # are measured over identical case sets.
        smode="$(suite_mode "$SUITES_DIR/$suite.manifest")"
        out="$(mktemp)"
        if [ "$frames" != "-" ]; then
            run_bin --manifest "$SUITES_DIR/$suite.manifest" --mode "$smode" \
                --jobs "$JOBS" --frames "$frames" --json "$out" >/dev/null 2>&1 || true
        else
            run_bin --manifest "$SUITES_DIR/$suite.manifest" --mode "$smode" \
                --jobs "$JOBS" --json "$out" >/dev/null 2>&1 || true
        fi
        passed="$(json_field "$out" passed)"
        total="$(json_field "$out" total)"
        rm -f "$out"
        tp=$((tp + passed)); tt=$((tt + total))
        printf '| %s | %s | %s |\n' "$suite" "$passed" "$total"
    done
    printf '| **Total** | **%s** | **%s** |\n' "$tp" "$tt"
}

# --- main --------------------------------------------------------------------
[ $# -ge 1 ] || { usage; exit 1; }

case "$1" in
    -h|--help) usage; exit 0 ;;
    list)      list; exit 0 ;;
    setup)     setup; exit 0 ;;
    build)     build; exit 0 ;;
esac

# report-update regenerates the README table (and ratchets the threshold()
# floors up to the measured counts). It has two callers with OPPOSITE needs,
# told apart by an EXPLICIT flag rather than by guessing from ambient state:
#
#   * `make report-update`      -- MANUAL. The user asked for a fresh table, so
#                                  it ALWAYS regenerates. Deciding whether to run
#                                  from what happens to be staged is exactly the
#                                  silent no-op this has regressed into twice:
#                                  once the skip fired on a clean tree, then once
#                                  it fired whenever ANY non-core file was staged
#                                  (the normal mid-work state). No flag -> run.
#   * `make report-update-hook` -- the pre-commit hook (always_run), which passes
#                                  --pre-commit. Fast-skip when no core/manifest
#                                  change is staged, so a docs/tooling commit does
#                                  not pay ~60s of suites for counts that cannot
#                                  have moved.
#
# The flag is reliable because WE pass it from .pre-commit-config.yaml; it does
# not depend on pre-commit exporting PRE_COMMIT (which it did not do dependably).
# Never download ROMs during a commit -- skip if the binary/ROM set isn't ready
# (CI keeps the table honest regardless; the one build it will do is the
# staleness rebuild below). update_readme_report stages its own edits, so the
# table and ratchet land in the same commit rather than aborting it.
if [ "$1" = "report-update" ]; then
    if [ "${2:-}" = "--pre-commit" ] \
        && git diff --cached --quiet -- rustyboi-core rustyboi-test-runner/suites 2>/dev/null; then
        exit 0  # committing, but no source/manifest change staged -> counts can't have moved
    fi
    log "report-update: regenerating the README suite table (grading suites)"
    # Staleness guard: a runner binary older than the staged core/runner
    # sources grades the PREVIOUS tree and writes confidently wrong counts into
    # README (it has happened repeatedly: false 205 and 215 gbc_hw rows). This
    # is the one case where building during a commit is worth the wall clock --
    # we only get here with core/runner sources staged, i.e. counts really can
    # move. A failed build aborts the commit, which is also what we want.
    if [ -x "$BIN" ]; then
        newest_src=$(git -C "$ROOT" diff --cached --name-only -- \
                rustyboi-core rustyboi-test-runner 2>/dev/null \
            | while IFS= read -r f; do
                [ -f "$ROOT/$f" ] && stat -c %Y "$ROOT/$f"
            done | sort -rn | head -n 1)
        bin_time=$(stat -c %Y "$BIN" 2>/dev/null || echo 0)
        if [ -n "$newest_src" ] && [ "$bin_time" -lt "$newest_src" ]; then
            log "runner binary is older than the staged sources; rebuilding"
            build || die "rebuild failed; not grading with the stale $BIN"
        fi
    fi
    if [ -x "$BIN" ] && [ -f "$ROMS/.rb-setup-complete" ]; then
        # update_readme_report refuses (nonzero) when some suite's ROMs are
        # absent. Locally that is an ordinary state -- the first-party ROMs are
        # gitignored build output, and pre-commit runs with them stashed away --
        # so skip loudly rather than block the commit. In CI the table is the
        # authoritative artifact, so the same condition is a hard failure: a
        # green run that quietly published a stale table is the degradation we
        # are trying to make impossible.
        if ! update_readme_report; then
            [ -z "${CI:-}" ] \
                || die "report-update: refusing to publish an unmeasurable progress table"
            echo "run-suites: report-update skipped (some suites' ROMs are not present)"
        fi
        # Say what happened -- the whole battery runs silently for ~60s, so a
        # "no change" result is otherwise indistinguishable from a broken no-op.
        if git -C "$ROOT" diff --cached --quiet -- README.md 2>/dev/null; then
            log "report-update: README table already current (nothing to change)"
        else
            log "report-update: README table updated and staged"
        fi
    else
        echo "run-suites: report-update skipped (binary or ROM set not present)"
    fi
    exit 0
fi

# A suite (or 'all'/'report'): ensure ROMs + binary exist, then run.
[ "${RB_SKIP_SETUP:-0}" = "1" ] || setup
if [ "${RB_SKIP_BUILD:-0}" != "1" ] && ! bin_ready; then build; fi
bin_ready || die "runner binary not found at $BIN (run: $0 build)"

# Propagate generate_report's refusal (3) instead of exiting 0 on an empty table.
if [ "$1" = "report" ]; then generate_report || exit $?; exit 0; fi

if [ "$1" = "all" ]; then
    # shellcheck disable=SC2086  # ORDER is a space-separated list of bare words
    set -- $ORDER
fi

rc=0
for suite in "$@"; do
    run_suite "$suite" || rc=1
done
exit "$rc"
