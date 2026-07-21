//! CGB boot ROM per-game DMG-compatibility palette assignment.
//!
//! When a DMG (non-CGB) cart boots on CGB hardware, the boot ROM colorizes it:
//! it sums the 16 title bytes ($0134-$0143) and, for Nintendo-published games
//! ($014B == $01, or $014B == $33 with new-licensee "01"), looks the sum up in
//! a 79-entry checksum table; 14 checksum collisions are disambiguated by the
//! 4th title character. The resulting entry selects one of 29 BG/OBJ0/OBJ1
//! palette combinations, so e.g. Tetris, Zelda and Super Mario Land each get
//! their signature colors, while unrecognized titles fall back to the default
//! dark-green scheme. A button combo held during the logo overrides the
//! automatic choice.
//!
//! Everything here is lifted from the CGB boot ROM itself (cgb_boot.bin,
//! md5 dbfce9db9deaa2567f6a84fde55f9680; the AGB boot ROM carries identical
//! tables and code): selection logic at $0475-$04E8, combination expansion at
//! $04E9-$051D, and the data tables at $06C7 (checksums), $0716 (4th letters),
//! $0733 (palette ID per checksum), $0791 (combination triplets), $07E8
//! (palette colors), $08E4/$08F0 (button combos). See also Pan Docs
//! "Compatibility palettes" and TCRF "Game Boy Color Bootstrap ROM".

/// The three palettes the boot ROM installs for a DMG cart: CGB BG palette 0
/// and OBJ palettes 0/1, each 4 RGB555 little-endian color pairs.
pub(crate) struct CompatPalettes {
    pub bg: [u8; 8],
    pub(crate) obj0: [u8; 8],
    pub(crate) obj1: [u8; 8],
}

/// $06C7: title checksums of Nintendo-published DMG games. The first 65 map
/// 1:1 to a palette ID; the last 14 are shared by several games and need the
/// 4th title letter to disambiguate.
const TITLE_CHECKSUMS: [u8; 79] = [
    0x00, 0x88, 0x16, 0x36, 0xD1, 0xDB, 0xF2, 0x3C, 0x8C, 0x92, 0x3D, 0x5C,
    0x58, 0xC9, 0x3E, 0x70, 0x1D, 0x59, 0x69, 0x19, 0x35, 0xA8, 0x14, 0xAA,
    0x75, 0x95, 0x99, 0x34, 0x6F, 0x15, 0xFF, 0x97, 0x4B, 0x90, 0x17, 0x10,
    0x39, 0xF7, 0xF6, 0xA2, 0x49, 0x4E, 0x43, 0x68, 0xE0, 0x8B, 0xF0, 0xCE,
    0x0C, 0x29, 0xE8, 0xB7, 0x86, 0x9A, 0x52, 0x01, 0x9D, 0x71, 0x9C, 0xBD,
    0x5D, 0x6D, 0x67, 0x3F, 0x6B, 0xB3, 0x46, 0x28, 0xA5, 0xC6, 0xD3, 0x27,
    0x61, 0x18, 0x66, 0x6A, 0xBF, 0x0D, 0xF4,
];

/// $0716: 4th-title-letter rows for the 14 ambiguous checksums (row stride 14;
/// the third row only covers the first column).
const FOURTH_LETTERS: [u8; 29] = *b"BEFAARBEKEK R-URAR INAILICE R";

