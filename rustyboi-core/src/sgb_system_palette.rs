//! The Super Game Boy's 32 built-in system palettes and its per-game
//! recognition table.
//!
//! A DMG cart running on an SGB is not grayscale: the SNES-side firmware
//! colorizes it. Every SGB carries 32 fixed four-color palettes, selectable by
//! the user from the SGB menu as `1-A`..`4-H`, with `1-A` the power-on default.
//! The firmware additionally recognizes a short list of Nintendo-published
//! titles by their cart-header name and picks that game's signature palette
//! automatically (Tetris, Dr. Mario, Metroid II, ... each get their own).
//!
//! Both tables are lifted verbatim from the SGB1 program ROM (SNES LoROM, so
//! `file_off -> bank = off >> 15, addr = 0x8000 | (off & 0x7FFF)`):
//! the palettes at file `0x10000` (`$02:8000`) as 32x4 little-endian SNES CGRAM
//! words, and the recognition table at file `0x3F000` (`$07:F000`) as 27
//! contiguous 17-byte records (16-char NUL-padded title + one palette index),
//! terminated by an `FF FF` record at `0x3F1CB`. `bios/sgb2.sfc` carries both
//! blocks byte-identically (pinned by the tests below).
//!
//! The stored words are plain RGB555 with red in the low 5 bits, so
//! [`rgb555_to_rgb888`](crate::ppu::controller::rgb555_to_rgb888) decodes them
//! directly -- no channel swap. Note the emulator expands 5 bits to 8 as
//! `(v * 255) / 31`, whereas Gambatte's `gbcpalettes.h` uses `v << 3`; the two
//! tables agree exactly in the RGB555 domain (pinned by
//! `palette_values_match_gambatte`) and differ only in that final expansion,
//! e.g. `1-A` color 0 is `#FFEECD` here and `#F8E8C8` there.
//!
//! See Pan Docs "SGB Color Palettes" / "SGB Functions" and gg8's SGB Functions
//! wiki page.

