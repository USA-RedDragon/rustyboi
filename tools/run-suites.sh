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
#   RB_MODE     hardware modes            (default: dmg,cgb)
#   RB_JOBS     parallel case workers     (default: nproc-derived)
#   RB_ROMS     ROM dir                   (default: gb-test-roms)
#   RB_BIN      runner binary             (default: target/release/rustyboi-test-runner)
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

# --- config ------------------------------------------------------------------
ROMS="${RB_ROMS:-gb-test-roms}"
BIN="${RB_BIN:-target/release/rustyboi-test-runner}"
MODE="${RB_MODE:-dmg,cgb}"
SUITES_DIR="rustyboi-test-runner/suites"
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
# Measured on campaign-ci HEAD (c-sp v7.0 + gambatte-core oracles). `>=` gate.
# gambatte is special-cased in run_suite (assert failed<=16, not passed>=).
# A plain `case` (not an associative array) keeps this portable to bash 3.2,
# which is the /bin/bash that GitHub's macOS runners ship.
threshold() {
    case "$1" in
        acid2)              echo "3 -" ;;
        cgb_acid_hell)      echo "1 -" ;;
        mealybug)           echo "32 -" ;;
        mooneye)            echo "183 -" ;;
        mooneye_wilbertpol) echo "167 -" ;;
        age)                echo "37 -" ;;
        gbmicrotest)        echo "479 -" ;;
        samesuite_apu)      echo "70 -" ;;
        samesuite_nonapu)   echo "6 -" ;;
        samesuite_sgb)      echo "2 -" ;;
        sgb)                echo "1 -" ;;
        blargg)             echo "15 4000" ;;
        blargg_singles)     echo "41 2000" ;;
        scribbltests)       echo "7 -" ;;
        turtle_tests)       echo "4 -" ;;
        little_things_gb)   echo "4 -" ;;
        bully)              echo "2 -" ;;
        strikethrough)      echo "2 -" ;;
        rtc3test)           echo "6 -" ;;
        mbc3_tester)        echo "1 -" ;;
        gambatte)           echo "5241 -" ;;  # gated on failed<=16 (GAMBATTE_MAX_FAIL)
        *)                  echo "" ;;
    esac
}
GAMBATTE_MAX_FAIL=16

# Deterministic suite order for `all`.
ORDER="acid2 cgb_acid_hell mealybug mooneye mooneye_wilbertpol age gbmicrotest \
samesuite_apu samesuite_nonapu samesuite_sgb sgb blargg blargg_singles \
scribbltests turtle_tests little_things_gb bully strikethrough rtc3test \
mbc3_tester gambatte"

# --- helpers -----------------------------------------------------------------
log()  { printf '%s\n' "==> $*"; }
warn() { printf '%s\n' "!!  $*" >&2; }
die()  { printf '%s\n' "!!  $*" >&2; exit 1; }

usage() { sed -n '2,33p' "$0"; }

# json_field <file> <key>  -> integer value (no jq dependency; python3 is on
# every GitHub runner and is already a build prerequisite here).
json_field() {
    python3 -c "import json,sys;print(json.load(open(sys.argv[1]))[sys.argv[2]])" "$1" "$2"
}

# --- setup: fetch ROMs (idempotent) ------------------------------------------
setup() {
    if [ -f "$ROMS/.rb-setup-complete" ]; then
        log "ROM set already present ($ROMS); skipping setup (rm $ROMS/.rb-setup-complete to force)"
        return 0
    fi
    log "Fetching c-sp gameboy-test-roms $CSP_VERSION"
    mkdir -p "$ROMS"
    local zip="$ROMS/game-boy-test-roms-${CSP_VERSION}.zip"
    curl -fsSL -o "$zip" "$CSP_URL"
    # The archive is flat (suite dirs at its root) -> extract straight into $ROMS.
    # Extract with python3 (already required for JSON parsing) so this works
    # identically on Linux, macOS and Windows (Git Bash), where `unzip` may be
    # absent and GNU `tar` cannot read zips.
    python3 -c "import sys,zipfile;zipfile.ZipFile(sys.argv[1]).extractall(sys.argv[2])" "$zip" "$ROMS"
    rm -f "$zip"
    log "Syncing gambatte-core oracles (.bin/.dump) @ ${GAMBATTE_CORE_REF:0:12}"
    sync_gambatte_oracles
    log "Sourcing sgb-ext-test ROM (not in the c-sp set) @ ${SHOOTOUT_REF:0:12}"
    sync_sgb_rom
    touch "$ROMS/.rb-setup-complete"
    log "ROM setup complete"
}

