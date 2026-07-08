//! Offline game identification via the bundled No-Intro CRC32→name index.
//!
//! [`identify`] CRC32s a ROM and looks the checksum up in a compact blob
//! (`data/no_intro.bin`, built by `tools/gen-nointro.py` from the No-Intro DAT
//! for Game Boy + Game Boy Color). The canonical No-Intro name drives the window
//! title, the ROM library, and the libretro cheat-DB fetch. Homebrew, hacks, bad
//! dumps and overdumps won't match; callers fall back to the cartridge header
//! title via [`resolve_game_name`].

use std::sync::OnceLock;

use crate::patch::crc32;

static BLOB: &[u8] = include_bytes!("../data/no_intro.bin");

/// The parsed index as `(crc32, name)` sorted by crc for binary search; names
/// borrow the static blob. Built once on first use.
fn index() -> &'static [(u32, &'static str)] {
    static INDEX: OnceLock<Vec<(u32, &'static str)>> = OnceLock::new();
    INDEX.get_or_init(|| parse(BLOB).unwrap_or_default())
}

/// Parse the `RBNI` blob: magic, version byte, u32 count, then `count` entries of
/// (crc32 u32, name_len u16, name utf-8). Returns `None` on any malformation.
fn parse(blob: &'static [u8]) -> Option<Vec<(u32, &'static str)>> {
    if blob.get(0..4)? != b"RBNI" || *blob.get(4)? != 1 {
        return None;
    }
    let count = u32::from_le_bytes(blob.get(5..9)?.try_into().ok()?) as usize;
    let mut out = Vec::with_capacity(count);
    let mut p = 9;
    for _ in 0..count {
        let crc = u32::from_le_bytes(blob.get(p..p + 4)?.try_into().ok()?);
        let len = u16::from_le_bytes(blob.get(p + 4..p + 6)?.try_into().ok()?) as usize;
        p += 6;
        let name = std::str::from_utf8(blob.get(p..p + len)?).ok()?;
        p += len;
        out.push((crc, name));
    }
    Some(out)
}

/// The canonical No-Intro name for `rom`, or `None` if its CRC32 isn't indexed.
pub fn identify(rom: &[u8]) -> Option<&'static str> {
    let crc = crc32(rom);
    let idx = index();
    idx.binary_search_by_key(&crc, |(c, _)| *c).ok().map(|i| idx[i].1)
}

/// The cartridge header title (0x0134..0x0143), used as a fallback when the ROM
/// isn't in the No-Intro index. Stops at the first NUL or non-printable byte, so
/// it also handles CGB carts whose title region is shorter (the CGB flag / a
/// manufacturer code terminates it).
pub fn header_title(rom: &[u8]) -> Option<String> {
    let raw = rom.get(0x134..0x144)?;
    let end = raw
        .iter()
        .position(|&b| !(0x20..0x7f).contains(&b))
        .unwrap_or(raw.len());
    let s = std::str::from_utf8(&raw[..end]).ok()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// A human-readable game name for `rom`: the canonical No-Intro name if known,
/// else the cartridge header title, else `None`.
pub fn resolve_game_name(rom: &[u8]) -> Option<String> {
    identify(rom).map(str::to_string).or_else(|| header_title(rom))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_parses_and_is_sorted() {
        let idx = index();
        assert!(idx.len() > 4000, "expected the full GB/GBC index, got {}", idx.len());
        assert!(idx.windows(2).all(|w| w[0].0 <= w[1].0), "index must be crc-sorted");
    }

    #[test]
    fn header_title_reads_ascii() {
        let mut rom = vec![0u8; 0x150];
        rom[0x134..0x13b].copy_from_slice(b"TETRIS\0");
        assert_eq!(header_title(&rom).as_deref(), Some("TETRIS"));
    }
}
