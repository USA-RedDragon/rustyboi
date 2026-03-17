# Implementation Gaps — Completeness Audit vs Pan Docs / gb-ctr

Audit date: 2026-07-06, tree `e70c5a9`. Method: every Pan Docs section (78-entry
TOC, walked from the [pandocs source repo](https://github.com/gbdev/pandocs))
plus gekkio's [gb-ctr](https://gekkio.fi/files/gb-ctr/gb-ctr.pdf) chapter list
was checked against the actual rustyboi source; uncertain items were verified
empirically (build + purpose-built micro-ROMs, marked **[R]** like
`KNOWN_FAILURES.md`). This report covers *breadth* gaps — documented GB/GBC
behaviors that are missing, partial, or wrong. The *depth/accuracy* frontier
(mid-mode-3 timing floors, sub-dot STOP counters, session-flicker captures) is
already exhaustively tracked in `KNOWN_FAILURES.md` and is only summarized
here, not re-litigated.

**Headline:** the emulated register-level DMG/CGB machine is essentially
complete and world-class (6448/6465 across 26 suites; all 17 failures are
proven floors or in-flight open targets). The real gaps are on the *periphery*:
exotic cartridge mappers, cartridge peripherals (camera/printer/IR/link),
the SGB visual layer beyond palettes, plain-STOP low-power mode, and
persistence plumbing (RTC across sessions, rumble output). Trigger for this
audit: discovery that only MBC1/2/3/5 are implemented — confirmed, plus a
longer tail documented below.

Status legend: **MISSING** (no code), **PARTIAL** (some behavior modeled),
**WRONG** (behaves contrary to documentation), **IN-PROGRESS** (another agent
is landing it now — do not re-start), **COMPLETE** (verified done, listed only
in the section-disposition table).

---

## 1. The owner's collection — what is actually affected

Scanned **1148 ROMs** (543 zips in `~/Downloads/gb/GBC/`, 605 in
`~/Downloads/gb/GB/`), headers parsed straight from the zips in memory
(cartridge type `$0147`, CGB flag `$0143`, SGB flag `$0146`, old licensee
`$014B`). Zero parse errors.

### Mapper histogram

| Mapper family ($0147) | Count | rustyboi status |
|---|---|---|
| MBC1 ($01/$02/$03) | 554 | supported (incl. MBC1M multicart detection) |
| MBC5 ($19/$1A/$1B) | 474 | supported |
| ROM only ($00) | 49 | supported (but see Sachen/Wisdom Tree liars below) |
| MBC2 ($06) | 20 | supported |
| MBC3 ($10/$11/$13) | 20 (13 with RTC) | supported incl. MBC30; **RTC state lost between sessions** (§2.2) |
| MBC5+RUMBLE ($1C/$1E) | 16 | banking supported; **rumble motor not wired to any frontend** (§2.3) |
| Rocket Games ($97/$99, unlicensed) | 10 | **MISSING** |
| POCKET CAMERA ($FC) | 1 | **MISSING** |
| Makon/Ka Sheng ($EA, unlicensed) | 1 | **MISSING** |
| MBC7 ($22) | 1 | **IN-PROGRESS** |
| HuC3 ($FE) | 1 | **IN-PROGRESS** |
| HuC1 ($FF) | 1 | **MISSING** |

CGB flag: 344 CGB-only, 185 CGB-compatible, 619 DMG-only.
SGB-flagged (`$0146=$03` + `$014B=$33`): **185 games** — every one of them
currently renders on SGB hardware mode without borders and with at best
whole-screen (not per-region) SGB colorization (§7).

### The "won't run correctly today" list (owner's games, by name)

**Licensed, mapper missing (fully broken until fixed):**
- `GB/Gameboy Camera (UE) [S][!]` — POCKET CAMERA $FC (§2.6)
- `GBC/Pokemon Card GB (J) [C][T+Eng]` — HuC1 $FF (§2.5)
- `GBC/Kirby's Tilt 'n' Tumble (U) [C][!]` — MBC7 $22 (IN-PROGRESS)
- `GBC/Robopon - Sun Version (U) [C][!]` — HuC3 $FE (IN-PROGRESS)

**Licensed, degraded:**
- 13 MBC3-RTC games — clock resets to zero every emulator launch (no `.rtc`
  persistence / wall-clock catch-up): all owned Pokémon Gold/Silver/Crystal
  variants (7), Harvest Moon GB, E.T. Digital Companion, Mary-Kate & Ashley
  Pocket Planner, Pokemon 2004/Diamond-hack, Telefang hack (§2.2).
- 16 MBC5+RUMBLE games — no rumble output anywhere (Pokémon Pinball, Perfect
  Dark, Star Wars Ep. I Racer, Vigilante 8, Ready 2 Rumble, 10-Pin Bowling,
  3-D Ultra Pinball, Hole in One Golf, Little Mermaid II, Missile Command,
  NASCAR Challenge, Polaris SnoCross, Test Drive Off-Road 3, Tonka Raceway,
  Top Gear Pocket, Zebco Fishing) (§2.3).
- 185 SGB-flagged games — no SGB border, no per-region ATTR colorization, no
  SGB sound effects (§7). Includes marquee titles present in the collection
  (Donkey Kong Country, Kirby games, Pokémon R/B/Y...).
- Printer-capable games — no Game Boy Printer peripheral: Game Boy Camera,
  Link's Awakening DX, Super Mario Bros. Deluxe, Donkey Kong Country, Perfect
  Dark, Pokémon G/S/C, Pokémon Pinball, and more in-collection (§8.2).
- IR-capable games — no CGB IR transport: Pokémon G/S/C Mystery Gift, Perfect
  Dark, Super Mario Bros. DX score exchange, Pokémon Pinball (§6.1).

**Unlicensed / bootleg, mapper missing (out of Pan Docs scope, documented in
emulator/forum sources):**
- Rocket Games $97: ATV Racing, Hang Time Basketball, Karate Joe, Painter,
  Pocket Smash Out, Space Invasion; $99 (2-in-1s): ATV Racing & Karate Joe,
  Pocket Smash Out & Race Time, Race Time, Space Invasion & Karate Joe.
  (Type bytes and mapper identified in [gbdev forum "Cartridges with Rare
  Mappers"](https://gbdev.gg8.se/forums/viewtopic.php?id=948) and the
  [MiSTer unlicensed-support thread](https://misterfpga.org/viewtopic.php?t=3129);
  hhugboy implements it.)
- Makon/Ka Sheng $EA: Sonic 3D Blast 5; also Sonic 6, Sonic 7, Super Mario
  Special 3, Pokemon Adventure, Pocket Monsters GO!GO!GO! (headers claim $01
  but need Makon/Sintax-family handling).
- Sachen (header-spoofing, need Sachen MMC): 2nd Space, A-Force, Ant Soldiers,
  Black Forest Tale, Captain Knick-Knack, Crazy Burger, Deep, Duck Adventures,
  Magical Tower, Sky Ace, Worm Visitor (11 carts claiming $00/$01 with 64-128KB
  ROMs **[R]** — banking silently no-ops beyond 32KB on $00).
- Wisdom Tree (whole-32KB bankswitch, [Pan Docs Other MBCs](https://gbdev.io/pandocs/othermbc.html)):
  Exodus (128KB), Spiritual Warfare (256KB), both header $00 **[R]**.
- Misc pirate multicarts/devices: 1993 Collection 128-in-1, 46/58/72-in-1,
  Magic Ball, Super Ayanami SlideShow, SHARK MX (Datel GBMail modem cart —
  also a serial peripheral), Nectaris GB `[b2]` (bad dump: header $00 but
  512KB; the real cart is HuC1).

Zero owner carts use MMM01, M161, MBC6, TAMA5, or EMS — those mappers are
documentation-completeness items only (§2.7-2.10).

---

## 2. Cartridge & peripherals

Dispatch evidence: `rustyboi-core/src/cartridge.rs:562-582`
(`get_cartridge_type`) recognizes only $01-$03, $05-$06, $0F-$13, $19-$1E;
**everything else falls to `CartridgeType::NoMBC`**, which has no banking and
no external RAM (`cartridge.rs:1263` read arm `_ => 0xFF`, `:1430` write arm
`_ => {}`).

### 2.1 ROM+RAM $08/$09 — WRONG (silent RAM loss)
- Doc: [Pan Docs "No MBC"](https://gbdev.io/pandocs/nombc.html) — "Optionally
  up to 8 KiB of RAM could be connected at $A000-BFFF".
- Status: NoMBC arm returns $FF on all external-RAM reads and drops writes;
  `has_battery()` (`cartridge.rs:904-912`) returns false so $09 never saves.
  **[R]** verified: micro-ROM with type $08 + 8KB RAM header writes $77 to
  $A000, reads back $FF (mem oracle `FF80=77` fails).
- Impact: no licensed cart is known to use $08/$09 (Pan Docs cartridge-header
  note), but homebrew and test ROMs do; also any mis-headered dump.
- Effort: **S** — add RAM(+battery) handling to the NoMBC arm.

### 2.2 MBC3 RTC persistence — PARTIAL (state lost across sessions)
- Doc: [Pan Docs MBC3](https://gbdev.io/pandocs/MBC3.html) (battery-backed RTC:
  the coin cell keeps the clock running while powered off);
  gb-ctr `chapter/cartridges/mbc3.typ`.
- Status: in-session RTC is cycle-exact and deterministic
  (`cartridge.rs:990-1063`, rtc3test passes), but nothing saves or restores it:
  `get_save_file_path` (`cartridge.rs:668-679`) writes `.sav` only, there is no
  `.rtc` sidecar reader/writer, and `rtc_memory_mut` (`cartridge.rs:1112-1134`,
  the libretro `RETRO_MEMORY_RTC` view) leaves its 8 timestamp bytes zero and
  has no load path. No wall-clock catch-up on load exists anywhere.
- Impact: **13 owner games** (all Pokémon G/S/C variants, Harvest Moon GB, ...)
  reset their clock every launch; berry growth/day-night/week events never
  progress between sessions.
- Effort: **S** — BGB/VBA `.rtc` format (48 bytes: 10 regs + 8-byte UNIX
  timestamp; the buffer layout already matches) + elapsed-seconds catch-up on
  load. Keep the deterministic cycle-derived path for test runs (catch-up only
  when loading a sidecar).

### 2.3 MBC5 rumble output — PARTIAL (latch modeled, no consumer)
- Doc: [Pan Docs MBC5](https://gbdev.io/pandocs/MBC5.html) ($4000-5FFF bit 3
  drives the motor).
- Status: latch modeled (`cartridge.rs:1346-1352`), accessor `rumble_active()`
  (`:1143-1145`) exists "for the libretro frontend" — but no crate in this
  workspace consumes it (grep across `rustyboi-egui/src`,
  `rustyboi-platform/src`: zero hits).
- Impact: 16 owner games advertise rumble; silently absent. Census note also
  lists "MBC5 rumble" as a no-public-test gap (make-our-own target).
- Effort: **S** — controller-rumble event in the frontends (egui: gilrs;
  platform: host API).

### 2.4 MBC7 (accelerometer + 93LC56 EEPROM) — IN-PROGRESS
- Doc: [Pan Docs MBC7](https://gbdev.io/pandocs/MBC7.html); gb-ctr
  `chapter/cartridges/mbc7.typ`.
- Status: absent at `e70c5a9`; a parallel agent is landing it (census wave-1
  fix launched 2026-07-06). Do not duplicate. Needs: $0000/$4000 double
  enable, $A0x0-$A0x8 register file, latched 2-axis analog values
  ($81D0 center), bit-serial EEPROM with EWEN/ERASE/WRITE/WRAL state machine,
  frontend tilt input source.
- Impact: Kirby's Tilt 'n' Tumble (owned). docboy suite has MBC7
  accel+EEPROM tests.
- Effort: **M** (in flight).

### 2.5 HuC1 (banking + IR mode) — MISSING
- Doc: [Pan Docs HuC1](https://gbdev.io/pandocs/HuC1.html) — explicitly "differs
  from MBC1 significantly": $0000-1FFF selects RAM vs IR mode ($0E = IR),
  ROM bank $2000 (6-bit, bank 0 usable), RAM bank $4000, no RAM-disable;
  in IR mode $A000-BFFF reads the IR receiver ($C0/$C1) and writes the LED.
- Status: type $FF falls to NoMBC → **no banking at all**, game cannot run.
- Impact: Pokémon Card GB (owned, translated). Census: no public HuC1 test
  suite exists (make-our-own target with user hardware).
- Effort: **S** for banking (a day of work, immediately un-breaks the game);
  IR mode can initially return "no light seen" (matches the CGB RP stub) — the
  cart is then fully playable single-player. Full IR transport is §6.1.

### 2.6 POCKET CAMERA (MAC-GBD + M64282FP sensor) — MISSING
- Doc: [Pan Docs Game Boy Camera](https://gbdev.io/pandocs/Gameboy_Camera.html)
  (AntonioND's reverse-engineering): MBC-like banking (ROM bank $2000 6-bit;
  $4000-5FFF RAM bank 0-$0F or CAM register file when bit 4 set), A000 trigger
  /status register, write-only sensor registers (exposure, dither matrix, edge
  ops), 128KB RAM, capture timing formula.
- Status: type $FC falls to NoMBC. Nothing implemented.
- Impact: Game Boy Camera (owned). The selfie/minigame suite is fully playable
  in emulators that model the sensor with a static/webcam image source.
  Census: no public graded camera tests exist (make-our-own target).
- Effort: **L** — banking+registers M; faithful capture pipeline (exposure,
  3-bit dither matrices, 1D filtering, timing) is the long pole.

### 2.7 MMM01 — MISSING
- Doc: [Pan Docs MMM01](https://gbdev.io/pandocs/MMM01.html); gb-ctr
  `chapter/cartridges/mmm01.typ`. Boots "unmapped" mapping the **last** 32KB
  (menu) at $0000-7FFF; game-select bits + mask; MBC1-compatible per-game view.
- Impact: owner has zero MMM01 carts (Momotarou Collection, Taito Variety
  Pack). Correctness-completeness only.
- Effort: **M** (the unmapped-boot + insertion logic is fiddly; docboy/mooneye
  have no coverage; Tauwasser's docs are the reference).

### 2.8 MBC6 — MISSING
- Doc: [Pan Docs MBC6](https://gbdev.io/pandocs/MBC6.html): split $4000/$6000
  ROM banks (8KB granularity), split $A000/$B000 4KB RAM banks, plus a
  Macronix flash chip with write/erase protocol.
- Impact: one game ever (Net de Get, JP, needs Mobile Adapter anyway); owner
  has none. docboy has MBC6 rows.
- Effort: **M** banking, **L** with flash program/erase.

### 2.9 TAMA5 (Bandai Tamagotchi 3) — MISSING
- Doc: gb-ctr `chapter/cartridges/tama5.typ` (partial, with TODO markers —
  the TAMA5 is only partly understood upstream);
  [Pan Docs "Other MBCs"](https://gbdev.io/pandocs/othermbc.html) does not
  cover it. RTC + EEPROM behind a nibble-wide command interface at $A000/$A001.
- Impact: owner has none. "Game de Hakken!! Tamagotchi 3" only. Census lists
  TAMA5 as a no-public-oracle gap.
- Effort: **M**, oracle-poor (validate against gb-ctr + GBE+ notes).

### 2.10 M161 — MISSING
- Doc: [Pan Docs M161](https://gbdev.io/pandocs/M161.html): single latched
  whole-32KB bankswitch (one shot, locks until reset).
- Impact: one cart ever (Mani 4 in 1, JP); owner has none.
- Effort: **S**.

### 2.11 Unlicensed mappers — MISSING (documented outside Pan Docs)
- Wisdom Tree ([Pan Docs Other MBCs](https://gbdev.io/pandocs/othermbc.html)):
  whole-32KB switch, bank selected by **address** low byte on any $0000-7FFF
  write. Owner impact: Exodus, Spiritual Warfare. Effort **S** (plus the
  detection heuristics Pan Docs proposes: title "WISDOM TREE"/$C0+$D1 magic).
- EMS ($1B+region $E1 magic, Pan Docs Other MBCs, flagged "to be verified"
  upstream — [pandocs#423](https://github.com/gbdev/pandocs/issues/423)).
  Owner impact: none. Effort S.
- Rocket Games $97/$99, Sachen MMC1/MMC2 (logo-spoof + address-scramble),
  Makon/Sintax family: documented only in emulator sources (hhugboy) and the
  [gbdev wiki/forum](https://gbdev.gg8.se/forums/viewtopic.php?id=948). Owner
  impact: 22 unlicensed carts (list in §1). Effort **M** each family,
  oracle = real carts or hhugboy cross-check (below our usual provenance bar —
  mark any implementation as unlicensed-best-effort).

### 2.12 Cartridge misc
- MBC1M multicart heuristic, MBC30, MBC2 nibble RAM+echo, MBC5 9-bit banks,
  RTC latch semantics, rumble-bit/RAM-bank sharing: **COMPLETE** (mbc3-tester,
  rtc3test, mooneye MBC suites, magentests mbc_oob_sram all pass).
- Game Genie ROM patches: implemented (`cartridge.rs:1152-1170`); GameShark RAM
  pokes via `GB::write_memory` (`gb.rs:876-878`). [Pan Docs
  Shark Cheats](https://gbdev.io/pandocs/Shark_Cheats.html). COMPLETE (core
  hooks; UI wiring is a frontend concern).

---

## 3. CPU

All 245+CB opcodes, DAA, halt bug, IME/EI delay, interrupt dispatch timing:
**COMPLETE** (blargg cpu_instrs, daid/bully, sketchtests daa,
mooneye/wilbertpol timing all pass). Illegal opcodes $D3/$DB/$DD/$E3/$E4/$EB/
$EC/$ED/$F4/$FC/$FD hard-lock correctly (`cpu/opcodes.rs:294-302`, Gambatte
freeze model).

### 3.1 Plain STOP (low-power mode) — WRONG (executes straight through)
- Doc: [Pan Docs "Reducing Power Consumption"](https://gbdev.io/pandocs/Reducing_Power_Consumption.html)
  + the Lior Halphon STOP chart embedded there; gb-ctr instruction set (STOP).
  STOP without an armed speed switch must idle the CPU until a P10-P13 line
  goes low, with the documented decision table (button-held / pending-IF cases
  degenerate to NOP/HALT/1-byte forms), DIV reset, and per-model LCD behavior
  (DMG: line artifact if LCD left on; CGB: black screen unless mode 3).
- Status: the CGB speed-switch arm of STOP is modeled to sub-dot depth
  (`cpu/opcodes.rs:8-287` — the spsw campaign), but the fall-through arm just
  sets `cpu.stopped = true` (`cpu/opcodes.rs:288-292`) **which nothing reads**
  (only other reference: `sm83.rs:602` uses it to pick an IRQ anchor;
  `gb.rs:963` clears it on reset). **[R]** verified: micro-ROM `STOP $00`
  followed by `ld a,$5A; ldh ($81),a` writes the marker with no joypad input —
  STOP behaved as a NOP.
- Impact: Pan Docs: "No licensed rom makes use of STOP outside of CGB speed
  switching" — so zero owner-game impact; but homebrew, accuracy suites
  (AntonioND gbc-hw-tests `corrupted_stop`/`stop` variants queued in the census
  wave 3), and the STOP chart cases are all unimplementable until this exists.
- Effort: **M** — the state itself is simple (halt-until-joypad-low + DIV
  reset + LCD gating), but it must be threaded carefully around the existing
  sub-dot speed-switch machinery (high regression risk area; gate strictly on
  "KEY1 bit 0 clear").

### 3.2 Joypad line edge from JOYP select writes — PARTIAL
- Doc: [Pan Docs Interrupt Sources](https://gbdev.io/pandocs/Interrupt_Sources.html)
  (joypad IRQ on any P10-P13 high→low edge) + gb-ctr `peripherals/p1.typ`:
  writing P14/P15 selects while a button is held produces such an edge.
- Status: host button changes raise IF.4 (`input.rs:96-111` returns the edge,
  caller flags), but the JOYP **write** path (`input.rs:157-173`,
  `mmio.rs:4690`) recomputes the nibble without ever requesting the interrupt.
- Impact: no known retail game depends on it (games poll); it is the
  documented trigger mechanism for STOP wake and appears in docboy's joypad
  rows. Cheap correctness.
- Effort: **S** (compare old/new low nibble in the write path; route IF.4).

---

## 4. PPU

Modes/STAT/LYC, OAM scan, both fetchers/FIFOs, window (incl. WX edge cases),
mid-scanline register effects, VRAM/OAM locking, DMG STAT-write bug
(`mmio.rs:257`, `ppu/controller.rs:5725`), CGB palettes/priority/OPRI, VRAM
banking, OAM DMA (incl. bus conflicts beyond gb-ctr's own TODO), HDMA/GDMA
(incl. the world-first 2xGDMA word-bus conflict): **COMPLETE** — acid2 3/3,
cgb-acid-hell, mealybug, AGE, samesuite, gbmicrotest 509/513 pass.

- OAM corruption bug ([Pan Docs](https://gbdev.io/pandocs/OAM_Corruption_Bug.html)):
  **COMPLETE** — read/write/IDU triggers with SameBoy-faithful
  secondary/tertiary/quaternary read patterns (`cpu/bus.rs:520-556`,
  `mmio.rs:2970-3060`); blargg oam_bug passes on DMG and CGB **[R]** (suite run
  this audit: blargg 15/15).
- Remaining PPU items are the documented accuracy frontier, all tracked with
  proofs in `KNOWN_FAILURES.md`: `windesync-validate` (pre-CGB window-desync
  glitch, real-SGB LA oracle) — OPEN-TARGET, fix in flight; mid-m3
  `tile_sel_win_change2`/`win_map_change2` capture-phase residuals; nothing
  else known.

## 5. APU

All four channels, frame sequencer, NRx4 length-glitch (per CGB revision),
NRx2 zombie mode (`audio/square.rs:508`, `noise.rs:319`), DMG wave-RAM
read-during-play + trigger corruption (`audio/wave.rs:293`), PCM12/PCM34
(`mmio.rs:4589-4590`), power-off register clears, per-revision gates
(CGB0/B/C/D/E/AGB, `gb.rs:140-160`): **COMPLETE** — samesuite APU 70/70,
blargg dmg_sound/cgb_sound pass. No documented register-level APU behavior is
known missing; remaining depth is analog output characterization (census:
MDFourier-style spectral capture = future idea, no public oracle).

---

## 6. CGB systems

Speed switch (KEY1) incl. sub-dot STOP bridging, KEY0/DMG-compat lock
(magentests `key0_lock` passes), VBK/SVBK, BCPS/BCPD/OCPS/OCPD incl. mode-3
lock, OPRI, HDMA5 semantics, undocumented FF72/FF73/FF74/FF75 with exact
unused-bit masks (`mmio.rs:4540-4556`), unmapped-hole reads: **COMPLETE**
(mooneye boot_hwio/unused_hwio-C, docboy FF72-75 semantics match).

### 6.1 Infrared communication (RP $FF56) — PARTIAL (register only, no transport)
- Doc: [Pan Docs CGB Registers](https://gbdev.io/pandocs/CGB_Registers.html)
  (RP) + [Infrared Communication](https://gbdev.io/pandocs/IR.html).
- Status: register bit behavior is exact (write mask $C1 `mmio.rs:4922-4926`,
  read forces bit 1 = "no light" `mmio.rs:4533-4535`, power-on $3E
  `gb.rs:330`) — i.e. a permanently dark room. There is no way for an emitted
  IR pulse to be observed by anything: no loopback device, no second-instance
  transport, no recorded-signal injection.
- Impact: Pokémon G/S/C GBC-to-GBC Mystery Gift, Perfect Dark pickups, SMB DX
  score exchange, Pokémon Pinball score sharing (all owned) simply report "no
  partner". Also blocks HuC1/HuC3 IR modes (§2.5) sharing the same transport.
  docboy has `rp_write_read` rows (register-level — those we already satisfy).
- Effort: **M** — define an IR bus trait (pulse timeline), loopback + paired
  instance; games are timing-tolerant compared with the serial port.

### 6.2 DMG-compat boot palette table — COMPLETE
- Doc: [Pan Docs Power-Up Sequence](https://gbdev.io/pandocs/Power_Up_Sequence.html)
  (CGB boot ROM: per-game compatibility palettes chosen by title-checksum +
  4th title letter, plus button-combo overrides).
- Status: skip-BIOS now runs the boot ROM's full selection
  (`cgb_compat_palette.rs`, tables and algorithm lifted from the cgb_boot.bin
  dump: $0475-$051D code, $06C7-$08FB data): Nintendo-licensee gate, 16-byte
  title checksum, 79-entry table + 4th-letter disambiguation rows, 29
  offset-triplet combinations with the per-slot flag bits and overlapped
  palette pointers, and the 12 boot button-combo overrides (buttons held when
  `skip_bios()` runs). Unrecognized titles resolve to the boot ROM's default
  entry — byte-identical to the previously hard-coded fixed palette, so every
  hwtest grader is unmoved. Validated with `--validate-bios` palette-RAM diffs
  against the real boot ROM: Tetris / Super Mario Land / Link's Awakening /
  Pokémon Red / Pokémon Blue / Kirby / Metroid II / dmg-acid2 all 0-diff.
- Remaining nuance: selection is sampled once at skip-BIOS time; the real boot
  ROM lets a combo pressed at any point during the logo override the choice
  (needs a boot-long input timeline, only observable with a real BIOS).

### 6.3 CGB compat-path boot DIV per-header variance — PARTIAL
- Status: CGB DMG-cart hand-off DIV is a single anchor (0x2678,
  `gb.rs:402-412`) calibrated on mooneye's cart; the real compat path duration
  varies with header contents (palette lookup work) — the docboy census
  attributes 38 `cgb boot per-header DIV` rows to this. With a real BIOS the
  value is emergent and already reproduces both anchors (comment
  `gb.rs:386-401`).
- Impact: invisible to games; test-suite depth only. Effort: **M** (derive the
  duration formula from the boot ROM path, or always recommend real-BIOS runs
  for such tests).

### 6.4 Real boot ROM acceptance limited to DMG/MGB + CGB — PARTIAL
- Status: `load_bios` (`mmio.rs:1129-1159`) accepts only 256-byte images
  matching the DMG CRC (byte $FD masked, so DMG and MGB both pass) and
  2304-byte images matching CGB. DMG0, SGB, SGB2, CGB0 and AGB boot ROM dumps
  are **rejected**, even though synthetic post-boot states for all of them
  exist and pass mooneye's boot fingerprints (`gb.rs:175-541`).
- Impact: none for games; blocks real-boot validation runs for those models.
- Effort: **S** (accept + route per-model CRCs; SGB boot additionally
  exercises the JOYP header-packet transmit our SGB receiver models).

---

## 7. Super Game Boy

Packet receiver is hardware-exact (idle-armed, last-pulse-wins, both-low abort
— pinned by the 27-variant real-SGB `sgb-ext-test` capture), MLT_REQ including
the glitched 3-player mode (beyond SameBoy), MASK_EN all four modes, PAL01/23/
03/12, PAL_SET (+mask-cancel bit), PAL_TRN (512 system palettes): **COMPLETE**
(`sgb.rs`; samesuite SGB rows + sgb.manifest pass). Per-model SGB/SGB2 boot
states incl. the header-popcount-dependent boot DIV: COMPLETE (`gb.rs:443-448`).

### 7.1 ATTR_BLK/ATTR_LIN/ATTR_DIV/ATTR_CHR geometry + ATTR_TRN/ATTR_SET — COMPLETE
- Doc: [Pan Docs SGB Color Attribute Commands](https://gbdev.io/pandocs/SGB_Command_Attribute.html).
- Status: full geometry implemented to spec (`sgb.rs`): ATTR_BLK
  (inside/line/outside + the only-inside/only-outside implicit-line
  exception), ATTR_LIN, ATTR_DIV, ATTR_CHR (LTR/TTB walk, multi-packet
  data), ATTR_TRN (45-file ATF store), ATTR_SET (+cancel-mask), PAL_SET
  byte-9 ATF-apply/9-bit palette indices, and the shared color-0 backdrop
  across all four palettes. The *_TRN readout captures from the DISPLAYED
  frame (the SGB re-digitizes the video signal), not a flat VRAM read —
  Donkey Kong '94 transfers with LCDC.4=0 and only works this way. MASK_EN
  Freeze latches the pre-freeze frame (transfer screens stay hidden).
- Validated: Donkey Kong '94, Kirby's Dream Land 2, Pokémon Red render
  their authentic multi-palette screens; unit tests cover each command.

### 7.2 SGB border (CHR_TRN / PCT_TRN) — COMPLETE (core; frontend presentation opt-in)
- Doc: [Pan Docs SGB Border commands](https://gbdev.io/pandocs/SGB_Command_Border.html)
  / [VRAM transfers](https://gbdev.io/pandocs/SGB_VRAM_Transfer.html).
- Status: CHR_TRN (2 x 128 SNES 4bpp tiles) and PCT_TRN (32x28 map +
  palettes 4-7) are stored (`sgb.rs`), and
  `GB::sgb_composited_frame()` returns the full 256x224 RGB888 output —
  backdrop (shared color 0), the masked/colorized GB screen at (48,40),
  and the border tiles (color 0 transparent) on top, per SameBoy's
  layering. INTEGRATION POINT: the accessor is off-screen by design — the
  160x144 `Frame` path is byte-identical (suite graders unaffected) and
  `frame_ready` is not consumed. A frontend presents borders by calling
  `gb.sgb_composited_frame()` after `run_until_frame` and falling back to
  the standard frame on `None` (non-SGB hardware, or no border transferred
  yet); dimensions in `ppu::SGB_FRAME_WIDTH/HEIGHT`. The pixels-based
  platform frontend keeps its fixed 160x144 surface for now.
- Validated: DK '94 arcade-cabinet border, KDL2 checkerboard border,
  Pokémon Red plaid border all composite correctly.
- Remaining (below the fold): border fade/latch timing races
  (little-things `sgbears`, pinobatch trnstress) — transfers apply at the
  next frame boundary, not the real 5-frame window.

### 7.3 SGB sound commands (SOUND $08, SOU_TRN $09) — MISSING
- Doc: [Pan Docs SGB Sound commands](https://gbdev.io/pandocs/SGB_Command_Sound.html).
- Status: decoded, no-op (`sgb.rs:321-325`).
- Impact: SGB-side jingles/effects (and music in e.g. Space Invaders arcade
  mode via SOU_TRN programs) are silent. Requires an SPC700/SNES-APU HLE.
- Effort: **L** (canned-sample HLE for the built-in effect table is a possible
  M-sized 80% solution).

### 7.4 Remaining SGB command set — MISSING (no-ops), mostly by design
- OBJ_TRN ($18, SNES-OBJ mode) and PAL_PRI ($19, palette priority): visible
  effects on hardware, used by a handful of titles;
  [Pan Docs System Commands](https://gbdev.io/pandocs/SGB_Command_System.html).
  Effort M, near-zero owner impact.
- ICON_EN ($0E): gates SGB in-menu functions — harmless as no-op until
  borders/palette menus exist. DATA_SND/DATA_TRN/JUMP ($0F/$10/$12): write
  SNES RAM / jump — correctly no-GB-visible in an HLE model (DATA_TRN's VBlank
  VRAM read is modeled). ATRC_EN/TEST_EN ($0C/$0D) and the removed/undocumented
  commands ($0x range oddities): no-op is the right model.
- MLT_REQ-dependent SGB *joypad* IRQ nuances (`sgbkeyirq` in census wave-3):
  untested here.

### 7.5 SGB unlock gate ($0146/$014B) — WRONG (always unlocked)
- Doc: [Pan Docs "Unlocking and Detecting SGB Functions"](https://gbdev.io/pandocs/SGB_Unlocking.html):
  header SGB flag $03 + old licensee $33 required, otherwise "it cannot access
  any of the special SGB functions".
- Status: `enable_sgb` is unconditional on SGB hardware (`gb.rs:161-163`);
  no code reads $0146 (grep: zero hits). Non-SGB-flagged games can drive
  packets; on hardware they cannot.
- Impact: only misbehaving/homebrew ROMs notice. Effort: **S** (gate packet
  dispatch on the header at insert time).

---

## 8. Serial / link cable

### 8.1 Link peer transport & external clock — MISSING (disconnected-cable only)
- Doc: [Pan Docs Serial Data Transfer](https://gbdev.io/pandocs/Serial_Data_Transfer_(Link_Cable).html);
  gb-ctr `peripherals/serial.typ`.
- Status: internal-clock transfers are Gambatte-exact (DIV-aligned schedule,
  CGB fast clock, DIV-write realign — `serial.rs:85-134`) and disconnected
  semantics are right ($FF shifts in, `serial.rs:152`; external-clock starts
  never complete, `serial.rs:76-78` — correct with no peer). But there is no
  peer: no API to connect two cores, no external-clock data path, no
  cross-instance timing negotiation.
- Impact: all 2-player features across the collection (Tetris, Pokémon trades
  /battles, F-1 Race...); every downstream serial peripheral (§8.2-8.4).
  Census confirms no comprehensive public link-timing suite exists
  (make-our-own target; Pan Docs itself carries a "TODO: only measured on CGB
  rev E" on disconnect behavior).
- Effort: **L** — transport trait + lockstep scheduling between instances;
  the master/slave byte-swap protocol itself is simple.

### 8.2 Game Boy Printer — DONE
- Doc: [Pan Docs Game Boy Printer](https://gbdev.io/pandocs/Gameboy_Printer.html)
  (packet protocol: sync $88 $33, commands 1/2/4/$F, checksum, status byte,
  160-wide 2bpp banded bitmap).
- Status: implemented. `serial.rs` gained a pluggable `SerialDevice` link-port
  hook (latches the peer's response byte at transfer start, delivers the
  shifted-out byte at completion — the real simultaneous-exchange constraint);
  `printer.rs` is the self-contained slave device: the full INIT/DATA/PRINT/
  BREAK/STATUS packet state machine, checksum verification, RLE decompression,
  band accumulation, palette/exposure compositing, busy-status lifecycle, and a
  dependency-free grayscale PNG encoder. Disconnected is the default so every
  existing serial behavior stays byte-identical (samesuite/blargg/mooneye/
  sketchtests serial suites all unchanged). Desktop frontend exposes it via
  `--printer` and an Emulation-menu toggle; captured prints are written as
  `<rom>-print-<n>.png` next to the `.sav`. Timing is master-clock derived
  (deterministic, no wall clock). Validated against mmuszkow/gbprinter's PAT
  test pattern and against real-hardware print captures (Raphael-Boichot's
  GameboyPrinterSniffer) of Pokémon Crystal (Pokédex page) and Zelda DX (photo)
  replayed through the emulated serial path — both reproduce the exact print.
- Impact: owned printer-capable games listed in §1 (Camera, Zelda DX photos,
  SMB DX, DKC, Perfect Dark, Pokémon G/S/C/Pinball...). Standard emulator
  feature: capture to PNG.
- Remaining: the printer is the desktop/CLI's device; the §8.1 two-GB link peer
  and the libretro/Android print sinks are still future work.

### 8.3 4-Player Adapter (DMG-07) — MISSING
- Doc: [Pan Docs 4-Player Adapter](https://gbdev.io/pandocs/Four_Player_Adapter.html)
  (external-clock broadcast protocol, ping/transmission phases).
- Impact: F-1 Race, Wave Race, Yoshi's Cookie multiplayer — negligible until
  §8.1 exists. Effort: **M** after 8.1.

### 8.4 Mobile Adapter GB — MISSING
- Doc: not in Pan Docs (dan-docs/wiki territory,
  [gbdev wiki](https://gbdev.gg8.se/wiki/articles/Mobile_Game_Boy_Adapter)).
  Census: confirmed gap, JP-only service, community re-implementation
  (REON/libmobile) exists.
- Impact: owner has zero Mobile-Adapter games (Net de Get absent). Effort: L.
  Lowest priority.
- Same bucket: Barcode Boy / Barcode Taisen Bardigun reader (serial), WorkBoy
  keyboard (serial, documented 2020) — zero owner impact, effort S-M each,
  listed for completeness.

---

## 9. Boot / power-on — COMPLETE (minor notes in §6.2-6.4)

Per-model post-boot CPU registers (incl. DMG H/C from header checksum, AGB
B=1/Z quirk), per-model DIV counters (incl. SGB header-popcount-dependent
timing), WRAM/OAM/FEA0/HRAM power-on patterns, wave RAM, VRAM logo residue,
post-boot PPU frame phase, JOYP select-line hand-off, per-cart CGB vs
DMG-compat paths: all modeled and pinned by mooneye boot_* fingerprints and
gambatte dumper oracles (`gb.rs:175-581`). Real-BIOS execution path validated
against both hand-off anchors. Gaps: only §6.2 (compat palette table), §6.3
(compat DIV variance), §6.4 (other-model boot ROM acceptance).

---

## 10. Pan Docs section-by-section disposition

| Pan Docs section(s) | Status |
|---|---|
| Memory Map / Echo RAM / Not Usable region | COMPLETE (per-model FEA0-FEFF incl. CGB $E7 mirror) |
| I/O ranges, unused-bit masks, unmapped holes | COMPLETE (boot_hwio/unused_hwio grade) |
| Graphics: tiles, maps, OAM, DMA, window, LCDC, STAT, scrolling, palettes, rendering, FIFO | COMPLETE (frontier items in KNOWN_FAILURES.md) |
| OAM DMA / HDMA / GDMA | COMPLETE (beyond-reference bus-conflict modeling) |
| Audio (registers + details) | COMPLETE |
| Joypad Input | COMPLETE except §3.2 select-write IRQ edge |
| Serial Data Transfer | PARTIAL — §8.1 |
| Timer & Divider + Obscure Behaviour | COMPLETE |
| Interrupts / Sources / HALT | COMPLETE (double-halt refetch OPEN-TARGET in flight) |
| CGB Registers | COMPLETE except IR transport §6.1 |
| Infrared Communication | MISSING transport — §6.1 |
| SGB (all 14 sections) | PARTIAL — §7 (protocol/palettes/ATTR/border done; sound §7.3 + OBJ_TRN/PAL_PRI §7.4 not) |
| CPU (specs, registers, instruction set) | COMPLETE (illegal-op lockup included) |
| Cartridge header | COMPLETE parse; §7.5 SGB flag unused |
| No MBC | WRONG for $08/$09 — §2.1 |
| MBC1 / MBC2 / MBC3 / MBC5 (+MBC1M, MBC30) | COMPLETE |
| MBC6 | MISSING — §2.8 |
| MBC7 | IN-PROGRESS — §2.4 |
| MMM01 | MISSING — §2.7 |
| M161 | MISSING — §2.10 |
| HuC1 | MISSING — §2.5 |
| HuC-3 | IN-PROGRESS |
| Other MBCs (Wisdom Tree, EMS, multicart magics) | MISSING — §2.11 |
| Game Boy Printer | DONE — §8.2 |
| Game Boy Camera | MISSING — §2.6 |
| 4-Player Adapter | MISSING — §8.3 |
| Game Genie / Shark | COMPLETE (core hooks) |
| Power-Up Sequence | COMPLETE — §9 |
| Reducing Power Consumption (STOP) | WRONG — §3.1 |
| Accessing VRAM/OAM (locking) | COMPLETE |
| OAM Corruption Bug | COMPLETE **[R]** |
| External Connectors / GBC Approval | n/a (physical/process) |

gb-ctr-only chapters cross-checked: SM83 core & timing (COMPLETE), clocks
(COMPLETE), P1 port (§3.2 edge), boot ROM chapter (COMPLETE), DMA chapter
incl. its "OAM DMA bus conflicts: TODO" (we exceed the reference), MBC30
(COMPLETE), TAMA5 (§2.9).

---

## 11. Prioritized roadmap (owner-impact x correctness, with effort)

| # | Item | Owner impact | Effort |
|---|---|---|---|
| 1 | Land MBC7 (Kirby's Tilt 'n' Tumble) — IN-PROGRESS, verify + real-game test | 1 marquee game | M (in flight) |
| 2 | Land HuC-3 (Robopon Sun) — IN-PROGRESS, verify + real-game test | 1 game | M (in flight) |
| 3 | MBC3 RTC persistence: `.rtc` sidecar + wall-clock catch-up (§2.2) | 13 games incl. all Pokémon G/S/C | S |
| 4 | HuC1 banking + IR-mode register (§2.5) | Pokémon Card GB un-broken | S |
| 5 | ~~Game Boy Printer serial device → PNG (§8.2)~~ DONE | 10+ games' print features | — |
| 6 | ~~SGB ATTR geometry + ATTR_TRN/ATTR_SET store (§7.1)~~ DONE | 185 SGB games colorize correctly | — |
| 7 | ~~SGB border CHR_TRN/PCT_TRN + 256x224 output (§7.2)~~ DONE (core; frontend presentation opt-in) | 185 SGB games | — |
| 8 | Link-cable peer transport + external clock (§8.1) | all 2-player/trading | L |
| 9 | Game Boy Camera mapper + M64282FP pipeline (§2.6) | owned cart | L |
| 10 | Plain-STOP mode per the STOP chart (§3.1) | correctness + gbc-hw-tests wave | M (regression-sensitive) |
| 11 | CGB IR transport (loopback + paired instance) (§6.1) | Mystery Gift, Perfect Dark, SMB DX | M |
| 12 | ROM+RAM $08/$09 NoMBC external RAM (§2.1) | homebrew/mis-dumps | S |
| 13 | Rumble output wiring in frontends (§2.3) | 16 games | S |
| 14 | JOYP select-write joypad-IRQ edge (§3.2) | correctness | S |
| 15 | CGB DMG-compat per-game boot palette table for skip-BIOS (§6.2) | 619 DMG games' colors on CGB | S/M |

Backlog (below the fold): Wisdom Tree (S) → un-breaks 2 owned carts; Rocket
Games $97/$99 (M) → 10 owned unlicensed carts; Sachen (M) → 11 owned carts;
Makon (M) → 6 owned pirates; SGB unlock gate (S); boot-ROM acceptance for
DMG0/SGB/CGB0/AGB dumps (S); MMM01 (M); M161 (S); TAMA5 (M); MBC6 (M/L);
SGB SOUND HLE (L); OBJ_TRN/PAL_PRI (M); DMG-07 4-player (M, after #8);
Mobile Adapter GB (L); compat-path boot DIV formula (M).

Already-tracked accuracy frontier (not re-listed here): see
`KNOWN_FAILURES.md` (17 cases, all proven floors or in-flight open targets)
and the internal-suite fleet notes.