/// The 32 system palettes in menu order (`1-A`..`4-H`), four RGB555 words each
/// (index 0 = lightest DMG shade .. 3 = darkest). Firmware `0x10000`.
pub const SGB_SYSTEM_PALETTES: [[u16; 4]; 32] = [
    // 1-A  #FFEECD #DE944A #AC2920 #311852
    [0x67BF, 0x265B, 0x10B5, 0x2866],
    // 1-B  #DEDEC5 #CDB473 #B45210 #000000
    [0x637B, 0x3AD9, 0x0956, 0x0000],
    // 1-C  #FFC5FF #EE9C52 #9C3962 #39399C
    [0x7F1F, 0x2A7D, 0x30F3, 0x4CE7],
    // 1-D  #FFFFAC #C5834A #FF0000 #521800
    [0x57FF, 0x2618, 0x001F, 0x006A],
    // 1-E  #FFDEB4 #7BC57B #6A8B41 #5A3920
    [0x5B7F, 0x3F0F, 0x222D, 0x10EB],
    // 1-F  #DEEEFF #E68B52 #AC0000 #004110
    [0x7FBB, 0x2A3C, 0x0015, 0x0900],
    // 1-G  #000052 #00A4EE #7B7B00 #FFFF5A
    [0x2800, 0x7680, 0x01EF, 0x2FFF],
    // 1-H  #FFEEE6 #FFBD8B #834100 #311800
    [0x73BF, 0x46FF, 0x0110, 0x0066],
    // 2-A  #F6CDA4 #C58B4A #297B00 #000000
    [0x533E, 0x2638, 0x01E5, 0x0000],
    // 2-B  #FFFFFF #FFEE52 #FF3100 #52005A
    [0x7FFF, 0x2BBF, 0x00DF, 0x2C0A],
    // 2-C  #FFC5FF #EE8B8B #7B31EE #29299C
    [0x7F1F, 0x463D, 0x74CF, 0x4CA5],
    // 2-D  #FFFFA4 #00FF00 #FF3100 #000052
    [0x53FF, 0x03E0, 0x00DF, 0x2800],
    // 2-E  #FFCD83 #94B4E6 #291062 #100810
    [0x433F, 0x72D2, 0x3045, 0x0822],
    // 2-F  #D5FFFF #FF9452 #A40000 #180000
    [0x7FFA, 0x2A5F, 0x0014, 0x0003],
    // 2-G  #6ABD39 #E65241 #E6BD83 #001800
    [0x1EED, 0x215C, 0x42FC, 0x0060],
    // 2-H  #FFFFFF #BDBDBD #737373 #000000
    [0x7FFF, 0x5EF7, 0x39CE, 0x0000],
    // 3-A  #FFD59C #73C5C5 #FF6229 #314A62
    [0x4F5F, 0x630E, 0x159F, 0x3126],
    // 3-B  #DEDEC5 #E68320 #005200 #001010
    [0x637B, 0x121C, 0x0140, 0x0840],
    // 3-C  #E6ACCD #FFFF7B #00BDFF #20205A
    [0x66BC, 0x3FFF, 0x7EE0, 0x2C84],
    // 3-D  #F6FFBD #E6AC7B #08CD00 #000000
    [0x5FFE, 0x3EBC, 0x0321, 0x0000],
    // 3-E  #FFFFC5 #E6B46A #B47B20 #524A73
    [0x63FF, 0x36DC, 0x11F6, 0x392A],
    // 3-F  #7B7BCD #FF6AFF #FFD500 #414141
    [0x65EF, 0x7DBF, 0x035F, 0x2108],
    // 3-G  #62DE52 #FFFFFF #CD3139 #390000
    [0x2B6C, 0x7FFF, 0x1CD9, 0x0007],
    // 3-H  #E6FFA4 #7BCD39 #4A8B18 #081800
    [0x53FC, 0x1F2F, 0x0E29, 0x0061],
    // 4-A  #F6AC6A #7BACFF #D500D5 #00007B
    [0x36BE, 0x7EAF, 0x681A, 0x3C00],
    // 4-B  #F6EEF6 #EEA462 #417B39 #180808
    [0x7BBE, 0x329D, 0x1DE8, 0x0423],
    // 4-C  #FFE6E6 #DEA4D5 #9CA4E6 #080000
    [0x739F, 0x6A9B, 0x7293, 0x0001],
    // 4-D  #FFFFBD #94CDCD #4A6A7B #08204A
    [0x5FFF, 0x6732, 0x3DA9, 0x2481],
    // 4-E  #FFDEAC #E6AC7B #7B5A8B #002031
    [0x577F, 0x3EBC, 0x456F, 0x1880],
    // 4-F  #BDD5D5 #DE83DE #8300A4 #390000
    [0x6B57, 0x6E1B, 0x5010, 0x0007],
    // 4-G  #B4E618 #BD205A #291000 #008362
    [0x0F96, 0x2C97, 0x0045, 0x3200],
    // 4-H  #FFFFCD #BDC55A #838B41 #415229
    [0x67FF, 0x2F17, 0x2230, 0x1548],
];

/// `1-A`: the palette a real SGB powers on with, and the fallback for any cart
/// the recognition table does not name.
pub const DEFAULT_INDEX: u8 = 0;

const LABELS: [&str; 32] = [
    "1-A", "1-B", "1-C", "1-D", "1-E", "1-F", "1-G", "1-H",
    "2-A", "2-B", "2-C", "2-D", "2-E", "2-F", "2-G", "2-H",
    "3-A", "3-B", "3-C", "3-D", "3-E", "3-F", "3-G", "3-H",
    "4-A", "4-B", "4-C", "4-D", "4-E", "4-F", "4-G", "4-H",
];

const OPTION_IDS: [&str; 32] = [
    "1a", "1b", "1c", "1d", "1e", "1f", "1g", "1h",
    "2a", "2b", "2c", "2d", "2e", "2f", "2g", "2h",
    "3a", "3b", "3c", "3d", "3e", "3f", "3g", "3h",
    "4a", "4b", "4c", "4d", "4e", "4f", "4g", "4h",
];

