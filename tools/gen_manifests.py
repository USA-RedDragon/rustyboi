#!/usr/bin/env python3
"""Regenerate every manifest consumed by `rustyboi-test-runner --manifest`.

Two manifest families, one script:

  internal  — the c-sp public-suite manifests under rustyboi-test-runner/suites/
              (acid2, mealybug, blargg, gbmicrotest, mooneye, wilbertpol, age,
              cgb-acid-hell, scribbltests, turtle-tests, little-things-gb,
              bully, strikethrough, same-suite, rtc3test, mbc3-tester).
              ROM set: c-sp/gameboy-test-roms (default v7.0), unzipped at --roms.
  shootout  — the GBEmulatorShootout test set under suites/shootout/, graded
              with the shootout's own screenshot rule (`png_shootout`). The
              spec is extracted directly from the shootout checkout's Python
              test definitions (--shootout points at the repo).

Manifest line format:
  <id>|<dmg|cgb|agb>|<grading>|<rom_path>[|<arg>...]
grading: png | png_fixed | png_shootout | serial | blargg_mem | memauto | mem |
         mooneye | mooneye_ed
Trailing arg tokens: reference PNG path(s) (`;`-separated OR-match for
png_shootout), ADDR=VAL (mem), rev=<model>, input=<script>, frames=<N>.

Usage:
  tools/gen_manifests.py [--roms DIR] [--shootout DIR] [--out DIR]
                         [--only SUITE[,SUITE...]]
"""

import argparse
import math
import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent

# ---------------------------------------------------------------------------
# emission helpers
# ---------------------------------------------------------------------------


def write_manifest(out: Path, name: str, header: list[str], lines: list[str]) -> None:
    out.mkdir(parents=True, exist_ok=True)
    path = out / f"{name}.manifest"
    body = [f"# {h}" if h else "#" for h in header] + lines
    path.write_text("\n".join(body) + "\n")
    print(f"  {name}: {len(lines)} cases")


def png_dir_cases(label: str, rom: Path, refs: list[Path]) -> list[str]:
    """PNG mini-suite rule: ref suffix encodes the device (-dmg / -cgb /
    -cgb-dmg / bare); bare and -cgb-dmg run on both devices."""
    if not rom.is_file():
        return []
    stem = rom.stem
    lines = []
    for ref in refs:
        if not ref.is_file():
            continue
        rp = ref.stem
        if rp.endswith(("-cgb-dmg", "-dmg-cgb")):
            modes = ["dmg", "cgb"]
        elif rp.endswith("-dmg"):
            modes = ["dmg"]
        elif rp.endswith("-cgb"):
            modes = ["cgb"]
        else:
            modes = ["dmg", "cgb"]
        for m in modes:
            lines.append(f"{label}/{stem}|{m}|png|{rom}|{ref}")
    return lines


# ---------------------------------------------------------------------------
# internal suites (c-sp gameboy-test-roms layout)
# ---------------------------------------------------------------------------


def gen_acid2(roms: Path, out: Path) -> None:
    d, c = roms / "dmg-acid2", roms / "cgb-acid2"
    lines = []
    if (d / "dmg-acid2.gb").is_file():
        lines.append(f"dmg-acid2|dmg|png|{d}/dmg-acid2.gb|{d}/dmg-acid2-dmg.png")
        lines.append(f"dmg-acid2-on-cgb|cgb|png|{d}/dmg-acid2.gb|{d}/dmg-acid2-cgb.png")
    if (c / "cgb-acid2.gbc").is_file():
        lines.append(f"cgb-acid2|cgb|png|{c}/cgb-acid2.gbc|{c}/cgb-acid2.png")
    write_manifest(out, "acid2", ["dmg/cgb-acid2 PPU reference screens (c-sp). Run: --frames 60"], lines)


