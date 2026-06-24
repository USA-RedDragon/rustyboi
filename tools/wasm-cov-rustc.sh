#!/usr/bin/env bash
# RUSTC_WRAPPER for `make coverage-web`: instrument ONLY the rustyboi-web crate
# (its lib + integration tests), NOT its dependencies.
#
# Why: the wasm coverage build sets `-Cinstrument-coverage` workspace-wide, but
# rustyboi-core / -egui / -frontend / -debugger each also emit a `cdylib` +
# `staticlib` in the same rustc call, and instrumenting those makes their final
# wasm link require `__llvm_profile_runtime` — a symbol only the minicov runtime
# provides, and those crates don't (and mustn't) depend on minicov. Scoping
# instrumentation to rustyboi-web sidesteps that AND is exactly what we want to
# measure (the web glue). rustyboi-web's own cdylib link resolves the symbol via
# its cfg-gated minicov dep; the test binaries get it from wasm-bindgen-test.
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

# Non-web crate: drop only the instrumentation flags (keep cfgs, link-args, the
# normal --emit=dep-info,link, everything else) so its cdylib/staticlib links.
args=()
for a in "$@"; do
  case "$a" in
    -Cinstrument-coverage | -Cinstrument-coverage=* | \
    -Zno-profiler-runtime | --emit=llvm-ir)
      continue;;
    *) args+=("$a");;
  esac
done
exec "$rustc" "${args[@]}"
