//! Cartridge-header decode: the ROM-header offsets, CGB-support flag, the
//! standard `$0147` cartridge-type byte constants, the publisher (licensee)
//! lookup tables, and the boot-ROM Nintendo-logo search. All of this is pure
//! header/ROM inspection with no live mapper state, so it sits in its own
//! module alongside the `Cartridge` container.

use serde::{Deserialize, Serialize};

// Cartridge header offsets
pub(super) const CARTRIDGE_TYPE_OFFSET: usize = 0x0147;
pub(super) const ROM_SIZE_OFFSET: usize = 0x0148;
pub(super) const RAM_SIZE_OFFSET: usize = 0x0149;
pub(super) const CGB_FLAG_OFFSET: usize = 0x0143;

// CGB support flags
pub(super) const CGB_COMPATIBLE: u8 = 0x80; // Works on both DMG and CGB
pub(super) const CGB_ONLY: u8 = 0xC0; // CGB only

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CgbSupport {
    None,       // DMG only
    Compatible, // Works on both DMG and CGB (0x80)
    Only,       // CGB only (0xC0)
}

/// Destination-code ($014A) region hint: $00 = Japanese market, anything else
/// = overseas. A header-level signal distinct from the No-Intro filename region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    Japanese,
    Overseas,
}

// Cartridge types for MBC1
pub(super) const MBC1: u8 = 0x01;
pub(super) const MBC1_RAM: u8 = 0x02;
pub(super) const MBC1_RAM_BATTERY: u8 = 0x03;

// Cartridge types for MBC2
pub(super) const MBC2: u8 = 0x05;
pub(super) const MBC2_BATTERY: u8 = 0x06;

// Bankless carts (Pan Docs "No MBC"): the ROM is wired straight to the bus and
// the optional RAM chip at $A000-BFFF has no banking or enable gate. $00 is
// ROM ONLY, $08 adds RAM, $09 adds a battery. No licensed cart is known to use
// these type bytes, but homebrew, test ROMs and mis-headered dumps do.
pub(super) const ROM_ONLY: u8 = 0x00;
pub(super) const ROM_RAM: u8 = 0x08;
pub(super) const ROM_RAM_BATTERY: u8 = 0x09;

// Cartridge types for MBC3
pub(super) const MBC3_TIMER_BATTERY: u8 = 0x0F;
pub(super) const MBC3_TIMER_RAM_BATTERY: u8 = 0x10;
pub(super) const MBC3: u8 = 0x11;
pub(super) const MBC3_RAM: u8 = 0x12;
pub(super) const MBC3_RAM_BATTERY: u8 = 0x13;

// Cartridge types for MBC5
pub(super) const MBC5: u8 = 0x19;
pub(super) const MBC5_RAM: u8 = 0x1A;
pub(super) const MBC5_RAM_BATTERY: u8 = 0x1B;
pub(super) const MBC5_RUMBLE: u8 = 0x1C;
pub(super) const MBC5_RUMBLE_RAM: u8 = 0x1D;
pub(super) const MBC5_RUMBLE_RAM_BATTERY: u8 = 0x1E;

// MBC6: the one-off Mobile Adapter board ("Net de Get - Minigame @ 100"). Two
// independent 8 KiB ROM/flash windows, two independent 4 KiB SRAM windows, and
// a 1 MiB flash chip the game downloads minigames into. The type byte carries
// no suffix (like POCKET CAMERA and TAMA5) but the board does have SRAM and a
// battery. See `mbc6.rs`.
pub(super) const MBC6: u8 = 0x20;

// MBC7+SENSOR+RUMBLE+RAM+BATTERY (Kirby Tilt 'n' Tumble, Command Master).
// The "RAM" is a 93LC56 serial EEPROM (256 bytes) and the sensor is a 2-axis
// ADXL202E accelerometer; despite the official type name no MBC7 cart has a
// rumble motor. The Japan-only Command Master uses the larger 93LC66 EEPROM
// (512 bytes) - not modeled (remaining gap; would need header-checksum
// sniffing since the type byte is identical).
pub(super) const MBC7_SENSOR_RUMBLE_RAM_BATTERY: u8 = 0x22;

// HuC-3: ROM/RAM banking + RTC + IR + piezo speaker (Robopon, Pocket Family).
// The type byte implies RAM+BATTERY+RTC.
pub(super) const HUC3: u8 = 0xFE;