/// The SGB menu label for a palette index (`"1-A"`..`"4-H"`); out-of-range
/// indices fall back to the default's label.
pub fn label(i: u8) -> &'static str {
    LABELS[(i as usize).min(31)]
}

/// The stable lowercase id for a palette index (`"1a"`..`"4h"`), for libretro
/// option keys and the CLI.
pub fn option_id(i: u8) -> &'static str {
    OPTION_IDS[(i as usize).min(31)]
}

/// Titles the firmware recognizes, paired with the system-palette index it
/// assigns, in firmware order. Firmware `0x3F000`.
///
/// Names are stored exactly as the ROM holds them, including the one that is
/// corrupt there: `BALLOON KID`'s two `O`s are `F4 F4` (a nibble swap of
/// `4F`), so on real hardware that entry can never match and Balloon Kid gets
/// the default palette. Kept verbatim so the firmware-verify test is a true
/// byte comparison and so we reproduce the hardware's behavior.
static GAME_PALETTE_TABLE: &[(&[u8], u8)] = &[
    (b"ZELDA", 5), // 1-F
    (b"SUPER MARIOLAND", 6), // 1-G
    (b"MARIOLAND2", 20), // 3-E
    (b"SUPERMARIOLAND3", 2), // 1-C
    (b"KIRBY DREAM LAND", 11), // 2-D
    (b"HOSHINOKA-BI", 11), // 2-D
    (b"KIRBY'S PINBALL", 3), // 1-D
    (b"YOSSY NO TAMAGO", 12), // 2-E
    (b"MARIO & YOSHI", 12), // 2-E
    (b"YOSSY NO COOKIE", 4), // 1-E
    (b"YOSHI'S COOKIE", 4), // 1-E
    (b"DR.MARIO", 18), // 3-C
    (b"TETRIS", 17), // 3-B
    (b"YAKUMAN", 19), // 3-D
    (b"METROID2", 31), // 4-H
    (b"KAERUNOTAMENI", 9), // 2-B
    (b"GOLF", 24), // 4-A
    (b"ALLEY WAY", 22), // 3-G
    (b"BASEBALL", 15), // 2-H
    (b"TENNIS", 23), // 3-H
    (b"F1RACE", 30), // 4-G
    (b"KID ICARUS", 14), // 2-G
    (b"BALL\xf4\xf4N KID", 1), // 1-B
    (b"QIX", 25), // 4-B
    (b"SOLARSTRIKER", 7), // 1-H
    (b"X", 28), // 4-E
    (b"GBWARS", 21), // 3-F
];