def gen_mealybug(roms: Path, out: Path) -> None:
    # ppu/*.gb only (dma/*.gb have no reference PNGs). DMG ref = <stem>_dmg_blob;
    # CGB ref = <stem>_cgb_c (CGB-CPU-04 ~ rev C). Exact-stem matching avoids the
    # m3_bgp_change vs m3_bgp_change_sprites prefix collision.
    mb = roms / "mealybug-tearoom-tests" / "ppu"
    lines = []
    if mb.is_dir():
        for rom in sorted(mb.glob("*.gb")):
            stem = rom.stem
            dmg = mb / f"{stem}_dmg_blob.png"
            cgb = mb / f"{stem}_cgb_c.png"
            if dmg.is_file():
                lines.append(f"mealybug/{stem}|dmg|png|{rom}|{dmg}")
            if cgb.is_file():
                lines.append(f"mealybug/{stem}|cgb|png|{rom}|{cgb}")
    write_manifest(out, "mealybug", ["mealybug-tearoom PPU mid-mode-3 reference screens. Run: --frames 60"], lines)


def gen_blargg(roms: Path, out: Path) -> None:
    # Aggregate ROMs, each with the oracle that ROM actually exposes:
    # serial (prints), blargg_mem (0xA000 protocol), png (screen + LD B,B),
    # png_fixed (LCD off after result screen; flat cycle budget).
    bl = roms / "blargg"
    lines: list[str] = []

    def emit(ident, rom, grading, modes, ref=None):
        rom = bl / rom
        if not rom.is_file():
            return
        for m in modes.split():
            if ref:
                lines.append(f"{ident}|{m}|{grading}|{rom}|{bl / ref}")
            else:
                lines.append(f"{ident}|{m}|{grading}|{rom}")

    emit("cpu_instrs", "cpu_instrs/cpu_instrs.gb", "serial", "dmg cgb")
    emit("instr_timing", "instr_timing/instr_timing.gb", "serial", "dmg cgb")
    emit("mem_timing", "mem_timing/mem_timing.gb", "serial", "dmg cgb")
    emit("mem_timing-2", "mem_timing-2/mem_timing.gb", "blargg_mem", "dmg cgb")
    emit("dmg_sound", "dmg_sound/dmg_sound.gb", "blargg_mem", "dmg")
    emit("cgb_sound", "cgb_sound/cgb_sound.gb", "blargg_mem", "cgb")
    emit("halt_bug", "halt_bug.gb", "png", "dmg cgb", "halt_bug-dmg-cgb.png")
    emit("oam_bug-dmg", "oam_bug/oam_bug.gb", "png_fixed", "dmg", "oam_bug/oam_bug-dmg.png")
    emit("oam_bug-cgb", "oam_bug/oam_bug.gb", "png_fixed", "cgb", "oam_bug/oam_bug-cgb.png")
    emit("interrupt_time", "interrupt_time/interrupt_time.gb", "png", "cgb", "interrupt_time/interrupt_time-cgb.png")
    write_manifest(out, "blargg", ["blargg test ROMs (best oracle per ROM). Run: --frames 4000"], lines)

    singles = []
    for sub, grading, mode in [
        ("cpu_instrs/individual", "serial", "dmg"),
        ("mem_timing/individual", "serial", "dmg"),
        ("mem_timing-2/rom_singles", "blargg_mem", "dmg"),
        ("dmg_sound/rom_singles", "blargg_mem", "dmg"),
        ("cgb_sound/rom_singles", "blargg_mem", "cgb"),
    ]:
        label = sub.split("/")[0]
        for rom in sorted((bl / sub).glob("*.gb")):
            singles.append(f"{label}/{rom.stem}|{mode}|{grading}|{rom}")
    write_manifest(out, "blargg_singles", ["blargg per-subtest singles. Run: --frames 2000"], singles)


