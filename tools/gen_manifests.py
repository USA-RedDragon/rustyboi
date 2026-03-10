#!/usr/bin/env python3
"""Regenerate every manifest consumed by `rustyboi-test-runner --manifest`.

Generates the c-sp public-suite manifests under rustyboi-test-runner/suites/
(acid2, mealybug, blargg, gbmicrotest, mooneye, wilbertpol, age, cgb-acid-hell,
scribbltests, turtle-tests, little-things-gb, bully, strikethrough, same-suite,
rtc3test, mbc3-tester). ROM set: c-sp/gameboy-test-roms (default v7.0), unzipped
at --roms. The sgb, daid and cpp suites are curated manually (their ROMs are not
in the c-sp set; they are sourced from GBEmulatorShootout by run-suites.sh setup)
and are not regenerated here.

Manifest line format:
  <id>|<dmg|cgb|agb>|<grading>|<rom_path>[|<arg>...]
grading: png | png_fixed | png_shootout | serial | blargg_mem | memauto | mem |
         mooneye | mooneye_ed
Trailing arg tokens: reference PNG path(s) (`;`-separated OR-match for
png_shootout), ADDR=VAL (mem), rev=<model>, input=<script>, frames=<N>.

Usage:
  tools/gen_manifests.py [--roms DIR] [--out DIR]
                         [--only SUITE[,SUITE...]]
"""

