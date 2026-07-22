#!/usr/bin/env python3
"""Transcode the docboy test corpus into rustyboi's manifest format.

docboy (github.com/Docheinstein/docboy, pinned in tools/sync-docboy-roms.sh)
ships its test suite as 4 JSON configs -- one per hardware model -- that are a
manifest in another shape. Each entry is one of:

  {rom, framebuffer, max_ticks?, check_at_instruction?}  -> compare final screen
  {rom, serial:[3,5,8,13,21,34]}                          -> mooneye fib serial
  {rom, memory:[{address,value,fail_value?}]}             -> register/mem value

This script reads those configs and emits, per model, three manifests in
rustyboi's `<name>|<mode>|<grading>|<rom>[|<ref>][|tokens]` format:

  docboy_anchored_<model>.manifest
      Entries whose expectation is hardware-anchorable BY CONSTRUCTION: the
      author-encoded `serial` / `memory` values, plus `framebuffer` references
      published under a non-`docboy` author folder. These may be GRADED (they
      are the same class of oracle as our mooneye/mem/png suites) -- but many
      duplicate suites we already run (see the redundancy report).

  docboy_diff_<model>.manifest
      Entries graded against a `results/<model>/docboy/*.png` self-screenshot.
      docboy captures these with its own emulator (F12); there is NO hardware
      provenance. These are DIFF-ONLY: we run them, compare, and surface
      disagreements as leads. A disagreement is NEVER a gate failure -- a
      top-tier emulator's screenshot is not our oracle.

  docboy_deferred_<model>.manifest
      Entries we cannot faithfully run yet, one reason per row comment:
      disabled tests, scripted joypad input, two-player (rom2), DMG palette
      overrides, non-`ld b,b` instruction checks, and -- crucially -- every
      NON-DMG framebuffer, whose color-space comparison is not yet validated
      (see the module docstring in the color section below).

THE COLOR PROBLEM (solved for DMG here):
    docboy renders DMG in a green LCD palette (its 4 shades come out as
    (16,64,0) darkest .. (131,149,0) lightest after docboy's color correction).
    rustyboi grades DMG at the SHADE-INDEX level: `normalize_frame` reduces the
    framebuffer to shade 0..3 and emits canonical grayscale
    (0=0xFFFFFF .. 3=0x000000). A raw-RGB diff of green-vs-grey is ~100%
    mismatch = pure palette noise. So we fold each docboy DMG reference through
    a FIXED per-shade map (green shade i -> canonical grey shade i) at transcode
    time; the existing `png` oracle then compares shade-for-shade, and the
    palette difference cancels exactly. The map is fixed (not per-image dense
    rank) so tests using <4 shades still land on the correct shade.

    CGB / cgb_dmg_mode output is COLOR (a DMG cart on CGB uses the compat
    palette). This is solved WITHOUT a transcode by grading at the PPU's 15-bit
    palette level, which is invariant to color correction:

      * rustyboi's `ColorCorrection::Linear` emits `floor(v5*255/31)` per channel;
        the runner's 0xF8 compare mask keeps the top 5 bits, and
        `floor(v5*255/31) >> 3 == v5` for every v5 in 0..31 -- so Linear+0xF8
        recovers the exact 5-bit RGB555 palette entry the PPU chose.
      * docboy stores the SAME palette entry: its LCD is RGB565 (R/B are the raw
        5 bits; G is `round(g5*63/31)` expanded to 6), then 565->888 for the PNG.
        Masking docboy's PNG to the top 5 bits recovers the identical 15-bit
        value (`round(g5*63/31)` and `floor(g5*255/31)` share their top 5 bits
        for all 32 shades).

    So the EXISTING `png`/`png_fixed` oracle (already Linear+0xF8 for CGB) is a
    correction-invariant 15-bit palette compare -- proven bucket-exact over all
    32768 colors and every color in the corpus (2866 distinct), and validated
    10/10 pass WITH vs 0/10 (~full-frame noise) under the runner's `--csp-raw`
    control (Lcd curve + exact compare). CGB references are therefore emitted
    AS-IS (docboy's original PNG); no green fold. serial/memory anchored entries
    are color-independent and are emitted for every model.

Idempotent: the same corpus produces byte-identical outputs.
"""