def gen_gbmicrotest(roms: Path, out: Path) -> None:
    gm = roms / "gbmicrotest"
    lines = [f"{rom.stem}|dmg|memauto|{rom}" for rom in sorted(gm.glob("*.gb"))] if gm.is_dir() else []
    write_manifest(
        out,
        "gbmicrotest",
        [
            "gbmicrotest (DMG-CPU-08). FF82==0x01 pass. Run: --frames 60",
            "VERIFIED (src @gbmicrotest463eb6b, oracle: libgambatte externalRead FF80-82 @60 frames):",
            "480/513. The 33 remaining fails are NOT emulation bugs reachable under this grading:",
            " - 29 display-only testbenches (no test_finish/test_end in source; verdict is a raw",
            "   measured value looped to VRAM $8000, never FF82). FF82=0x64 = DMG power-on HRAM residue.",
            " - 2 DMA-clobber artifacts (400-dma, dma_basic): the ROM copies its DMA routine INTO",
            "   HRAM over FF80-82 and never writes a verdict; rustyboi == Gambatte byte-identical.",
            " - 2 hardware-only quirks where rustyboi == Gambatte (both fail identically):",
            "   halt_op_dupe_delay, stat_write_glitch_l154_d (needs GateBoy-level LCD-enable modeling).",
            "Run --frames 600 for is_if_set_during_ime0.",
        ],
        lines,
    )


MOONEYE_REV = {
    "boot_div2-S": "sgb", "boot_div-S": "sgb", "boot_hwio-S": "sgb",
    "boot_div-dmg0": "dmg0", "boot_hwio-dmg0": "dmg0", "boot_regs-dmg0": "dmg0",
    "boot_regs-mgb": "mgb",
    "boot_div-A": "agb", "boot_regs-A": "agb",
    "boot_div-cgb0": "cgb0",
    "unused_hwio-C": "cgb", "boot_div-cgbABCDE": "cgb", "boot_hwio-C": "cgb",
}


def mooneye_modes(stem: str) -> list[str]:
    # Device-suffix model rule: -dmg*/-mgb/-S/-GS -> DMG, -cgb*/-C/-A -> CGB,
    # -sgb/-sgb2 skipped (SGB rows are curated manually), no-suffix -> both.
    if stem.endswith(("-sgb", "-sgb2")):
        return []
    if stem.endswith(("-dmg0", "-dmgABC", "-dmgABCmgb", "-mgb", "-S", "-GS")):
        return ["dmg"]
    if stem.endswith(("-cgb0", "-cgbABCDE", "-cgb", "-C", "-A")):
        return ["cgb"]
    return ["dmg", "cgb"]


def gen_mooneye(roms: Path, out: Path) -> None:
    mn = roms / "mooneye-test-suite"
    lines = []
    if mn.is_dir():
        found = [
            p
            for sub in ("acceptance", "emulator-only", "misc", "madness")
            for p in mn.rglob("*.gb")
            if f"/{sub}/" in str(p)
        ]
        for rom in sorted(set(found)):
            stem = rom.stem
            rel = rom.relative_to(roms)
            rev = MOONEYE_REV.get(stem, "")
            for m in mooneye_modes(stem):
                lines.append(f"{rel}|{m}|mooneye|{rom}|rev={rev}" if rev else f"{rel}|{m}|mooneye|{rom}")
    write_manifest(
        out,
        "mooneye",
        [
            "mooneye-test-suite (Fibonacci magic registers). Mooneye uses an",
            "internal cycle cap; --frames is ignored. boot_* may need --real-bios.",
        ],
        lines,
    )


def wilbertpol_modes(stem: str) -> list[str]:
    # wilbertpol adds plain -dmg/-cgb/-G suffixes; -G = original Game Boy.
    if stem.endswith(("-sgb", "-sgb2")):
        return []
    if stem.endswith(("-dmg", "-dmg0", "-dmgABC", "-dmgABCmgb", "-mgb", "-S", "-GS", "-G")):
        return ["dmg"]
    if stem.endswith(("-cgb", "-cgb0", "-cgbABCDE", "-C", "-A")):
        return ["cgb"]
    return ["dmg", "cgb"]