import argparse
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
    # A few cases settle their FF82 verdict later than the 60-frame default.
    gbmicro_frames = {"is_if_set_during_ime0": 600}
    # A handful of ROMs report their verdict to VRAM $8000 instead of FF82 (they
    # predate / skip the test_finish macro). Each has ONE disassembly-justified
    # hardware-correct byte, so grade them via `mem` at $8000 = that byte:
    #  poweron              APU NR10 read-mask: write $00, read back -> $00|0x80.
    #                       Mask 0x80 is the in-source SameBoy read_mask table.
    #  004-tima_boot_phase  4 TIMA adds; `add $55 - PASS`(PASS=10 DMG) lands the
    #                       correct boot-phase sum on $55.
    #  004-tima_cycle_timer 4 TIMA adds; `sub OVERHEAD`(9) calibrates correct to $55.
    #  ppu_spritex_vs_scx   real self-check (176 `cp result/jp nz,fail`): writes
    #                       $55 to $8000 on all-pass, $FF on any fail.
    gbmicro_vram_verdict = {
        "poweron": ("8000", 0x80),
        "004-tima_boot_phase": ("8000", 0x55),
        "004-tima_cycle_timer": ("8000", 0x55),
        "ppu_spritex_vs_scx": ("8000", 0x55),
        # 2026-07-05 re-dig (see header): 9 more display-only ROMs recovered with
        # disassembly + real-DMG-hardware-justified bytes (never emulator output).
        "000-oam_lock": ("8000", 0xFF),
        "000-write_to_x8000": ("8000", 0x55),
        "002-vram_locked": ("8000", 0x84),
        "007-lcd_on_stat": ("8000", 0x00),
        "cpu_bus_1": ("FF80", 0x55),
        "flood_vram": ("8000", 0xFF),
        "lcdon_write_timing": ("8000", 0x00),
        "ly_while_lcd_off": ("8000", 0x00),
        "mode2_stat_int_to_oam_unlock": ("8000", 0xFF),
        # 2026-07-05 deep-dig, wave 2 (see header): 14 more no-verdict ROMs
        # recovered with disassembly + hardware-documentation-justified bytes
        # (never emulator output); 400-dma is the 15th via `oamdump` below.
        "001-vram_unlocked": ("8000", 0x55),
        "800-ppu-latch-scx": ("8002", 0x7E),
        "801-ppu-latch-scy": ("8002", 0x7E),
        "802-ppu-latch-tileselect": ("8002", 0x7E),
        "803-ppu-latch-bgdisplay": ("8002", 0x7E),
        "audio_testbench": ("FF26", 0xF9),
        "dma_basic": ("FE9F", 0xFF),
        "oam_sprite_trashing": ("8000", 0xFF),
        "ppu_scx_vs_bgp": ("8000", 0x80),
        "ppu_sprite_testbench": ("8010", 0xFF),
        "ppu_win_vs_wx": ("8010", 0xFF),
        "ppu_wx_early": ("8001", 0xFF),
        "toggle_lcdc": ("FF40", 0x00),
        "wave_write_to_0xC003": ("C003", 0x55),
    }

    # 400-dma's oracle is a 160-byte OAM region dump == the ROM's own DMA
    # source block at $0200-$029F (documented OAM-DMA semantics copy it 1:1
    # from a ROM source regardless of PPU phase, and nothing writes OAM
    # afterwards). Extract the reference bytes FROM THE CART IMAGE here so the
    # provenance is executable and never an emulator capture.
    dma_ref = out / "refs" / "400-dma.oam.dump"
    dma_rom = gm / "400-dma.gb"
    if dma_rom.is_file():
        dma_ref.parent.mkdir(parents=True, exist_ok=True)
        dma_ref.write_bytes(dma_rom.read_bytes()[0x200:0x2A0])
    # Manifest lines use repo-root-relative paths (the runner and CI run from
    # the repo root), matching the relative `gb-test-roms/...` ROM paths.
    try:
        dma_ref_line = dma_ref.resolve().relative_to(Path.cwd())
    except ValueError:
        dma_ref_line = dma_ref

    def line_for(rom: Path) -> str:
        stem = rom.stem
        if stem == "400-dma":
            return f"{stem}|dmg|oamdump|{rom}|{dma_ref_line}"
        if stem in gbmicro_vram_verdict:
            addr, val = gbmicro_vram_verdict[stem]
            return f"{stem}|dmg|mem|{rom}|{addr}={val:02X}"
        return f"{stem}|dmg|memauto|{rom}" + (
            f"|frames={gbmicro_frames[stem]}" if stem in gbmicro_frames else ""
        )

    lines = (
        [line_for(rom) for rom in sorted(gm.glob("*.gb"))] if gm.is_dir() else []
    )
    write_manifest(
        out,
        "gbmicrotest",
        [
            'gbmicrotest (DMG-CPU-08). FF82==0x01 pass. Run: --frames 60',
            'VERIFIED (src @gbmicrotest463eb6b, oracle: libgambatte externalRead FF80-82 @60 frames):',
            '509/513. 28 ROMs never call any test_end/test_finish macro (verified against',
            'the source AND the shipped binaries), so they never write an FF82 verdict;',
            'they are graded via `mem <addr>=VAL` / `oamdump` with bytes justified from',
            'disassembly + real DMG-CPU-08 hardware documentation (NOT from any emulator',
            'output). First 13 (landed earlier):',
            '  poweron=80 (NR10 read-mask), 004-tima_boot_phase/004-tima_cycle_timer=55',
            '  (calibrated-add pass byte), ppu_spritex_vs_scx=55 (self-check),',
            '  000-write_to_x8000=55 / flood_vram=FF (constant VRAM fill -- VRAM-writable),',
            '  cpu_bus_1=FF80:55 (constant HRAM write), 007-lcd_on_stat=00 / ly_while_lcd_off=00',
            '  (LY reads 0 after LCD-on / while LCD off, Pan Docs), 002-vram_locked=84',
            "  (STAT=10000100 at line-0 HBlank start, author's DMG note),",
            '  000-oam_lock=FF / mode2_stat_int_to_oam_unlock=FF (locked-OAM CPU read = $FF),',
            '  lcdon_write_timing=00 (OAM write dropped mid-render -> reads cleared $00).',
            'The OAM/LY/STAT timing behind the last five is independently validated by the',
            'passing oam_read_*/oam_write_*/lcdon_to_*/stat_* verdict tests (same engine).',
            '15 more recovered on the 2026-07-05 deep-dig (the earlier "no $8000 write at',
            'all" claim was WRONG for 9 of them -- the shipped binaries DO plant',
            'deterministic bytes; every write target below was re-verified in the .gb',
            'images, not just the .s sources):',
            ' - 001-vram_unlocked=55 (discriminating; see its paragraph below),',
            " - 400-dma (`oamdump` == the ROM's own $0200-$029F DMA source block,",
            '   extracted from the cart image by gen_manifests.py itself),',
            ' - dma_basic=FE9F:FF (VRAM-source DMA completion byte, clean-read in HBlank),',
            ' - audio_testbench=FF26:F9 (NR52 readback: ch4 undying + boot-ch1 active),',
            ' - 800/801/802/803-ppu-latch=8002:7E (load_box VBlank write; the latch',
            "   subject stays screen-only -- author's DELAY tables quoted below),",
            ' - oam_sprite_trashing=8000:FF, ppu_scx_vs_bgp=8000:80, ppu_wx_early=8001:FF,',
            '   ppu_win_vs_wx/ppu_sprite_testbench=8010:FF (LCD-off tile loads, never',
            '   rewritten; the swept display subject stays screen-only),',
            ' - toggle_lcdc=FF40:00 (final LCDC readback, fully R/W per Pan Docs),',
            ' - wave_write_to_0xC003=C003:55 (constant WRAM write, cpu_bus_1 class).',
            'The 4 remaining fails have NO hardware-derivable oracle (proven per-test):',
            ' - 500-scx-timing / minimal (byte-identical ROMs, cmp-verified): dual-halt',
            '   TIMA measure of mode-3 length -> $8000 (FF80-82 never written). Intended',
            '   control flow confirmed by patched-ROM probes (halt1 wakes at line-1',
            '   mode-2 start LY=1/STAT=A2, halt2 at line-1 HBlank STAT=88). The author',
            '   hardware rows are RELATIVE-only: DMG "overhead 65; 0 1 1 1 1 2 2 2" and',
            '   AGS "70; 0 0 0 1 1 1 1 2" for SCROLL=0..7 -- both delta patterns equal',
            '   (scx + ((-scx-e) mod 4))/4, the M-grid quantization of the HBlank IF',
            '   edge, with e=0 (DMG) / e=2 (AGS boot-phase offset), and a SCROLL=0..7',
            '   patched-ROM sweep reproduces all 8 DMG deltas ($4A..$4C). But no',
            '   ABSOLUTE byte is documented anywhere: "overhead 65" decodes to no',
            '   consistent absolute, GateBoy/MetroBoy skip this ROM (their harnesses',
            '   are FF80==FF81&&FF82-only -- verified in both repos), and a hand',
            '   derivation from Pan Docs/gb-ctr alone could not reproduce the byte (it',
            '   stacks >=6 sub-M-cycle constants: halt dispatch latency, mode-0 IF-edge',
            "   dot, IO-write cycle placement, DIV-write TIMA tick, tick-vs-read races).",
            "   Grading the emulator's $4A would be oracle-gaming. The same physics IS",
            '   hardware-graded by the 40+ passing hblank_int_scx0-7/_if_a-d/_nops',
            '   FF82-verdict tests.',
            ' - temp: a lone `nop` dev stub. PC slides through the zero-filled ROM into',
            '   VRAM-as-code and collapses into an RST38 loop whose pushes walk SP down',
            '   through IO/OAM/WRAM (C000 reads back a pushed return-PC byte in',
            '   practice). On real hardware the trajectory additionally depends on the',
            '   boot-logo bytes in VRAM ($8010+, executed as opcodes) and on which',
            '   fetches hit mode-3 lock ($FF = RST38), so no cell is hardware-derivable.',
            ' - halt_op_dupe_delay: hardware-only glitch (needs the set_stat_int_hblank async',
            '   STAT-IF clear); SameBoy built from source also produces 0x01 and FAILS',
            '   identically. rustyboi 0x01 is cycle-accurate.',
            'Run --frames 600 for is_if_set_during_ime0.',
            '000-oam_lock: reads OAM ($FE00) at DELAY=69 then puts it on VRAM ($8000);',
            'never writes FF82. At 69 cycles OAM is still locked (mode 2/3), so a DMG CPU',
            'read returns $FF (Pan Docs: locked-OAM reads are $FF). Author\'s DMG note "69 -',
            'black" (= all-set pixels). rustyboi\'s OAM-lock timing is validated by the 11',
            'passing oam_read_* / oam_write_* verdict tests.',
            '000-write_to_x8000: `ld a,$55 / ld ($8000),a` loop -- constant $55 to VRAM.',
            'The verdict IS the VRAM cell holding the written value (VRAM-writable sanity).',
            '001-vram_unlocked: the per-line STAT-OAM ISR writes $00 then, DELAY nops',
            'later, $55 to $8000 ("when does VRAM lock, relative to the OAM int?").',
            'Author DMG notes: "3 - dots / 4 - no dots"; the shipped binary is DELAY=3',
            '($48: 3E 00 EA 00 80 00 00 00 3E 55 EA 00 80 D9). At DELAY=3 the $55 write',
            'LANDS on real hardware (the dots ARE the $55 bitplane); at 4 it is mode-3',
            'dropped and $8000 stays $00 all frame. So the final cell DOES discriminate',
            '(the earlier "unconditionally $55" claim missed that the $55 write itself',
            'races the lock): an emulator locking >=1 M-cycle early reads $00. One-sided',
            "but hardware-grounded; the runner's flat-budget stop lands in the mode-1",
            'tail (LY=0 STAT=A5, ~10 lines after the last ISR), far from the transient.',
            '002-vram_locked: reads STAT ($41) at DELAY=71 -> VRAM. At line-0 HBlank start',
            '(cycle 71) mode=0 (HBlank), LYC=LY coincidence (LYC=0=LY) -> STAT bit7=1,',
            'bit2=1, mode=00 = 0x84. Matches the author\'s DMG note "71 - stat 10000100".',
            '007-lcd_on_stat: VBlank ISR turns LCD off then on, then reads LY ($44) -> VRAM.',
            'Toggling LCDC bit7 off resets the PPU; LY is 0 immediately after re-enable ->',
            '$00. (First-principles: LY=0 at the start of frame after LCD-on.)',
            'cpu_bus_1: `ld hl,$FF80 / ld a,$55 / ld (hl),a / jr -` -- writes $55 to HRAM',
            'FF80 in a loop. The verdict is the HRAM cell holding the written value',
            '(CPU->HRAM bus sanity). rustyboi holds FF80=$55.',
            'flood_vram: fills $8000-9FFF with $FF (`ld a,$FF / ld (hl+),a` until bit5 of B).',
            'The verdict is VRAM holding the flooded value; $8000=$FF.',
            'lcdon_write_timing: after LCD-on, clears OAM($FE00)=0, waits DELAY=132, writes',
            '$91 to $FE00, waits for VBlank, reads $FE00 -> VRAM. At DELAY=132 (mid-render)',
            'OAM is locked so the write is dropped and the read returns the cleared $00.',
            'Same validated OAM-write-lock engine as the 11 passing oam_write_* verdicts.',
            'ly_while_lcd_off: clears LCDC (LCD off), reads LY ($44) -> VRAM. While the LCD',
            'is disabled LY reads 0 (Pan Docs) -> $00.',
            'mode2_stat_int_to_oam_unlock: clears OAM($FE00)=0, arms the mode-2 STAT int,',
            'HALTs; the ISR waits DELAY=54 then reads $FE00 -> VRAM. At 54 cycles into the',
            'OAM-int the OAM is still locked, so the DMG read returns $FF (author note "54 -',
            'black"). Same validated OAM-lock engine as the passing oam_read_* verdicts.',
            '400-dma: copies its DMA routine into HRAM (clobbering FF80-82 with E0 46 00;',
            'no FF82 verdict is ever written), runs OAM DMA from ROM $0200, then idles',
            'forever. Documented DMA semantics (Pan Docs / gekkio gb-ctr: 160 bytes',
            'XX00-XX9F -> FE00-FE9F; DMA owns the OAM bus; a ROM source shares no bus',
            'with the PPU) leave OAM == ROM[$0200..$029F] regardless of PPU phase, and',
            'nothing writes OAM afterwards. gen_manifests.py extracts the 160 reference',
            'bytes from the cart image into refs/400-dma.oam.dump (disassembly-derived,',
            'NOT an emulator capture).',
            'dma_basic: pokes $8000=$FF and $809F=$FF (both land pre-lock: writes at',
            '~M11/M15 after the $0100 handoff; mode 3 first begins at ~M35 per the',
            'poweron_stat_000 hardware timeline, whose FF82-verdict siblings all pass',
            'here), copies E0 46 18 FE to FF80-83 (clobbering the verdict cells) and',
            'runs OAM DMA from $8000 with the LCD ON, looping in HRAM forever. The',
            "transfer's middle overlaps mode 3 (VRAM-source/PPU contention is",
            'undocumented) and $8010-$809E hold boot-logo tiles on real hardware, so',
            'the bulk of OAM is NOT gradeable -- but byte 159 (source $809F, the ROM\'s',
            'own $FF) is read at ~M205, inside line-1 HBlank (M192-242 on the same',
            'hardware timeline, >=13 M margin): an uncontended, documented VRAM read.',
            'FE9F==$FF asserts the 160-byte VRAM-source DMA ran to completion with its',
            'final byte intact.',
            'audio_testbench: sets NR50/NR51/NR52 then triggers ch4 with NR42=$F0 (DAC',
            'on, volume F, envelope period 0 -> no decay), NR43=$5F, NR44=$87 (trigger,',
            'length DISABLED) and spins at `jr -2`. On hardware NR52 then reads $F9',
            'forever: bit7 power, bits 6-4 unused (read 1), bit3 ch4 active (nothing',
            'can stop it: length off, no envelope disable path, DAC on), bit0 ch1 still',
            'active from the boot chime (envelope reaches volume 0 but a channel only',
            'disables via length expiry / DAC off / power off -- Pan Docs NR52 +',
            '"obscure behavior"). The graded byte is that live APU readback -- the',
            "test's actual subject (the noise channel IS running).",
            '800/801/802/803-ppu-latch-*: mid-line SCX/SCY/LCDC pokes from a per-line',
            'STAT-OAM ISR; the swept latch subject is screen-only (author DMG tables in',
            'tests/*.s: scx "5/6/7 no scroll, 8/9/10 scroll" DELAY=8; scy "9 - two',
            'scrolled columns" DELAY=9) and never reaches memory. Deterministic on',
            'hardware instead: load_box writes $7E to $8002-$800D during VBlank',
            '(binary: 3E 7E 21 02 80 22x12, before interrupts are enabled) and the',
            'ISRs only touch FF42/FF43/FF40 -- Pan Docs: VRAM is CPU-writable in mode',
            '1, so $8002 reads $7E. Graded on that cell; the latch timing itself',
            'remains ungraded.',
            'oam_sprite_trashing: writes OAM[0].y=70 at DELAY=7 nops into every visible',
            'line. The author DMG table records the on-screen effect only ("7 - right',
            'square (sprite 39) missing", "8 - square moved", 9/10 no move); the OAM',
            'content left behind by a mode-2/3 CPU write is nowhere documented',
            "(gekkio's OAM-corruption research covers 16-bit inc/dec patterns, not",
            'write landings), so FE00 itself is not gradeable. Deterministic instead:',
            'the $FF fill of $8000-$800F written with the LCD OFF (binary: 21 00 80 3E',
            'FF 22x16) and never touched again (the per-line pokes go to FE00) ->',
            '$8000=$FF (LCD-off VRAM writes always land, Pan Docs).',
            'ppu_scx_vs_bgp: per-line mid-render SCX pokes (author header: "cycles 21 &',
            '22 are weird"); the swept behavior is screen-only. The tile-0 load to',
            '$8000-$800F (first byte %10000000=$80, from ROM $02AC) happens with the',
            'LCD OFF and is never rewritten -> $8000=$80 on hardware.',
            'ppu_sprite_testbench: static sprite scaffold; the shipped build\'s per-line',
            '"mess with the line" loops are pure nops, so the display is the only swept',
            'output. tile 1 (all-$FF, ROM $0307) is loaded to $8010-$801F with the LCD',
            'OFF and never rewritten -> $8010=$FF.',
            'ppu_win_vs_wx: per-line WX + LCDC-window-map toggles; screen-only subject.',
            'tile_black (all-$FF, ROM $02E5) is loaded to $8010-$801F while LCDC bit7=0',
            '(both pre-enable LCDC writes keep the LCD off) and never rewritten ->',
            '$8010=$FF.',
            'ppu_wx_early: per-line WX 30->200->30 toggles; screen-only subject. tile1',
            '(alternating 00/FF rows, ROM $02D1) is loaded to $8000-$800F with the LCD',
            'OFF and never rewritten -> $8001=$FF.',
            'toggle_lcdc: sixteen back-to-back LCDC $80/$00 writes ending OFF, then',
            '`jr` self (binary: last write 3E 00 EA 40 FF at $019B, tail 18 FE at',
            '$01A0). The rapid-toggle rendering effects are screen-only; the',
            'deterministic hardware state is the final LCDC readback -- FF40 is fully',
            'R/W (Pan Docs) -> $00.',
            'wave_write_to_0xC003: shipped build is `ld a,$55 / ld hl,$C003 / ld',
            '(hl),a / jr -3` -- a constant WRAM write loop (the wave-RAM probe in the',
            'source is commented out). The verdict is the WRAM cell holding the written',
            'value, same class as the accepted cpu_bus_1 (FF80=55).',
            'minimal: byte-identical to 500-scx-timing.gb (cmp-verified) -- same closure.',
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


# madness/mgb_oam_dma_halt_sprites is NOT a register test: it starts OAM DMA at
# VBlank then HALTs forever with no interrupts, so no done-marker is ever reached
# and the Fibonacci `mooneye`/`mooneye_ed` oracle can never apply. It is a
# screenshot test (`*_expected.png` ships beside the ROM), and per Gekkio's own
# source (madness/mgb_oam_dma_halt_sprites.s) the rendered sprite is MGB-specific:
# "MGB: as visualized by *_expected.png; DMG: a different sprite; CGB: no sprite".
# Verified against SameBoy (DMG=746 differing px, CGB=0 sprite px, neither the
# 18-px MGB glyph). So the only valid grading is a single MGB run (dmg mode +
# rev=mgb) against the MGB reference, up to a consistent recoloring (png_layout):
# the reference is grayscale but rustyboi renders the DMG-compat palette. This
# stays a fail — the underlying "magic value" DMA/HALT merge quirk is undocumented
# ("Why & $FC? I have no idea" -- Gekkio) and unmodeled by SameBoy too, so pixel-
# matching it would be test-fitting one silicon capture. Emitting the honest MGB
# png_layout row (vs the old dmg+cgb register rows) makes the failure truthful.
def mgb_oam_dma_halt_row(rel: Path, rom: Path) -> str:
    ref = rom.with_name(f"{rom.stem}_expected.png")
    return f"{rel}|dmg|png_layout|{rom}|{ref}|rev=mgb"


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
            if stem == "mgb_oam_dma_halt_sprites":
                lines.append(mgb_oam_dma_halt_row(rel, rom))
                continue
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
            # The per-silicon boot tests (boot_regs-A/-mgb, boot_hwio-S/-C,
            # unused_hwio-C) share the mooneye ROMs and target a specific
            # revision's post-boot state, so they carry the same rev= token as in
            # the mooneye manifest. Without it they run on the plain dmg/cgb model
            # and the revision-specific register/HWIO table never matches.
            if rom.stem == "mgb_oam_dma_halt_sprites":
                # Screenshot/MGB-only (see mgb_oam_dma_halt_row); the ROM halts
                # forever, so the mooneye_ed done-marker never fires either.
                lines.append(mgb_oam_dma_halt_row(rel, rom))
                continue
            rev = MOONEYE_REV.get(rom.stem, "")
            for m in wilbertpol_modes(rom.stem):
                lines.append(
                    f"{rel}|{m}|mooneye_ed|{rom}|rev={rev}" if rev else f"{rel}|{m}|mooneye_ed|{rom}"
                )
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


# Mirror AGE's own runner exclusions (c-sp/age: build/test-blacklist.txt). AGE
# blacklists these paths from its own suite, so their references are not reliable
# targets: _in-progress is unfinished, and oam-write-dmgC / speed-switch/caution
# capture Nintendo-undefined post-STOP / metastable real-silicon behavior the AGE
# author declines to hold his own emulator to. We exclude the same paths rather
# than gate on a ref its own maker disowns. Paths are relative to age-test-roms/.
AGE_BLACKLIST = (
    "_in-progress",
    "oam/oam-write-dmgC",
    "speed-switch/caution",
)


def _age_blacklisted(rel_age: Path) -> bool:
    s = rel_age.as_posix()
    return any(s == b or s.startswith(b + "/") or s.startswith(b + ".") for b in AGE_BLACKLIST)


def gen_age(roms: Path, out: Path) -> None:
    age = roms / "age-test-roms"
    lines = []
    if age.is_dir():
        # Register-graded .gb: dirs with no PNGs; device tokens name the modes.
        for rom in sorted(age.rglob("*.gb")):
            if _age_blacklisted(rom.relative_to(age)):
                continue
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
            if _age_blacklisted(png.relative_to(age)):
                continue
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
            "Excludes AGE's own blacklist (build/test-blacklist.txt): _in-progress,",
            "oam/oam-write-dmgC, speed-switch/caution -- refs AGE itself disowns.",
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
    # scxly-cgb: the CGB ref is the DMG layout recolored to DMG-green (a capture-
    # emulator artifact for a DMG-compat cart); rustyboi uses the hardware-correct
    # CGB compat palette. The SCX/LY LAYOUT is what the test measures -> grade it
    # up to a consistent recoloring (png_layout). The DMG ref stays exact.
    lines = [
        ln.replace("scribbltests/scxly|cgb|png|", "scribbltests/scxly|cgb|png_layout|")
        for ln in lines
    ]
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
                # The CGB ref's compat green (#7BFF4A) is a capture-emulator
                # shade; rustyboi renders the boot-ROM-correct #7BFF31. The
                # bank-sweep checkbox LAYOUT is what the test checks -> grade it
                # up to a consistent recoloring (png_layout).
                grading = "png_layout" if dev == "cgb" else "png"
                lines.append(
                    f"mbc3-tester/mbc3-tester|{dev}|{grading}|{mbc3}/mbc3-tester.gb|{ref}|frames=100"
                )
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
    ap.add_argument("--out", type=Path, default=HERE / "rustyboi-test-runner" / "suites")
    ap.add_argument("--only", help="comma-separated suite names")
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
    print("done.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
