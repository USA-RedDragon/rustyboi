//! Game Genie + GameShark cheat codes, parsed here and applied through the
//! existing core hooks.
//!
//! - **Game Genie** (`AAA-BBB-CCC` or `AAA-BBB`): a ROM patch. A 9-digit code
//!   carries a replacement byte, a 16-bit address, and a compare byte; the
//!   6-digit form drops the compare. Applied once (per code) via
//!   [`Cartridge::apply_rom_patch`], which honors the compare.
//! - **GameShark** (`ABCDEFGH`): a RAM poke. Byte `AB` is the external RAM bank
//!   (unused by our flat write path), `CD` the new value, `GHEF` the
//!   little-endian address. Re-applied every frame by writing through the bus
//!   ([`GB::write_memory`]), which is exactly how the libretro frontend pokes
//!   GameShark RAM.
//!
//! The session stores active codes and (re)applies them: Game Genie once on
//! (re)insert / enable, GameShark on every `run_frame`. Removal clears the
//! stored code; Game Genie removal cannot un-patch an already-loaded ROM in
//! place (the patch lives in `rom_data`), so it takes effect on the next ROM
//! (re)load — documented, not silently wrong.

use rustyboi_core_lib::gb::GB;
use serde::{Deserialize, Serialize};

/// A parsed cheat code, ready to apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cheat {
    /// Game Genie ROM patch: replace the byte at `addr` with `value`, optionally
    /// only if the existing byte equals `compare`.
    GameGenie {
        addr: u16,
        value: u8,
        compare: Option<u8>,
    },
    /// GameShark RAM poke: write `value` to `addr` every frame.
    GameShark { addr: u16, value: u8 },
}

/// Why a cheat string failed to parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheatError {
    /// Wrong length / separator layout for both known formats.
    BadFormat,
    /// A digit was not valid hexadecimal.
    BadHexDigit,
}

impl core::fmt::Display for CheatError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CheatError::BadFormat => write!(f, "unrecognized cheat code format"),
            CheatError::BadHexDigit => write!(f, "cheat code contains a non-hex digit"),
        }
    }
}

impl std::error::Error for CheatError {}

impl Cheat {
    /// Parse a Game Genie (`AAA-BBB[-CCC]`, hyphens optional) or GameShark
    /// (`ABCDEFGH`) code, case-insensitively.
    pub fn parse(code: &str) -> Result<Cheat, CheatError> {
        let hex: Vec<u8> = code
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-' && *c != ':')
            .map(|c| c.to_ascii_uppercase())
            .map(|c| c.to_digit(16).map(|d| d as u8).ok_or(CheatError::BadHexDigit))
            .collect::<Result<_, _>>()?;

        match hex.len() {
            6 | 9 => Ok(Self::parse_game_genie(&hex)),
            8 => Ok(Self::parse_gameshark(&hex)),
            _ => Err(CheatError::BadFormat),
        }
    }

    /// Decode a Game Genie code from its hex nibbles.
    ///
    /// Layout (nibble indices): `0 1 = value`; `3 4 5 2 = address` with the
    /// high nibble (index 2) XORed into place last; a 9-nibble code adds a
    /// compare byte at nibbles `6 8` (each rotated right by 2 bits) XOR 0xBA.
    /// This matches the standard GB Game Genie encoding.
    fn parse_game_genie(n: &[u8]) -> Cheat {
        let value = (n[0] << 4) | n[1];
        // Address: nibbles 5,2,3,4 form 0x?XYZ where the top nibble (n[2]) is
        // XORed with 0xF. Standard layout: addr = n[5..] combine.
        let addr = ((n[3] as u16) << 8)
            | ((n[4] as u16) << 4)
            | (n[5] as u16)
            | (((n[2] ^ 0xF) as u16) << 12);
        let compare = if n.len() == 9 {
            let raw = (n[6] << 4) | n[8];
            // Rotate right by 2 and XOR with 0xBA (GB Game Genie compare enc.).
            Some((raw.rotate_right(2)) ^ 0xBA)
        } else {
            None
        };
        Cheat::GameGenie { addr, value, compare }
    }