/// $0733: palette ID per checksum-table hit. Low 5 bits index
/// `PALETTE_COMBINATIONS`; the top 3 bits steer which triplet slots feed
/// OBJ0/OBJ1 (see `palettes_for_id`). Entry 0 doubles as the default for
/// non-Nintendo carts, unmatched checksums and unmatched 4th letters (the
/// boot ROM resets its table index to 0 on every miss).
const PALETTE_PER_CHECKSUM: [u8; 94] = [
    0x7C, 0x08, 0x12, 0xA3, 0xA2, 0x07, 0x87, 0x4B, 0x20, 0x12, 0x65, 0xA8,
    0x16, 0xA9, 0x86, 0xB1, 0x68, 0xA0, 0x87, 0x66, 0x12, 0xA1, 0x30, 0x3C,
    0x12, 0x85, 0x12, 0x64, 0x1B, 0x07, 0x06, 0x6F, 0x6E, 0x6E, 0xAE, 0xAF,
    0x6F, 0xB2, 0xAF, 0xB2, 0xA8, 0xAB, 0x6F, 0xAF, 0x86, 0xAE, 0xA2, 0xA2,
    0x12, 0xAF, 0x13, 0x12, 0xA1, 0x6E, 0xAF, 0xAF, 0xAD, 0x06, 0x4C, 0x6E,
    0xAF, 0xAF, 0x12, 0x7C, 0xAC, 0xA8, 0x6A, 0x6E, 0x13, 0xA0, 0x2D, 0xA8,
    0x2B, 0xAC, 0x64, 0xAC, 0x6D, 0x87, 0xBC, 0x60, 0xB4, 0x13, 0x72, 0x7C,
    0xB5, 0xAE, 0xAE, 0x7C, 0x7C, 0x65, 0xA2, 0x6C, 0x64, 0x85,
];

/// $0791: 29 (obj0, obj1, bg) triplets of byte offsets into `PALETTE_DATA`.
/// Offsets are raw bytes, not palette indices: Nintendo overlaps entries
/// mid-palette to save space (e.g. offset 0x1E straddles palettes 3 and 4).
const PALETTE_COMBINATIONS: [u8; 87] = [
    0x80, 0xB0, 0x40, 0x88, 0x20, 0x68, 0xDE, 0x00, 0x70,
    0xDE, 0x20, 0x78, 0x20, 0x20, 0x38, 0x20, 0xB0, 0x90,
    0x20, 0xB0, 0xA0, 0xE0, 0xB0, 0xC0, 0x98, 0xB6, 0x48,
    0x80, 0xE0, 0x50, 0x1E, 0x1E, 0x58, 0x20, 0xB8, 0xE0,
    0x88, 0xB0, 0x10, 0x20, 0x00, 0x10, 0x20, 0xE0, 0x18,
    0xE0, 0x18, 0x00, 0x18, 0xE0, 0x20, 0xA8, 0xE0, 0x20,
    0x18, 0xE0, 0x00, 0x20, 0x18, 0xD8, 0xC8, 0x18, 0xE0,
    0x00, 0xE0, 0x40, 0x28, 0x28, 0x28, 0x18, 0xE0, 0x60,
    0x20, 0x18, 0xE0, 0x00, 0x00, 0x08, 0xE0, 0x18, 0x30,
    0xD0, 0xD0, 0xD0, 0x20, 0xE0, 0xE8,
];

