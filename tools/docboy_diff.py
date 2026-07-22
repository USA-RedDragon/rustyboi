#!/usr/bin/env python3
"""Differential harness for the docboy DIFF-ONLY manifests (NON-GATING).

A docboy self-screenshot is NOT our oracle, so this NEVER fails a build: it runs
rustyboi against docboy's own reference screens and reports where the two
DISAGREE, as leads to investigate. Output is a JSON + a human summary; the exit
code is always 0.

Screenshot-moment fidelity
--------------------------
docboy's FramebufferRunner passes as soon as the framebuffer matches the
reference (polled at VBlank, up to ~1424 frames) -- or, for `ld b,b` tests, at
that one instruction. Our `png` oracle instead grades a single captured frame,
and rustyboi's frame-capture-at-`ld b,b` can land one frame off docboy's
last-completed frame -- which shows up as a spurious ~full-screen mismatch even
when rustyboi renders docboy's EXACT screen a frame earlier or later.

To compare behavior rather than capture timing, this harness reproduces docboy's
"screen ever matches" semantics: it grades each test with `png_fixed` across a
window of frame budgets and counts the test as AGREE if rustyboi's shade-folded
frame equals the reference at ANY budget in the window. A test that never
matches is a genuine disagreement. (A native, one-pass alternative would be a
small crate change adding the existing `--scan-frames` forward-scan to the
`CspPng` path; see the rollout notes in the task report.)

Because the references are specific 2-4 shade patterns, a coincidental
full-frame match is effectively impossible, so "ever matches" does not launder
real behavioral bugs -- a wrong render never equals the exact reference.
"""

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
from collections import Counter

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEFAULT_BIN = os.path.join("target", "release", "rustyboi-test-runner")
PIXELS_RE = re.compile(r"(\d+) differing pixels")


def load_diff_manifest(path):
    rows = []
    for line in open(path):
        line = line.rstrip("\n")
        if not line.strip() or line.startswith("#"):
            continue
        f = line.split("|")
        if len(f) < 5:
            continue
        rows.append({"name": f[0], "mode": f[1], "rom": f[3], "ref": f[4]})
    return rows


def write_fixed_manifest(rows, path):
    with open(path, "w") as fh:
        for r in rows:
            # png_fixed grades the final held frame after a flat --frames budget.
            fh.write(f"{r['name']}|{r['mode']}|png_fixed|{r['rom']}|{r['ref']}\n")


def run_budget(binary, manifest, mode, budget, jobs, jdir):
    jf = os.path.join(jdir, f"b{budget}.json")
    subprocess.run(
        [binary, "--manifest", manifest, "--mode", mode, "--frames", str(budget),
         "--jobs", str(jobs), "--json", jf],
        capture_output=True, check=False,
    )
    return json.load(open(jf))


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("manifest", help="a docboy_diff_<model>.manifest")
    ap.add_argument("--mode", default="dmg")
    ap.add_argument("--min-frames", type=int, default=1)
    ap.add_argument("--max-frames", type=int, default=20,
                    help="upper bound of the match window (docboy's own is ~1424)")
    ap.add_argument("--jobs", type=int, default=max(1, (os.cpu_count() or 2) - 1))
    ap.add_argument("--limit", type=int, default=0, help="grade only the first N rows")
    ap.add_argument("--bin", default=os.environ.get("RB_BIN", DEFAULT_BIN))
    ap.add_argument("--json", default=None, help="write the full result JSON here")
    args = ap.parse_args()

    os.chdir(ROOT)
    if not os.path.exists(args.bin):
        sys.exit(f"error: runner binary {args.bin} not found (cargo build --release "
                 f"-p rustyboi-test-runner)")

    rows = load_diff_manifest(args.manifest)
    if args.limit:
        rows = rows[:args.limit]
    if not rows:
        sys.exit(f"error: no runnable rows in {args.manifest}")
    all_roms = {r["rom"] for r in rows}

    with tempfile.TemporaryDirectory() as jdir:
        fixed = os.path.join(jdir, "fixed.manifest")
        write_fixed_manifest(rows, fixed)
        first_match = {}          # rom -> earliest budget that matched
        min_diff = {}             # rom -> smallest differing-pixel count seen
        for b in range(args.min_frames, args.max_frames + 1):
            r = run_budget(args.bin, fixed, args.mode, b, args.jobs, jdir)
            failed = set()
            for x in r["failures"]:
                failed.add(x["rom"])
                m = PIXELS_RE.search(x["detail"])
                if m:
                    n = int(m.group(1))
                    min_diff[x["rom"]] = min(min_diff.get(x["rom"], 1 << 30), n)
            for rom in all_roms - failed:
                first_match.setdefault(rom, b)

    agree = set(first_match)
    disagree = sorted(all_roms - agree, key=lambda r: min_diff.get(r, 1 << 30))
    name_of = {r["rom"]: r["name"] for r in rows}

    def stem(rom):
        return name_of.get(rom, rom)

    result = {
        "manifest": args.manifest,
        "mode": args.mode,
        "window": [args.min_frames, args.max_frames],
        "total": len(all_roms),
        "agree": len(agree),
        "disagree": len(disagree),
        "disagreements": [
            {"name": stem(rom), "min_differing_pixels": min_diff.get(rom, -1)}
            for rom in disagree
        ],
    }
    if args.json:
        json.dump(result, open(args.json, "w"), indent=2)

    pct = 100.0 * len(disagree) / len(all_roms)
    print(f"docboy differential ({args.manifest}, mode {args.mode}, "
          f"window {args.min_frames}..{args.max_frames} frames)")
    print(f"  tests    = {len(all_roms)}")
    print(f"  AGREE    = {len(agree)} ({100 - pct:.1f}%)  (rustyboi renders docboy's "
          f"exact screen at some frame)")
    print(f"  DISAGREE = {len(disagree)} ({pct:.1f}%)  (leads; a docboy screenshot "
          f"is not our oracle -- non-gating)")
    buck = Counter()
    for rom in disagree:
        n = min_diff.get(rom, -1)
        b = ("1-8" if 0 < n <= 8 else "9-32" if n <= 32 else "33-128" if n <= 128
             else "129-512" if n <= 512 else "512+")
        buck[b] += 1
    print("  disagreement magnitude (min differing pixels over the window):")
    for k in ["1-8", "9-32", "33-128", "129-512", "512+"]:
        if buck.get(k):
            print(f"    {k:>8} px : {buck[k]}")
    print("  closest leads (smallest disagreement = strongest sub-dot signal):")
    for rom in disagree[:10]:
        print(f"    {min_diff.get(rom, -1):>5}px  {stem(rom)}")


if __name__ == "__main__":
    main()