/// The system palette an SGB picks for a cart header, mirroring
/// [`cgb_compat_palette::select_palette_id`](crate::cgb_compat_palette::select_palette_id):
/// only Nintendo-published carts (`$014B == $01`, or `$33` with new-licensee
/// `"01"`) are eligible, and anything unrecognized gets [`DEFAULT_INDEX`].
/// `title` is `$0134-$0143`, NUL-padded as the header stores it.
pub fn select_auto(title: &[u8; 16], old_licensee: u8, new_licensee: [u8; 2]) -> u8 {
    let nintendo = if old_licensee == 0x33 {
        new_licensee == *b"01"
    } else {
        old_licensee == 0x01
    };
    if !nintendo {
        return DEFAULT_INDEX;
    }
    for (name, idx) in GAME_PALETTE_TABLE {
        let mut padded = [0u8; 16];
        padded[..name.len()].copy_from_slice(name);
        if padded == *title {
            return *idx;
        }
    }
    DEFAULT_INDEX
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ppu::controller::rgb555_to_rgb888;

    fn title(name: &[u8]) -> [u8; 16] {
        let mut t = [0u8; 16];
        t[..name.len()].copy_from_slice(name);
        t
    }

    /// Golden pin of every stored word: these are the bytes a real SGB shows,
    /// and every SGB screenshot/sweep frame depends on them.
    #[test]
    fn palette_values_are_stable() {
        assert_eq!(SGB_SYSTEM_PALETTES, GOLDEN);
        assert_eq!(LABELS.len(), SGB_SYSTEM_PALETTES.len());
        assert_eq!(OPTION_IDS.len(), SGB_SYSTEM_PALETTES.len());
        assert_eq!(label(0), "1-A");
        assert_eq!(label(31), "4-H");
        assert_eq!(option_id(0), "1a");
        assert_eq!(option_id(31), "4h");
    }

    /// Independent cross-check against Gambatte's `gbcpalettes.h` (1-A..4-H in
    /// order), compared in the RGB555 domain because Gambatte expands 5->8 bits
    /// with `<< 3` while this emulator uses `* 255 / 31`.
    #[test]
    fn palette_values_match_gambatte() {
        for (i, colors) in GAMBATTE_RGB888.iter().enumerate() {
            let words = colors.map(|c| {
                let (r, g, b) = ((c >> 16) as u8, (c >> 8) as u8, c as u8);
                (r >> 3) as u16 | (((g >> 3) as u16) << 5) | (((b >> 3) as u16) << 10)
            });
            assert_eq!(SGB_SYSTEM_PALETTES[i], words, "palette {}", label(i as u8));
        }
    }

    #[test]
    fn default_is_1a() {
        assert_eq!(DEFAULT_INDEX, 0);
        assert_eq!(label(DEFAULT_INDEX), "1-A");
        assert_eq!(SGB_SYSTEM_PALETTES[0], [0x67BF, 0x265B, 0x10B5, 0x2866]);
        let rgb: Vec<_> =
            SGB_SYSTEM_PALETTES[0].iter().map(|&w| rgb555_to_rgb888(w)).collect();
        assert_eq!(
            rgb,
            vec![(0xFF, 0xEE, 0xCD), (0xDE, 0x94, 0x4A), (0xAC, 0x29, 0x20), (0x31, 0x18, 0x52)]
        );
    }

    #[test]
    fn auto_recognizes_known_titles() {
        let n = |t: &[u8]| select_auto(&title(t), 0x01, *b"\0\0");
        assert_eq!(n(b"TETRIS"), 17);
        assert_eq!(n(b"ZELDA"), 5);
        assert_eq!(n(b"SUPER MARIOLAND"), 6);
        assert_eq!(n(b"DR.MARIO"), 18);
        assert_eq!(n(b"METROID2"), 31);
        assert_eq!(n(b"KIRBY DREAM LAND"), 11); // exactly 16 chars, no padding
        assert_eq!(n(b"GBWARS"), 21); // last record before the FF FF terminator
        // Japanese and western names of the same game share a palette.
        assert_eq!(n(b"YOSSY NO TAMAGO"), n(b"MARIO & YOSHI"));
        assert_eq!(n(b"HOSHINOKA-BI"), n(b"KIRBY DREAM LAND"));
        // New-licensee Nintendo carts are eligible too.
        assert_eq!(select_auto(&title(b"TETRIS"), 0x33, *b"01"), 17);
    }

    #[test]
    fn unrecognized_and_third_party_fall_back_to_1a() {
        // Nintendo-published but not in the table.
        assert_eq!(select_auto(&title(b"POKEMON RED"), 0x01, *b"\0\0"), DEFAULT_INDEX);
        // In the table, but not Nintendo-published.
        assert_eq!(select_auto(&title(b"TETRIS"), 0x7F, *b"\0\0"), DEFAULT_INDEX);
        assert_eq!(select_auto(&title(b"TETRIS"), 0x33, *b"XX"), DEFAULT_INDEX);
        // Homebrew / test ROMs.
        assert_eq!(select_auto(&title(b"DMG-ACID2"), 0x00, *b"\0\0"), DEFAULT_INDEX);
        assert_eq!(select_auto(&title(b"HOMEBREW"), 0x33, *b"XX"), DEFAULT_INDEX);
        // A prefix of a listed name must not match: the firmware compares all
        // 16 header bytes, so "TETRIS 2" is a different cart.
        assert_eq!(select_auto(&title(b"TETRIS 2"), 0x01, *b"\0\0"), DEFAULT_INDEX);
        // The corrupt BALLOON KID record cannot be reached by a real header.
        assert_eq!(select_auto(&title(b"BALLOON KID"), 0x01, *b"\0\0"), DEFAULT_INDEX);
    }

    fn sgb1() -> Option<Vec<u8>> {
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../bios/sgb1.sfc")).ok()
    }

    /// The baked palettes must stay byte-identical to the SGB1 firmware when a
    /// dump is present (skipped silently otherwise, like `tables_match_cgb_boot_bin`).
    #[test]
    fn palette_table_matches_sgb_firmware() {
        let Some(bin) = sgb1() else { return };
        assert_eq!(bin.len(), 0x40000);
        let mut baked = Vec::with_capacity(256);
        for pal in SGB_SYSTEM_PALETTES {
            for w in pal {
                baked.extend_from_slice(&w.to_le_bytes());
            }
        }
        assert_eq!(&bin[0x10000..0x10100], &baked[..]);
    }

    /// Likewise for the recognition table: same names, same indices, same
    /// order, and the `FF FF` terminator right where the list ends.
    #[test]
    fn game_table_matches_sgb_firmware() {
        let Some(bin) = sgb1() else { return };
        let mut baked = Vec::new();
        for (name, idx) in GAME_PALETTE_TABLE {
            let mut rec = [0u8; 17];
            rec[..name.len()].copy_from_slice(name);
            rec[16] = *idx;
            baked.extend_from_slice(&rec);
        }
        let end = 0x3F000 + baked.len();
        assert_eq!(&bin[0x3F000..end], &baked[..]);
        assert_eq!(&bin[end..end + 2], &[0xFF, 0xFF], "list must end at {end:#X}");
        // Every index is a real palette slot.
        assert!(GAME_PALETTE_TABLE.iter().all(|&(_, i)| (i as usize) < SGB_SYSTEM_PALETTES.len()));
    }

    /// SGB2 ships the same palettes (and the same recognition list), so one
    /// baked table serves both models.
    #[test]
    fn sgb1_and_sgb2_share_the_palette_table() {
        let Some(one) = sgb1() else { return };
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../bios/sgb2.sfc");
        let Ok(two) = std::fs::read(path) else { return };
        assert_eq!(two.len(), 0x80000);
        assert_eq!(one[0x10000..0x10100], two[0x10000..0x10100]);
        let end = 0x3F000 + GAME_PALETTE_TABLE.len() * 17 + 2;
        assert_eq!(one[0x3F000..end], two[0x3F000..end]);
    }

    const GOLDEN: [[u16; 4]; 32] = [
        [0x67BF, 0x265B, 0x10B5, 0x2866],
        [0x637B, 0x3AD9, 0x0956, 0x0000],
        [0x7F1F, 0x2A7D, 0x30F3, 0x4CE7],
        [0x57FF, 0x2618, 0x001F, 0x006A],
        [0x5B7F, 0x3F0F, 0x222D, 0x10EB],
        [0x7FBB, 0x2A3C, 0x0015, 0x0900],
        [0x2800, 0x7680, 0x01EF, 0x2FFF],
        [0x73BF, 0x46FF, 0x0110, 0x0066],
        [0x533E, 0x2638, 0x01E5, 0x0000],
        [0x7FFF, 0x2BBF, 0x00DF, 0x2C0A],
        [0x7F1F, 0x463D, 0x74CF, 0x4CA5],
        [0x53FF, 0x03E0, 0x00DF, 0x2800],
        [0x433F, 0x72D2, 0x3045, 0x0822],
        [0x7FFA, 0x2A5F, 0x0014, 0x0003],
        [0x1EED, 0x215C, 0x42FC, 0x0060],
        [0x7FFF, 0x5EF7, 0x39CE, 0x0000],
        [0x4F5F, 0x630E, 0x159F, 0x3126],
        [0x637B, 0x121C, 0x0140, 0x0840],
        [0x66BC, 0x3FFF, 0x7EE0, 0x2C84],
        [0x5FFE, 0x3EBC, 0x0321, 0x0000],
        [0x63FF, 0x36DC, 0x11F6, 0x392A],
        [0x65EF, 0x7DBF, 0x035F, 0x2108],
        [0x2B6C, 0x7FFF, 0x1CD9, 0x0007],
        [0x53FC, 0x1F2F, 0x0E29, 0x0061],
        [0x36BE, 0x7EAF, 0x681A, 0x3C00],
        [0x7BBE, 0x329D, 0x1DE8, 0x0423],
        [0x739F, 0x6A9B, 0x7293, 0x0001],
        [0x5FFF, 0x6732, 0x3DA9, 0x2481],
        [0x577F, 0x3EBC, 0x456F, 0x1880],
        [0x6B57, 0x6E1B, 0x5010, 0x0007],
        [0x0F96, 0x2C97, 0x0045, 0x3200],
        [0x67FF, 0x2F17, 0x2230, 0x1548],
    ];

    /// Gambatte `gbcpalettes.h`, `1-A`..`4-H` in order, as `0xRRGGBB`.
    const GAMBATTE_RGB888: [[u32; 4]; 32] = [
        // 1-A
        [0xF8E8C8, 0xD89048, 0xA82820, 0x301850],
        // 1-B
        [0xD8D8C0, 0xC8B070, 0xB05010, 0x000000],
        // 1-C
        [0xF8C0F8, 0xE89850, 0x983860, 0x383898],
        // 1-D
        [0xF8F8A8, 0xC08048, 0xF80000, 0x501800],
        // 1-E
        [0xF8D8B0, 0x78C078, 0x688840, 0x583820],
        // 1-F
        [0xD8E8F8, 0xE08850, 0xA80000, 0x004010],
        // 1-G
        [0x000050, 0x00A0E8, 0x787800, 0xF8F858],
        // 1-H
        [0xF8E8E0, 0xF8B888, 0x804000, 0x301800],
        // 2-A
        [0xF0C8A0, 0xC08848, 0x287800, 0x000000],
        // 2-B
        [0xF8F8F8, 0xF8E850, 0xF83000, 0x500058],
        // 2-C
        [0xF8C0F8, 0xE88888, 0x7830E8, 0x282898],
        // 2-D
        [0xF8F8A0, 0x00F800, 0xF83000, 0x000050],
        // 2-E
        [0xF8C880, 0x90B0E0, 0x281060, 0x100810],
        // 2-F
        [0xD0F8F8, 0xF89050, 0xA00000, 0x180000],
        // 2-G
        [0x68B838, 0xE05040, 0xE0B880, 0x001800],
        // 2-H
        [0xF8F8F8, 0xB8B8B8, 0x707070, 0x000000],
        // 3-A
        [0xF8D098, 0x70C0C0, 0xF86028, 0x304860],
        // 3-B
        [0xD8D8C0, 0xE08020, 0x005000, 0x001010],
        // 3-C
        [0xE0A8C8, 0xF8F878, 0x00B8F8, 0x202058],
        // 3-D
        [0xF0F8B8, 0xE0A878, 0x08C800, 0x000000],
        // 3-E
        [0xF8F8C0, 0xE0B068, 0xB07820, 0x504870],
        // 3-F
        [0x7878C8, 0xF868F8, 0xF8D000, 0x404040],
        // 3-G
        [0x60D850, 0xF8F8F8, 0xC83038, 0x380000],
        // 3-H
        [0xE0F8A0, 0x78C838, 0x488818, 0x081800],
        // 4-A
        [0xF0A868, 0x78A8F8, 0xD000D0, 0x000078],
        // 4-B
        [0xF0E8F0, 0xE8A060, 0x407838, 0x180808],
        // 4-C
        [0xF8E0E0, 0xD8A0D0, 0x98A0E0, 0x080000],
        // 4-D
        [0xF8F8B8, 0x90C8C8, 0x486878, 0x082048],
        // 4-E
        [0xF8D8A8, 0xE0A878, 0x785888, 0x002030],
        // 4-F
        [0xB8D0D0, 0xD880D8, 0x8000A0, 0x380000],
        // 4-G
        [0xB0E018, 0xB82058, 0x281000, 0x008060],
        // 4-H
        [0xF8F8C8, 0xB8C058, 0x808840, 0x405028],
    ];
}