# The sgb suite's ROM (cpp/sgb-ext-test.gb + .png) is absent from the c-sp set;
# it lives in GBEmulatorShootout/testroms/cpp. Pull just that dir with the same
# shallow / blobless / sparse checkout used for the gambatte oracles.
sync_sgb_rom() {
    local dst="$ROMS/cpp"
    [ -f "$dst/sgb-ext-test.gb" ] && return 0
    local tmp
    tmp="$(mktemp -d)"
    git -C "$tmp" init -q
    git -C "$tmp" remote add origin "$SHOOTOUT_URL"
    git -C "$tmp" config core.sparseCheckout true
    git -C "$tmp" sparse-checkout init --cone >/dev/null 2>&1 || true
    git -C "$tmp" sparse-checkout set testroms/cpp >/dev/null 2>&1 || true
    git -C "$tmp" fetch -q --depth 1 --filter=blob:none origin "$SHOOTOUT_REF"
    git -C "$tmp" checkout -q FETCH_HEAD
    mkdir -p "$dst"
    cp -p "$tmp/testroms/cpp/sgb-ext-test.gb"  "$dst/" || warn "sgb-ext-test.gb missing in shootout checkout"
    cp -p "$tmp/testroms/cpp/sgb-ext-test.png" "$dst/" 2>/dev/null || true
    rm -rf "$tmp"
    log "Sourced sgb-ext-test into $dst"
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
    log "Building release test runner"
    cargo build --release -p rustyboi-test-runner
}

# --- run one suite + gate ----------------------------------------------------
# echoes a one-line result; returns 0 pass / 1 regression / 2 config error.
run_suite() {
    local suite="$1"
    local row
    row="$(threshold "$suite")"
    [ -n "$row" ] || { warn "unknown suite: $suite (see 'list')"; return 2; }
    local manifest="$SUITES_DIR/$suite.manifest"
    [ -f "$manifest" ] || { warn "missing manifest: $manifest"; return 2; }

    local floor frames
    floor="$(printf '%s' "$row" | cut -d' ' -f1)"
    frames="$(printf '%s' "$row" | cut -d' ' -f2)"

    local out
    out="$(mktemp)"

    # Only two suites (blargg, blargg_singles) override the global frame budget;
    # branch rather than use an array so this is safe under `set -u` on bash 3.2
    # (macOS /bin/bash), where expanding an empty "${arr[@]}" is an error.
    log "Running suite '$suite' (mode=$MODE jobs=$JOBS ${frames#-} frames)"
    if [ "$frames" != "-" ]; then
        "$BIN" --manifest "$manifest" --mode "$MODE" --jobs "$JOBS" \
            --frames "$frames" --json "$out" || true
    else
        "$BIN" --manifest "$manifest" --mode "$MODE" --jobs "$JOBS" \
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
    printf '%-22s %-14s %s\n' "SUITE" "FLOOR" "FRAMES"
    local suite row floor frames
    for suite in $ORDER; do
        row="$(threshold "$suite")"
        floor="$(printf '%s' "$row" | cut -d' ' -f1)"
        frames="$(printf '%s' "$row" | cut -d' ' -f2)"
        [ "$frames" = "-" ] && frames="(default)"
        if [ "$suite" = "gambatte" ]; then floor="failed<=$GAMBATTE_MAX_FAIL"; fi
        printf '%-22s %-14s %s\n' "$suite" "$floor" "$frames"
    done
}

# --- main --------------------------------------------------------------------
[ $# -ge 1 ] || { usage; exit 1; }

case "$1" in
    -h|--help) usage; exit 0 ;;
    list)      list; exit 0 ;;
    setup)     setup; exit 0 ;;
    build)     build; exit 0 ;;
esac

# A suite (or 'all'): ensure ROMs + binary exist, then run + gate.
[ "${RB_SKIP_SETUP:-0}" = "1" ] || setup
if [ "${RB_SKIP_BUILD:-0}" != "1" ] && [ ! -x "$BIN" ]; then build; fi
[ -x "$BIN" ] || die "runner binary not found at $BIN (run: $0 build)"

if [ "$1" = "all" ]; then
    # shellcheck disable=SC2086  # ORDER is a space-separated list of bare words
    set -- $ORDER
fi

rc=0
for suite in "$@"; do
    run_suite "$suite" || rc=1
done
exit "$rc"