def gen_wilbertpol(roms: Path, out: Path) -> None:
    wp = roms / "mooneye-test-suite-wilbertpol"
    lines = []
    if wp.is_dir():
        found = [
            p
            for sub in ("acceptance", "emulator-only", "misc", "madness")
            for p in wp.rglob("*.gb")
            if f"/{sub}/" in str(p)
        ]
        for rom in sorted(set(found)):
            rel = rom.relative_to(roms)
            for m in wilbertpol_modes(rom.stem):
                lines.append(f"{rel}|{m}|mooneye_ed|{rom}")
        sp = wp / "manual-only" / "sprite_priority.gb"
        for dev in ("dmg", "cgb"):
            ref = wp / "manual-only" / f"sprite_priority-{dev}.png"
            if sp.is_file() and ref.is_file():
                lines.append(f"mooneye-test-suite-wilbertpol/manual-only/sprite_priority|{dev}|png|{sp}|{ref}")
    write_manifest(out, "mooneye_wilbertpol", ["mooneye-test-suite-wilbertpol (0xED done-marker; grading mooneye_ed)."], lines)


# CPU-CGB-D/E-behavior rows (rev=cgbe): E-silicon expectations per the
# filename device list (see the CGBE revision gates in rustyboi-core).
AGE_REV_CGBE_STEMS = (
    "lcd-align-ly-cgbE", "ly-cgbE", "ly-ncmE", "oam-read-cgbE", "oam-read-ncmE",
    "spsw-interrupts-cgbE", "spsw-tima-cgbE", "stat-mode-cgbE", "stat-mode-ncmE",
    "m3-bg-bgp-ncmE",
)


def gen_age(roms: Path, out: Path) -> None:
    age = roms / "age-test-roms"
    lines = []
    if age.is_dir():
        # Register-graded .gb: dirs with no PNGs; device tokens name the modes.
        for rom in sorted(age.rglob("*.gb")):
            if any(rom.parent.glob("*.png")):
                continue
            stem = rom.stem
            modes = []
            if "dmg" in stem:
                modes.append("dmg")
            if "cgb" in stem or "ncm" in stem:
                modes.append("cgb")
            rel = rom.relative_to(roms)
            rev = "|rev=cgbe" if stem in AGE_REV_CGBE_STEMS else ""
            for m in modes:
                lines.append(f"{rel}|{m}|mooneye|{rom}{rev}")
        # Screenshot pass: for every PNG pick the longest-prefix ROM in its dir;
        # mode from the PNG's own device token (ncm = DMG cart on CGB -> cgb).
        for png in sorted(age.rglob("*.png")):
            pstem = png.stem
            best = None
            for cand in png.parent.glob("*.gb"):
                cstem = cand.stem
                if pstem == cstem or pstem.startswith(cstem + "-"):
                    if best is None or len(cstem) > len(best.stem):
                        best = cand
            if best is None:
                print(f"  WARN: no ROM for {png}", file=sys.stderr)
                continue
            mode = "dmg" if "dmg" in pstem else "cgb"
            ident = f"age-test-roms/{png.parent.name}/{pstem}"
            rev = "|rev=cgbe" if pstem in AGE_REV_CGBE_STEMS else ""
            lines.append(f"{ident}|{mode}|png|{best}|{png}{rev}")
    write_manifest(
        out,
        "age",
        [
            "age-test-roms (CGB timing; LD B,B + Fibonacci registers, else PNG).",
            "Register tests: grading mooneye (0x40). Screenshot tests: grading png.",
        ],
        lines,
    )