import json
import math
import os
import sys
from collections import Counter, defaultdict

try:
    from PIL import Image
except ImportError:
    sys.exit("error: Pillow (PIL) is required: pip install Pillow")

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
DOCBOY = os.path.join(ROOT, "gb-test-roms", "docboy")
CONFIG_DIR = os.path.join(DOCBOY, "tests", "config")
GEN_DIR = os.path.join(DOCBOY, "generated")
MANIFEST_DIR = os.path.join(GEN_DIR, "manifests")
REFS_DIR = os.path.join(GEN_DIR, "refs")

# Manifest paths are resolved by the runner relative to the repo root (CWD),
# so every path we emit is rooted at gb-test-roms/docboy/.
REL_DOCBOY = os.path.join("gb-test-roms", "docboy")

MODELS = ["dmg", "cgb", "cgb_dmg_mode", "cgb_dmg_ext_mode"]
# The runner's Mode selects the silicon. A DMG cart run under Mode::Cgb boots in
# CGB DMG-compatibility mode, which is exactly what cgb_dmg_mode/ext_mode want.
MODEL_MODE = {
    "dmg": "dmg",
    "cgb": "cgb",
    "cgb_dmg_mode": "cgb",
    "cgb_dmg_ext_mode": "cgb",
}

CYCLES_PER_FRAME = 70224
FIB = [3, 5, 8, 13, 21, 34]
GB_W, GB_H = 160, 144

# docboy's default DMG palette (main.cpp DMG_PALETTE / colormap.h
# DEFAULT_APPEARANCE), as it comes out of the reference PNGs after docboy's
# color correction. Keyed dark->light == shade 3..0.
DOCBOY_DMG_GREEN = {
    (16, 64, 0): 3,    # darkest
    (41, 85, 0): 2,
    (74, 105, 0): 1,
    (131, 149, 0): 0,  # lightest
}
# rustyboi's canonical grayscale for shade 0..3 (frame::normalize_mono).
CANON_GRAY = {0: (255, 255, 255), 1: (170, 170, 170), 2: (85, 85, 85), 3: (0, 0, 0)}
CANON_GRAY_SET = set(CANON_GRAY.values())

# Author folders that correspond to suites rustyboi already runs first-party, so
# an anchored entry from one is a redundant-coverage CANDIDATE (same ROM family,
# same class of author reference) rather than new coverage.
REDUNDANT_AUTHORS = {
    "blargg": "blargg",
    "mooneye": "mooneye / mooneye_wilbertpol",
    "mealybug": "mealybug",
    "samesuite": "samesuite_apu / samesuite_nonapu",
    "age": "age",
    "mattcurrie": "acid2 (dmg-acid2 / cgb-acid2)",
    "little-things-gb": "little_things_gb",
    "daid": "daid",
    "cpp": "cpp",
    "magen": "magentests",
    "hacktix": "bully / strikethrough",
}


def die(msg):
    sys.exit(f"error: {msg}")


def load_config(model):
    path = os.path.join(CONFIG_DIR, f"{model}.json")
    if not os.path.isfile(path):
        die(f"missing config {path} -- run tools/sync-docboy-roms.sh first")
    with open(path) as fh:
        return json.load(fh)


def frames_for_ticks(ticks):
    return max(1, math.ceil(ticks / CYCLES_PER_FRAME))


