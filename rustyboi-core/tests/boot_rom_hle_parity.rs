//! HLE post-boot state vs. the REAL boot ROM, per model.
//!
//! rustyboi boots every model from a hand-written post-boot state
//! (`GB::skip_bios`) so that **boot ROMs are never required to run a game**.
//! `GB::load_bios_bytes` + `GB::run_boot_rom` is the opt-in path that instead
//! executes the genuine boot ROM from PC=0. This suite runs both on the *same*
//! cartridge and diffs the hand-off state, so a drift in either path is caught.
//!
//! **Why a Rust test and not a first-party test ROM.** Post-boot register state
//! is normally GB-observable, and it is already covered by ROMs: mooneye's
//! `boot_regs-dmg0/dmgABC/mgb/sgb/sgb2/cgb/A` and `boot_div-*` are in-suite and
//! green. This test deliberately does **not** duplicate those. What it checks is
//! an A/B of two *emulator run modes* on one cartridge — a ROM cannot express
//! that, because a ROM cannot ask the emulator to boot itself two different
//! ways. The ROM suites pin the values against silicon; this pins the two
//! rustyboi code paths against each other.
//!
//! **Oracle split.** For the *values*, the mooneye ROMs are the silicon
//! authority. For the *sequence*, the real boot ROM is authoritative — but only
//! as faithfully as rustyboi executes it, which is exactly what a divergence
//! here tells us. So this suite never "fixes" an HLE constant: it pins the
//! current, documented truth and fails loudly if either side moves.
//!
//! Skips silently when `bios/` is absent (mirrors
//! `cgb_compat_palette::tables_match_cgb_boot_bin`) so CI without dumps is green.

use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Hardware, GB};

/// Where each boot ROM revision keeps its copy of the header logo. Offsets
/// only — no logo bytes live in rustyboi. `Mmio::seed_rocket_boot_logo` reaches
/// the same offsets at runtime by searching the image
/// (`cartridge::find_logo_in_boot_rom`); this table is the independently
/// written expectation that pins it. `None` means the revision carries no copy
/// at all and performs no logo check.
#[derive(Clone, Copy)]
enum Logo {
    At(usize),
    None,
}

struct Model {
    hw: Hardware,
    bios: &'static str,
    logo: Logo,
    /// Cartridge CGB flag at 0x143. 0x80 takes the CGB boot ROM's full-CGB
    /// hand-off path, 0x00 its DMG-compatibility path — the two differ in
    /// hand-off DIV *and* register set, so both are exercised.
    cgb_flag: u8,
    /// `hle_div_counter - real_div_counter` at hand-off. The internal 16-bit
    /// timer counter, not the DIV byte. See `EXPECTED` for the per-row rationale.
    div_gap: i32,
    /// I/O registers allowed to differ, with the reason. Anything else that
    /// differs fails the test.
    io_exceptions: &'static [u16],
}

/// PPU *phase* at hand-off. `skip_bios` starts the panel from a clean frame
/// (LY=0), while the real boot ROM hands off wherever the scanline counter
/// happens to be. Neither is a post-boot "constant", so both registers are
/// excluded on every model rather than pinned to a timing-sensitive value.
const PPU_PHASE: &[u16] = &[0xFF41, 0xFF44];

/// CGB0-only divergences, kept as findings rather than silently tolerated:
///   * FF30-FF3F (wave RAM) — the CGB0 boot ROM leaves wave RAM all-zero, but
///     `skip_bios` seeds every CGB-like model with the CGB-A..E post-boot
///     alternating 00/FF pattern.
///   * FF48/FF49 (OBP0/OBP1) — the CGB0 boot ROM zeroes the object palettes;
///     `skip_bios` excludes CGB0 from its `obp_init = 0x00` arm, leaving 0xFF.
///
/// Both are reported, not repaired: CGB0's HLE row is pinned by the mooneye
/// `boot_regs-cgb0` / `boot_div-cgb0` ROMs, which use DMG-flagged carts, and
/// changing it needs a silicon oracle rather than this A/B.
const CGB0_QUIRKS: &[u16] = &[
    0xFF30, 0xFF31, 0xFF32, 0xFF33, 0xFF34, 0xFF35, 0xFF36, 0xFF37,
    0xFF38, 0xFF39, 0xFF3A, 0xFF3B, 0xFF3C, 0xFF3D, 0xFF3E, 0xFF3F,
    0xFF48, 0xFF49,
];