def gen_cgb_acid_hell(roms: Path, out: Path) -> None:
    cah = roms / "cgb-acid-hell"
    lines = []
    if (cah / "cgb-acid-hell.gbc").is_file() and (cah / "cgb-acid-hell.png").is_file():
        lines.append(f"cgb-acid-hell|cgb|png|{cah}/cgb-acid-hell.gbc|{cah}/cgb-acid-hell.png")
    write_manifest(out, "cgb_acid_hell", ["cgb-acid-hell (CGB PPU reference screen; docboy FAILS this). --frames 60"], lines)


def gen_scribbltests(roms: Path, out: Path) -> None:
    scr = roms / "scribbltests"
    lines = []
    if scr.is_dir():
        for sub in sorted(p for p in scr.iterdir() if p.is_dir()):
            for rom in sorted(sub.glob("*.gb")):
                stem = rom.stem
                refs = sorted(sub.glob(f"{stem}-*.png"))
                alt = stem.replace("-", "_")
                if alt != stem:
                    refs += sorted(sub.glob(f"{alt}-*.png"))
                lines += png_dir_cases("scribbltests", rom, refs)
    write_manifest(out, "scribbltests", ["scribbltests (PPU screenshots). statcount_auto needs ~270 frames."], lines)


def gen_turtle(roms: Path, out: Path) -> None:
    tur = roms / "turtle-tests"
    lines = []
    if tur.is_dir():
        for sub in sorted(p for p in tur.iterdir() if p.is_dir()):
            for rom in sorted(sub.glob("*.gb")):
                lines += png_dir_cases("turtle-tests", rom, [sub / f"{rom.stem}.png"])
    write_manifest(out, "turtle_tests", ["turtle-tests (window Y-trigger PPU screenshots). --frames 60"], lines)


TELLINGLYS_INPUT = (
    "input=30@5:A,45:-,60@21:B,75:-,90@42:Up,105:-,120@84:Down,135:-,"
    "150@105:Left,165:-,180@120:Right,195:-,210@135:Start,225:-,240@10:Select,255:-"
)


def gen_little_things(roms: Path, out: Path) -> None:
    ltg = roms / "little-things-gb"
    lines = []
    if ltg.is_dir():
        for ref in sorted(ltg.glob("firstwhite-*.png")):
            lines.append(f"little-things-gb/firstwhite|dmg|png_fixed|{ltg}/firstwhite.gb|{ref}")
            lines.append(f"little-things-gb/firstwhite|cgb|png_fixed|{ltg}/firstwhite.gb|{ref}")
        if (ltg / "tellinglys.gb").is_file():
            for dev in ("cgb", "dmg"):
                ref = ltg / f"tellinglys-{dev}.png"
                if ref.is_file():
                    lines.append(
                        f"little-things-gb/tellinglys|{dev}|png|{ltg}/tellinglys.gb|{ref}|{TELLINGLYS_INPUT}|frames=700"
                    )
    write_manifest(
        out,
        "little_things_gb",
        [
            "little-things-gb PPU screenshots. tellinglys uses scripted button input",
            "(input= token): its joypad-IRQ entropy check needs all 8 keys pressed at",
            "distinct LY positions (>=5 bits of LY spread), hence the @<ly> targets.",
            "firstwhite is input-less; it fails on the first-frame-after-LCD-enable",
            "display blanking (hardware shows white; PPU presentation gap, see notes).",
        ],
        lines,
    )


def gen_bully(roms: Path, out: Path) -> None:
    bly = roms / "bully"
    lines = []
    if (bly / "bully.gb").is_file() and (bly / "bully.png").is_file():
        lines.append(f"bully|dmg|png|{bly}/bully.gb|{bly}/bully.png")
        lines.append(f"bully|cgb|png|{bly}/bully.gb|{bly}/bully.png")
    write_manifest(out, "bully", ["bully (all-device conformance screen). bully.png is the CGB result."], lines)


