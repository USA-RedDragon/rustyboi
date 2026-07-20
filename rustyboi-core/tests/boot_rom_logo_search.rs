//! Locating the Nintendo logo inside a boot ROM image.
//!
//! `Mmio::seed_rocket_boot_logo` hands the Rocket-Games mapper the logo out of
//! whatever boot ROM is loaded, so its locked-CGB phase can satisfy a running
//! boot ROM's logo check without rustyboi embedding any logo bytes. Getting the
//! *offset* wrong seeds 48 bytes of unrelated boot-ROM code, and the offset is
//! not a function of the image length: DMG0 keeps its copy at $CB where DMG/MGB
//! use $A8, and both images are 256 bytes; SGB/SGB2 have no copy at all.
//!
//! **Why a Rust test and not a first-party test ROM.** The project prefers
//! first-party ROMs whenever behaviour is observable from the Game Boy CPU, and
//! the *consequence* here is GB-observable (a Rocket cart either passes the boot
//! ROM's logo check or hangs). The *cause* is not addressable from a ROM:
//!   * The bug only exists for the DMG0, SGB and SGB2 boot ROMs. `test-roms`
//!     models are `dmg|cgb|agb` — there is no token that selects a DMG0 or SGB
//!     boot image, and on dmg/cgb/agb the old code was already correct, so a
//!     test ROM could not be made to fail.
//!   * A ROM cannot choose which boot ROM the emulator loads, and the test ROM
//!     would itself have to *be* a Rocket cartridge (header logo summing to
//!     2756, mapper $97) to reach the seeding path at all — at which point it is
//!     a mapper fixture, not a test ROM that can report results.
//!
//! What is being pinned is a host-side configuration semantic (which boot ROM
//! image is loaded x how the mapper is seeded), which is the documented
//! carve-out for Rust tests.
//!
//! Dump-backed cases skip silently when `bios/` is absent (mirrors
//! `cgb_compat_palette::tables_match_cgb_boot_bin`) so CI without dumps is
//! green; the synthetic cases always run.

use rustyboi_core_lib::cartridge::{find_logo_in_boot_rom, Cartridge};
use rustyboi_core_lib::gb::{Hardware, GB};
use rustyboi_core_lib::memory::Addressable;

fn bios(name: &str) -> Option<Vec<u8>> {
    let path = format!("{}/../bios/{}", env!("CARGO_MANIFEST_DIR"), name);
    std::fs::read(path).ok()
}

/// Every shipped dump, with the offset independently read out of the image by
/// scanning for the logo's byte signature. `None` = the revision embeds no logo.
const SHIPPED: &[(&str, Option<usize>)] = &[
    ("dmg_boot.bin", Some(0xA8)),
    ("mgb_boot.bin", Some(0xA8)),
    ("dmg0_boot.bin", Some(0xCB)),
    ("sgb_boot.bin", None),
    ("sgb2_boot.bin", None),
    ("cgb_boot.bin", Some(0x42)),
    ("cgb0_boot.bin", Some(0x42)),
    ("cgbE_boot.bin", Some(0x42)),
    ("agb_boot.bin", Some(0x42)),
];

#[test]
fn search_matches_every_shipped_dump() {
    for &(name, want) in SHIPPED {
        let Some(bin) = bios(name) else { continue };
        assert_eq!(
            find_logo_in_boot_rom(&bin),
            want,
            "{name}: logo offset (len {})",
            bin.len()
        );
    }
}

/// A 48-byte window carrying the logo's two checksums, standing in for the real
/// bitmap. No logo bytes are embedded anywhere in rustyboi, so the fixtures the
/// search is exercised against are synthesised from the sums alone: 24 bytes
/// summing to 1492 followed by 24 summing to 3954 (48-byte total 5446).
fn logo_shaped_window() -> [u8; 48] {
    let mut w = [0u8; 48];
    for (i, b) in w.iter_mut().enumerate() {
        *b = if i < 24 { 62 } else { 164 };
    }
    w[23] = 66; // 23 * 62 + 66 == 1492
    w[47] = 182; // 23 * 164 + 182 == 3954
    debug_assert_eq!(w.iter().map(|&b| u32::from(b)).sum::<u32>(), 5446);
    debug_assert_eq!(w[..24].iter().map(|&b| u32::from(b)).sum::<u32>(), 1492);
    w
}