def transcode_dmg_ref(src_png, dst_png):
    """Fold a docboy DMG reference to canonical grayscale by shade index.

    Returns (True, None) on success, or (False, offending_colors) if the image
    uses any color outside docboy's fixed 4-shade green palette (or the
    already-canonical grayscale some vendored author refs use), in which case we
    refuse to guess a shade and the entry is deferred.
    """
    im = Image.open(src_png).convert("RGB")
    colors = {c for _, c in im.getcolors(maxcolors=1 << 16)}
    if colors <= set(DOCBOY_DMG_GREEN):
        lut = {c: CANON_GRAY[DOCBOY_DMG_GREEN[c]] for c in colors}
    elif colors <= CANON_GRAY_SET:
        lut = {c: c for c in colors}  # vendored author blob, already canonical
    else:
        bad = colors - set(DOCBOY_DMG_GREEN) - CANON_GRAY_SET
        return False, sorted(bad)
    out = Image.new("RGB", im.size)
    out.putdata([lut[p] for p in im.getdata()])
    os.makedirs(os.path.dirname(dst_png), exist_ok=True)
    out.save(dst_png, "PNG", optimize=False)
    return True, None


def cgb_ref_ok(src_png):
    """A CGB / cgb_dmg_mode colour reference needs NO transcode -- unlike DMG.

    docboy's screen is 15-bit CGB colour. The runner grades these with
    `ColorCorrection::Linear` + the 0xF8 compare mask, which keeps the top 5 bits
    of each 8-bit channel -- i.e. the exact RGB555 palette entry the PPU chose
    (`floor(v*255/31) >> 3 == v` for all v in 0..31). docboy stores the same
    palette entry (its LCD is RGB565: R/B are the raw 5 bits, G is
    `round(g5*63/31)` expanded to 6, then 565->888); masking both to the top 5
    bits recovers the identical 15-bit value on either side -- proven
    bucket-exact over all 32768 colours and every colour in the corpus. So a CGB
    reference is gradable AS-IS iff its geometry matches the framebuffer; there is
    no palette to fold. Returns (True, None) or (False, reason)."""
    try:
        im = Image.open(src_png)
        size = im.size
    except (OSError, ValueError) as exc:
        return False, f"unreadable reference PNG ({exc})"
    if size != (GB_W, GB_H):
        return False, f"reference is {size[0]}x{size[1]}, not {GB_W}x{GB_H}"
    return True, None


class Row:
    __slots__ = ("name", "mode", "grading", "rom", "ref", "tokens", "bucket",
                 "author", "reason")

    def __init__(self, name, mode, grading, rom, ref=None, tokens=None,
                 bucket="anchored", author=None, reason=None):
        self.name = name
        self.mode = mode
        self.grading = grading
        self.rom = rom
        self.ref = ref
        self.tokens = tokens or []
        self.bucket = bucket
        self.author = author
        self.reason = reason

    def line(self):
        parts = [self.name, self.mode, self.grading, self.rom]
        if self.ref is not None:
            parts.append(self.ref)
        parts.extend(self.tokens)
        base = "|".join(parts)
        if self.bucket == "deferred" and self.reason:
            return f"# [{self.reason}] {base}"
        return base


