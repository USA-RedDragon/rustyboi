#!/usr/bin/env python3
"""Generate rustyboi-test-runner manifests for the GBEmulatorShootout test set,
graded with the shootout's OWN screenshot rule (`png_shootout` grading).

Source of truth: a JSON spec extracted from the shootout's Python test
definitions (default `/tmp/shootout_tests.json`), a dict keyed by suite, each a
list of `{name, rom, model, runtime, pass_refs}`.

Each emitted manifest line is:
    <id>|<mode>|png_shootout|<rom_abspath>|<ref1[;ref2;...]>|frames=<N>

- `png_shootout` selects `frame::shootout_mismatch` grading (PIL "L" grayscale
  diff, pass iff every pixel's diff <= 50), matching `util.compareImage`.
- Multiple refs (`;`-separated) are an OR-match, mirroring the shootout's
  `pass_result` list (`Test.checkResult`). Only pass refs are emitted; `.fail.png`
  siblings are for the shootout's FAIL *reporting* and never create passes.
- `frames=<N>` is derived from the shootout `runtime` seconds. The shootout runs
  UP TO `runtime` seconds but early-exits the moment the screen matches, so
  `runtime` is an upper bound; rustyboi grades the final held frame, so we run a
  flat budget of `ceil(runtime*60)` frames plus a small settle margin, floored so
  even the 0.5 s tests reach a stable screen.

SGB tests are skipped (rustyboi has no SGB mode). Tests whose pass ref does not
exist on disk are INFO-only in the shootout (`result=None` with no auto-derived
`.png`); the shootout does not score them, so we skip them too.
"""
import json
import math
import os
import sys

# rustyboi has no SGB hardware mode; drop those cases (the shootout SGB tests).
MODEL = {"DMG": "dmg", "CGB": "cgb"}

# The shootout's `Emulator.runTest` polls for a screenshot match UP TO
# `runtime/speed + startup_time + 5.0` seconds (emulator.py:71), with default
# `speed=1` and `startup_time=1.0`, and early-exits the instant the screen
# matches. So the shootout's own TRUE deadline is `runtime + 6.0` seconds. We
# grade the final held frame (no polling), so we run that full window: this is
# exactly the emulated time the shootout would have allowed. It matters for the
# blargg cpu_instrs subtests, whose result text renders slowly under skip_bios
# (the shootout's recorded `runtime` is a faster reference emulator's match time,
# well inside its own +6 s slack).
SHOOTOUT_SLACK_S = 6.0  # startup_time (1.0) + poll grace (5.0)
FRAME_FLOOR = 90        # never grade before 1.5 s of emulated time


def frames_for(runtime_seconds: float) -> int:
    return max(math.ceil((runtime_seconds + SHOOTOUT_SLACK_S) * 60), FRAME_FLOOR)


def main(spec_path: str, outdir: str) -> int:
    data = json.load(open(spec_path))
    os.makedirs(outdir, exist_ok=True)

    summary = {}
    grand = 0
    for suite, tests in data.items():
        lines = []
        skipped_sgb = 0
        skipped_info = 0
        multiref = 0
        for t in tests:
            mode = MODEL.get(t["model"])
            if mode is None:
                skipped_sgb += 1
                continue
            # Keep only refs that exist on disk (OR-match); an all-missing ref
            # list is an INFO-only shootout test (result=None) — not scored.
            refs = [r for r in t["pass_refs"] if os.path.exists(r)]
            if not os.path.exists(t["rom"]) or not refs:
                skipped_info += 1
                continue
            if len(refs) > 1:
                multiref += 1
            ident = t["name"].replace("|", "_")
            refs_field = ";".join(refs)
            frames = frames_for(t["runtime"])
            lines.append(
                f"{ident}|{mode}|png_shootout|{t['rom']}|{refs_field}|frames={frames}"
            )
        with open(os.path.join(outdir, f"{suite}.manifest"), "w") as f:
            f.write("\n".join(lines) + ("\n" if lines else ""))
        summary[suite] = dict(
            lines=len(lines),
            skipped_sgb=skipped_sgb,
            skipped_info=skipped_info,
            multiref=multiref,
        )
        grand += len(lines)

    for s, v in summary.items():
        print(
            f'  {s:12} {v["lines"]:4} tests  '
            f'(sgb-skipped={v["skipped_sgb"]}, info-skipped={v["skipped_info"]}, '
            f'multiref={v["multiref"]})'
        )
    print("total gradeable shootout tests:", grand)
    return 0


if __name__ == "__main__":
    spec = sys.argv[1] if len(sys.argv) > 1 else "/tmp/shootout_tests.json"
    out = sys.argv[2] if len(sys.argv) > 2 else os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
        "rustyboi-test-runner", "suites", "shootout",
    )
    sys.exit(main(spec, out))