/// $07E8: the boot ROM's palette colors (30 aligned palettes' worth of RGB555
/// little-endian pairs, addressed by raw byte offset).
const PALETTE_DATA: [u8; 240] = [
    0xFF, 0x7F, 0xBF, 0x32, 0xD0, 0x00, 0x00, 0x00,
    0x9F, 0x63, 0x79, 0x42, 0xB0, 0x15, 0xCB, 0x04,
    0xFF, 0x7F, 0x31, 0x6E, 0x4A, 0x45, 0x00, 0x00,
    0xFF, 0x7F, 0xEF, 0x1B, 0x00, 0x02, 0x00, 0x00,
    0xFF, 0x7F, 0x1F, 0x42, 0xF2, 0x1C, 0x00, 0x00,
    0xFF, 0x7F, 0x94, 0x52, 0x4A, 0x29, 0x00, 0x00,
    0xFF, 0x7F, 0xFF, 0x03, 0x2F, 0x01, 0x00, 0x00,
    0xFF, 0x7F, 0xEF, 0x03, 0xD6, 0x01, 0x00, 0x00,
    0xFF, 0x7F, 0xB5, 0x42, 0xC8, 0x3D, 0x00, 0x00,
    0x74, 0x7E, 0xFF, 0x03, 0x80, 0x01, 0x00, 0x00,
    0xFF, 0x67, 0xAC, 0x77, 0x13, 0x1A, 0x6B, 0x2D,
    0xD6, 0x7E, 0xFF, 0x4B, 0x75, 0x21, 0x00, 0x00,
    0xFF, 0x53, 0x5F, 0x4A, 0x52, 0x7E, 0x00, 0x00,
    0xFF, 0x4F, 0xD2, 0x7E, 0x4C, 0x3A, 0xE0, 0x1C,
    0xED, 0x03, 0xFF, 0x7F, 0x5F, 0x25, 0x00, 0x00,
    0x6A, 0x03, 0x1F, 0x02, 0xFF, 0x03, 0xFF, 0x7F,
    0xFF, 0x7F, 0xDF, 0x01, 0x12, 0x01, 0x00, 0x00,
    0x1F, 0x23, 0x5F, 0x03, 0xF2, 0x00, 0x09, 0x00,
    0xFF, 0x7F, 0xEA, 0x03, 0x1F, 0x01, 0x00, 0x00,
    0x9F, 0x29, 0x1A, 0x00, 0x0C, 0x00, 0x00, 0x00,
    0xFF, 0x7F, 0x7F, 0x02, 0x1F, 0x00, 0x00, 0x00,
    0xFF, 0x7F, 0xE0, 0x03, 0x06, 0x02, 0x20, 0x01,
    0xFF, 0x7F, 0xEB, 0x7E, 0x1F, 0x00, 0x00, 0x7C,
    0xFF, 0x7F, 0xFF, 0x3F, 0x00, 0x7E, 0x1F, 0x00,
    0xFF, 0x7F, 0xFF, 0x03, 0x1F, 0x00, 0x00, 0x00,
    0xFF, 0x03, 0x1F, 0x00, 0x0C, 0x00, 0x00, 0x00,
    0xFF, 0x7F, 0x3F, 0x03, 0x93, 0x01, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x42, 0x7F, 0x03, 0xFF, 0x7F,
    0xFF, 0x7F, 0x8C, 0x7E, 0x00, 0x7C, 0x00, 0x00,
    0xFF, 0x7F, 0xEF, 0x1B, 0x80, 0x61, 0x00, 0x00,
];

/// $08E4/$08F0: boot-time button combos (JOYP-style byte, dpad in the high
/// nibble / A-B in the low, as the boot ROM's $021D poll builds it) and the
/// palette ID each one forces.
const KEY_COMBO_JOYP: [u8; 12] = [
    0x40, 0x41, 0x42, 0x20, 0x21, 0x22, 0x80, 0x81, 0x82, 0x10, 0x11, 0x12,
];
const KEY_COMBO_ID: [u8; 12] = [
    0x12, 0xB0, 0x79, 0xB8, 0xAD, 0x16, 0x17, 0x07, 0xBA, 0x05, 0x7C, 0x13,
];

/// $007A/$007B: the two title checksums the boot ROM singles out at $05F8 for
/// an extra `call $03DA` (a Nintendo-logo tilemap fixup). Reached only from the
/// Nintendo path, because $D000 — the byte compared against these — is written
/// only by the title-hash walk. Pinned against the images by
/// `tables_match_cgb_boot_bin`.
const FIXUP_CHECKSUMS: [u8; 2] = [0x58, 0x43];

/// Palette ID for a cart header, per the boot ROM's $0475-$04D6 walk:
/// `title` = $0134-$0143, `old_licensee` = $014B, `new_licensee` = $0144-$0145.
pub(crate) fn select_palette_id(title: &[u8; 16], old_licensee: u8, new_licensee: [u8; 2]) -> u8 {
    walk(title, old_licensee, new_licensee).0
}

/// The boot ROM's "this cart is Nintendo-published, colorize it" predicate:
/// old licensee $01, or the $33 escape with a "01" new-licensee code.
fn is_nintendo(old_licensee: u8, new_licensee: [u8; 2]) -> bool {
    if old_licensee == 0x33 {
        new_licensee == *b"01"
    } else {
        old_licensee == 0x01
    }
}