def gen_strikethrough(roms: Path, out: Path) -> None:
    stk = roms / "strikethrough"
    lines = png_dir_cases("strikethrough", stk / "strikethrough.gb", sorted(stk.glob("strikethrough-*.png")))
    write_manifest(out, "strikethrough", ["strikethrough (PPU screenshot). --frames 60"], lines)


def gen_samesuite(roms: Path, out: Path) -> None:
    ss = roms / "same-suite"
    lines = []
    for sub in ("ppu", "dma", "interrupt"):
        d = ss / sub
        if d.is_dir():
            for rom in sorted(d.rglob("*.gb")):
                lines.append(f"{rom.relative_to(roms)}|cgb|mooneye|{rom}")
    lines.sort(key=lambda l: l.split("|")[3])
    write_manifest(out, "samesuite_nonapu", ["same-suite non-APU (ppu/dma/interrupt). grading mooneye (0x40)."], lines)

    sgb = []
    d = ss / "sgb"
    if d.is_dir():
        for rom in sorted(d.rglob("*.gb")):
            sgb.append(f"{rom.relative_to(roms)}|dmg|mooneye|{rom}|rev=sgb")
    write_manifest(
        out,
        "samesuite_sgb",
        [
            "same-suite sgb/ (Super Game Boy). High-level SGB (JOYP packet protocol +",
            "MLT_REQ command handling); constructed as Hardware::SGB via `rev=sgb`.",
        ],
        sgb,
    )


def gen_rtc3test(roms: Path, out: Path) -> None:
    rtc = roms / "rtc3test"
    lines = []
    if rtc.is_dir():
        specs = [
            ("basic", "rtc3test-basic-tests", "input=20:A,30:-", 850),
            ("range", "rtc3test-range-tests", "input=20:Down,30:-,40:A,50:-", 700),
            ("subsecond", "rtc3test-sub-second-writes", "input=20:Down,30:-,40:Down,50:-,60:A,70:-", 1750),
        ]
        for tag, refbase, script, frames in specs:
            for dev in ("cgb", "dmg"):
                ref = rtc / f"{refbase}-{dev}.png"
                if ref.is_file():
                    lines.append(f"rtc3test-{tag}/rtc3test|{dev}|png|{rtc}/rtc3test.gb|{ref}|{script}|frames={frames}")
    write_manifest(
        out,
        "rtc3test",
        [
            "rtc3test (MBC3 RTC). The ROM is an interactive menu: each sub-suite is",
            "selected with scripted button input (input= tokens; see parse_input_script).",
            "Sub-suite durations (emulated): basic 13s, range 8s, sub-second writes 26s;",
            "each row carries its own frames= budget (menu + run + settle).",
        ],
        lines,
    )


def gen_mbc3_tester(roms: Path, out: Path) -> None:
    mbc3 = roms / "mbc3-tester"
    lines = []
    if mbc3.is_dir():
        for dev in ("cgb", "dmg"):
            ref = mbc3 / f"mbc3-tester-{dev}.png"
            if ref.is_file():
                lines.append(f"mbc3-tester/mbc3-tester|{dev}|png|{mbc3}/mbc3-tester.gb|{ref}|frames=100")
    write_manifest(
        out,
        "mbc3_tester",
        [
            "mbc3-tester (MBC30 bank test; no input needed). The ROM loops its bank",
            "sweep forever, so the result screen is only stable ~frames 60-200:",
            "frames=100 pins the grading point (needs MBC30 for banks 0x80-0xFF).",
            "The CGB ref is a shipped-reference palette artifact: every differing",
            "pixel is our #7BFF31 (the c-sp-documented compat shade) vs its #7BFF4A.",
        ],
        lines,
    )



