# Implementation Gaps — Completeness Audit vs Pan Docs / gb-ctr

Audit date: 2026-07-06, tree `e70c5a9`; **status refreshed 2026-07-06** after
the periphery roadmap landed (all of §11 items 1-15 + M161 §2.10 are now DONE;
only the no-oracle backlog and owner-deferred CGB IR remain). Method: every Pan
Docs section (78-entry
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
complete and world-class. The audit's original periphery gaps have since been
**closed**: every documented cartridge mapper with an in-tree reference or a
gradable ROM is implemented (MBC1/2/3/5/7, HuC1, HuC3, POCKET CAMERA, all four
unlicensed families, and M161), the cartridge peripherals (camera, printer,
two-GB link cable) are done, SGB has per-region ATTR colorization + borders,
plain-STOP follows the Pan Docs chart, and persistence (`.rtc`/`.sav`, rumble
output) is plumbed. What remains is a small completeness backlog with **no
owner impact and no oracle** — MMM01, MBC6, TAMA5 (no in-tree reference or test
ROM), CGB IR transport (owner-deferred), SGB sound HLE — tracked in §11. Trigger
for the original audit: discovery that only MBC1/2/3/5 were implemented; that
gap is now resolved.

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
| ROM only ($00) | 49 | supported (Sachen/Wisdom Tree header-liars content-detected — §2.11) |
| MBC2 ($06) | 20 | supported |
| MBC3 ($10/$11/$13) | 20 (13 with RTC) | supported incl. MBC30; **RTC state lost between sessions** (§2.2) |
| MBC5+RUMBLE ($1C/$1E) | 16 | banking supported; **rumble motor not wired to any frontend** (§2.3) |
| Rocket Games ($97/$99, unlicensed) | 10 | supported (logo-checksum detection + inner/outer banking) |
| POCKET CAMERA ($FC) | 1 | supported (MAC-GBD + M64282FP sensor pipeline — §2.6) |
| Makon/Ka Sheng ($EA, unlicensed) | 1 | supported (SONIC5 -> plain-MBC1 routing per hhugboy) |
| MBC7 ($22) | 1 | supported (93LC56 EEPROM + accelerometer — §2.4) |
| HuC3 ($FE) | 1 | supported (RTC + banking + IR stub — §2.4/§2.5) |
| HuC1 ($FF) | 1 | supported (banking + IR-mode register — §2.5) |

CGB flag: 344 CGB-only, 185 CGB-compatible, 619 DMG-only.
SGB-flagged (`$0146=$03` + `$014B=$33`): **185 games** — every one of them
currently renders on SGB hardware mode without borders and with at best
whole-screen (not per-region) SGB colorization (§7).

### The "won't run correctly today" list (owner's games, by name)

**Licensed, mapper previously missing — ALL FIXED:**
- ~~`GB/Gameboy Camera (UE) [S][!]` — POCKET CAMERA $FC~~ FIXED (§2.6): boots,
  shoots off the M64282FP pipeline, saves to the album, gallery works.
- ~~`GBC/Pokemon Card GB (J) [C][T+Eng]` — HuC1 $FF~~ FIXED (§2.5).
- ~~`GBC/Kirby's Tilt 'n' Tumble (U) [C][!]` — MBC7 $22~~ FIXED (§2.4).
- ~~`GBC/Robopon - Sun Version (U) [C][!]` — HuC3 $FE~~ FIXED (§2.4).

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

**Unlicensed / bootleg — FIXED (content-detected mappers, §2.11; all owner
carts below verified booting into menus/gameplay):**
- Rocket Games $97/$99 (10 games / 12 dumps incl. 2-in-1 sub-game switching):
  ATV Racing, Hang Time Basketball, Karate Joe, Painter, Pocket Smash Out,
  Race Time, Space Invasion + the three 2-in-1s — `UnlMapper::Rocket`.
- Makon/Ka Sheng: Sonic 3D Blast 5 ($EA -> plain-MBC1 routing; previously the
  $20 RAM-size byte made the loader error out), Super Mario Special 3
  (NT-old-2 board), Sonic 6, Sonic 7, Pokemon Adventure, Pocket Monsters
  GO!GO!GO! (fixed dumps, header MBC1 verified working).
- Sachen: Captain Knick-Knack ($00/TETRIS header liar -> plain-MBC1 routing;
  was bankless-broken); the ten $01 singles are descrambled GoodGBx dumps that
  run correctly as plain MBC1 (verified); raw scrambled dumps take the real
  Sachen MMC1/MMC2 emulation (verified frame-exact vs the descrambled dump).
- Wisdom Tree: Exodus, Spiritual Warfare — whole-32KB address-latch mapper.

**Unlicensed / bootleg, still missing:**
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

### 2.4 MBC7 (accelerometer + 93LC56 EEPROM) + HuC-3 — DONE
- Doc: [Pan Docs MBC7](https://gbdev.io/pandocs/MBC7.html); gb-ctr
  `chapter/cartridges/mbc7.typ`; [Pan Docs HuC3](https://gbdev.io/pandocs/HuC3.html).
- Status: both implemented in `cartridge.rs`. MBC7: $0000/$4000 two-stage
  enable, the $A0x0-$A0x8 register file, latched 2-axis analog values
  ($8000 idle / ~$81D0 center, fed via `set_accelerometer`), and the
  bit-serial 93LC56 EEPROM (EWEN/EWDS/ERASE/WRITE/WRAL/READ state machine,
  contents in `ram_data` so the `.sav` persists them). HuC-3: 7-bit ROM /
  RAM banking, the RTC MCU mailbox ($0B command / $0C-$0D response+semaphore,
  256-nibble internal memory with the minute-of-day + 12-bit day counters),
  the IR stub, and `.rtc` sidecar persistence with wall-clock catch-up.
- Verified: Kirby's Tilt 'n' Tumble tilts and saves; Robopon Sun boots and
  keeps time across sessions. Unit tests cover the EEPROM state machine and
  the HuC-3 catch-up cascade.

### 2.5 HuC1 (banking + IR mode) — DONE
- Doc: [Pan Docs HuC1](https://gbdev.io/pandocs/HuC1.html) — explicitly "differs
  from MBC1 significantly": $0000-1FFF selects RAM vs IR mode ($0E = IR),
  ROM bank $2000 (6-bit, bank 0 usable), RAM bank $4000, no RAM-disable;
  in IR mode $A000-BFFF reads the IR receiver ($C0/$C1) and writes the LED.
- Status: implemented (`CartridgeType::HuC1`): the low-nibble $0E IR-select at
  $0000-1FFF, 6-bit ROM banking with bank 0 selectable, RAM banking with no
  enable gate (RAM always mapped), and the IR register at $A000-BFFF (reads the
  documented idle $C0 "no light", writes latch the LED for a future transport).
- Verified: Pokémon Card GB boots and plays single-player. Unit tests cover the
  IR-mode region switch and the always-enabled banked RAM. Full IR transport
  (peer/loopback) is still §6.1; the cart is fully playable without it.

### 2.6 POCKET CAMERA (MAC-GBD + M64282FP sensor) — DONE
- Doc: [Pan Docs Game Boy Camera](https://gbdev.io/pandocs/Gameboy_Camera.html)
  (AntonioND's reverse-engineering): MBC-like banking (ROM bank $2000 6-bit;
  $4000-5FFF RAM bank 0-$0F or CAM register file when bit 4 set), A000 trigger
  /status register, write-only sensor registers (exposure, dither matrix, edge
  ops), 128KB RAM, capture timing formula.
- Status: implemented in `cartridge.rs` (`CartridgeType::PocketCamera`):
  6-bit ROM banking (bank 0 selectable), 16 RAM banks, write-only-gate RAMG
  (reads always enabled per AntonioND), the 54-byte CAM register file
  (mirrored every $80; only A000 readable = stored bits 1-2 + live busy),
  RAM reads $00 / writes dropped while capturing, stop/resume via A000 bit 0.
  Capture busy window = 4x(32446 + (N?0:512) + 16xexposure) master dots off
  the deterministic `tick_rtc` path (PHI-doubled in CGB double speed).
  M64282FP pipeline per the GiiBiiAdvance reference model reproduced in Pan
  Docs, exact-integer: exposure scale + level squash, invert, the N/VH/E3
  3x3 kernels (2D/H enhancement+extraction, V modes, the mode-1 constant-
  color quirk) with the A000-selected P/M 1-D filter sets, 4x4x3 dither/
  contrast matrix, 2bpp tile packing to RAM bank 0 $0100 (committed at
  window expiry, streamed to the `.sav`). Sensor input: built-in
  deterministic test pattern, or a frontend-fed 128x112 grayscale via
  `Cartridge::set_camera_image` (see `camera-drive` harness bin; webcam
  wiring in the GUI frontends remains open).
- Verified: owner's `Gameboy Camera (UE) [S][!]` boots to the SHOOT/VIEW/PLAY
  menu, live viewfinder renders the fed image through the sensor pipeline,
  shutter+SAVE stores to the album ("30 left" -> "29 left"), gallery shows
  the photo, and the album survives a process restart via the 128KB `.sav`.
  Census: no public graded camera tests exist (make-our-own target).

### 2.7 MMM01 — MISSING
- Doc: [Pan Docs MMM01](https://gbdev.io/pandocs/MMM01.html); gb-ctr
  `chapter/cartridges/mmm01.typ`. Boots "unmapped" mapping the **last** 32KB
  (menu) at $0000-7FFF; game-select bits + mask; MBC1-compatible per-game view.
- Impact: owner has zero MMM01 carts (Momotarou Collection, Taito Variety
  Pack). Correctness-completeness only.
- Effort: **M** (the unmapped-boot + insertion logic is fiddly; docboy/mooneye
  have no coverage; Tauwasser's docs are the reference).
- Status: DEFERRED (considered). gambatte ships an authoritative reference
  (`mem/mbc/mmm01.cpp` + `presumedMmm01`), but the port has two wrinkles that
  cannot be validated in this repo — no MMM01 test ROM exists anywhere, and no
  owner cart boots it. The `badMmm01` literal-$0B-$0D path needs a load-time
  ROM rotation (gambatte moves the first 32KB to the end), and the multiplexed
  MBC1-superset banking is ~15 interacting sub-functions where a silent porting
  slip is invisible without a booting game. Rather than ship an unverifiable
  mapper, this waits for a real MMM01 dump (or a purpose-built test ROM) to
  grade against. The clean detection + banking port itself is straightforward
  from gambatte when an oracle is available.

### 2.8 MBC6 — MISSING
- Doc: [Pan Docs MBC6](https://gbdev.io/pandocs/MBC6.html): split $4000/$6000
  ROM banks (8KB granularity), split $A000/$B000 4KB RAM banks, plus a
  Macronix flash chip with write/erase protocol.
- Impact: one game ever (Net de Get, JP, needs Mobile Adapter anyway); owner
  has none. docboy has MBC6 rows.
- Effort: **M** banking, **L** with flash program/erase.
- Status: DEFERRED (considered). No in-tree reference implementation (gambatte
  returns `LOADRES_UNSUPPORTED_MBC_MBC6` — it does not emulate MBC6) and no
  graded test ROM, so the Macronix flash program/erase protocol could only be
  built blind from the datasheet. Waits for an oracle; the game also needs the
  Mobile Adapter (§8.4) to do anything, so it is doubly low-value.

### 2.9 TAMA5 (Bandai Tamagotchi 3) — MISSING
- Doc: gb-ctr `chapter/cartridges/tama5.typ` (partial, with TODO markers —
  the TAMA5 is only partly understood upstream);
  [Pan Docs "Other MBCs"](https://gbdev.io/pandocs/othermbc.html) does not
  cover it. RTC + EEPROM behind a nibble-wide command interface at $A000/$A001.
- Impact: owner has none. "Game de Hakken!! Tamagotchi 3" only. Census lists
  TAMA5 as a no-public-oracle gap.
- Effort: **M**, oracle-poor (validate against gb-ctr + GBE+ notes).
- Status: DEFERRED (considered). No in-tree reference (gambatte returns
  `LOADRES_UNSUPPORTED_MBC_TAMA5`), no graded test ROM, and gb-ctr's chapter is
  itself partial (TODO markers — the chip is only partly understood upstream).
  The nibble-wide RTC+EEPROM command interface would be pure guesswork here;
  waits for a real cart or a settled spec.

### 2.10 M161 — DONE
- Doc: [Pan Docs M161](https://gbdev.io/pandocs/M161.html): single latched
  whole-32KB bankswitch (one shot, locks until reset).
- Status: implemented (`CartridgeType::M161`), a direct port of gambatte
  `m161.cpp`: the FIRST write anywhere in $0000-$7FFF latches the 32KB pair
  from data bits 0-2 (even 16KB half at $0000-3FFF, odd at $4000-7FFF); later
  writes are ignored until reset; the external-RAM line is permanently off.
  Content-detected exactly like gambatte's `presumedM161` (256KB image, header
  $10, title "TETRIS SET") so the MBC3-spoofing header never misroutes a real
  cart. Unit tested (latch-once, even/odd mapping, RAM disabled, no misroute).
- Impact: one cart ever (Mani 4 in 1, JP); owner has none — completeness only.

### 2.11 Unlicensed mappers — IMPLEMENTED (unlicensed-best-effort)

Content-based detection (`cartridge.rs detect_unl_mapper` -> `UnlMapper`)
overrides the spoofed header type byte; references are the two emulators with
verified support (hhugboy `CartDetection.cpp`/`MbcUnl*.cpp`, mGBA
`_detectUnlMBC`/`unlicensed.c`), [Pan Docs Other MBCs](https://gbdev.io/pandocs/othermbc.html),
and the [gbdev forum thread](https://gbdev.gg8.se/forums/viewtopic.php?id=948).
Detection was swept over the owner's full 1148-zip collection + all 4535 suite
ROMs: exactly the 15 target unlicensed dumps hit, zero licensed carts.

- Wisdom Tree: whole-32KB switch, bank = low 6 bits of the **address** of any
  $0000-3FFF write. Detected via the $C0+$D1 header magic or (type $00, >32KB)
  the "WISDOM TREE" publisher string.
- Rocket Games $97/$99: inner 16KB bank at $3F00, outer 256KB bank at $3FC0
  (2-in-1 sub-game select), boot lock with the logo-XOR window. Detected via
  the Rocket logo checksum (2756).
- Sachen MMC1/MMC2: base/mask outer banking, $01xx address descramble, boot
  lock phases (locked reads force RA7 high); `skip_bios` seeds the SACHEN
  logo tiles MMC1 games check in VRAM. Detected via Nintendo/Sachen logo
  sums at the scrambled offsets — fires only on raw scrambled dumps; the
  GoodGBx descrambled singles correctly stay plain MBC1.
- Makon/NT old 1/2: MBC1/MBC3-style banking + $5000-$5FFF multicart base /
  bank-window / bank-line-swap registers. Plus hhugboy's plain-MBC1 routes
  for the header liars (SONIC5 $EA, TETRIS/Captain Knick-Knack, 256KB POCKET
  MONSTER), with garbage ROM/RAM-size header bytes now non-fatal.

Still missing (owner impact none): EMS ($1B+region $E1 magic,
[pandocs#423](https://github.com/gbdev/pandocs/issues/423)); NT-new and
Sintax-proper boards (no owner carts; no auto-detection exists in either
reference emulator — GBX-footer/manual-only there).

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

### 3.1 Plain STOP (low-power mode) — DONE (Pan Docs STOP chart)
- Doc: [Pan Docs "Reducing Power Consumption"](https://gbdev.io/pandocs/Reducing_Power_Consumption.html)
  + the Lior Halphon STOP chart embedded there; gb-ctr instruction set (STOP).
- Status: **implemented to the chart** (`cpu/opcodes.rs::stop` fall-through,
  `gb.rs::step_instruction` freeze/wake, `ppu/controller.rs::enter_stop_mode_panel`):
  - Button held on a SELECTED line (`JOYP & 0xF != 0xF`, checked before the
    KEY1 arm, SameBoy-style): pending IE&IF → 1-byte NOP (no DIV reset);
    none pending → 2-byte, HALT mode entered, DIV not reset.
  - No button, KEY1 armed: the existing sub-dot spsw speed-switch path,
    untouched (age spsw / gambatte speedchange stayed byte-identical).
  - No button, no switch: STOP mode — DIV reset through the FF04 write path
    (Tima::divReset glitch edge + DIV-APU fold), whole-machine clock freeze
    (master_cc stops: timer/PPU/APU/serial/DMA), 1-byte iff IE&IF pending
    (the operand executes on wake), terminated ONLY by a selected P10-P13
    line going low (+8 T-cycles wake advance, SameBoy-matched).
  - Panel per Pan Docs: DMG blanks to white; CGB goes black unless mid-mode-3
    (keeps the picture). Verified against the daid real-reference PNGs at the
    FINAL held frame (all-white / all-black / kept-PASS-text respectively).
  - Micro-checks: `gb.rs stop_tests` (6 tests: DIV-reset+freeze+selected-line
    wake, 1-byte pending form, NOP form, HALT form, panel, armed tripwire).
- Deferred (documented in `opcodes.rs`): the armed+pending IME-on chart leaf
  ("1-byte, mode doesn't change, switch happens") is approximated by the
  armed path's existing pending-interrupt early wake from the 0x20000 window;
  the armed+pending IME-off "CPU glitches non-deterministically" corruption
  is intentionally not invented (SameBoy also models it as the deterministic
  continue). The DMG single-black-line panel artifact (row unpinned, panel
  physics) is unmodeled. The MBC3 RTC crystal (cart-local, really keeps
  counting through STOP) freezes with master_cc — accepted simplification.

### 3.2 Joypad line edge from JOYP select writes — DONE
- Doc: [Pan Docs Interrupt Sources](https://gbdev.io/pandocs/Interrupt_Sources.html)
  (joypad IRQ on any P10-P13 high→low edge) + gb-ctr `peripherals/p1.typ`:
  writing P14/P15 selects while a button is held produces such an edge.
- Status: implemented — the JOYP write path now compares the old/new low
  nibble and requests IF.4 on any high→low transition (passing through the
  8-dot input filter), so a select write while a button is held raises the
  joypad interrupt exactly like hardware. This is the documented STOP-wake
  trigger.

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

### 6.1 Infrared communication (RP $FF56) — DONE (transport landed)
- Doc: [Pan Docs CGB Registers](https://gbdev.io/pandocs/CGB_Registers.html)
  (RP) + [Infrared Communication](https://gbdev.io/pandocs/IR.html).
- Status: implemented (`ir` module). The register bits are exact and RP bit 1
  now reads a real receiver: a connected peer's emitter (RP bit 0) illuminates
  it, gated on the read-enable bits 6-7 ($C0), one GBC never seeing its own LED.
  `GB::connect_ir(a, b)` points two instances at each other through a shared
  Arc<Mutex> level channel (the serial LinkCable pattern — passive, no clock,
  determinism from the harness pump; clones/savestates sever it);
  `attach_ir_peer` takes one end for a socket/process transport;
  `set_ir_loopback` is a self-test. This is the transport the two-player IR
  protocols need (Pokémon G/S/C Mystery Gift, TCG "Card Pop", Pinball score,
  Bomberman trades — all on/off pulse coupling).
- Grounded vs not: the digital emitter/receiver coupling is modelled; the
  analog signal "fade" (~3ms, distance-dependent) and the ambient-light
  $00->$C0 sensing quirk are deliberately not (no digital spec, no GBC<->GBC
  protocol depends on them). Disconnected default keeps the RP path
  byte-identical. Also unblocks HuC1/HuC3 IR modes sharing the same channel.

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

### 7.3 SGB sound commands (SOUND $08, SOU_TRN $09) — PARTIAL (command decode done; audio HLE out of scope)
- Doc: [Pan Docs SGB Sound commands](https://gbdev.io/pandocs/SGB_Command_Sound.html).
- Status: the SOUND command is now fully decoded (`sgb.rs`): Effect A/B codes,
  per-effect pitch (bits 0-1 / 4-5) and volume (bits 2-3 / 6-7), and mute (A
  volume field == 3, per Pan Docs note 1), exposed via `Sgb::sound()` as an
  inspectable `SgbSound`. SOU_TRN is recognised as a sound-data transfer.
- Grounded boundary: making these audible is NOT groundable from public specs —
  it needs an SNES-APU HLE (an SPC700 running the SGB BIOS' N-SPC engine plus a
  copyrighted sound-data ROM), which no public documentation reproduces. So the
  command *decode* layer is complete (for a frontend hook or test); synthesising
  audio is deliberately not attempted rather than inventing samples.
- Impact: SGB-side jingles/effects stay silent until a SNES-APU HLE + sound ROM
  is available. No GB-visible effect, so the suites are byte-identical.

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

### 7.5 SGB unlock gate ($0146/$014B) — DONE
- Doc: [Pan Docs "Unlocking and Detecting SGB Functions"](https://gbdev.io/pandocs/SGB_Unlocking.html):
  header SGB flag $03 + old licensee $33 required, otherwise "it cannot access
  any of the special SGB functions".
- Status: implemented — SGB command-packet dispatch is now gated on the header
  ($0146 == $03 && $014B == $33) at insert time, so only SGB-flagged carts may
  drive the SGB functions, matching hardware.

---

## 8. Serial / link cable

### 8.1 Link peer transport & external clock — CORE DONE (two-GB link cable)
- Doc: [Pan Docs Serial Data Transfer](https://gbdev.io/pandocs/Serial_Data_Transfer_(Link_Cable).html);
  gb-ctr `peripherals/serial.typ`.
- Status: implemented. A `SerialDevice::Link` variant (`serial.rs`) joins two
  GB instances through a shared `LinkCable` (an `Arc<Mutex<..>>` exchange
  buffer with a per-side `{live_sb, armed, armed_internal, deposit}` handshake
  — no wall clock, no threads required; determinism is inherited from the
  pump schedule). Both clock roles are modeled:
  - **Internal clock (master).** At the SC=$8x write the device latches the
    peer side's live shift register (`LinkStart::Ready`) and the transfer runs
    on the *exact same* DIV-aligned schedule as a disconnected cable — a ready
    peer never perturbs internal-clock timing (proven byte-identical vs the
    disconnected oracle in `link_two_instance_exchange_bytes_cc_and_irq`). If
    the peer instance has not armed yet the transfer *holds* (`AwaitPeer`): the
    shift clock freezes (SC.7 stays set, no bits move, no IRQ) until the peer
    arms, at which point the 8-bit window is re-anchored (DIV-snapped) at the
    arm cc — hardware-faithful "master stalls for the slave". A stall timeout
    (`LINK_STALL_TIMEOUT_CC`, ~4 frames) falls back to the peer's live SB so a
    partner that never joins the link menu degrades to the disconnected UX
    instead of hanging the game.
  - **External clock (slave).** Idle serial polls the cable each dot; when the
    master's completed window deposits its byte, the slave's SB takes it, SC.7
    clears and the serial IRQ fires at *this* instance's cc (external clock
    edges arrive asynchronously to anything local, exactly like hardware). A
    side that never armed SC.7 still gets its shift register clocked through
    (SB replaced, no flag/IRQ).
  - **Both-internal clock conflict** (both sides drive the clock): each side
    completes its own window against the other's live byte, index-locked
    across all 8 bytes (`link_both_internal_clock_conflict_exchanges`).
  - **CGB double-speed / fast clock** (SC.1): the master window is 8×16 cc; a
    DMG slave follows at the master's rate
    (`link_cgb_fast_clock_master_dmg_slave`).
  Disconnected is still the default and stays byte-identical (the whole serial
  suite matrix — samesuite 70+6+2, blargg 15+41, mooneye 193, sketchtests 6,
  gambatte floor 9 — is unchanged; a link cable never touches any codepath
  until `connect_link`/`attach_link_peer` is called). `LinkPeer` clones/
  savestates sever the cable (a cloned instance must not ghost-drive its
  twin), behaving like an unplugged end.
- API: `GB::connect_link(&mut a, &mut b)` wires two in-process instances;
  `GB::attach_link_peer(peer)` attaches one end (the other can live behind a
  socket/process transport). `LinkCable::pair()` mints the two ends.
- Validation: `rustyboi-core/src/serial.rs` `#[cfg(test)]` — 8 tests including
  the headless two-instance proof (A sends 0x01..=0x08, B sends 0xA0..=0xA7,
  both receive in order, master cc/IF byte-identical to the disconnected
  oracle, slave completions within lockstep skew), the stall/timeout/severed
  cases, the clock conflict, and the CGB fast clock. Real-game: two Pokémon
  Blue instances connected via `SerialDevice::Link` boot, load the GREG
  battery save, reach CONTINUE with correct stats and walk the Viridian
  overworld (driven headless by the `link_demo` example).
- Frontend integration point (secondary; follows the §8.2 printer pattern):
  the reference driver is `rustyboi-core/examples/link_demo.rs` — it creates
  two `GB`s, `GB::connect_link`s them, and pumps both `run_until_frame` in a
  shared loop (per-frame is enough; the hold/arm handshake absorbs sub-frame
  interleave), with scripted per-side input and PNG capture. A desktop
  two-window or libretro netplay build is the same three lines
  (`connect_link` at session start, drive both loops); a remote/socket peer
  swaps the in-process `LinkCable` for a socket-backed transport behind the
  `attach_link_peer` seam (the cable is the only shared state). Not yet wired
  into the winit/egui menu (single-instance frontend today).
- Remaining: the 4-player adapter (§8.3) and Mobile Adapter (§8.4) build on
  this; a socket transport + desktop two-window UI; full in-game trade
  completion (the transport is proven; reaching the Cable Club tile is pure
  in-game navigation).

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

### 8.3 4-Player Adapter (DMG-07) — DONE (protocol + serial hub)
- Doc: [Pan Docs 4-Player Adapter](https://gbdev.io/pandocs/Four_Player_Adapter.html)
  (external-clock broadcast protocol, ping/transmission phases).
- Status: implemented (`dmg07` module). Full ping/transmission state machine:
  the ping packet `[$FE, STAT1-3]` with the connection bitmap (bits 4-7) + per-
  port player ID (bits 0-2), the `$88/$88/RATE/SIZE` replies, connection
  tracking on the header ACK, the four-`$AA` -> four-`$CC` transmission entry,
  the `SIZE*4` data broadcast with the one-packet delay (P1..P4, zeros for
  absent players), and the `SIZE*4`-`$FF` ping restart. Unit tests reproduce Pan
  Docs' worked byte-example sequences. `SerialDevice::FourPlayer` hooks it into
  the external-clock deposit path (the adapter is the clock master);
  `GB::connect_four_player(&mut [..])` wires 2-4 instances to one shared hub,
  the frontend pumping all instances like `connect_link`.
- Grounded vs not: the byte protocol is exact; the analog packet cadence (~17ms)
  has no capture/test ROM, so the model advances one exchange per armed pull
  (deposit-on-arm) — correct byte sequence, not silicon timing.
- Impact: F-1 Race, Wave Race, Yoshi's Cookie multiplayer (needs a frontend that
  pumps the connected instances).

### 8.4 Mobile Adapter GB — PARTIAL (protocol + session/config; networking is backend)
- Doc: not in Pan Docs; grounded in the REONTeam **libmobile** reference
  (`serial.c`/`commands.c`), the de-facto spec.
- Status: implemented (`mobile` module). Exact port of libmobile's 8-bit serial
  state machine — the `$99 $66` magic, 4-byte header, data, 16-bit big-endian
  checksum, device-ID + acknowledge exchange, `$4B` idle check, and response
  framing (idle byte `$D2`). Wired as `SerialDevice::Mobile`, an internal-clock
  slave like the printer (`GB::attach_mobile_adapter`). Deterministic offline
  commands: START (the "NINTENDO" magic handshake that begins a session, used
  for detection), END, REINIT, CHECK_STATUS (line disconnected), and EEPROM
  config read/write over a local 512-byte config.
- Grounded boundary: the telephone/PPP/TCP/UDP/DNS networking is NOT a
  deterministic emulator feature and has no offline spec (libmobile delegates
  every socket to host callbacks). Those commands return a libmobile-shaped
  error packet so a game fails them cleanly; live connectivity needs a transport
  backend + server (future frontend work).
- Impact: owner has zero Mobile-Adapter games (Net de Get absent); a game can now
  detect + configure the adapter, but online play needs the backend above.
- Same bucket (still missing): Barcode Boy / Barcode Taisen Bardigun reader
  (serial), WorkBoy keyboard (serial) — zero owner impact, listed for
  completeness.

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
| Joypad Input | COMPLETE incl. §3.2 select-write IRQ edge |
| Serial Data Transfer | COMPLETE (link cable §8.1, printer §8.2, 4-player §8.3, Mobile Adapter §8.4) |
| Timer & Divider + Obscure Behaviour | COMPLETE |
| Interrupts / Sources / HALT | COMPLETE (double-halt refetch OPEN-TARGET in flight) |
| CGB Registers | COMPLETE incl. IR transport §6.1 |
| Infrared Communication | DONE — §6.1 (two-instance + loopback) |
| SGB (all 14 sections) | PARTIAL — §7 (protocol/palettes/ATTR/border done; SOUND §7.3 decoded, synth needs SNES-APU HLE; OBJ_TRN/PAL_PRI §7.4 not) |
| CPU (specs, registers, instruction set) | COMPLETE (illegal-op lockup included) |
| Cartridge header | COMPLETE parse; §7.5 SGB unlock gate enforced |
| No MBC | COMPLETE incl. $08/$09 external RAM — §2.1 |
| MBC1 / MBC2 / MBC3 / MBC5 (+MBC1M, MBC30) | COMPLETE |
| MBC6 | MISSING (deferred, no oracle) — §2.8 |
| MBC7 | COMPLETE — §2.4 |
| MMM01 | MISSING (deferred, no oracle) — §2.7 |
| M161 | COMPLETE — §2.10 |
| HuC1 | COMPLETE — §2.5 |
| HuC-3 | COMPLETE — §2.4 |
| Other MBCs (Wisdom Tree, Rocket, Sachen, Makon) | IMPLEMENTED — §2.11 (EMS/multicart magics still missing) |
| Game Boy Printer | DONE — §8.2 |
| Game Boy Camera | DONE — §2.6 (mapper + M64282FP pipeline; webcam feed is a frontend opt-in) |
| 4-Player Adapter | DONE — §8.3 (protocol + hub) |
| Mobile Adapter GB | PARTIAL — §8.4 (protocol/session/config; networking = backend) |
| Game Genie / Shark | COMPLETE (core hooks) |
| Power-Up Sequence | COMPLETE — §9 |
| Reducing Power Consumption (STOP) | COMPLETE — §3.1 (glitch/panel-line leaves deferred) |
| Accessing VRAM/OAM (locking) | COMPLETE |
| OAM Corruption Bug | COMPLETE **[R]** |
| External Connectors / GBC Approval | n/a (physical/process) |

gb-ctr-only chapters cross-checked: SM83 core & timing (COMPLETE), clocks
(COMPLETE), P1 port (§3.2 edge), boot ROM chapter (COMPLETE), DMA chapter
incl. its "OAM DMA bus conflicts: TODO" (we exceed the reference), MBC30
(COMPLETE), TAMA5 (§2.9).

---

## 11. Prioritized roadmap (owner-impact x correctness, with effort)

The owner-impact roadmap below is **fully landed** (every row 1-15 DONE); the
table is kept as a record. What remains open is the completeness backlog and
the deferred CGB IR transport (owner-deferred), listed after it.

| # | Item | Owner impact | Status |
|---|---|---|---|
| 1 | MBC7 (Kirby's Tilt 'n' Tumble) (§2.4) | 1 marquee game | DONE |
| 2 | HuC-3 (Robopon Sun) (§2.4) | 1 game | DONE |
| 3 | MBC3 RTC persistence: `.rtc` sidecar + wall-clock catch-up (§2.2) | 13 games incl. all Pokémon G/S/C | DONE |
| 4 | HuC1 banking + IR-mode register (§2.5) | Pokémon Card GB un-broken | DONE |
| 5 | Game Boy Printer serial device → PNG (§8.2) | 10+ games' print features | DONE |
| 6 | SGB ATTR geometry + ATTR_TRN/ATTR_SET store (§7.1) | 185 SGB games colorize | DONE |
| 7 | SGB border CHR_TRN/PCT_TRN + 256x224 output (§7.2) | 185 SGB games | DONE (core) |
| 8 | Link-cable peer transport + external clock (§8.1) | all 2-player/trading | DONE (core) |
| 9 | Game Boy Camera mapper + M64282FP pipeline (§2.6) | owned cart | DONE |
| 10 | Plain-STOP mode per the STOP chart (§3.1) | correctness + gbc-hw-tests | DONE |
| 11 | ROM+RAM $08/$09 NoMBC external RAM (§2.1) | homebrew/mis-dumps | DONE |
| 12 | Rumble output wiring (libretro) (§2.3) | 16 games | DONE |
| 13 | JOYP select-write joypad-IRQ edge (§3.2) | correctness | DONE |
| 14 | CGB DMG-compat per-game boot palette table (§6.2) | 619 DMG games on CGB | DONE |
| 15 | Unlicensed mappers: Wisdom Tree / Rocket / Sachen / Makon (§2.11) | 29 owned carts | DONE |

**Open completeness backlog** (zero owner impact; each requires an oracle we do
not yet have — a real cart or a graded test ROM — so all are held rather than
shipped blind):

- CGB IR transport (§6.1) — **DONE** (two-instance + loopback channel; Mystery
  Gift, TCG Card Pop, Pinball, Bomberman trades).
- 4-Player Adapter DMG-07 (§8.3) — **DONE** (ping/transmission protocol + serial
  hub; needs a frontend that pumps the connected instances for live play).
- Mobile Adapter GB (§8.4) — **PARTIAL/DONE**: protocol + session/config done;
  live networking needs a transport backend (not a deterministic feature).
- SGB SOUND (§7.3) — command **decode DONE**; audible synthesis needs an
  SNES-APU HLE + sound ROM (not groundable from public specs).
- M161 (§2.10) — **DONE** (gambatte port).
- MMM01 (§2.7), MBC6 (§2.8), TAMA5 (§2.9) — DEFERRED, no in-tree reference or
  test ROM (see each section for the specific blocker).
- OBJ_TRN/PAL_PRI (§7.4, M); SGB unlock gate — DONE.
- Boot-ROM acceptance for DMG0/SGB/CGB0/AGB dumps (§6.4, S) — needs the actual
  dumps to derive verifiable masked-CRCs; deferred rather than guess constants.
- compat-path boot DIV formula (§6.3, M); Barcode Boy / WorkBoy serial
  peripherals (§8.4, zero owner impact).

Already-tracked accuracy frontier (not re-listed here): see
`KNOWN_FAILURES.md` (17 cases, all proven floors or in-flight open targets)
and the internal-suite fleet notes.