    /// Decode a GameShark code: `AB CD EF GH` -> bank `AB` (ignored by the flat
    /// bus write), value `CD`, address `GHEF` (little-endian).
    fn parse_gameshark(n: &[u8]) -> Cheat {
        let value = (n[2] << 4) | n[3];
        let addr = ((n[6] as u16) << 12)
            | ((n[7] as u16) << 8)
            | ((n[4] as u16) << 4)
            | (n[5] as u16);
        Cheat::GameShark { addr, value }
    }
}

/// The active cheat set for a session. Game Genie codes are applied once when
/// enabled/ROM (re)loaded; GameShark codes are re-poked each frame.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CheatSet {
    codes: Vec<(String, Cheat)>,
}

impl CheatSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse and add a code. Returns the parsed cheat. Duplicate raw strings
    /// are ignored (idempotent add).
    pub fn add(&mut self, code: &str) -> Result<Cheat, CheatError> {
        let cheat = Cheat::parse(code)?;
        if !self.codes.iter().any(|(c, _)| c == code) {
            self.codes.push((code.to_string(), cheat));
        }
        Ok(cheat)
    }

    /// Remove a previously-added raw code string. Returns true if present.
    pub fn remove(&mut self, code: &str) -> bool {
        let before = self.codes.len();
        self.codes.retain(|(c, _)| c != code);
        self.codes.len() != before
    }

    /// Remove all codes.
    pub fn clear(&mut self) {
        self.codes.clear();
    }

    /// The raw code strings currently active.
    pub fn codes(&self) -> impl Iterator<Item = &str> {
        self.codes.iter().map(|(c, _)| c.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.codes.is_empty()
    }

    /// Apply every Game Genie ROM patch to the inserted cartridge. Call after
    /// (re)inserting a ROM or when the cheat set changes; a no-op if there is
    /// no cartridge.
    pub fn apply_rom_patches(&self, gb: &mut GB) {
        let Some(cart) = gb.cartridge_mut() else { return };
        for (_, cheat) in &self.codes {
            if let Cheat::GameGenie { addr, value, compare } = cheat {
                cart.apply_rom_patch(*addr, *value, *compare);
            }
        }
    }

    /// Poke every GameShark RAM code through the bus. Call once per frame
    /// (after emulating, before presenting), mirroring the libretro path.
    pub fn apply_ram_pokes(&self, gb: &mut GB) {
        for (_, cheat) in &self.codes {
            if let Cheat::GameShark { addr, value } = cheat {
                gb.write_memory(*addr, *value);
            }
        }
    }

    /// True if any GameShark code is active (so the session knows to poke each
    /// frame instead of doing nothing).
    pub fn has_ram_pokes(&self) -> bool {
        self.codes
            .iter()
            .any(|(_, c)| matches!(c, Cheat::GameShark { .. }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gameshark() {
        // 01FF C0DE: bank 01, value FF, addr C0DE -> little-endian GHEF.
        let c = Cheat::parse("01FFDEC0").unwrap();
        assert_eq!(c, Cheat::GameShark { addr: 0xC0DE, value: 0xFF });
    }

    #[test]
    fn parses_game_genie_six_digit_has_no_compare() {
        let c = Cheat::parse("00A-B7F").unwrap();
        match c {
            Cheat::GameGenie { compare, .. } => assert!(compare.is_none()),
            _ => panic!("expected Game Genie"),
        }
    }

    #[test]
    fn parses_game_genie_nine_digit_has_compare() {
        let c = Cheat::parse("00A-B7F-C61").unwrap();
        match c {
            Cheat::GameGenie { compare, value, .. } => {
                assert!(compare.is_some());
                assert_eq!(value, 0x00);
            }
            _ => panic!("expected Game Genie"),
        }
    }

    #[test]
    fn rejects_bad_length_and_hex() {
        // Valid hex, but 7 nibbles matches no known format.
        assert_eq!(Cheat::parse("ABCDEF0").unwrap_err(), CheatError::BadFormat);
        // Right length (8) but a non-hex digit.
        assert_eq!(Cheat::parse("ZZZZZZZZ").unwrap_err(), CheatError::BadHexDigit);
    }

    #[test]
    fn set_add_remove_is_idempotent() {
        let mut set = CheatSet::new();
        set.add("01FFDEC0").unwrap();
        set.add("01FFDEC0").unwrap(); // dup ignored
        assert_eq!(set.codes().count(), 1);
        assert!(set.has_ram_pokes());
        assert!(set.remove("01FFDEC0"));
        assert!(set.is_empty());
    }
}
