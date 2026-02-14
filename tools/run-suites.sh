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
MODE="${RB_MODE:-dmg,cgb}"
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
        acid2)              echo "3 -" ;;
        cgb_acid_hell)      echo "1 -" ;;
        mealybug)           echo "39 -" ;;
        mooneye)            echo "188 -" ;;
        mooneye_wilbertpol) echo "188 -" ;;
        age)                echo "41 -" ;;
        gbmicrotest)        echo "481 -" ;;
        samesuite_apu)      echo "70 -" ;;
        samesuite_nonapu)   echo "6 -" ;;
        samesuite_sgb)      echo "2 -" ;;
        sgb)                echo "1 -" ;;
        blargg)             echo "15 4000" ;;
        blargg_singles)     echo "41 2000" ;;
        scribbltests)       echo "10 -" ;;
        turtle_tests)       echo "4 -" ;;
        little_things_gb)   echo "4 -" ;;
        bully)              echo "2 -" ;;
        strikethrough)      echo "2 -" ;;
        daid)               echo "8 -" ;;
        rtc3test)           echo "6 -" ;;
        mbc3_tester)        echo "2 -" ;;
        cpp)                echo "3 -" ;;
        gambatte)           echo "5241 -" ;;  # gated on failed<=16 (GAMBATTE_MAX_FAIL)
        *)                  echo "" ;;
    esac
}
GAMBATTE_MAX_FAIL=16

# Deterministic suite order for `all`.
ORDER="acid2 cgb_acid_hell mealybug mooneye mooneye_wilbertpol age gbmicrotest \
samesuite_apu samesuite_nonapu samesuite_sgb sgb blargg blargg_singles \
scribbltests turtle_tests little_things_gb bully strikethrough daid rtc3test \
mbc3_tester cpp gambatte"

# --- helpers -----------------------------------------------------------------
log()  { printf '%s\n' "==> $*"; }
warn() { printf '%s\n' "!!  $*" >&2; }
die()  { printf '%s\n' "!!  $*" >&2; exit 1; }

usage() { sed -n '2,33p' "$0"; }

# python3 on Linux/macOS; `python` on Windows (Git Bash ships no `python3`).
PY="$(command -v python3 || command -v python || true)"
[ -n "$PY" ] || die "python3 (or python) is required but was not found on PATH"

# json_field <file> <key>  -> integer value (no jq dependency; python is already
# a prerequisite here, for zip extraction).
json_field() {
    "$PY" -c "import json,sys;print(json.load(open(sys.argv[1]))[sys.argv[2]])" "$1" "$2"
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
    log "Sourcing cpp + daid ROMs (not in the c-sp set) @ ${SHOOTOUT_REF:0:12}"
    sync_shootout_roms
    log "ROM setup complete"
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
    log "Building release test runner${TARGET:+ for $TARGET}"
    if [ -n "$TARGET" ]; then
        cargo build --release -p rustyboi-test-runner --target "$TARGET"
    else
        cargo build --release -p rustyboi-test-runner
    fi
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

update_readme_report() {
    local readme="$ROOT/README.md" tmp out
    grep -q '<!-- SUITE-PROGRESS:START -->' "$readme" \
        && grep -q '<!-- SUITE-PROGRESS:END -->' "$readme" \
        || die "SUITE-PROGRESS markers not found in $readme"

    tmp="$(mktemp)"
    generate_report > "$tmp"

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
    printf '| Suite | Passing | Total |\n'
    printf '| :--- | ---: | ---: |\n'
    local suite row frames out passed total tp=0 tt=0
    for suite in $ORDER; do
        row="$(threshold "$suite")"
        frames="$(printf '%s' "$row" | cut -d' ' -f2)"
        out="$(mktemp)"
        if [ "$frames" != "-" ]; then
            "$BIN" --manifest "$SUITES_DIR/$suite.manifest" --mode "$MODE" \
                --jobs "$JOBS" --frames "$frames" --json "$out" >/dev/null 2>&1 || true
        else
            "$BIN" --manifest "$SUITES_DIR/$suite.manifest" --mode "$MODE" \
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

# report-update is the pre-commit hook (always_run): keep it FAST and SAFE.
# Regenerate the README table (and ratchet the threshold() floors up to the
# measured counts) only when the staged change could actually move a pass count
# (emulator source or a suite manifest); never download ROMs or build during a
# commit -- skip silently if the binary/ROM set isn't ready (CI keeps the table
# honest regardless). update_readme_report stages its own edits, so the table
# and ratchet land in the same commit rather than aborting it.
if [ "$1" = "report-update" ]; then
    if git diff --cached --quiet -- rustyboi-core rustyboi-test-runner/suites 2>/dev/null; then
        exit 0  # no source/manifest change staged -> counts can't have moved
    fi
    if [ -x "$BIN" ] && [ -f "$ROMS/.rb-setup-complete" ]; then
        update_readme_report
    else
        echo "run-suites: report-update skipped (binary or ROM set not present)"
    fi
    exit 0
fi

# A suite (or 'all'/'report'): ensure ROMs + binary exist, then run.
[ "${RB_SKIP_SETUP:-0}" = "1" ] || setup
if [ "${RB_SKIP_BUILD:-0}" != "1" ] && [ ! -x "$BIN" ]; then build; fi
[ -x "$BIN" ] || die "runner binary not found at $BIN (run: $0 build)"

if [ "$1" = "report" ]; then generate_report; exit 0; fi

if [ "$1" = "all" ]; then
    # shellcheck disable=SC2086  # ORDER is a space-separated list of bare words
    set -- $ORDER
fi

rc=0
for suite in "$@"; do
    run_suite "$suite" || rc=1
done
exit "$rc"
