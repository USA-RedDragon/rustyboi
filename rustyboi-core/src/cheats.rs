//! Canonical Game Boy cheat-code decoders (Game Genie + GameShark).
//!
//! Both frontends (`rustyboi-session` and `rustyboi-libretro`) decode cheat
//! strings through the functions here so there is exactly ONE nibble ordering
//! in the codebase. The decode is only the string -> fields step; the actual
//! effect (ROM patch / RAM poke) is applied through
//! [`crate::cartridge::Cartridge::apply_rom_patch`] and
//! [`crate::gb::GB::write_memory`] by the caller.
//!
//! ## Game Genie (9 nibbles `ABC-DEF-GHI`, or 6 nibbles `ABC-DEF`)
//!
//! Layout is taken bit-for-bit from mGBA's authoritative implementation
//! (`mgba-emu/mgba`, `src/gb/cheats.c`, `GBCheatAddGameGenieLine`):
//!
//! ```text
//! op1 = ABC  op2 = DEF  op3 = GHI            (each 3 nibbles, high-first)
//! value   = op1 >> 4                          => byte AB
//! address = (op1 & 0xF) << 8                  => nibble C at bits 8..11
//!         | (op2 >> 4)                         => byte DE at bits 0..7
//!         | ((op2 & 0xF) ^ 0xF) << 12          => nibble (F ^ 0xF) at bits 12..15
//! ```
//!
//! So, writing the code nibbles as `A B C D E F [G H I]`, the 16-bit address is
//! `(F ^ 0xF) C D E` (high nibble first). The optional check/compare byte from
//! `op3 = GHI` is `ROR((G<<28)|I, 2) | (>>24)` XOR `0xBA`, which reduces to the
//! 8-bit form `rotate_right((G<<4)|I, 2) ^ 0xBA` (nibble H is unused).
//!
//! ## GameShark (8 nibbles `ABCDEFGH`)
//!
//! From mGBA `GBCheatAddGameShark`: `value = op>>16` (byte CD), and
//! `address = ((op & 0xFF) << 8) | ((op >> 8) & 0xFF)` = `GHEF` (byte AB is the
//! external-RAM bank, ignored by our flat write path).

/// A decoded Game Genie ROM patch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GameGenie {
    /// CPU address (0x0000-0x7FFF) whose ROM byte is replaced.
    pub addr: u16,
    /// Replacement byte.
    pub value: u8,
    /// Optional compare byte: only patch if the existing byte matches.
    pub compare: Option<u8>,
}

/// A decoded GameShark RAM poke: write `value` to `addr` (re-applied per frame).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GameShark {
    /// Target RAM address (little-endian `GHEF` from the code).
    pub addr: u16,
    /// Value to write.
    pub value: u8,
}

/// Turn a hex character into its 0-15 value, or `None` if not a hex digit.
fn hex_nibble(c: char) -> Option<u8> {
    c.to_digit(16).map(|d| d as u8)
}

/// Strip separators and decode a code string into hex nibbles. Whitespace,
/// `-` and `:` are ignored; any other non-hex character fails.
fn nibbles(code: &str) -> Option<Vec<u8>> {
    code.chars()
        .filter(|c| !c.is_whitespace() && *c != '-' && *c != ':')
        .map(hex_nibble)
        .collect()
}

/// Decode a Game Genie code (`ABC-DEF` or `ABC-DEF-GHI`, separators optional)
/// into a [`GameGenie`]. Returns `None` for any input that is not 6 or 9 valid
/// hex nibbles. See the module docs for the exact (mGBA-derived) layout.
pub fn decode_game_genie(code: &str) -> Option<GameGenie> {
    let n = nibbles(code)?;
    if n.len() != 6 && n.len() != 9 {
        return None;
    }
    decode_game_genie_nibbles(&n)
}

