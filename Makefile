# rustyboi — single build/test/dev interface. Run `make help`.
#
# This Makefile is the front door for everything under tools/. The thin build
# wrappers are inlined here; the android/ios builds live in per-dir sub-makefiles
# (android/Makefile, ios/Makefile — like test-roms/Makefile) that this one calls.
# A few load-bearing engines stay as files that targets exec (run-suites.sh —
# self-rewrites its floor table + runs on macOS's Make 3.81; rust-cross.sh — the
# shared cross-compile matrix + container builder; gen_manifests.py,
# deatomize-coverage.py). You only ever type `make <target>`.
#
# Passthrough is via make variables (make libretro TARGETS="linux-x86_64");
# every existing RB_*/RUSTBOI_* env knob still works (recipes inherit the env).

SHELL := bash
.SHELLFLAGS := -euo pipefail -c
.ONESHELL:
.DEFAULT_GOAL := help
MAKEFLAGS += --no-print-directory

# Multi-line recipes need .ONESHELL (GNU Make >= 3.82). Single-line exec targets
# (suite, setup, …) don't, so they still run on macOS's system Make 3.81 (the
# suite runner runs there in CI). Multi-line recipes call this guard as their
# first line so a too-old make fails cleanly instead of running the body under a
# shell-per-line that silently drops state.
MIN := 3.82
NEED_382 = if [ "$(filter $(MIN),$(firstword $(sort $(MAKE_VERSION) $(MIN))))" != "$(MIN)" ]; then \
  echo "make: this target needs GNU Make >= $(MIN) (macOS system make is 3.81; brew install make, use gmake)" >&2; exit 1; fi

COV_CRATES := -p rustyboi-core -p rustyboi-session -p rustyboi-frontend \
  -p rustyboi-egui -p rustyboi-debugger -p rustyboi-test-runner \
  -p rustyboi-libretro-sys -p rustyboi-libretro -p rustyboi-platform
# Coverage artifact DAG: runner -> profdata -> lcov.info. The two intermediates
# are .PHONY (below) so `make coverage` always regenerates from a fresh build.
COV_TD       := target/llvm-cov-target
COV_RUNNER   := $(COV_TD)/release/rustyboi-test-runner
COV_PROFDATA := $(COV_TD)/rustyboi.profdata
# Prefix every coverage recipe: point cargo at the instrumented target dir and
# pull in llvm-cov's coverage env (RUSTFLAGS, LLVM_PROFILE_FILE). Deterministic,
# so re-sourcing it per recipe is fine.
COV_SETENV = export CARGO_TARGET_DIR=$(COV_TD); source <(cargo llvm-cov show-env --sh)

# Web (wasm) coverage — a SEPARATE pipeline from `make coverage`. The host
# llvm-cov build can't compile rustyboi-web (web-sys/wasm-bindgen are wasm-only),
# so the web glue is instead measured by instrumenting the wasm build and running
# the headless-browser tests under minicov (wasm-bindgen-test's coverage path).
# Needs the nightly toolchain (`-Z` flags) + its llvm-tools, the wasm target,
# wasm-pack, and a headless browser + driver (same as the `web` CI job).
COVW_TC      ?= nightly
COVW_BROWSER ?= chrome
COVW_TD      := target/wasm-cov
COVW_LCOV    := rustyboi-web.lcov

.PHONY: help libretro native runner web android ios pgo targets \
        pgo-gen pgo-flags pgo-path pgo-clean \
        setup build-runner suite suites suites-list report report-update \
        coverage coverage-web bench manifests roms \
        $(COV_RUNNER) $(COV_PROFDATA)