/// The boot ROM's DMG-compat palette walk, as both its outcome and its cost:
/// `(palette id, master cycles from $0475 to the $04D0 reconvergence)`.
///
/// The cost half is what makes the CGB/AGB post-boot divider cart-dependent,
/// so the two are produced together — a second copy of this control flow would
/// be free to drift away from the palette it is supposed to be timing.
fn walk(title: &[u8; 16], old_licensee: u8, new_licensee: [u8; 2]) -> (u8, i32) {
    let default = PALETTE_PER_CHECKSUM[0];
    // $0475 ld hl,$014B / ld a,(hl) / cp $33.
    let mut cc = 28;
    let nintendo = if old_licensee == 0x33 {
        // jr nz not taken, then ld l,$44 / ld e,$30 / ldi a,(hl) / cp e.
        cc += 8 + 28;
        if new_licensee[0] != b'0' {
            cc += 12; // jr nz taken -> $04CE
            false
        } else {
            // jr nz not taken / inc e / jr $048C / ldi a,(hl) / cp e.
            cc += 8 + 4 + 12 + 12;
            if new_licensee[1] != b'1' {
                cc += 12;
                false
            } else {
                cc += 8;
                true
            }
        }
    } else {
        // jr nz taken, then ld l,$4B / ld e,$01 / ldi a,(hl) / cp e.
        cc += 12 + 28;
        if old_licensee != 0x01 {
            cc += 12;
            false
        } else {
            cc += 8;
            true
        }
    };
    if !nintendo {
        return (default, cc + 8); // $04CE ld c,$00
    }
    // $0490: ld l,$34 / ld bc,$0010 / the 16-pass title sum / ld ($D000),a /
    // ld hl,$06C7 / ld c,$00.
    cc += 564;
    let checksum = title.iter().fold(0u8, |s, &b| s.wrapping_add(b));
    let Some(mut idx) = TITLE_CHECKSUMS.iter().position(|&c| c == checksum) else {
        // All 79 probes fail, then jr $04CE / ld c,$00.
        return (default, cc + 3808);
    };
    // 48 cc per rejected probe, 24 to fall out of the loop on the hit.
    cc += 48 * idx as i32 + 24;
    if idx < 65 {
        // ld a,c / sub $41 / jr c taken -> $04D0.
        return (PALETTE_PER_CHECKSUM[idx], cc + 24);
    }
    // Ambiguous checksum: `jr c` not taken, then scan the 4th-letter rows
    // (stride 14) until the running index leaves the 94-entry table, exactly
    // like the boot ROM.
    cc += 20 + 32;
    let mut row_idx = idx;
    loop {
        if FOURTH_LETTERS[row_idx - 65] == title[3] {
            idx = row_idx;
            cc += 44; // the matching pass, ending in `jr z` -> $04D0
            break;
        }
        row_idx += 14;
        if row_idx >= PALETTE_PER_CHECKSUM.len() {
            // Final pass falls out of the loop, then $04CE ld c,$00.
            return (default, cc + 88 + 8);
        }
        cc += 92;
    }
    (PALETTE_PER_CHECKSUM[idx], cc)
}