// HuC-1: ROM/RAM banking + IR link (Pokemon Card GB). The type byte implies
// RAM+BATTERY. Differs from MBC1 (Pan Docs HuC1): there is NO RAM-enable
// gate -- the 0x0000-0x1FFF register instead switches A000-BFFF between RAM
// mode and the IR transceiver ($0E selects IR, anything else RAM).
pub(super) const HUC1_RAM_BATTERY: u8 = 0xFF;

// POCKET CAMERA (Game Boy Camera): MAC-GBD controller + M64282FP "retina"
// image sensor. MBC3-like banking, 128KB battery-backed RAM, and a 54-byte
// write-only sensor/dither register file mapped over A000-BFFF when the RAM
// bank select has bit 4 set (Pan Docs "Game Boy Camera", reverse-engineered
// by AntonioND: github.com/AntonioND/gbcam-rev-engineer). The type byte
// implies RAM+BATTERY.
pub(super) const POCKET_CAMERA: u8 = 0xFC;

// BANDAI TAMA5: the licensed three-chip Tamagotchi board (TAMA5 gate array +
// TAMA6 MCU/RTC + TAMA7 mask ROM), used only by the three "Game de Hakken!!
// Tamagotchi" carts. Unlike every other licensed mapper its register file is
// reached through the cart-RAM window $A000-$BFFF, not $0000-$7FFF, so nothing
// in the $0000-$7FFF write dispatch applies to it. The type byte implies
// RAM+BATTERY+RTC. See `tama5.rs` for the protocol.
pub(super) const TAMA5: u8 = 0xFD;

/// Whether `$0147` holds a value the Pan Docs cartridge-type table defines,
/// implemented or not (MMM01 $0B-$0D is listed here even though rustyboi has no
/// board for it — the byte still names a real board, so it is not evidence of a
/// garbage header).
///
/// Anything outside this set is not a cartridge type at all: the header was
/// never finalized, or game data/title text overran the field (the usual case
/// on Taiwanese/HK unlicensed carts). Used to decide whether a >32KB ROM whose
/// type byte cannot be honoured should be given an inferred mapper rather than
/// a bankless board that leaves most of the chip unreachable.
pub(super) fn is_documented_type(cartridge_type: u8) -> bool {
    matches!(cartridge_type,
        0x00 | 0x01 | 0x02 | 0x03 | 0x05 | 0x06 | 0x08 | 0x09
        | 0x0B | 0x0C | 0x0D | 0x0F | 0x10 | 0x11 | 0x12 | 0x13
        | 0x19 | 0x1A | 0x1B | 0x1C | 0x1D | 0x1E
        | 0x20 | 0x22 | 0xFC | 0xFD | 0xFE | 0xFF)
}

/// Whether `$0147` names a board that carries an external RAM chip in the
/// $A000-$BFFF window. MBC2's 512x4 cells and MBC7's EEPROM are on-die, not an
/// external chip, so those types are excluded; TAMA5 and the Pocket Camera
/// allocate their own storage from the type byte elsewhere.
///
/// Used only to size RAM when the RAM-size byte at $0149 is out of spec: the
/// board named by the type byte physically has the chip, so the cart gets one
/// bank rather than none.
pub(super) fn header_type_has_external_ram(cartridge_type: u8) -> bool {
    matches!(cartridge_type,
        MBC1_RAM | MBC1_RAM_BATTERY | ROM_RAM | ROM_RAM_BATTERY
        | 0x0C | 0x0D // MMM01+RAM, MMM01+RAM+BATTERY
        | MBC3_TIMER_RAM_BATTERY | MBC3_RAM | MBC3_RAM_BATTERY
        | MBC5_RAM | MBC5_RAM_BATTERY | MBC5_RUMBLE_RAM | MBC5_RUMBLE_RAM_BATTERY
        // MBC6's suffix-less type byte still names a board with a 32 KiB SRAM
        // chip in the window (two 4 KiB halves x 8 banks).
        | MBC6
        | HUC3 | HUC1_RAM_BATTERY)
}

/// Byte sum of the 48-byte Nintendo logo at its usual $0104 location. Also
/// consulted by the unlicensed-board detection in the container module.
pub(super) const LOGO_SUM_NINTENDO: u32 = 5446;
/// Sum of the Nintendo logo's first 24 bytes. Paired with `LOGO_SUM_NINTENDO`
/// by `find_logo_in_boot_rom` because the 48-byte sum alone is ambiguous: an
/// unrelated window at $0001 of dmg_boot/mgb_boot also sums to 5446.
const LOGO_SUM_NINTENDO_HALF: u32 = 1492;