def classify(model, cat, entry, stats):
    """Map one docboy config entry to a Row (or None to drop silently)."""
    mode = MODEL_MODE[model]
    rom_field = entry["rom"]
    rom = f"{REL_DOCBOY}/tests/roms/{model}/{rom_field}"
    name = f"docboy/{model}/{os.path.splitext(rom_field)[0]}"

    disabled = entry.get("enabled") is False

    if "serial" in entry:
        if entry["serial"] != FIB:
            return Row(name, mode, "mooneye", rom, bucket="deferred",
                       reason="serial: non-fib sequence")
        if disabled:
            return Row(name, mode, "mooneye", rom, bucket="deferred",
                       reason="disabled")
        return Row(name, mode, "mooneye", rom, bucket="anchored", author="mooneye")

    if "memory" in entry:
        mem = entry["memory"]
        if len(mem) != 1:
            return Row(name, mode, "mem", rom, bucket="deferred",
                       reason=f"memory: {len(mem)} addresses (oracle checks one)")
        if "inputs" in entry:
            return Row(name, mode, "mem", rom, bucket="deferred",
                       reason="memory: scripted joypad input")
        if disabled:
            return Row(name, mode, "mem", rom, bucket="deferred", reason="disabled")
        addr = mem[0]["address"]
        val = mem[0]["value"]
        tokens = []
        if "max_ticks" in entry:
            tokens.append(f"frames={frames_for_ticks(entry['max_ticks'])}")
        return Row(name, mode, f"mem {addr:04X}={val:02X}", rom, tokens=tokens,
                   bucket="anchored", author="docboy-mem")

    if "framebuffer" in entry:
        fb_field = entry["framebuffer"]
        author = fb_field.split("/")[0]
        bucket = "diff" if author == "docboy" else "anchored"
        src_ref = os.path.join(DOCBOY, "tests", "results", model, fb_field)

        if disabled:
            return Row(name, mode, "png", rom, bucket="deferred", author=author,
                       reason="disabled")
        for key, why in (("inputs", "scripted joypad input"),
                         ("rom2", "two-player (rom2/framebuffer2)"),
                         ("palette", "DMG palette override")):
            if key in entry:
                return Row(name, mode, "png", rom, bucket="deferred",
                           author=author, reason=f"framebuffer: {why}")
        cai = entry.get("check_at_instruction")
        if cai is not None and cai != "ld b,b":
            return Row(name, mode, "png", rom, bucket="deferred", author=author,
                       reason=f"framebuffer: check_at_instruction '{cai}'")

        # Grading + frame budget are common to DMG and CGB.
        grading = "png"
        tokens = []
        if "check_at_tick" in entry:
            grading = "png_fixed"
            tokens.append(f"frames={frames_for_ticks(entry['check_at_tick'])}")
        elif "max_ticks" in entry:
            tokens.append(f"frames={frames_for_ticks(entry['max_ticks'])}")

        if model == "dmg":
            # DMG: fold the green reference to canonical grayscale (shade compare).
            rel_ref = os.path.join(REFS_DIR, model, fb_field)
            ok, bad = transcode_dmg_ref(src_ref, rel_ref)
            if not ok:
                stats["dmg_palette_skips"][name] = bad
                return Row(name, mode, "png", rom, bucket="deferred", author=author,
                           reason=f"framebuffer: DMG palette has non-shade colors {bad}")
            ref = os.path.relpath(rel_ref, ROOT)
        else:
            # CGB / cgb_dmg_mode: colour output, graded by the runner's
            # correction-invariant 15-bit-palette compare (Linear + 0xF8 mask).
            # No palette fold -- the reference is docboy's ORIGINAL PNG.
            ok, reason = cgb_ref_ok(src_ref)
            if not ok:
                stats["cgb_ref_skips"][name] = reason
                return Row(name, mode, "png", rom, bucket="deferred", author=author,
                           reason=f"framebuffer: {reason}")
            ref = os.path.relpath(src_ref, ROOT)

        return Row(name, mode, grading, rom, ref=ref, tokens=tokens,
                   bucket=bucket, author=author)

    return Row(name, MODEL_MODE[model], "png", rom, bucket="deferred",
               reason=f"unrecognized entry keys {sorted(entry)}")


HEADERS = {
    "anchored": ("docboy ANCHORED ({model}): serial/mem + author-published "
                 "framebuffer references. Hardware-anchorable by construction; "
                 "may be graded, but NOT yet added to any floor. Many duplicate "
                 "existing suites -- see generated/report.json."),
    "diff": ("docboy DIFF-ONLY ({model}): graded against docboy's OWN F12 "
             "self-screenshots (no hardware provenance). Run + compare; a "
             "disagreement is a LEAD, never a gate failure. {color_note} Run "
             "e.g.: rustyboi-test-runner --manifest THIS --mode {mode} --frames "
             "60 --scan-frames 240 --json out.json"),
    "deferred": ("docboy DEFERRED ({model}): entries not yet runnable "
                 "faithfully; each row is commented with the reason. Rows are "
                 "commented out so the runner ignores them."),
}

# Per-model note about how the colour space is reconciled for framebuffer refs.
COLOR_NOTE = {
    "dmg": ("DMG references were folded green->canonical-grey so the shade-index "
            "compare cancels the palette."),
    "cgb": ("CGB references are docboy's ORIGINAL PNGs, graded by the runner's "
            "correction-invariant 15-bit-palette compare (Linear + 0xF8 mask "
            "recovers the RGB555 palette entry, so colour correction cancels)."),
}