def gen_gambatte(roms: Path, out: Path) -> None:
    """Gambatte hwtests as a normal manifest. The ROMs are the c-sp prebuilt
    set (gb-test-roms/gambatte == gambatte-core/test/hwtests built); the oracle
    and modes are filename-encoded, so every row is `auto|gambatte` and expands
    in the manifest parser (`cases_for_rom`) exactly like the old --suite
    walker. Dumper oracles (`*_dmg08.bin`/`*_cgb.bin`/`*.dump`) are sibling
    files the c-sp set does not ship: sync them from a gambatte-core checkout
    when present."""
    gam = roms / "gambatte"
    lines = []
    if gam.is_dir():
        hw = HERE / "gambatte-core" / "test" / "hwtests"
        if hw.is_dir():
            import shutil

            for oracle in list(hw.rglob("*.bin")) + list(hw.rglob("*.dump")):
                dest = gam / oracle.relative_to(hw)
                if not dest.exists() and dest.parent.is_dir():
                    shutil.copy2(oracle, dest)
        for rom in sorted(gam.rglob("*.gb")) + sorted(gam.rglob("*.gbc")):
            lines.append(f"gambatte/{rom.relative_to(gam)}|auto|gambatte|{rom}")
        lines.sort(key=lambda l: l.split("|")[3])
    write_manifest(
        out,
        "gambatte",
        [
            "gambatte hwtests (filename-encoded oracles; mode auto expands per",
            "cases_for_rom). 16-failure floor = real-silicon dumper/hdma cases",
            "where rustyboi >= Gambatte; see .baselines/.",
        ],
        lines,
    )


# ---------------------------------------------------------------------------
# shootout suites (GBEmulatorShootout checkout)
# ---------------------------------------------------------------------------

# The shootout's `Emulator.runTest` polls for a screenshot match up to
# `runtime + startup_time(1.0) + 5.0` seconds and early-exits on a match, so
# its true deadline is runtime+6s; we run that full window as a frame budget.
SHOOTOUT_SLACK_S = 6.0
FRAME_FLOOR = 90

# E-silicon SameSuite rows (SameSuite is CGB-E-validated; these tests exercise
# the C-vs-D/E revision forks modeled behind Hardware::CGBE - see the CGBE
# gates in rustyboi-core).
SHOOTOUT_REV = {
    "samesuite/apu/channel_1/channel_1_align.gb": "cgbe",
    "samesuite/apu/channel_1/channel_1_align_cpu.gb": "cgbe",
    "samesuite/apu/channel_2/channel_2_align.gb": "cgbe",
    "samesuite/apu/channel_2/channel_2_align_cpu.gb": "cgbe",
    "samesuite/apu/channel_4/channel_4_align.gb": "cgbe",
    "samesuite/apu/channel_4/channel_4_freq_change.gb": "cgbe",
}


def shootout_frames(runtime_seconds: float) -> int:
    return max(math.ceil((runtime_seconds + SHOOTOUT_SLACK_S) * 60), FRAME_FLOOR)


def extract_shootout(shootout: Path) -> dict[str, list[dict]]:
    """Import the shootout's own test definitions with GUI deps stubbed."""
    import types

    for mod in ("pyautogui", "requests", "tqdm"):
        m = types.ModuleType(mod)
        if mod == "tqdm":
            m.tqdm = lambda *a, **k: None
        sys.modules.setdefault(mod, m)
    sys.path.insert(0, str(shootout))
    cwd = os.getcwd()
    os.chdir(shootout)  # Test.__init__ asserts on testroms/-relative paths
    try:
        import testroms.acid, testroms.ashiepaws, testroms.ax6, testroms.blargg
        import testroms.cpp, testroms.daid, testroms.mealybug, testroms.mooneye
        import testroms.samesuite

        suites = {
            "acid": testroms.acid.all, "blargg": testroms.blargg.all,
            "daid": testroms.daid.all, "ax6": testroms.ax6.all,
            "mooneye": testroms.mooneye.all, "samesuite": testroms.samesuite.all,
            "ashiepaws": testroms.ashiepaws.all, "cpp": testroms.cpp.all,
            "mealybug": testroms.mealybug.all,
        }
        out = {}
        for suite, tests in suites.items():
            rows = []
            for t in tests:
                refs = t.pass_result_filename
                refs = refs if isinstance(refs, list) else [refs]
                rows.append(
                    dict(
                        name=str(t.name),
                        rom=os.path.abspath(t.rom),
                        model=t.model,
                        runtime=t.runtime,
                        pass_refs=[os.path.abspath(r) for r in refs],
                    )
                )
            out[suite] = rows
        return out
    finally:
        os.chdir(cwd)
        sys.path.remove(str(shootout))