help: ## Show this help
	@grep -hE '^[a-zA-Z0-9_-]+:.*?## ' $(MAKEFILE_LIST) \
	  | sort \
	  | awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-14s\033[0m %s\n",$$1,$$2}'

# ---------------------------------------------------------------------------
# Cross-compiled artifacts — one Make goal per target (libretro-<name>), so Make
# owns the DAG, per-target success/failure, and `-j` parallelism. Each build uses
# its OWN --target-dir (target/cross/<name>) so parallel builds don't serialize
# on cargo's per-target-dir build lock. Costs: host proc-macros recompile per
# dir + more disk under target/cross/. `cross-fetch` warms the shared registry
# once so concurrent builds only read it. The per-target recipes are single-line
# (delegate to rust-cross.sh) — no .ONESHELL, so they run on macOS Make 3.81 too.
# The per-OS artifact naming + matrix stay the single source of truth in bash.
# ---------------------------------------------------------------------------

# Name lists come from the single source (rust-cross.sh), never copied into Make.
# Computed only when libretro/native are actual goals (no $(shell) fork for
# help/suite/coverage), and TARGETS is then required.
ifneq (,$(filter libretro native runner,$(MAKECMDGOALS)))
  ifeq (,$(strip $(TARGETS)))
    $(error set TARGETS="name..." or TARGETS=all  — see `make targets`)
  endif
  LIBRETRO_ALL := $(shell . tools/rust-cross.sh && rc_names_libretro)
  NATIVE_ALL   := $(shell . tools/rust-cross.sh && rc_names_native)
  RUNNER_ALL   := $(shell . tools/rust-cross.sh && rc_names_runner)
endif
libretro_sel = $(if $(filter all,$(TARGETS)),$(LIBRETRO_ALL),$(TARGETS))
native_sel   = $(if $(filter all,$(TARGETS)),$(NATIVE_ALL),$(TARGETS))
runner_sel   = $(if $(filter all,$(TARGETS)),$(RUNNER_ALL),$(TARGETS))

# NB: the per-target goals (libretro-<name>/native-<name>/runner-<name>) must NOT
# be .PHONY — Make skips implicit/pattern-rule search for phony targets, which
# would suppress the libretro-%/native-%/runner-% rules below. No file by those
# names is ever created, so the pattern rules always fire regardless.
.PHONY: cross-fetch

targets: ## Print the cross-compile target table
	@source tools/rust-cross.sh && rc_print_list

libretro: $(addprefix libretro-,$(libretro_sel)) ## Build libretro cores (TARGETS="name..." | TARGETS=all)
	@echo "Cores are under target/libretro/<name>/ with rustyboi_libretro.info alongside."
libretro-%: | cross-fetch
	@source tools/rust-cross.sh && rc_emit_libretro "$*"

native: $(addprefix native-,$(native_sel)) ## Build the desktop rustyboi binary (TARGETS="name..." | TARGETS=all)
	@echo "Binaries are under target/native/<name>/."
native-%: | cross-fetch
	@source tools/rust-cross.sh && rc_emit_native "$*"

runner: $(addprefix runner-,$(runner_sel)) ## Cross-build the test runner (TARGETS="name..." | TARGETS=all)
	@echo "Runners are under target/runner/<name>/."
runner-%: | cross-fetch
	@source tools/rust-cross.sh && rc_emit_runner "$*"

# Warm the shared cargo registry once before the parallel per-target builds.
cross-fetch:
	@source tools/rust-cross.sh && rc_fetch

web: ## Build the wasm frontend (MODE=release|profiling)
	@$(NEED_382)
	MODE="$(or $(MODE),release)"
	OUT=www/pkg
	WASM="rustyboi-web/$$OUT/rustyboi_web_bg.wasm"
	if [ "$$MODE" = profiling ]; then
	  PACK_PROFILE="--profiling"; OPT_FLAGS="-O3 -g -all"
	  export CARGO_PROFILE_RELEASE_STRIP=false CARGO_PROFILE_RELEASE_DEBUG=1
	  echo "==> PROFILING build (optimized + symbols)"
	else
	  PACK_PROFILE="--release"; OPT_FLAGS="-O3 -all"
	fi
	echo "==> wasm-pack build ($$PACK_PROFILE; bundled wasm-opt disabled)"
	rm -rf "rustyboi-web/$$OUT"
	PGO_FLAGS="$$(make -s pgo-flags 2>/dev/null || true)"
	RUSTFLAGS="$$PGO_FLAGS $${RUSTFLAGS:-}" wasm-pack build rustyboi-web --target web --out-dir "$$OUT" "$$PACK_PROFILE"
	if command -v wasm-opt >/dev/null 2>&1; then
	  before=$$(stat -c%s "$$WASM")
	  echo "==> wasm-opt $$OPT_FLAGS ($$(wasm-opt --version))"
	  wasm-opt $$OPT_FLAGS "$$WASM" -o "$$WASM.tmp"; mv "$$WASM.tmp" "$$WASM"
	  echo "    $$before -> $$(stat -c%s "$$WASM") bytes"
	else
	  echo "WARNING: wasm-opt not found — shipping UN-optimized wasm (install binaryen)."
	fi
	sw_id=$$(sha256sum "$$WASM" | cut -c1-12)
	printf 'self.BUILD_ID = "%s";\n' "$$sw_id" > "rustyboi-web/www/sw-version.js"
	echo "==> sw-version.js BUILD_ID=$$sw_id"
	echo "Serve for Firefox:  (cd rustyboi-web/www && python3 -m http.server 8080)"

replay-web:
	@$(NEED_382)
	OUT=pkg
	WASM="rustyboi-replay-web/$$OUT/rustyboi_replay_web_bg.wasm"
	echo "==> wasm-pack build rustyboi-replay-web (--release; bundled wasm-opt disabled)"
	rm -rf "rustyboi-replay-web/$$OUT"
	wasm-pack build rustyboi-replay-web --target web --out-dir "$$OUT" --release
	if command -v wasm-opt >/dev/null 2>&1; then
	  before=$$(stat -c%s "$$WASM")
	  wasm-opt -O3 -all "$$WASM" -o "$$WASM.tmp"; mv "$$WASM.tmp" "$$WASM"
	  echo "==> wasm-opt: $$before -> $$(stat -c%s "$$WASM") bytes"
	fi

android: ## Build the Android app (RELEASE=1 BUNDLE=1 [ABI=arm64-v8a])
	@$(NEED_382)
	$(MAKE) -C android build RELEASE='$(RELEASE)' BUNDLE='$(BUNDLE)' ABI='$(ABI)'

ios: ## Build the iOS app (SIMULATOR=1 DEBUG=1)
	@$(NEED_382)
	$(MAKE) -C ios build SIMULATOR='$(SIMULATOR)' DEBUG='$(DEBUG)'

# ---------------------------------------------------------------------------
# PGO — the shared, version-keyed profile every build consumes. Not committed
# (lives under target/, gitignored). Profile through bench --drive (gameplay,
# pure emulation): profiling through the framebuffer-hashing sweep makes PGO
# ~30% SLOWER; the pure drive gives +40-60%, byte-identical.
# ---------------------------------------------------------------------------

pgo: pgo-gen ## PGO-optimized native build into target/pgo-use (ROMS="dir..." [SAMPLE=N] [FRAMES=N])
	@$(NEED_382)
	FLAGS="$$($(MAKE) -s pgo-flags)"
	if [ -z "$$FLAGS" ]; then echo "PGO profile unusable; aborting" >&2; exit 1; fi
	echo "==> Optimized build (-Cprofile-use) into target/pgo-use"
	RUSTFLAGS="$$FLAGS $${RUSTFLAGS:-}" \
	  cargo build --release -p rustyboi-test-runner -p rustyboi-platform --target-dir target/pgo-use
	echo; echo "PGO binaries under target/pgo-use/release/ (rustyboi, bench, rustyboi-test-runner)"

pgo-gen: ## Generate the PGO profile (ROMS="dir..." [CONTAINER=1] [SUITE=1] [SAMPLE=N] [FRAMES=N])
	@$(NEED_382)
	ROOT="$$(pwd)"
	PGO_DIR="$$ROOT/target/pgo"
	RUSTC_TAG="$$(rustc --version 2>/dev/null | tr -c 'A-Za-z0-9._' '-')"
	PROFILE="$$PGO_DIR/profile-$$RUSTC_TAG.profdata"
	RAW_DIR="$$PGO_DIR/raw"; INSTR_TARGET="$$PGO_DIR/instr"
	frames="$(FRAMES)"; [ -n "$$frames" ] || frames=1200
	sample="$(SAMPLE)"; [ -n "$$sample" ] || sample="$${RB_PGO_SAMPLE:-50}"
	sweep_dirs=($(ROMS)); [ $${#sweep_dirs[@]} -ne 0 ] || sweep_dirs=($${RB_PGO_ROMS:-})
	if [ $${#sweep_dirs[@]} -eq 0 ] && [ -z "$(SUITE)" ]; then
	  echo "pgo-gen: no ROMs. Pass ROMS=\"dirA dirB\" (or RB_PGO_ROMS), or SUITE=1 for the" >&2
	  echo "         free-test-ROM fallback (a menu/torture profile, not gameplay-representative)." >&2
	  exit 2
	fi
	# CONTAINER=1: re-run gen INSIDE the rust-cross image so the profile is keyed
	# to the container's rustc (the one rc_build uses). Needs make in the image.
	if [ -n "$(CONTAINER)" ]; then
	  if [ $${#sweep_dirs[@]} -eq 0 ]; then echo "pgo-gen CONTAINER=1 needs ROMS" >&2; exit 2; fi
	  source tools/rust-cross.sh; rc_engine
	  mounts=(); incroms=(); i=0
	  for d in "$${sweep_dirs[@]}"; do
	    mounts+=(-v "$$(cd "$$d" && pwd)":/roms/$$i:ro); incroms+=("/roms/$$i"); i=$$((i+1))
	  done
	  echo "==> [container $$IMAGE] generating PGO profile"
	  incmd="set -e
	    find \"\$$(rustc --print sysroot)\" -name 'llvm-profdata*' | grep -q . || rustup component add llvm-tools
	    make pgo-gen ROMS=\"$${incroms[*]}\" SAMPLE=$$sample FRAMES=$$frames
	    chown -R $$HOST_UIDGID target/pgo"
	  cname="pgo-$$$$-$$RANDOM"
	  trap '"$$ENGINE" kill "'"$$cname"'" >/dev/null 2>&1 || true; exit 130' INT TERM
	  "$$ENGINE" run --rm --name "$$cname" -v "$$PROJECT_ROOT":/project -w /project \
	    -v "$$CARGO_VOL":/usr/local/cargo/registry "$${mounts[@]}" \
	    "$$IMAGE" sh -c "$$incmd"
	  trap - INT TERM
	  exit 0
	fi
	profdata="$$(find "$$(rustc --print sysroot)" -name 'llvm-profdata*' 2>/dev/null | head -1)"
	[ -n "$$profdata" ] || { echo "llvm-profdata not found; rustup component add llvm-tools" >&2; exit 1; }
	rm -rf "$$RAW_DIR" "$$PROFILE"; mkdir -p "$$RAW_DIR"
	echo "==> instrumented build (test-runner)"
	RUSTFLAGS="-Cprofile-generate=$$RAW_DIR" \
	  cargo build --release -p rustyboi-test-runner --target-dir "$$INSTR_TARGET"
	bench="$$INSTR_TARGET/release/bench"
	if [ $${#sweep_dirs[@]} -gt 0 ]; then
	  # Enumerate the library then stratify-sample to ~$$sample ROMs (every Nth,
	  # alphabetical) — within ~1% of a whole-library profile. Drive each through
	  # bench --drive (masher gameplay input, pure emulation) as SEPARATE PROCESSES
	  # (private counters — no atomic false-sharing, so it parallelizes).
	  all=()
	  while IFS= read -r rom; do all+=("$$rom"); done < <(
	    find "$${sweep_dirs[@]}" -type f \( -iname '*.zip' -o -iname '*.gb' -o -iname '*.gbc' \) | sort)
	  n=$${#all[@]}; stride=1; picked=()
	  if [ "$$n" -gt "$$sample" ]; then stride=$$((n / sample)); fi
	  i=0; while [ "$$i" -lt "$$n" ]; do picked+=("$${all[$$i]}"); i=$$((i + stride)); done
	  echo "==> profiling workload: $$n ROMs -> $${#picked[@]} sampled, bench --drive (gameplay, parallel)"
	  printf '%s\0' "$${picked[@]}" | xargs -0 -P "$$(nproc 2>/dev/null || echo 4)" -I {} \
	    sh -c 'LLVM_PROFILE_FILE="'"$$RAW_DIR"'/p-%p-%m.profraw" "'"$$bench"'" "$$1" '"$$frames"' --drive >/dev/null 2>&1' _ {}
	else
	  echo "==> profiling workload: test suites (SUITE=1; not gameplay-representative)"
	  for s in acid2 cgb_acid_hell scribbltests mealybug blargg; do
	    RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 RB_JOBS=1 \
	      RB_BIN="$$INSTR_TARGET/release/rustyboi-test-runner" \
	      ./tools/run-suites.sh "$$s" >/dev/null 2>&1 || true
	  done
	fi
	if ! ls "$$RAW_DIR"/*.profraw >/dev/null 2>&1; then echo "no .profraw produced; aborting" >&2; exit 1; fi
	echo "==> merging profile ($$(ls "$$RAW_DIR"/*.profraw | wc -l) raw)"
	"$$profdata" merge -o "$$PROFILE" "$$RAW_DIR"/*.profraw
	echo "profile: $$PROFILE ($$(du -h "$$PROFILE" | cut -f1))"

pgo-flags: ## Print the -Cprofile-use fragment for the active rustc (empty if none/incompatible)
	@$(NEED_382)
	[ -z "$${RB_NO_PGO:-}" ] || exit 0
	PGO_DIR="$$(pwd)/target/pgo"
	RUSTC_TAG="$$(rustc --version 2>/dev/null | tr -c 'A-Za-z0-9._' '-')"
	PROFILE="$$PGO_DIR/profile-$$RUSTC_TAG.profdata"
	if [ ! -f "$$PROFILE" ]; then
	  echo "pgo: no profile ($$PROFILE); build without PGO (make pgo-gen)" >&2; exit 0
	fi
	tmp="$$(mktemp -d)"
	err="$$(printf 'pub fn f(){}\n' | rustc --crate-type lib -Cprofile-use="$$PROFILE" \
	  --emit=obj -o "$$tmp/probe.o" - 2>&1 || true)"
	rm -rf "$$tmp"
	if echo "$$err" | grep -qiE 'invalid instrumentation profile|bad magic|truncated profile|unsupported.*version|malformed'; then
	  echo "pgo: profile incompatible with active rustc (regenerate: make pgo-gen); building without PGO" >&2
	else
	  echo "-Cprofile-use=$$PROFILE"
	fi

pgo-path: ## Print the active-rustc profile path
	@echo "$$(pwd)/target/pgo/profile-$$(rustc --version 2>/dev/null | tr -c 'A-Za-z0-9._' '-').profdata"

pgo-clean: ## Remove the PGO profile tree
	@rm -rf "$$(pwd)/target/pgo"; echo "removed target/pgo"

# ---------------------------------------------------------------------------
# Test suites — the regression gate. run-suites.sh stays an engine (self-rewrites
# its floor table, shards via subprocesses + a Python aggregator, runs on macOS's
# Make 3.81); these targets are single-line execs so 3.81 handles them fine.
# All RB_* env knobs pass through unchanged.
# ---------------------------------------------------------------------------

setup: ## Fetch/provision the ROM set (idempotent)
	@./tools/run-suites.sh setup

build-runner: ## Build the release test runner (RB_TARGET=, RB_PGO=1)
	@./tools/run-suites.sh build

.PHONY: library-baseline
library-baseline: ## Regenerate tools/library-baseline.jsonl — gate manifest only, no media/ffmpeg (ROMS="<lib>/GB <lib>/GBC" [BIOS_DIR=bios])
	@$(NEED_382)
	roms=($(ROMS)); [ $${#roms[@]} -ne 0 ] || roms=($${RB_ROMS:-})
	if [ $${#roms[@]} -eq 0 ]; then
	  echo 'library-baseline: no ROMs. Pass ROMS="<lib>/GB <lib>/GBC" (or RB_ROMS).' >&2
	  exit 2
	fi
	bios_dir="$(BIOS_DIR)"; [ -n "$$bios_dir" ] || bios_dir=bios
	out="target/library-baseline-sweep"
	echo "==> building sweep (release)"
	cargo build --release -p rustyboi-test-runner --bin sweep
	# --no-screens = manifest only: no posters, no videos, no ffmpeg. BIOS boot
	# hash rows still emit (decoupled from media), so the baseline gates boots too.
	echo "==> gate-only sweep (--no-screens, no ffmpeg) over $${#roms[@]} dir(s), bios-dir=$$bios_dir"
	rm -rf "$$out"
	./target/release/sweep run --roms "$${roms[@]}" --strip-names --out "$$out" --bios-dir "$$bios_dir" --no-screens
	cp "$$out/manifest.jsonl" tools/library-baseline.jsonl
	echo "==> wrote tools/library-baseline.jsonl ($$(wc -l < tools/library-baseline.jsonl) rows, incl. 8 bios boot rows)"

suite: ## Run one or more suites, gate on floors (SUITE="acid2 age")
	@./tools/run-suites.sh $(SUITE)

suites: ## Run every suite, gate each
	@./tools/run-suites.sh all

suites-list: ## Print the known suites + pass floors
	@./tools/run-suites.sh list

report: ## Print the markdown progress table
	@./tools/run-suites.sh report

report-update: ## Regenerate the README table + ratchet floors (pre-commit hook)
	@./tools/run-suites.sh report-update

# ---------------------------------------------------------------------------
# Coverage / bench / manifests / first-party ROMs.
# ---------------------------------------------------------------------------

coverage: lcov.info ## Combined coverage (unit tests + ROM suites) -> lcov.info

# The instrumented runner: build the unit tests + runner under llvm-cov's env,
# then de-atomize its coverage counters in place (rustc hardcodes ATOMIC counters
# -> 15-21x on the hot loop; lock->nop = ~2.4x). .PHONY so coverage is always fresh.
$(COV_RUNNER):
	@$(NEED_382)
	$(COV_SETENV)
	rm -f "$(COV_TD)"/*.profraw "$(COV_TD)"/*.profdata
	cargo test $(COV_CRATES)
	cargo build --release -p rustyboi-test-runner
	python3 tools/deatomize-coverage.py "$@"

# Run the whole suite board against the instrumented runner ($<) to emit its
# .profraw, then merge the tree (unit-test + suite profraws) into one profdata.
# RB_SHARDS=4: instrumented threads false-share the counter array, so shard into
# processes (merged via %p). Report-only (|| true): the test matrix owns the gate.
$(COV_PROFDATA): $(COV_RUNNER)
	@$(NEED_382)
	$(COV_SETENV)
	RB_SKIP_SETUP=1 RB_SKIP_BUILD=1 RB_SHARDS=4 RB_BIN="$<" ./tools/run-suites.sh all || true
	"$$(rustc --print target-libdir)/../bin/llvm-profdata" merge -sparse -o "$@" "$(COV_TD)"/*.profraw

# Export ONE lcov over the runner + every unit-test binary. cargo-llvm-cov's
# `report` can't map the raw-cargo-built runner (reads 0%), so drive llvm-cov.
# $< = profdata, and Make guarantees the runner prereq is built (no hand check).
lcov.info: $(COV_PROFDATA) $(COV_RUNNER)
	@$(NEED_382)
	$(COV_SETENV)
	# fromjson? skips non-JSON lines; select the test executables + print paths.
	objs="$$(cargo test --no-run --message-format=json $(COV_CRATES) 2>/dev/null \
	  | jq -rR 'fromjson? | select(.executable and .profile.test) | .executable')"
	"$$(rustc --print target-libdir)/../bin/llvm-cov" export -format=lcov -instr-profile="$<" \
	  "$(COV_RUNNER)" $$(printf ' -object %s' $$objs) \
	  --ignore-filename-regex='(\.cargo/registry|\.rustup/|/rustc/)' > "$@"
	echo "wrote $@ ($$(grep -c '^SF:' "$@") source files)"

bench: ## Perf benchmark over ROMs (ROMS="f1 f2..." | ROMS_DIR=dir; FRAMES=5000 BIN=...)
	@$(NEED_382)
	BIN="$(or $(BIN),./target/release/bench)"; FRAMES="$(or $(FRAMES),5000)"
	roms=($(ROMS))
	if [ $${#roms[@]} -eq 0 ] && [ -n "$(ROMS_DIR)" ]; then
	  while IFS= read -r r; do roms+=("$$r"); done < <(
	    find "$(ROMS_DIR)" -type f \( -iname '*.gb' -o -iname '*.gbc' -o -iname '*.zip' \) | sort)
	fi
	if [ $${#roms[@]} -eq 0 ]; then echo "bench: pass ROMS=\"file...\" or ROMS_DIR=dir" >&2; exit 2; fi
	for r in "$${roms[@]}"; do "$$BIN" "$$r" "$$FRAMES" 2>/dev/null; done

manifests: ## Regenerate suite manifests (ONLY=mealybug,age ROMS=gb-test-roms)
	@python3 tools/gen_manifests.py $(if $(ROMS),--roms $(ROMS)) $(if $(ONLY),--only $(ONLY))

roms: ## Assemble the first-party test ROMs (rgbds)
	@$(MAKE) -C test-roms roms

# Web (wasm) coverage -> rustyboi-web.lcov (Codecov flag `web`). Not a file DAG
# like `coverage` above: the run drives a real headless browser, so it is one
# always-fresh recipe. Only rustyboi-web is instrumented (tools/wasm-cov-rustc.sh
# strips the instrumentation flags from its cdylib/staticlib dependencies, whose
# wasm links would otherwise need a profiling runtime they do not carry). The
# covmap cannot be read back out of a `.wasm` (llvm-cov limitation), so we emit
# LLVM IR and turn it into an object with `llc` from the SAME toolchain (no
# version skew), then export against that plus the runtime counters. See
# rustyboi-web/Cargo.toml (cfg-gated minicov), lib.rs (_MINICOV_ANCHOR) and
# tools/wasm-math-shims.rs (naga libm gap).
coverage-web: ## Web (wasm) coverage via headless browser -> rustyboi-web.lcov (BROWSER=chrome|firefox)
	@$(NEED_382)
	rustup run $(COVW_TC) rustc -V >/dev/null 2>&1 || { echo "coverage-web needs the '$(COVW_TC)' toolchain (rustup toolchain install $(COVW_TC))" >&2; exit 1; }
	command -v wasm-pack >/dev/null || { echo "coverage-web needs wasm-pack" >&2; exit 1; }
	NBIN="$$(rustup run $(COVW_TC) rustc --print target-libdir)/../bin"
	[ -x "$$NBIN/llc" ] || { echo "coverage-web needs llvm-tools on $(COVW_TC) (rustup component add llvm-tools --toolchain $(COVW_TC))" >&2; exit 1; }
	rustup run $(COVW_TC) rustc --print target-list | grep -qx wasm32-unknown-unknown || { echo "coverage-web needs the wasm target (rustup target add wasm32-unknown-unknown --toolchain $(COVW_TC))" >&2; exit 1; }
	ROOT="$$(pwd)"; TD="$$ROOT/$(COVW_TD)"; SHIM="$$TD/wasm-math-shims.o"; PROFDIR="$$TD/profraw"
	rm -rf "$$PROFDIR"; mkdir -p "$$PROFDIR"
	echo "==> [1/4] naga libm trap shims -> wasm object"
	rustup run $(COVW_TC) rustc --target=wasm32-unknown-unknown --crate-type=staticlib --emit=obj -O tools/wasm-math-shims.rs -o "$$SHIM"
	echo "==> [2/4] instrument rustyboi-web + run headless $(COVW_BROWSER)"
	export RUSTUP_TOOLCHAIN=$(COVW_TC)
	export CARGO_TARGET_DIR="$$TD"
	export RUSTC_WRAPPER="$$ROOT/tools/wasm-cov-rustc.sh"
	# The shim link-arg + --allow-multiple-definition are web-only: the shim's
	# panic runtime redefines rust_begin_unwind, so it may only sit on the
	# rustyboi-web link (which tolerates the collision via the allow flag).
	# tools/wasm-cov-rustc.sh strips BOTH from every non-web crate's cdylib.
	export CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS="-Cinstrument-coverage -Zno-profiler-runtime --cfg=wasm_bindgen_unstable_test_coverage --emit=llvm-ir -Clink-arg=$$SHIM -Clink-arg=--allow-multiple-definition"
	export CFLAGS_wasm32_unknown_unknown="-matomics -mbulk-memory"
	export LLVM_PROFILE_FILE="$$PROFDIR/web-%p-%m.profraw"
	wasm-pack test --headless --$(COVW_BROWSER) rustyboi-web
	ls "$$PROFDIR"/*.profraw >/dev/null 2>&1 || { echo "no .profraw captured -- coverage did not run (browser/driver?)" >&2; exit 1; }
	echo "==> [3/4] merge counters + compile covmap objects (llc)"
	NBIN="$$(rustup run $(COVW_TC) rustc --print target-libdir)/../bin"
	HOST="$$(rustup run $(COVW_TC) rustc -vV | sed -n 's/^host: //p')"
	"$$NBIN/llvm-profdata" merge -sparse -o "$$TD/web.profdata" "$$PROFDIR"/*.profraw
	# Retarget the wasm IR to the HOST triple: llvm-cov can read the coverage map
	# out of a native object but NOT a wasm one, so llc must not honour the .ll's
	# wasm target. The map itself (function names, regions, hashes) is
	# target-independent, so it still matches the wasm run's counters.
	objs=()
	for ll in "$$TD"/wasm32-unknown-unknown/debug/deps/rustyboi_web*.ll; do \
	  [ -e "$$ll" ] || continue; \
	  o="$${ll%.ll}.covobj.o"; "$$NBIN/llc" -mtriple="$$HOST" -filetype=obj "$$ll" -o "$$o"; objs+=(-object "$$o"); \
	done
	[ $${#objs[@]} -gt 0 ] || { echo "no rustyboi_web LLVM IR emitted -- nothing to map" >&2; exit 1; }
	echo "==> [4/4] export lcov"
	"$$NBIN/llvm-cov" export -format=lcov -instr-profile="$$TD/web.profdata" "$${objs[@]}" --ignore-filename-regex='(\.cargo/registry|\.rustup/|/rustc/|rustyboi-web/tests/)' > "$(COVW_LCOV)"
	echo "wrote $(COVW_LCOV) ($$(grep -c '^SF:' "$(COVW_LCOV)") source files)"