/// Offset of the Nintendo logo inside a boot ROM image, or `None` if the image
/// carries no copy.
///
/// Located by checksum rather than by a per-revision offset table, for two
/// reasons. The revisions disagree on where they keep it — $A8 on DMG/MGB, $CB
/// on DMG0, $42 on CGB/AGB — and DMG0 and DMG are both 256 bytes, so image
/// length cannot tell them apart. SGB/SGB2 embed no logo at all (the SNES side
/// runs that check), so there is nothing to find and `None` is the answer
/// rather than 48 bytes of unrelated boot-ROM code.
///
/// Matching on checksums keeps rustyboi free of embedded logo bytes, the same
/// posture `detect_unl_mapper` already takes. A single sum is not selective
/// enough (see `LOGO_SUM_NINTENDO_HALF`), and an image with more than one
/// candidate window yields `None` rather than an arbitrary pick.
pub fn find_logo_in_boot_rom(bios: &[u8]) -> Option<usize> {
    fn sum(bytes: &[u8]) -> u32 {
        bytes.iter().map(|&b| u32::from(b)).sum()
    }
    let mut found = None;
    for off in 0..bios.len().saturating_sub(47) {
        let window = &bios[off..off + 48];
        if sum(window) != LOGO_SUM_NINTENDO || sum(&window[..24]) != LOGO_SUM_NINTENDO_HALF {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(off);
    }
    found
}

/// Publisher for a new-licensee code (two ASCII digits at $0144-$0145, used
/// when the old code is $33). Common Pan Docs entries; `None` if unmapped.
pub(super) fn new_licensee(a: u8, b: u8) -> Option<&'static str> {
    Some(match &[a, b] {
        b"00" => "None",
        b"01" | b"31" => "Nintendo",
        b"08" | b"38" => "Capcom",
        b"13" | b"69" => "Electronic Arts",
        b"18" => "Hudson Soft",
        b"20" => "KSS",
        b"22" => "Planning Office WADA",
        b"24" => "PCM Complete",
        b"25" => "San-X",
        b"28" => "Kemco",
        b"29" => "SETA",
        b"30" => "Viacom",
        b"32" => "Bandai",
        b"33" | b"93" => "Ocean/Acclaim",
        b"34" | b"54" => "Konami",
        b"35" => "Hector",
        b"37" => "Taito",
        b"39" => "Banpresto",
        b"41" => "Ubi Soft",
        b"42" => "Atlus",
        b"44" => "Malibu",
        b"46" => "Angel",
        b"47" => "Bullet-Proof Software",
        b"49" => "Irem",
        b"50" => "Absolute",
        b"51" => "Acclaim",
        b"52" => "Activision",
        b"53" => "American Sammy",
        b"55" => "Hi Tech Entertainment",
        b"56" => "LJN",
        b"57" => "Matchbox",
        b"58" => "Mattel",
        b"59" => "Milton Bradley",
        b"60" => "Titus",
        b"61" => "Virgin",
        b"64" => "LucasArts",
        b"67" => "Ocean",
        b"70" => "Infogrames",
        b"71" => "Interplay",
        b"72" => "Broderbund",
        b"73" => "Sculptured Software",
        b"75" => "The Sales Curve",
        b"78" => "THQ",
        b"79" => "Accolade",
        b"80" => "Misawa Entertainment",
        b"83" => "LOZC",
        b"86" => "Tokuma Shoten",
        b"87" => "Tsukuda Original",
        b"91" => "Chunsoft",
        b"92" => "Video System",
        b"95" => "Varie",
        b"96" => "Yonezawa/S'Pal",
        b"97" => "Kaneko",
        b"99" => "Pack-In-Video",
        b"A4" => "Konami (Yu-Gi-Oh!)",
        _ => return None,
    })
}