def gen_shootout(shootout: Path, out: Path) -> None:
    # rustyboi SGB rows are curated manually (sgb.manifest); the shootout SGB
    # tests are skipped here. Tests whose pass ref does not exist are INFO-only
    # in the shootout (not scored) and skipped.
    model_map = {"DMG": "dmg", "CGB": "cgb"}
    data = extract_shootout(shootout)
    out.mkdir(parents=True, exist_ok=True)
    grand = 0
    for suite, tests in data.items():
        lines = []
        for t in tests:
            mode = model_map.get(t["model"])
            if mode is None:
                continue
            refs = [r for r in t["pass_refs"] if os.path.exists(r)]
            if not os.path.exists(t["rom"]) or not refs:
                continue
            ident = t["name"].replace("|", "_")
            rom = os.path.relpath(t["rom"], HERE)
            refs_field = ";".join(os.path.relpath(r, HERE) for r in refs)
            rev = f"|rev={SHOOTOUT_REV[ident]}" if ident in SHOOTOUT_REV else ""
            lines.append(
                f"{ident}|{mode}|png_shootout|{rom}|{refs_field}|frames={shootout_frames(t['runtime'])}{rev}"
            )
        (out / f"{suite}.manifest").write_text("\n".join(lines) + ("\n" if lines else ""))
        print(f"  shootout/{suite}: {len(lines)} cases")
        grand += len(lines)
    print(f"  shootout total: {grand}")


# ---------------------------------------------------------------------------

INTERNAL = {
    "acid2": gen_acid2,
    "mealybug": gen_mealybug,
    "blargg": gen_blargg,
    "gbmicrotest": gen_gbmicrotest,
    "mooneye": gen_mooneye,
    "mooneye_wilbertpol": gen_wilbertpol,
    "age": gen_age,
    "cgb_acid_hell": gen_cgb_acid_hell,
    "scribbltests": gen_scribbltests,
    "turtle_tests": gen_turtle,
    "little_things_gb": gen_little_things,
    "bully": gen_bully,
    "strikethrough": gen_strikethrough,
    "samesuite": gen_samesuite,
    "rtc3test": gen_rtc3test,
    "mbc3_tester": gen_mbc3_tester,
    "gambatte": gen_gambatte,
}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--roms", type=Path, default=Path(os.environ.get("ROMS", "gb-test-roms")))
    ap.add_argument("--shootout", type=Path, default=HERE.parent / "GBEmulatorShootout")
    ap.add_argument("--out", type=Path, default=HERE / "rustyboi-test-runner" / "suites")
    ap.add_argument("--only", help="comma-separated suite names (internal names or 'shootout')")
    args = ap.parse_args()

    only = set(args.only.split(",")) if args.only else None
    roms = args.roms
    if not roms.is_dir():
        print(f"error: ROM set not found at {roms} (use --roms)", file=sys.stderr)
        return 1
    print(f"ROMs:    {roms}")
    print(f"Output:  {args.out}")

    for name, fn in INTERNAL.items():
        if only is None or name in only:
            fn(roms, args.out)
    if only is None or "shootout" in only:
        if args.shootout.is_dir():
            gen_shootout(args.shootout.resolve(), args.out / "shootout")
        else:
            print(f"  (shootout checkout not found at {args.shootout}; skipped)")
    print("done.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