/// CGB0 on a full-CGB cart additionally reads a different DIV byte, because
/// that row's hand-off counter is the unpinned one (see its `EXPECTED` entry).
/// The counter itself is still gated, by `div_gap`.
const CGB0_CGB_CART_QUIRKS: &[u16] = &[
    0xFF30, 0xFF31, 0xFF32, 0xFF33, 0xFF34, 0xFF35, 0xFF36, 0xFF37,
    0xFF38, 0xFF39, 0xFF3A, 0xFF3B, 0xFF3C, 0xFF3D, 0xFF3E, 0xFF3F,
    0xFF48, 0xFF49, 0xFF04,
];

const EXPECTED: &[Model] = &[
    // DMG-family: register set is byte-identical; the HLE counter sits 8 cc (2
    // M-cycles) past the FF50 write that ends `run_boot_rom`.
    Model { hw: Hardware::DMG0, bios: "dmg0_boot.bin", logo: Logo::At(0xCB), cgb_flag: 0x00, div_gap: 8, io_exceptions: &[] },
    Model { hw: Hardware::DMG,  bios: "dmg_boot.bin",  logo: Logo::At(0xA8), cgb_flag: 0x00, div_gap: 8, io_exceptions: &[] },
    Model { hw: Hardware::MGB,  bios: "mgb_boot.bin",  logo: Logo::At(0xA8), cgb_flag: 0x00, div_gap: 8, io_exceptions: &[] },

    // ---- SGB / SGB2: the Phase-7 subject. ----
    // Register set (A/F/B/C/D/E/H/L/SP/PC/IME/IE) and every I/O register
    // including JOYP (FF00) and NR52 (FF26) match the real boot ROM exactly.
    //
    // DIV is the one divergence, and it is a genuine, documented conflict —
    // NOT a bug fixed here. `skip_bios` models the SGB hand-off counter as
    // `0xDC88 - 4*popcount(header[$104..$14F])`, i.e. the boot ROM bit-bangs the
    // header to the SNES over FF00 and a set bit ships one M-cycle faster. That
    // law is pinned by mooneye `boot_div-S` / `boot_div2-S`, which pass.
    //
    // Executing the genuine sgb_boot.bin here instead hands off at 0xD840 for
    // this cartridge, where the law gives 0xDC08 (popcount 32) — 968 cc apart,
    // i.e. DIV reads 0xD8 rather than 0xD9. And the real-boot counter shows *no
    // measurable popcount dependence at all*: sweeping the header popcount over
    // 6..17 (where the law predicts a monotone 44 cc slide) leaves it inside
    // 0xD844..0xD858 with no trend. rustyboi has no SNES attached, so the boot
    // ROM's FF00 packet loop never sees the real handshake and its duration is
    // not a trustworthy DIV oracle; mooneye is silicon-verified, so the HLE
    // constant stands. The gap is pinned exactly so that fixing the SGB boot
    // port — or drifting the HLE constant — fails here and forces the two
    // oracles to be reconciled deliberately rather than silently.
    Model { hw: Hardware::SGB,  bios: "sgb_boot.bin",  logo: Logo::None, cgb_flag: 0x00, div_gap: 968, io_exceptions: &[0xFF04] },
    Model { hw: Hardware::SGB2, bios: "sgb2_boot.bin", logo: Logo::None, cgb_flag: 0x00, div_gap: 968, io_exceptions: &[0xFF04] },

    // CGB-family, full-CGB cart: identical register set, +3 cc residual.
    Model { hw: Hardware::CGB,  bios: "cgb_boot.bin",  logo: Logo::At(0x42), cgb_flag: 0x80, div_gap: 3, io_exceptions: &[] },
    Model { hw: Hardware::CGBE, bios: "cgbE_boot.bin", logo: Logo::At(0x42), cgb_flag: 0x80, div_gap: 3, io_exceptions: &[] },
    Model { hw: Hardware::AGB,  bios: "agb_boot.bin",  logo: Logo::At(0x42), cgb_flag: 0x80, div_gap: 3, io_exceptions: &[] },
    // …and DMG-compat cart: the boot ROM's longer compat path. Same +3 cc
    // residual as the CGB-cart rows above — the whole CGB family now sits at one
    // uniform offset on both cart paths.
    //
    // This row read -1 until the HLE learned that the compat hand-off is
    // CART-CONTENT dependent. Every one of these four boot ROM images carries,
    // verbatim, `21 4B 01 / 7E / FE 33 / 20 0B` — ld hl,$014B; ld a,(hl); cp $33;
    // jr nz — at 0x0475 (0x046F on CGB0, whose image is shifted but not changed
    // here). The taken branch costs 12 cc against 8 not taken, and the two arms
    // it selects reconverge 4 cc apart, so a cart with $014B != $33 hands off
    // exactly 4 cc later. Executing each image confirms it: hand-off moves
    // 0x2881->0x2885 (CGB0), 0x2675->0x2679 (CGB/CGBE), 0x2679->0x267D (AGB)
    // when only $014B changes.
    //
    // `cartridge_for` leaves $014B at 0x00, so these rows ride the != $33 arm
    // while the HLE constants are pinned by mooneye's boot_div carts, which all
    // carry $014B == $33. The old -1 was therefore not a residual at all: it was
    // a $33-calibrated constant measured against a non-$33 run, i.e. it recorded
    // the 4 cc the HLE was short by. -1 + 4 = +3 restores the family-wide offset.
    // `licensee_branch_costs_four_cc_on_every_cgb_boot_rom` below pins the +4 to
    // the images themselves so this can never silently drift back.
    Model { hw: Hardware::CGB,  bios: "cgb_boot.bin",  logo: Logo::At(0x42), cgb_flag: 0x00, div_gap: 3, io_exceptions: &[] },
    Model { hw: Hardware::CGBE, bios: "cgbE_boot.bin", logo: Logo::At(0x42), cgb_flag: 0x00, div_gap: 3, io_exceptions: &[] },
    Model { hw: Hardware::AGB,  bios: "agb_boot.bin",  logo: Logo::At(0x42), cgb_flag: 0x00, div_gap: 3, io_exceptions: &[] },

    // CGB0, DMG-compat cart — the path mooneye's cgb0 rows measure. CGB0's boot
    // ROM is NOT the CGB-A..E image (602 bytes differ, and the compat block sits
    // 6 bytes lower), so the $014B branch above was confirmed present in
    // cgb0_boot.bin specifically rather than inherited.
    Model { hw: Hardware::CGB0, bios: "cgb0_boot.bin", logo: Logo::At(0x42), cgb_flag: 0x00, div_gap: 3, io_exceptions: CGB0_QUIRKS },
    // CGB0, full-CGB cart: `skip_bios` documents this counter as unpinned (CGB0
    // only ever runs the mooneye boot rows, all DMG-flagged carts, so the
    // 0x7D8 compat delta cannot be assumed). The real boot ROM says 0x20A9 —
    // recorded here as the finding, not promoted to an HLE constant without a
    // silicon oracle.
    Model { hw: Hardware::CGB0, bios: "cgb0_boot.bin", logo: Logo::At(0x42), cgb_flag: 0x80, div_gap: 2011, io_exceptions: CGB0_CGB_CART_QUIRKS },
];