#[test]
fn search_finds_a_logo_shaped_window_anywhere_in_the_image() {
    let logo = logo_shaped_window();
    for off in [0usize, 1, 0x42, 0xA8, 0xCB, 200] {
        let mut image = vec![0u8; 256];
        image[off..off + 48].copy_from_slice(&logo);
        assert_eq!(find_logo_in_boot_rom(&image), Some(off));
    }
}

#[test]
fn search_declines_when_absent_or_ambiguous() {
    // Nothing that matches: no offset invented.
    assert_eq!(find_logo_in_boot_rom(&[0u8; 256]), None);
    // Shorter than one window.
    assert_eq!(find_logo_in_boot_rom(&[0u8; 47]), None);
    // Two candidates: declines rather than picking one arbitrarily.
    let logo = logo_shaped_window();
    let mut image = vec![0u8; 256];
    image[0x10..0x40].copy_from_slice(&logo);
    image[0xA8..0xD8].copy_from_slice(&logo);
    assert_eq!(find_logo_in_boot_rom(&image), None);
}

/// A Rocket Games cartridge: 512 KiB, mapper byte $97, and a header logo whose
/// byte sum is 2756 (what detection keys on). Rocket carts never carry the
/// Nintendo logo — that is precisely why the mapper has to source one from the
/// boot ROM.
fn rocket_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x80000];
    rom[0x147] = 0x97;
    rom[0x148] = 0x04;
    // 48 bytes summing to LOGO_SUM_ROCKET (2756): 47 * 0x39 + 0x4D.
    for b in rom[0x104..0x134].iter_mut() {
        *b = 0x39;
    }
    rom[0x133] = 0x4D;
    rom
}

/// Drive the Rocket boot lock into its locked-CGB phase (0x30 cart reads) and
/// return what $0104 presents there — the byte a running boot ROM's logo check
/// would see.
fn locked_cgb_logo_byte(gb: &mut GB) -> u8 {
    let cart = gb.cartridge_mut().expect("cartridge inserted");
    for _ in 0..0x31 {
        cart.read(0x0000);
    }
    cart.read(0x0104)
}

#[test]
fn rocket_cart_is_seeded_from_dmg0s_own_logo_offset() {
    let Some(bin) = bios("dmg0_boot.bin") else { return };
    let mut gb = GB::new(Hardware::DMG0);
    gb.insert(Cartridge::from_bytes(&rocket_rom()).unwrap());
    gb.load_bios_bytes(&bin).unwrap();
    // DMG0 keeps its logo at $CB. Seeding from the DMG/MGB $A8 hands the mapper
    // unrelated boot-ROM bytes and the logo check fails.
    assert_eq!(locked_cgb_logo_byte(&mut gb), bin[0xCB]);
    assert_ne!(bin[0xCB], bin[0xA8], "offsets must be distinguishable");
}

#[test]
fn rocket_cart_is_seeded_from_dmgs_own_logo_offset() {
    let Some(bin) = bios("dmg_boot.bin") else { return };
    let mut gb = GB::new(Hardware::DMG);
    gb.insert(Cartridge::from_bytes(&rocket_rom()).unwrap());
    gb.load_bios_bytes(&bin).unwrap();
    assert_eq!(locked_cgb_logo_byte(&mut gb), bin[0xA8]);
}

#[test]
fn sgb_seeds_nothing_rather_than_garbage() {
    for (name, hw) in [
        ("sgb_boot.bin", Hardware::SGB),
        ("sgb2_boot.bin", Hardware::SGB2),
    ] {
        let Some(bin) = bios(name) else { continue };
        let rom = rocket_rom();
        let mut gb = GB::new(hw);
        gb.insert(Cartridge::from_bytes(&rom).unwrap());
        gb.load_bios_bytes(&bin).unwrap();
        // No logo in the image, so the mapper is left unseeded and presents the
        // cart's own header bytes. Seeding from $A8 would substitute a byte of
        // unrelated SGB boot code here.
        let got = locked_cgb_logo_byte(&mut gb);
        assert_eq!(got, rom[0x104], "{name}: expected the raw cart byte");
        assert_ne!(got, bin[0xA8], "{name}: seeded boot-ROM garbage");
    }
}
