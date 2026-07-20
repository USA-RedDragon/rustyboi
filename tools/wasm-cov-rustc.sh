#!/usr/bin/env bash
# RUSTC_WRAPPER for `make coverage-web`: instrument ONLY the rustyboi-web crate
# (its lib + integration tests), NOT its dependencies.
#
# Why: the wasm coverage build sets `-Cinstrument-coverage` workspace-wide, but
# rustyboi-web's deps are exactly what we do NOT want to measure — the target is
# the web glue. Scoping instrumentation to rustyboi-web also keeps every
# `__llvm_profile_runtime` reference inside the one crate that can resolve it:
# rustyboi-web's own cdylib link gets the symbol from its cfg-gated minicov dep,
# and the test binaries get it from wasm-bindgen-test. No other workspace crate
# depends on minicov (or should).
#
# The coverage cfg (`--cfg=wasm_bindgen_unstable_test_coverage`) is left on every
# crate so wasm-bindgen-test still compiles its `__wbgtest_cov_dump` path.
set -euo pipefail

rustc="$1"; shift

is_web=0
for a in "$@"; do
  case "$a" in
    rustyboi-web/src/lib.rs | */rustyboi-web/src/lib.rs | \
    rustyboi-web/tests/* | */rustyboi-web/tests/*)
      is_web=1; break;;
  esac
done

if [ "$is_web" = 1 ]; then
  exec "$rustc" "$@"
fi

# Non-web crate: drop the instrumentation flags AND the web-only naga math-shim
# link-args (keep cfgs, other link-args, the normal --emit=dep-info,link,
# everything else). The shim staticlib carries its own Rust panic runtime (its
# `#[panic_handler]` emits `rust_begin_unwind` with the fixed `__rustc`
# lang-item mangling), so linking it into any std wasm crate collides with that
# crate's own `rust_begin_unwind` (duplicate symbol). Only rustyboi-web
# references naga's libm symbols, and only its link tolerates the shim via the
# paired `--allow-multiple-definition`.
args=()
for a in "$@"; do
  case "$a" in
    -Cinstrument-coverage | -Cinstrument-coverage=* | \
    -Zno-profiler-runtime | --emit=llvm-ir | \
    -Clink-arg=*wasm-math-shims.o | \
    -Clink-arg=--allow-multiple-definition)
      continue;;
    *) args+=("$a");;
  esac
done
exec "$rustc" "${args[@]}"