/// Publisher for an old-licensee byte ($014B). Common Pan Docs entries;
/// `None` if unmapped. $33 is handled by the caller (means "see new code").
pub(super) fn old_licensee(code: u8) -> Option<&'static str> {
    Some(match code {
        0x00 => "None",
        0x01 | 0x31 => "Nintendo",
        0x08 | 0x38 => "Capcom",
        0x09 => "Hot-B",
        0x0A | 0xE0 => "Jaleco",
        0x0B => "Coconuts Japan",
        0x0C | 0x6E => "Elite Systems",
        0x13 | 0x69 => "Electronic Arts",
        0x18 => "Hudson Soft",
        0x19 => "ITC Entertainment",
        0x1A => "Yanoman",
        0x1F => "Virgin",
        0x24 => "PCM Complete",
        0x25 => "San-X",
        0x28 => "Kotobuki Systems",
        0x29 => "SETA",
        0x30 | 0x70 => "Infogrames",
        0x32 => "Bandai",
        0x34 | 0x54 => "Konami",
        0x35 => "Hector",
        0x39 | 0x9D => "Banpresto",
        0x3E => "Gremlin",
        0x41 => "Ubi Soft",
        0x42 | 0xEB => "Atlus",
        0x44 | 0x4D => "Malibu",
        0x46 | 0xCF => "Angel",
        0x47 => "Spectrum Holobyte",
        0x49 => "Irem",
        0x4A => "Virgin",
        0x4F => "U.S. Gold",
        0x50 => "Absolute",
        0x51 | 0xB0 => "Acclaim",
        0x52 => "Activision",
        0x53 => "American Sammy",
        0x55 => "Park Place",
        0x56 | 0xDB | 0xFF => "LJN",
        0x57 => "Matchbox",
        0x59 => "Milton Bradley",
        0x5A => "Mindscape",
        0x5C => "Naxat Soft",
        0x5D => "Tradewest",
        0x60 => "Titus",
        0x61 => "Virgin",
        0x67 => "Ocean",
        0x6F => "Electro Brain",
        0x71 => "Interplay",
        0x72 | 0xAA => "Broderbund",
        0x73 => "Sculptured Software",
        0x75 => "The Sales Curve",
        0x78 => "THQ",
        0x79 => "Accolade",
        0x7C => "Microprose",
        0x7F | 0xC2 => "Kemco",
        0x80 => "Misawa Entertainment",
        0x83 => "LOZC",
        0x86 | 0xC4 => "Tokuma Shoten",
        0x8B => "Bullet-Proof Software",
        0x8C => "Vic Tokai",
        0x8E => "Ape",
        0x8F => "I'Max",
        0x91 => "Chunsoft",
        0x92 => "Video System",
        0x95 => "Varie",
        0x96 => "Yonezawa/S'Pal",
        0x97 => "Kaneko",
        0x9A => "Nihon Bussan",
        0x9B => "Tecmo",
        0x9C => "Imagineer",
        0xA2 | 0xB2 => "Bandai",
        0xA4 => "Konami",
        0xA6 => "Kawada",
        0xA7 => "Takara",
        0xA9 => "Technos Japan",
        0xAC => "Toei Animation",
        0xAF => "Namco",
        0xB1 => "ASCII/Nexsoft",
        0xB4 => "Square Enix",
        0xB6 => "HAL Laboratory",
        0xB7 => "SNK",
        0xB9 | 0xCE => "Pony Canyon",
        0xBA => "Culture Brain",
        0xBB => "Sunsoft",
        0xBD => "Sony Imagesoft",
        0xBF => "Sammy",
        0xC0 | 0xD0 => "Taito",
        0xC3 => "Squaresoft",
        0xC5 => "Data East",
        0xC6 => "Tonkinhouse",
        0xC8 => "Koei",
        0xCA => "Ultra",
        0xCB => "Vap",
        0xCC => "Use Corporation",
        0xCD => "Meldac",
        0xD1 => "Sofel",
        0xD2 => "Quest",
        0xD3 => "Sigma Enterprises",
        0xD4 => "ASK Kodansha",
        0xD6 => "Naxat Soft",
        0xD7 => "Copya System",
        0xDA => "Tomy",
        0xDD => "NCS",
        0xDE => "Human",
        0xDF => "Altron",
        0xE1 => "Towa Chiki",
        0xE2 => "Yutaka",
        0xE3 => "Varie",
        0xE5 => "Epoch",
        0xE7 => "Athena",
        0xE8 => "Asmik Ace",
        0xE9 => "Natsume",
        0xEA => "King Records",
        0xEC => "Epic/Sony Records",
        0xEE => "IGS",
        0xF0 => "A Wave",
        0xF3 => "Extreme Entertainment",
        _ => return None,
    })
}