fn bios_bytes(name: &str) -> Option<Vec<u8>> {
    let path = format!("{}/../bios/{}", env!("CARGO_MANIFEST_DIR"), name);
    let Ok(bin) = std::fs::read(path) else { return None };
    Some(bin)
}

/// A minimal 32 KiB no-MBC cartridge with a valid header checksum, and the
/// header logo lifted out of the boot ROM being tested so that revision's logo
/// check passes without rustyboi embedding any logo bytes.
fn cartridge_for(m: &Model, bios: &[u8]) -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    // 0x100: nop; jp 0x0150 — the usual entry stub.
    rom[0x101] = 0xC3;
    rom[0x102] = 0x50;
    rom[0x103] = 0x01;
    if let Logo::At(off) = m.logo {
        rom[0x104..0x134].copy_from_slice(&bios[off..off + 48]);
    }
    rom[0x134..0x13C].copy_from_slice(b"RUSTYBOI");
    rom[0x143] = m.cgb_flag;
    // 0x147/0x148/0x149 stay 0: no MBC, 32 KiB, no RAM.
    let mut sum = 0u8;
    for b in &rom[0x134..0x14D] {
        sum = sum.wrapping_sub(*b).wrapping_sub(1);
    }
    rom[0x14D] = sum;
    rom
}

