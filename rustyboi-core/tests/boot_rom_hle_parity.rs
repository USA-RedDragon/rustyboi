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
    // …and DMG-compat cart: the boot ROM's longer compat path, -1 cc residual.
    Model { hw: Hardware::CGB,  bios: "cgb_boot.bin",  logo: Logo::At(0x42), cgb_flag: 0x00, div_gap: -1, io_exceptions: &[] },
    Model { hw: Hardware::CGBE, bios: "cgbE_boot.bin", logo: Logo::At(0x42), cgb_flag: 0x00, div_gap: -1, io_exceptions: &[] },
    Model { hw: Hardware::AGB,  bios: "agb_boot.bin",  logo: Logo::At(0x42), cgb_flag: 0x00, div_gap: -1, io_exceptions: &[] },

    // CGB0, DMG-compat cart — the path mooneye's cgb0 rows measure: -1 cc.
    Model { hw: Hardware::CGB0, bios: "cgb0_boot.bin", logo: Logo::At(0x42), cgb_flag: 0x00, div_gap: -1, io_exceptions: CGB0_QUIRKS },
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
    let Some(diffs) = diff_model(m) else { return };
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