/// Decode from already-extracted nibbles (6 or 9). Shared with frontends that
/// have their own length dispatch.
pub fn decode_game_genie_nibbles(n: &[u8]) -> Option<GameGenie> {
    if n.len() != 6 && n.len() != 9 {
        return None;
    }
    // value = byte AB (nibbles 0,1).
    let value = (n[0] << 4) | n[1];
    // address = (F ^ 0xF) C D E  (nibble 5 is the high nibble, XOR 0xF).
    let addr = ((n[5] as u16) << 12) | ((n[2] as u16) << 8) | ((n[3] as u16) << 4) | (n[4] as u16);
    let addr = addr ^ 0xF000;
    let compare = if n.len() == 9 {
        // Compare byte from nibbles G (6) and I (8); H (7) is unused.
        let raw = (n[6] << 4) | n[8];
        Some(raw.rotate_right(2) ^ 0xBA)
    } else {
        None
    };
    Some(GameGenie { addr, value, compare })
}

/// Decode an 8-nibble GameShark code (`ABCDEFGH`, separators optional) into a
/// [`GameShark`]. Returns `None` unless it is exactly 8 valid hex nibbles.
pub fn decode_gameshark(code: &str) -> Option<GameShark> {
    let n = nibbles(code)?;
    if n.len() != 8 {
        return None;
    }
    decode_gameshark_nibbles(&n)
}

/// Decode from already-extracted nibbles (exactly 8).
pub fn decode_gameshark_nibbles(n: &[u8]) -> Option<GameShark> {
    if n.len() != 8 {
        return None;
    }
    // value = byte CD (nibbles 2,3); byte AB (bank) is ignored.
    let value = (n[2] << 4) | n[3];
    // address = GHEF (little-endian): low byte EF (nibbles 4,5), high byte GH
    // (nibbles 6,7).
    let addr = ((n[6] as u16) << 12) | ((n[7] as u16) << 8) | ((n[4] as u16) << 4) | (n[5] as u16);
    Some(GameShark { addr, value })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Vector worked out directly from mGBA `src/gb/cheats.c`:
    // 00A-B7F: op1=0x00A, op2=0xB7F.
    //   value   = op1>>4 = 0x00
    //   address = (op1&0xF)<<8 | (op2>>4) | ((op2&0xF)^0xF)<<12
    //           = 0xA00 | 0xB7 | (0xF^0xF)<<12 = 0x0AB7
    #[test]
    fn game_genie_six_digit_no_compare() {
        let gg = decode_game_genie("00A-B7F").unwrap();
        assert_eq!(gg, GameGenie { addr: 0x0AB7, value: 0x00, compare: None });
    }

    // 00A-B7F-C61: same addr/value; compare from G=0xC, I=0x1:
    //   raw = 0xC1, rotate_right(2) = 0x70, 0x70 ^ 0xBA = 0xCA
    #[test]
    fn game_genie_nine_digit_with_compare() {
        let gg = decode_game_genie("00A-B7F-C61").unwrap();
        assert_eq!(gg, GameGenie { addr: 0x0AB7, value: 0x00, compare: Some(0xCA) });
    }

    // A non-trivial address exercising the F^0xF high nibble and the C/D/E
    // rearrangement. 3E1-D62: nibbles 3 E 1 D 6 2.
    //   value = 0x3E
    //   address = (0x2^0xF)<<12 | 0x1<<8 | 0xD<<4 | 0x6 = 0xD1D6
    #[test]
    fn game_genie_high_nibble_xor() {
        let gg = decode_game_genie("3E1-D62").unwrap();
        assert_eq!(gg, GameGenie { addr: 0xD1D6, value: 0x3E, compare: None });
    }

    // GameShark 01FFDEC0: bank 01, value FF, addr little-endian GHEF = 0xC0DE.
    #[test]
    fn gameshark_basic() {
        let gs = decode_gameshark("01FFDEC0").unwrap();
        assert_eq!(gs, GameShark { addr: 0xC0DE, value: 0xFF });
    }

    #[test]
    fn rejects_bad_length_and_hex() {
        assert!(decode_game_genie("ABCDEF0").is_none()); // 7 nibbles
        assert!(decode_gameshark("ZZZZZZZZ").is_none()); // non-hex
        assert!(decode_gameshark("01FFDEC").is_none()); // 7 nibbles
    }
}