def write_manifest(path, header, rows):
    rows = sorted(rows, key=lambda r: r.name)
    lines = [f"# {header}"]
    lines.extend(r.line() for r in rows)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as fh:
        fh.write("\n".join(lines) + "\n")


def main():
    if not os.path.isdir(CONFIG_DIR):
        die(f"{CONFIG_DIR} not found -- run tools/sync-docboy-roms.sh first")

    report = {"pin": None, "models": {}}
    sha = os.path.join(DOCBOY, ".docboy-sha")
    if os.path.isfile(sha):
        report["pin"] = open(sha).read().strip()

    grand = defaultdict(int)
    for model in MODELS:
        cfg = load_config(model)
        buckets = {"anchored": [], "diff": [], "deferred": []}
        stats = {"dmg_palette_skips": {}, "cgb_ref_skips": {}}
        oracle_counts = Counter()
        anchored_authors = Counter()
        deferred_reasons = Counter()

        for cat, entries in cfg.items():
            for entry in entries:
                row = classify(model, cat, entry, stats)
                if row is None:
                    continue
                buckets[row.bucket].append(row)
                base_grading = row.grading.split()[0]
                oracle_counts[base_grading] += 1
                if row.bucket == "anchored" and row.author:
                    anchored_authors[row.author] += 1
                if row.bucket == "deferred":
                    deferred_reasons[(row.reason or "?").split(":")[0]] += 1

        for bucket, rows in buckets.items():
            path = os.path.join(MANIFEST_DIR, f"docboy_{bucket}_{model}.manifest")
            color_note = COLOR_NOTE["dmg" if model == "dmg" else "cgb"]
            header = HEADERS[bucket].format(
                model=model, mode=MODEL_MODE[model], color_note=color_note)
            write_manifest(path, header, rows)

        # Redundancy note: anchored entries whose author maps to a suite we run.
        redundant = Counter()
        for row in buckets["anchored"]:
            if row.author in REDUNDANT_AUTHORS:
                redundant[REDUNDANT_AUTHORS[row.author]] += 1

        report["models"][model] = {
            "mode": MODEL_MODE[model],
            "total_entries": sum(len(v) for v in buckets.values()),
            "anchored": len(buckets["anchored"]),
            "diff": len(buckets["diff"]),
            "deferred": len(buckets["deferred"]),
            "oracle_counts": dict(oracle_counts),
            "anchored_authors": dict(anchored_authors),
            "deferred_reasons": dict(deferred_reasons),
            "redundant_candidates": dict(redundant),
            "dmg_palette_skips": stats["dmg_palette_skips"],
            "cgb_ref_skips": stats["cgb_ref_skips"],
        }
        for k in ("anchored", "diff", "deferred"):
            grand[k] += len(buckets[k])

    report["totals"] = dict(grand)
    os.makedirs(GEN_DIR, exist_ok=True)
    with open(os.path.join(GEN_DIR, "report.json"), "w") as fh:
        json.dump(report, fh, indent=2, sort_keys=True)

    # Human summary to stdout.
    print(f"docboy pin: {report['pin']}")
    print(f"{'model':<18} {'mode':<5} {'anchored':>9} {'diff':>6} {'deferred':>9}")
    for model in MODELS:
        m = report["models"][model]
        print(f"{model:<18} {m['mode']:<5} {m['anchored']:>9} {m['diff']:>6} "
              f"{m['deferred']:>9}")
    print(f"{'TOTAL':<18} {'':<5} {grand['anchored']:>9} {grand['diff']:>6} "
          f"{grand['deferred']:>9}")
    print(f"\nmanifests -> {os.path.relpath(MANIFEST_DIR, ROOT)}")
    print(f"folded DMG refs -> {os.path.relpath(REFS_DIR, ROOT)}")
    print(f"full report -> {os.path.relpath(os.path.join(GEN_DIR, 'report.json'), ROOT)}")


if __name__ == "__main__":
    main()