/// Master cycles the CGB/AGB boot ROM's DMG-compat setup spends on **this**
/// cart header, relative to the shortest arm — old licensee $33 with a
/// new-licensee code that does not start with '0'. That reference is the shape
/// mooneye's `boot_div-{cgbABCDE,cgb0,A}` carts carry ($014B = $33, new
/// licensee "ZZ"), so it is exactly the arm the HLE's base constants are
/// calibrated on and this returns 0 for them.
///
/// Three costs stack, and only the first was ever modelled:
///   1. the licensee walk itself ([`walk`]), which is +4 for any non-$33 cart
///      and +36 for a $33 cart whose new-licensee code starts with '0' but is
///      not "01" — and, for the Nintendo arms, the whole title-hash search,
///      which runs 48 cc per rejected checksum and so is *position dependent*
///      (about +600 cc for a first-row hit, +4.4k for a miss);
///   2. installing the chosen palette ($04F5 and the $0582 index loop): 32 cc
///      per unit of the combination index, plus three per-pass branches keyed
///      on the id's top three bits;
///   3. the $05F8 fixup for the two title checksums the image singles out.
///
/// Every arm is derived from the images and pinned by
/// `boot_rom_hle_parity::compat_boot_cost_matches_every_cgb_boot_rom`.
pub(crate) fn compat_boot_extra_cc(
    title: &[u8; 16],
    old_licensee: u8,
    new_licensee: [u8; 2],
) -> i32 {
    /// The $0475 walk for the reference arm above.
    const REFERENCE_WALK_CC: i32 = 84;
    /// Combination index of [`PALETTE_PER_CHECKSUM`]`[0]`, the palette that
    /// reference arm installs.
    const REFERENCE_COMBINATION: i32 = 28;

    let (id, walk_cc) = walk(title, old_licensee, new_licensee);
    let mut extra = walk_cc - REFERENCE_WALK_CC;

    // $0582: `add hl,de` (de = 0x18) once per unit of the combination index.
    extra += 32 * ((id & 0x1F) as i32 - REFERENCE_COMBINATION);
    // $04F5, 30 passes: a clear flag bit leaves two `inc de` pairs in the path
    // (12 cc each pass, twice), and a set bit 2 leaves a 5-instruction tail
    // (36 cc each pass). The reference palette's flags are 0b011.
    let flags = id >> 5;
    if flags & 1 == 0 {
        extra += 720;
    }
    if flags & 2 == 0 {
        extra += 720;
    }
    if flags & 4 != 0 {
        extra += 1080;
    }

    // $05F8 compares the stored title checksum against the two bytes at $007A.
    // Only the Nintendo arms reach it with $D000 written; every other cart
    // leaves $D000 at its power-on value, which the boot ROM never matches
    // deterministically. Both fixups run the same $03DA routine, but land 40 cc
    // apart — the routine's VRAM tilemap writes are not phase-independent — so
    // they are two measured constants rather than one.
    if is_nintendo(old_licensee, new_licensee) {
        let checksum = title.iter().fold(0u8, |s, &b| s.wrapping_add(b));
        if checksum == FIXUP_CHECKSUMS[0] {
            extra += 1400;
        } else if checksum == FIXUP_CHECKSUMS[1] {
            extra += 1440;
        }
    }
    extra
}

/// Palette ID forced by a button combo held at boot ($0589-$05C8), or None if
/// the held-button byte is not one of the 12 recognized combos.
pub(crate) fn key_combo_palette_id(combo: u8) -> Option<u8> {
    KEY_COMBO_JOYP
        .iter()
        .position(|&c| c == combo)
        .map(|i| KEY_COMBO_ID[i])
}

/// The 12 boot-ROM DMG-compatibility palette schemes a user can hold at boot on
/// real CGB hardware, as `(stable id, display label, palette id)`. The palette
/// ids are exactly the [`KEY_COMBO_ID`] values the boot ROM assigns to each
/// button combo (pinned by a test), so a frontend can offer these as explicit
/// overrides of the title-hash auto-pick. Order matches [`KEY_COMBO_JOYP`].
pub const COMBO_SCHEMES: [(&str, &str, u8); 12] = [
    ("up", "Up", 0x12),
    ("up_a", "Up + A", 0xB0),
    ("up_b", "Up + B", 0x79),
    ("left", "Left", 0xB8),
    ("left_a", "Left + A", 0xAD),
    ("left_b", "Left + B", 0x16),
    ("down", "Down", 0x17),
    ("down_a", "Down + A", 0x07),
    ("down_b", "Down + B", 0xBA),
    ("right", "Right", 0x05),
    ("right_a", "Right + A", 0x7C),
    ("right_b", "Right + B", 0x13),
];