struct Post {
    regs: [(&'static str, u8); 8],
    sp: u16,
    pc: u16,
    ime: bool,
    ie: u8,
    io: [u8; 0x80],
    div_counter: u16,
}

fn capture(gb: &GB) -> Post {
    let r = gb.get_cpu_registers();
    let mut io = [0u8; 0x80];
    for (i, b) in io.iter_mut().enumerate() {
        *b = gb.read_memory(0xFF00 + i as u16);
    }
    Post {
        regs: [
            ("A", r.a), ("F", r.f), ("B", r.b), ("C", r.c),
            ("D", r.d), ("E", r.e), ("H", r.h), ("L", r.l),
        ],
        sp: r.sp,
        pc: r.pc,
        ime: r.ime,
        ie: gb.read_memory(0xFFFF),
        io,
        div_counter: gb.timer_internal_counter(),
    }
}

fn io_name(addr: u16) -> &'static str {
    match addr {
        0xFF00 => "JOYP", 0xFF01 => "SB", 0xFF02 => "SC", 0xFF04 => "DIV",
        0xFF05 => "TIMA", 0xFF06 => "TMA", 0xFF07 => "TAC", 0xFF0F => "IF",
        0xFF26 => "NR52", 0xFF40 => "LCDC", 0xFF41 => "STAT", 0xFF42 => "SCY",
        0xFF43 => "SCX", 0xFF44 => "LY", 0xFF45 => "LYC", 0xFF47 => "BGP",
        0xFF48 => "OBP0", 0xFF49 => "OBP1", 0xFF4A => "WY", 0xFF4B => "WX",
        0xFF4C => "KEY0", 0xFF4D => "KEY1", 0xFF4F => "VBK", 0xFF50 => "BOOT",
        0xFF56 => "RP", 0xFF70 => "SVBK", _ => "",
    }
}

/// Boot `m` both ways on one cartridge and return every field that differs.
/// `None` when the boot ROM is not on disk.
fn diff_model(m: &Model) -> Option<Vec<String>> {
    let bios = bios_bytes(m.bios)?;
    let rom = cartridge_for(m, &bios);

    let mut real = GB::new(m.hw);
    real.insert(Cartridge::from_bytes(&rom).expect("build cartridge"));
    real.load_bios_bytes(&bios)
        .unwrap_or_else(|e| panic!("{:?}: {} rejected: {e}", m.hw, m.bios));
    real.run_boot_rom();
    // `run_boot_rom` returns when the boot ROM unmaps itself by writing FF50 —
    // exactly the instruction after which execution would leave boot-ROM space.
    // If the ROM instead spun (bad logo, wedged check) it burns the step ceiling
    // and never unmaps, so assert the hand-off really happened.
    assert_eq!(
        real.read_memory(0xFF50),
        0xFF,
        "{:?}: {} never handed off (boot ROM still mapped)",
        m.hw,
        m.bios
    );

    let mut hle = GB::new(m.hw);
    hle.insert(Cartridge::from_bytes(&rom).expect("build cartridge"));
    hle.skip_bios();

    let (r, h) = (capture(&real), capture(&hle));
    let mut diffs = Vec::new();
    for (&(name, rv), &(_, hv)) in r.regs.iter().zip(h.regs.iter()) {
        if rv != hv {
            diffs.push(format!("{name:<10} real=0x{rv:02X} hle=0x{hv:02X}"));
        }
    }
    if r.sp != h.sp {
        diffs.push(format!("{:<10} real=0x{:04X} hle=0x{:04X}", "SP", r.sp, h.sp));
    }
    if r.pc != h.pc {
        diffs.push(format!("{:<10} real=0x{:04X} hle=0x{:04X}", "PC", r.pc, h.pc));
    }
    if r.ime != h.ime {
        diffs.push(format!("{:<10} real={} hle={}", "IME", r.ime, h.ime));
    }
    if r.ie != h.ie {
        diffs.push(format!("{:<10} real=0x{:02X} hle=0x{:02X}", "IE", r.ie, h.ie));
    }
    for i in 0..0x80usize {
        let addr = 0xFF00 + i as u16;
        if r.io[i] != h.io[i]
            && !PPU_PHASE.contains(&addr)
            && !m.io_exceptions.contains(&addr)
        {
            diffs.push(format!(
                "{:04X} {:<5} real=0x{:02X} hle=0x{:02X}",
                addr,
                io_name(addr),
                r.io[i],
                h.io[i]
            ));
        }
    }
    let gap = h.div_counter as i32 - r.div_counter as i32;
    if gap != m.div_gap {
        diffs.push(format!(
            "{:<10} real=0x{:04X} hle=0x{:04X} gap={gap:+} (documented {:+})",
            "DIV_CTR", r.div_counter, h.div_counter, m.div_gap
        ));
    }
    Some(diffs)
}

fn check(hw: Hardware, cgb_flag: u8) {
    let m = EXPECTED
        .iter()
        .find(|m| m.hw == hw && m.cgb_flag == cgb_flag)
        .expect("model row");
    // Loud, not fatal: a bios-less CI shard is legitimate (the ROMs are opt-in),
    // but the omission must be visible in the log so a shard that never ran the
    // real-image cross-check is never mistaken for one that passed it.
    let Some(diffs) = diff_model(m) else {
        eprintln!(
            "SKIP boot-ROM A/B for {:?} (cart CGB flag 0x{:02X}): bios/{} absent — \
             real-image cross-check NOT performed",
            m.hw, m.cgb_flag, m.bios
        );
        return;
    };
    assert!(
        diffs.is_empty(),
        "{:?} (cart CGB flag 0x{:02X}): HLE post-boot state diverges from {}:\n  {}",
        m.hw,
        m.cgb_flag,
        m.bios,
        diffs.join("\n  ")
    );
}

/// The Phase-7 gate: SGB's hand-written post-boot state must equal what the
/// genuine SGB boot ROM produces. See the `EXPECTED` row for the one pinned
/// exception (DIV) and why it is not "fixed" here.
#[test]
fn sgb_hle_matches_real_boot_rom() {
    check(Hardware::SGB, 0x00);
}

#[test]
fn sgb2_hle_matches_real_boot_rom() {
    check(Hardware::SGB2, 0x00);
}

/// The same gate for every other model with a shipped boot ROM. Broader cover
/// is nearly free once the harness exists, and it caught the CGB0 wave-RAM /
/// OBP divergences documented on `CGB0_QUIRKS`.
#[test]
fn dmg_family_hle_matches_real_boot_rom() {
    check(Hardware::DMG0, 0x00);
    check(Hardware::DMG, 0x00);
    check(Hardware::MGB, 0x00);
}

#[test]
fn cgb_family_hle_matches_real_boot_rom() {
    for hw in [Hardware::CGB0, Hardware::CGB, Hardware::CGBE, Hardware::AGB] {
        for flag in [0x80u8, 0x00] {
            check(hw, flag);
        }
    }
}

/// Every CGB-family boot ROM revision, on the DMG-compat cart path.
const COMPAT_ROWS: [(Hardware, &str); 4] = [
    (Hardware::CGB0, "cgb0_boot.bin"),
    (Hardware::CGB, "cgb_boot.bin"),
    (Hardware::CGBE, "cgbE_boot.bin"),
    (Hardware::AGB, "agb_boot.bin"),
];

/// Build a DMG-compat cartridge with a chosen title and licensee pair, keeping
/// the boot ROM's own logo so the revision's logo check passes.
fn compat_cartridge(bios: &[u8], title: &[u8], old: u8, new: [u8; 2]) -> Vec<u8> {
    let model = Model {
        hw: Hardware::CGB,
        bios: "",
        logo: Logo::At(0x42),
        cgb_flag: 0x00,
        div_gap: 0,
        io_exceptions: &[],
    };
    let mut rom = cartridge_for(&model, bios);
    rom[0x134..0x144].fill(0);
    rom[0x134..0x134 + title.len()].copy_from_slice(title);
    rom[0x144] = new[0];
    rom[0x145] = new[1];
    rom[0x14B] = old;
    let mut sum = 0u8;
    for b in &rom[0x134..0x14D] {
        sum = sum.wrapping_sub(*b).wrapping_sub(1);
    }
    rom[0x14D] = sum;
    rom
}

/// Pin the DMG-compat hand-off's cart-content dependence to the boot ROM images
/// themselves, arm by arm.
///
/// Every CGB-family image branches **three** ways on the licensee bytes at the
/// head of the compat palette setup (0x0475 in the CGB/CGBE/AGB images, 0x046F
/// in CGB0's, which is a genuinely different ROM):
///
/// ```text
///   ld hl,$014B / ld a,(hl) / cp $33 / jr nz $0488
///   $0488: ld l,$4B / ld e,$01 / ldi a,(hl) / cp e / jr nz $04CE
///   $047D: ld l,$44 / ld e,$30 / ldi a,(hl) / cp e / jr nz $04CE
///          inc e / jr $048C
/// ```
///
/// so the cart is Nintendo-published — and gets the title-hash palette lookup —
/// iff `$014B == $01`, or `$014B == $33` with the new-licensee code at
/// $0144-$0145 equal to `"01"`. Everything else falls straight through to
/// $04CE, which is why the four non-Nintendo arms sit within 36 cc of each
/// other while the Nintendo arms run thousands of cycles longer.
///
/// The cost is **not** a constant on the Nintendo side: the checksum table is
/// searched linearly, 48 cc per rejected entry, so it is position dependent
/// (Tetris +1612, Zelda +2772, an unrecognised title +4364), and then the
/// chosen palette is installed through two more data-dependent loops. That is
/// what `cgb_compat_palette::compat_boot_extra_cc` models, and what this
/// re-measures against the genuine images on every revision.
#[test]
fn compat_boot_cost_matches_every_cgb_boot_rom() {
    // (title, old licensee, new licensee, cc after the $33/non-'0' reference)
    const ARMS: &[(&[u8], u8, [u8; 2], i32)] = &[
        // --- non-Nintendo: the three cheap arms ---
        (b"RUSTYBOI", 0x33, *b"ZZ", 0), // the reference; mooneye's boot_div carts
        (b"RUSTYBOI", 0x00, *b"ZZ", 4), // any non-$33 old licensee
        (b"RUSTYBOI", 0xA4, *b"ZZ", 4),
        (b"RUSTYBOI", 0x33, *b"08", 36), // '0'-prefixed but not "01" (Capcom)
        (b"RUSTYBOI", 0x33, *b"00", 36),
        (b"RUSTYBOI", 0x33, *b"0Z", 36),
        // --- Nintendo, both spellings, across the search range ---
        (b"", 0x01, *b"\0\0", 604),          // checksum 0x00 -> table row 0
        (b"TETRIS", 0x01, *b"\0\0", 1612),   // row 29
        (b"POKEMON RED", 0x01, *b"\0\0", 1996),
        (b"ZELDA", 0x01, *b"\0\0", 2772),
        (b"SUPER MARIOLAND", 0x01, *b"\0\0", 3268), // ambiguous row, 4th letter 'E'
        (b"POKEMON BLUE", 0x01, *b"\0\0", 4308),    // ambiguous row, deeper column
        (b"ZZZZZZZZ", 0x01, *b"\0\0", 4364),        // no row at all: full search
        (b"ZZZZZZZZ", 0x33, *b"01", 4396),          // …the $33 spelling, +32 cc
        (b"TETRIS", 0x33, *b"01", 1644),
        // --- the two title checksums the image singles out at $05F8 ---
        // $D000 (the title sum) is compared against the pair at $007A, and a
        // match costs an extra `call $03DA`. Only reachable from a Nintendo arm.
        (b"\x58", 0x01, *b"\0\0", 3828),
        (b"\x43", 0x01, *b"\0\0", 3644),
    ];

    let mut checked = 0;
    for (hw, bios_name) in COMPAT_ROWS {
        let Some(bios) = bios_bytes(bios_name) else { continue };

        // The image must literally contain the three-way branch above; a
        // revision that lacks it is not modelled by `compat_boot_extra_cc` and
        // must fail loudly rather than inherit constants measured elsewhere.
        const SEQ: &[u8] = &[
            0x21, 0x4B, 0x01, // ld hl,$014B
            0x7E, // ld a,(hl)
            0xFE, 0x33, // cp $33
            0x20, 0x0B, // jr nz,$0488
            0x2E, 0x44, // ld l,$44
            0x1E, 0x30, // ld e,$30
            0x2A, 0xBB, // ldi a,(hl) / cp e
            0x20, 0x49, // jr nz,$04CE
            0x1C, // inc e
            0x18, 0x04, // jr $048C
            0x2E, 0x4B, // ld l,$4B
            0x1E, 0x01, // ld e,$01
            0x2A, 0xBB, // ldi a,(hl) / cp e
            0x20, 0x3E, // jr nz,$04CE
        ];
        assert!(
            bios.windows(SEQ.len()).any(|w| w == SEQ),
            "{bios_name}: the three-way licensee branch is not in this image — \
             the compat boot-duration model in cgb_compat_palette does not \
             apply to this revision and must not be inherited blindly",
        );
        // …and the pair of singled-out title checksums it fixes up at $05F8.
        assert_eq!(
            &bios[0x7A..0x7C],
            &[0x58, 0x43],
            "{bios_name}: the $05F8 fixup checksums moved",
        );

        let handoff = |title: &[u8], old: u8, new: [u8; 2]| {
            let rom = compat_cartridge(&bios, title, old, new);
            let mut gb = GB::new(hw);
            gb.insert(Cartridge::from_bytes(&rom).expect("build cartridge"));
            gb.load_bios_bytes(&bios).expect("load bios");
            gb.run_boot_rom();
            assert_eq!(gb.read_memory(0xFF50), 0xFF, "{bios_name}: never handed off");
            gb.timer_internal_counter()
        };

        let reference = handoff(ARMS[0].0, ARMS[0].1, ARMS[0].2);
        for &(title, old, new, expected) in ARMS {
            let measured = handoff(title, old, new).wrapping_sub(reference) as i32;
            assert_eq!(
                measured,
                expected,
                "{bios_name} ({hw:?}): title {title:?} / $014B=0x{old:02X} / \
                 new licensee {:?} hands off {measured:+} cc after the reference \
                 arm, model says {expected:+}",
                std::str::from_utf8(&new).unwrap_or("??"),
            );

            // …and the HLE must agree with the image it was derived from.
            let mut rom = compat_cartridge(&bios, title, old, new);
            rom[0x143] = 0x00;
            let mut hle = GB::new(hw);
            hle.insert(Cartridge::from_bytes(&rom).expect("build cartridge"));
            hle.skip_bios();
            let mut plain = GB::new(hw);
            plain.insert(
                Cartridge::from_bytes(&compat_cartridge(&bios, ARMS[0].0, ARMS[0].1, ARMS[0].2))
                    .expect("build cartridge"),
            );
            plain.skip_bios();
            assert_eq!(
                hle.timer_internal_counter().wrapping_sub(plain.timer_internal_counter()) as i32,
                expected,
                "{bios_name} ({hw:?}): skip_bios disagrees with the image on \
                 title {title:?} / $014B=0x{old:02X}",
            );
        }
        checked += 1;
    }
    // Loud, not fatal: `bios/` is legitimately absent on some CI shards, but the
    // real-image cross-check then did NOT run, so make the omission visible
    // rather than letting a silent zero-row pass look like a green cross-check.
    if checked == 0 {
        eprintln!(
            "SKIP compat_boot_cost_matches_every_cgb_boot_rom: no bios/ CGB-family \
             images present — real-image cross-check NOT performed"
        );
    }
    assert!(
        checked == 0 || checked == COMPAT_ROWS.len(),
        "expected all {} CGB-family compat rows, saw {checked}",
        COMPAT_ROWS.len()
    );
}

/// The named arms above are a curated list, so they can only catch drift in the
/// cases someone thought of. This sweeps the model against the image across the
/// *whole* checksum space instead: every one of the 256 title sums a cart can
/// have, which walks every position of the linear search, every hash miss, both
/// $05F8 fixups and — through the palette each row selects — every combination
/// index and flag pattern the install loops can take.
///
/// The full sweep runs on the canonical CGB image; the other three revisions
/// take a stride through it, which is enough to catch a revision whose tables or
/// loop structure differ while keeping the suite quick.
#[test]
fn compat_boot_cost_matches_the_whole_checksum_table() {
    let mut checked = 0;
    for (hw, bios_name) in COMPAT_ROWS {
        let Some(bios) = bios_bytes(bios_name) else { continue };
        let stride = if hw == Hardware::CGB { 1 } else { 16 };

        let counters = |title: &[u8]| {
            let rom = compat_cartridge(&bios, title, 0x01, *b"\0\0");
            let mut real = GB::new(hw);
            real.insert(Cartridge::from_bytes(&rom).expect("build cartridge"));
            real.load_bios_bytes(&bios).expect("load bios");
            real.run_boot_rom();
            assert_eq!(real.read_memory(0xFF50), 0xFF, "{bios_name}: never handed off");
            let mut hle = GB::new(hw);
            hle.insert(Cartridge::from_bytes(&rom).expect("build cartridge"));
            hle.skip_bios();
            (real.timer_internal_counter(), hle.timer_internal_counter())
        };

        // A one-byte title puts the sum under direct control; the 4th title
        // letter stays 0, so the ambiguous rows exercise their miss path.
        let (real0, hle0) = counters(&[0x00]);
        let mut swept = 0;
        for sum in (0u16..256).step_by(stride) {
            let (real, hle) = counters(&[sum as u8]);
            assert_eq!(
                hle.wrapping_sub(hle0) as i16,
                real.wrapping_sub(real0) as i16,
                "{bios_name} ({hw:?}): title checksum 0x{sum:02X} — skip_bios and \
                 the real boot ROM disagree on the compat hand-off",
            );
            swept += 1;
        }
        assert_eq!(swept, 256 / stride, "{bios_name}: sweep did not run");
        checked += 1;
    }
    // Loud, not fatal: see `compat_boot_cost_matches_every_cgb_boot_rom` — a
    // bios-less shard is allowed, but the skipped real-image sweep must be
    // announced so it is never confused with a passed cross-check.
    if checked == 0 {
        eprintln!(
            "SKIP compat_boot_cost_matches_the_whole_checksum_table: no bios/ \
             CGB-family images present — real-image sweep NOT performed"
        );
    }
    assert!(
        checked == 0 || checked == COMPAT_ROWS.len(),
        "expected all {} CGB-family compat rows, saw {checked}",
        COMPAT_ROWS.len()
    );
}

/// Guard the skip-if-absent escape hatch: when `bios/` *is* populated the suite
/// above must really have executed, not quietly returned. Fails only if a boot
/// ROM file exists but the harness could not use it.
#[test]
fn boot_roms_present_are_actually_exercised() {
    let mut ran = 0;
    for m in EXPECTED {
        if bios_bytes(m.bios).is_some() {
            assert!(diff_model(m).is_some(), "{} readable but not run", m.bios);
            ran += 1;
        }
    }
    eprintln!("boot-ROM A/B: {ran}/{} model rows exercised", EXPECTED.len());
}