/// Resolve a palette ID to the installed palettes, mirroring the boot ROM's
/// combination expansion at $04E9-$051D: the low 5 bits pick an
/// (obj0, obj1, bg) offset triplet, and the top 3 bits select per-slot whether
/// OBJ0/OBJ1 take their own column or fall back (OBJ0 to the BG palette, OBJ1
/// to the OBJ0 column or the BG palette).
pub(crate) fn palettes_for_id(id: u8) -> CompatPalettes {
    let comb = (id & 0x1F) as usize * 3;
    let (s0, s1, s2) = (
        PALETTE_COMBINATIONS[comb],
        PALETTE_COMBINATIONS[comb + 1],
        PALETTE_COMBINATIONS[comb + 2],
    );
    let flags = id >> 5;
    let obj0 = if flags & 1 != 0 { s0 } else { s2 };
    let obj1 = if flags & 4 != 0 {
        s1
    } else if flags & 2 != 0 {
        s0
    } else {
        s2
    };
    let pal = |off: u8| -> [u8; 8] {
        PALETTE_DATA[off as usize..off as usize + 8].try_into().unwrap()
    };
    CompatPalettes { bg: pal(s2), obj0: pal(obj0), obj1: pal(obj1) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title(name: &[u8]) -> [u8; 16] {
        let mut t = [0u8; 16];
        t[..name.len()].copy_from_slice(name);
        t
    }

    /// Unrecognized carts must reproduce the pre-table fixed palette exactly
    /// (BG #FFFFFF/#7BFF31/#0063C6/#000000, OBJ0 == OBJ1 red ramp): every
    /// hwtest suite grader relies on these bytes.
    #[test]
    fn default_palette_is_the_legacy_fixed_palette() {
        for id in [
            select_palette_id(&title(b"DMG-ACID2"), 0x00, *b"\0\0"),
            select_palette_id(&title(b"TETRIS"), 0x7F, *b"\0\0"), // not Nintendo
            select_palette_id(&title(b"ZZZZZZZZ"), 0x01, *b"\0\0"), // hash miss
            select_palette_id(&title(b"HOMEBREW"), 0x33, *b"XX"),
        ] {
            assert_eq!(id, 0x7C);
            let p = palettes_for_id(id);
            assert_eq!(p.bg, [0xFF, 0x7F, 0xEF, 0x1B, 0x80, 0x61, 0x00, 0x00]);
            assert_eq!(p.obj0, [0xFF, 0x7F, 0x1F, 0x42, 0xF2, 0x1C, 0x00, 0x00]);
            assert_eq!(p.obj1, p.obj0);
        }
    }

    #[test]
    fn tetris_gets_its_signature_palette() {
        let id = select_palette_id(&title(b"TETRIS"), 0x01, *b"\0\0");
        assert_eq!(id, 0x07);
        let p = palettes_for_id(id);
        // White / yellow / red / black across BG and both OBJ palettes.
        let expected = [0xFF, 0x7F, 0xFF, 0x03, 0x1F, 0x00, 0x00, 0x00];
        assert_eq!(p.bg, expected);
        assert_eq!(p.obj0, expected);
        assert_eq!(p.obj1, expected);
    }

    #[test]
    fn zelda_gets_distinct_obj_palettes() {
        // Flags 0b101: OBJ0 and OBJ1 take their own columns (green Link,
        // blue OBJ1) over the red BG ramp.
        let id = select_palette_id(&title(b"ZELDA"), 0x01, *b"\0\0");
        assert_eq!(id, 0xB1);
        let p = palettes_for_id(id);
        assert_eq!(p.bg, [0xFF, 0x7F, 0x1F, 0x42, 0xF2, 0x1C, 0x00, 0x00]);
        assert_eq!(p.obj0, [0xFF, 0x7F, 0xE0, 0x03, 0x06, 0x02, 0x20, 0x01]);
        assert_eq!(p.obj1, [0xFF, 0x7F, 0x8C, 0x7E, 0x00, 0x7C, 0x00, 0x00]);
    }

    #[test]
    fn super_mario_land_uses_fourth_letter_and_overlapped_offset() {
        // Checksum 0x46 is ambiguous (index 66); 4th letter 'E' resolves in
        // row 0. The OBJ offset 0x1E straddles two aligned palettes.
        let id = select_palette_id(&title(b"SUPER MARIOLAND"), 0x01, *b"\0\0");
        assert_eq!(id, 0x6A);
        let p = palettes_for_id(id);
        assert_eq!(p.bg, [0xD6, 0x7E, 0xFF, 0x4B, 0x75, 0x21, 0x00, 0x00]);
        assert_eq!(p.obj0, [0x00, 0x00, 0xFF, 0x7F, 0x1F, 0x42, 0xF2, 0x1C]);
        assert_eq!(p.obj1, p.obj0);
    }

    #[test]
    fn pokemon_versions_split_on_fourth_letter_rows() {
        // Red: unambiguous checksum 0x14 -> ID 0x30 (red BG, green OBJ0,
        // OBJ1 falls back to the BG palette per flags 0b001).
        let red = select_palette_id(&title(b"POKEMON RED"), 0x01, *b"\0\0");
        assert_eq!(red, 0x30);
        let p = palettes_for_id(red);
        assert_eq!(p.bg, [0xFF, 0x7F, 0x1F, 0x42, 0xF2, 0x1C, 0x00, 0x00]);
        assert_eq!(p.obj0, [0xFF, 0x7F, 0xEF, 0x1B, 0x00, 0x02, 0x00, 0x00]);
        assert_eq!(p.obj1, p.bg);
        // Blue: ambiguous checksum 0x61, 4th letter 'E' -> blue BG.
        let blue = select_palette_id(&title(b"POKEMON BLUE"), 0x01, *b"\0\0");
        assert_eq!(blue, 0x2B);
        assert_eq!(palettes_for_id(blue).bg, [0xFF, 0x7F, 0x8C, 0x7E, 0x00, 0x7C, 0x00, 0x00]);
    }

    #[test]
    fn fourth_letter_rows_walk_and_miss() {
        // Checksum 0xB3 (index 65) is the only column with a third row ('R').
        let mut t = [0u8; 16];
        t[3] = b'R';
        t[0] = 0xB3u8.wrapping_sub(b'R');
        assert_eq!(select_palette_id(&t, 0x01, *b"\0\0"), PALETTE_PER_CHECKSUM[93]);
        // Same checksum, letter in no row: default.
        t[3] = b'Z';
        t[0] = 0xB3u8.wrapping_sub(b'Z');
        assert_eq!(select_palette_id(&t, 0x01, *b"\0\0"), 0x7C);
        // Second column (checksum 0x46), row 1 letter 'R' (letters[15]).
        t[3] = b'R';
        t[0] = 0x46u8.wrapping_sub(b'R');
        assert_eq!(select_palette_id(&t, 0x01, *b"\0\0"), PALETTE_PER_CHECKSUM[66 + 14]);
    }

    #[test]
    fn key_combos_match_the_boot_tables() {
        assert_eq!(key_combo_palette_id(0x11), Some(0x7C)); // Right+A = default scheme
        assert_eq!(key_combo_palette_id(0x40), Some(0x12)); // Up
        assert_eq!(key_combo_palette_id(0x00), None);
        assert_eq!(key_combo_palette_id(0x43), None); // Up+A+B: no exact match
        assert_eq!(key_combo_palette_id(0x14), None); // Right+Select
    }

    // The user-facing scheme list must stay pinned to the boot ROM's button-combo
    // palette ids (same order as KEY_COMBO_JOYP), so a "Left + A" pick colorizes a
    // DMG game exactly as holding Left+A at boot would on real hardware.
    #[test]
    fn combo_schemes_match_key_combo_ids() {
        assert_eq!(COMBO_SCHEMES.len(), KEY_COMBO_ID.len());
        for (i, (_, _, pid)) in COMBO_SCHEMES.iter().enumerate() {
            assert_eq!(*pid, KEY_COMBO_ID[i]);
        }
        // Ids are unique.
        let mut ids: Vec<&str> = COMBO_SCHEMES.iter().map(|(id, _, _)| *id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), COMBO_SCHEMES.len());
    }

    /// The embedded tables must stay byte-identical to the real boot ROM dump
    /// when one is present (skipped silently otherwise).
    #[test]
    fn tables_match_cgb_boot_bin() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../bios/cgb_boot.bin");
        let Ok(bin) = std::fs::read(path) else { return };
        assert_eq!(bin.len(), 0x900);
        assert_eq!(bin[0x6C7..0x716], TITLE_CHECKSUMS);
        assert_eq!(bin[0x716..0x733], FOURTH_LETTERS);
        assert_eq!(bin[0x733..0x791], PALETTE_PER_CHECKSUM);
        assert_eq!(bin[0x791..0x7E8], PALETTE_COMBINATIONS);
        assert_eq!(bin[0x7E8..0x8D8], PALETTE_DATA);
        assert_eq!(bin[0x8E4..0x8F0], KEY_COMBO_JOYP);
        assert_eq!(bin[0x8F0..0x8FC], KEY_COMBO_ID);
        assert_eq!(bin[0x7A..0x7C], FIXUP_CHECKSUMS);
    }

    /// `walk` re-derives the Nintendo predicate inline so it can charge each
    /// branch, which is exactly the kind of second copy that drifts. Pin the
    /// two against each other over every shape of licensee pair.
    #[test]
    fn walk_agrees_with_the_nintendo_predicate() {
        for old in [0x00u8, 0x01, 0x33, 0xA4, 0xFF] {
            for new in [*b"01", *b"00", *b"08", *b"0Z", *b"ZZ", *b"1Z", *b"\0\0"] {
                let nintendo = is_nintendo(old, new);
                // Only the Nintendo arms can pick a non-default palette, and
                // only they can run long enough to reach the table.
                let picked_from_table = select_palette_id(&title(b"TETRIS"), old, new) != 0x7C;
                assert_eq!(
                    nintendo,
                    picked_from_table,
                    "$014B=0x{old:02X} new={:?}",
                    std::str::from_utf8(&new).unwrap_or("??")
                );
            }
        }
    }

    /// The reference arm — a $33 cart whose new-licensee code does not start
    /// with '0' — must cost exactly nothing, because that is the shape the HLE
    /// base constants in `gb.rs` are calibrated on (mooneye's boot_div carts).
    #[test]
    fn compat_extra_is_zero_for_the_calibration_arm() {
        for new in [*b"ZZ", *b"A4", *b"\0\0", *b"1Z"] {
            assert_eq!(compat_boot_extra_cc(&title(b"mooneye-gb test"), 0x33, new), 0);
        }
    }

    /// The arms the old one-constant model flattened. Values are re-measured
    /// against the real images by `tests/boot_rom_hle_parity.rs`; this pins the
    /// pure function so a refactor cannot quietly move them.
    #[test]
    fn compat_extra_covers_every_arm() {
        let t = |s: &[u8]| title(s);
        // Non-Nintendo: three arms within 36 cc.
        assert_eq!(compat_boot_extra_cc(&t(b"RUSTYBOI"), 0x00, *b"ZZ"), 4);
        assert_eq!(compat_boot_extra_cc(&t(b"RUSTYBOI"), 0xA4, *b"ZZ"), 4);
        assert_eq!(compat_boot_extra_cc(&t(b"RUSTYBOI"), 0x33, *b"08"), 36);
        // Nintendo: thousands of cycles, and title dependent.
        assert_eq!(compat_boot_extra_cc(&t(b""), 0x01, *b"\0\0"), 604);
        assert_eq!(compat_boot_extra_cc(&t(b"TETRIS"), 0x01, *b"\0\0"), 1612);
        assert_eq!(compat_boot_extra_cc(&t(b"ZELDA"), 0x01, *b"\0\0"), 2772);
        assert_eq!(compat_boot_extra_cc(&t(b"SUPER MARIOLAND"), 0x01, *b"\0\0"), 3268);
        assert_eq!(compat_boot_extra_cc(&t(b"ZZZZZZZZ"), 0x01, *b"\0\0"), 4364);
        // The $33 + "01" spelling reaches the same walk 32 cc later.
        assert_eq!(compat_boot_extra_cc(&t(b"ZZZZZZZZ"), 0x33, *b"01"), 4396);
        // The two singled-out checksums, and their neighbours to show the
        // fixup is keyed on the checksum and not on the palette it selects.
        assert_eq!(compat_boot_extra_cc(&t(b"\x58"), 0x01, *b"\0\0"), 3828);
        assert_eq!(compat_boot_extra_cc(&t(b"\x43"), 0x01, *b"\0\0"), 3644);
        // …and are unreachable from a non-Nintendo cart, which never writes
        // $D000 for the $05F8 compare.
        assert_eq!(compat_boot_extra_cc(&t(b"\x58"), 0x00, *b"ZZ"), 4);
    }
}
