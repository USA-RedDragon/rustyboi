//! Offline game identification via a No-Intro CRC32→name index.
//!
//! [`identify`] CRC32s a ROM and looks the checksum up in a runtime index built
//! from the No-Intro DATs for Game Boy + Game Boy Color. The canonical No-Intro
//! name drives the window title, the ROM library, and the libretro cheat-DB
//! fetch. Homebrew, hacks, bad dumps and overdumps won't match; callers fall back
//! to the cartridge header title via [`resolve_game_name`].
//!
//! Nothing is embedded at build time: the DATs are CC-BY-SA-4.0 libretro-database
//! material, so shipping them would place ShareAlike obligations on rustyboi
//! binaries. Instead the index starts empty and each frontend downloads the two
//! DATs at runtime (see [`dat_urls`]) and feeds them in via [`load_dats`]. Until
//! that happens the store is empty and every ROM reports as unidentified —
//! callers gracefully fall back to the header title.

use std::collections::BTreeMap;
use std::sync::RwLock;

use crate::patch::crc32;

/// The runtime index as `(crc32, name)` sorted by crc for binary search. Empty
/// until a frontend downloads the DATs and calls [`load_dats`].
static INDEX: RwLock<Vec<(u32, String)>> = RwLock::new(Vec::new());

/// The base of the raw libretro-database No-Intro DAT tree.
const DAT_BASE: &str =
    "https://raw.githubusercontent.com/libretro/libretro-database/master/metadat/no-intro/";

/// The two DAT filenames (Game Boy + Game Boy Color) whose CRC→name tables the
/// index is built from.
const DAT_FILES: [&str; 2] = ["Nintendo - Game Boy.dat", "Nintendo - Game Boy Color.dat"];

/// The two percent-encoded DAT URLs to download, in order. The frontend GETs each
/// (caching the body) and feeds the bodies back through [`load_dats`].
pub fn dat_urls() -> Vec<String> {
    DAT_FILES
        .iter()
        .map(|f| format!("{DAT_BASE}{}", crate::cheat_db::percent_encode(f)))
        .collect()
}

/// Parse one No-Intro DAT body into `(crc32, name)` pairs.
///
/// The DAT is clrmamepro text: each `game (...)` block has a `\tname "NAME"` line
/// and a later `rom ( … crc XXXXXXXX … )` line. A tab-anchored `name "…"` sets the
/// pending name; the next 8-hex `crc` pairs with it and clears the pending name.
pub fn parse_dat(text: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let mut pending: Option<String> = None;
    for line in text.lines() {
        if let Some(name) = parse_name_line(line) {
            pending = Some(name);
        } else if let Some(crc) = parse_crc(line)
            && let Some(name) = pending.take()
        {
            out.push((crc, name));
        }
    }
    out
}

/// A tab-anchored `name "NAME"` line yields `NAME`. Anchoring on the leading tab
/// distinguishes the game's own name line from the `rom ( name "…" … )` line,
/// whose `name` is preceded by `rom ( `, not a bare tab.
fn parse_name_line(line: &str) -> Option<String> {
    let rest = line.strip_prefix('\t')?.strip_prefix("name \"")?;
    let end = rest.rfind('"')?;
    Some(rest[..end].to_string())
}

/// The first `crc XXXXXXXX` (8 hex digits, word-boundary before `crc`) in `line`.
fn parse_crc(line: &str) -> Option<u32> {
    let mut from = 0;
    while let Some(rel) = line[from..].find("crc ") {
        let pos = from + rel;
        let boundary = pos == 0 || !is_word_byte(line.as_bytes()[pos - 1]);
        if boundary {
            let hex: String = line[pos + 4..].chars().take(8).collect();
            if hex.len() == 8 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return u32::from_str_radix(&hex, 16).ok();
            }
        }
        from = pos + 4;
    }
    None
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Parse each DAT body and merge its entries into the runtime index, then sort by
/// crc for binary search. Later entries (and later bodies) win on a crc collision.
/// Merges with any already-loaded entries, so bodies may be supplied incrementally.
pub fn load_dats(bodies: &[String]) {
    // BTreeMap keeps the merged set sorted by crc and dedupes (last write wins).
    let mut merged: BTreeMap<u32, String> = BTreeMap::new();
    {
        let existing = INDEX.read().expect("no_intro index poisoned");
        for (crc, name) in existing.iter() {
            merged.insert(*crc, name.clone());
        }
    }
    for body in bodies {
        for (crc, name) in parse_dat(body) {
            merged.insert(crc, name);
        }
    }
    *INDEX.write().expect("no_intro index poisoned") = merged.into_iter().collect();
}

/// Replace the runtime index outright with `entries` (sorted by crc here). Mainly
/// for tests / callers that already hold parsed pairs; frontends use [`load_dats`].
pub fn set_index(mut entries: Vec<(u32, String)>) {
    entries.sort_by_key(|(c, _)| *c);
    entries.dedup_by_key(|(c, _)| *c);
    *INDEX.write().expect("no_intro index poisoned") = entries;
}

/// The canonical No-Intro name for a ROM's CRC32, or `None` if unindexed (or the
/// index hasn't been downloaded yet). For callers that already have the checksum
/// (e.g. the ROM library, which CRC32s files during its scan).
pub fn name_for_crc(crc: u32) -> Option<String> {
    let idx = INDEX.read().expect("no_intro index poisoned");
    idx.binary_search_by_key(&crc, |(c, _)| *c).ok().map(|i| idx[i].1.clone())
}

/// The canonical No-Intro name for `rom`, or `None` if its CRC32 isn't indexed.
pub fn identify(rom: &[u8]) -> Option<String> {
    name_for_crc(crc32(rom))
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
    identify(rom).or_else(|| header_title(rom))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The runtime INDEX is a process-global RwLock. Tests that replace it via
    // `set_index` must not interleave with each other or with `load_dats`
    // readers, so index-touching tests serialize on this lock. (Tests that only
    // parse text or read a header don't touch the index and skip it.)
    static INDEX_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        INDEX_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A 0x150-byte ROM carrying `title` in the header title region.
    fn header_rom(title: &[u8]) -> Vec<u8> {
        let mut rom = vec![0u8; 0x150];
        rom[0x134..0x134 + title.len()].copy_from_slice(title);
        rom
    }

    #[test]
    fn dat_urls_are_percent_encoded() {
        let urls = dat_urls();
        assert_eq!(urls.len(), 2);
        assert!(urls[0].ends_with("/Nintendo%20-%20Game%20Boy.dat"));
        assert!(urls[1].ends_with("/Nintendo%20-%20Game%20Boy%20Color.dat"));
    }

    #[test]
    fn parse_dat_pairs_name_with_following_crc() {
        let dat = "\
clrmamepro (
\tname \"Nintendo - Game Boy\"
)

game (
\tname \"Tetris (World) (Rev 1)\"
\tdescription \"Tetris (World) (Rev 1)\"
\trom ( name \"Tetris (World) (Rev 1).gb\" size 32768 crc D5C0A94F md5 abc )
)

game (
\tname \"Alleyway (World)\"
\trom ( name \"Alleyway (World).gb\" size 131072 crc 0F1E2D3C )
)
";
        let pairs = parse_dat(dat);
        assert_eq!(
            pairs,
            vec![
                (0xD5C0A94F, "Tetris (World) (Rev 1)".to_string()),
                (0x0F1E2D3C, "Alleyway (World)".to_string()),
            ]
        );
    }

    #[test]
    fn load_dats_indexes_and_dedupes() {
        let _g = lock();
        // Unique crcs so this test is order-independent of any other test that
        // also loads into the shared global index.
        let a = "game (\n\tname \"Game A\"\n\trom ( crc AABBCC01 )\n)\n";
        let b = "game (\n\tname \"Game B\"\n\trom ( crc AABBCC02 )\n)\n";
        load_dats(&[a.to_string(), b.to_string()]);
        assert_eq!(name_for_crc(0xAABBCC01).as_deref(), Some("Game A"));
        assert_eq!(name_for_crc(0xAABBCC02).as_deref(), Some("Game B"));
        assert_eq!(name_for_crc(0xDEAD0000), None);
    }

    #[test]
    fn header_title_reads_ascii() {
        let mut rom = vec![0u8; 0x150];
        rom[0x134..0x13b].copy_from_slice(b"TETRIS\0");
        assert_eq!(header_title(&rom).as_deref(), Some("TETRIS"));
    }

    #[test]
    fn parse_crc_requires_a_word_boundary_before_crc() {
        // A standalone `crc` token (start of line) is accepted.
        assert_eq!(parse_crc("crc DEADBEEF"), Some(0xDEAD_BEEF));
        // ...and one preceded by a non-word byte (space) inside a rom line.
        assert_eq!(
            parse_crc("\trom ( name \"x.gb\" size 8 crc AABBCC33 )"),
            Some(0xAABB_CC33)
        );
        // A "crc" that is the tail of a word (preceded by 's') must NOT match,
        // and with no other candidate the line yields None.
        assert_eq!(parse_crc("\tname \"Descrc AABBCC44\""), None);
    }

    #[test]
    fn parse_name_line_only_matches_the_tab_anchored_name() {
        // The game's own name line (leading tab, then `name "`).
        assert_eq!(parse_name_line("\tname \"My Game\"").as_deref(), Some("My Game"));
        // The `rom ( name "…" )` line's name is preceded by `rom ( `, not a bare
        // tab, so it must be rejected (otherwise the wrong name would pair).
        assert!(parse_name_line("\trom ( name \"My Game.gb\" size 8 )").is_none());
    }

    #[test]
    fn header_title_trims_spaces_and_nul() {
        // Trailing spaces are within the printable range but trimmed off.
        let mut rom = vec![0u8; 0x150];
        rom[0x134..0x13c].copy_from_slice(b"GAME    ");
        assert_eq!(header_title(&rom).as_deref(), Some("GAME"));
        // A NUL terminates the title early.
        assert_eq!(header_title(&header_rom(b"POKEMON\0RED")).as_deref(), Some("POKEMON"));
    }

    #[test]
    fn header_title_edge_cases() {
        // An all-zero header region has no title.
        assert_eq!(header_title(&vec![0u8; 0x150]), None);
        // A non-printable / high byte terminates the title (so a title that
        // starts with one is empty → None).
        assert_eq!(header_title(&header_rom(&[0xFF, b'X'])), None);
        // A ROM too short to hold the header title region yields None.
        assert_eq!(header_title(&vec![0u8; 0x140]), None);
    }

    #[test]
    fn resolve_game_name_fallback_chain() {
        let _g = lock();
        // (1) A No-Intro index hit wins over the header title.
        let indexed = header_rom(b"HEADERTITLE");
        let crc = crate::patch::crc32(&indexed);
        set_index(vec![(crc, "Canonical No-Intro Name".to_string())]);
        assert_eq!(resolve_game_name(&indexed).as_deref(), Some("Canonical No-Intro Name"));

        // (2) A ROM absent from the index falls back to its header title.
        let headered = header_rom(b"HOMEBREW");
        assert_ne!(crate::patch::crc32(&headered), crc, "distinct crc so it is unindexed");
        assert_eq!(resolve_game_name(&headered).as_deref(), Some("HOMEBREW"));

        // (3) Neither indexed nor a usable header title → None.
        assert_eq!(resolve_game_name(&vec![0u8; 0x150]), None);
    }

    #[test]
    fn set_index_sorts_and_dedupes() {
        let _g = lock();
        set_index(vec![
            (0x00FF_EE03, "C".to_string()),
            (0x00FF_EE01, "A".to_string()),
            (0x00FF_EE02, "B".to_string()),
            (0x00FF_EE01, "A duplicate".to_string()),
        ]);
        // binary search succeeding on every key proves the store is sorted;
        // the first of the equal keys survives the dedupe.
        assert_eq!(name_for_crc(0x00FF_EE01).as_deref(), Some("A"));
        assert_eq!(name_for_crc(0x00FF_EE02).as_deref(), Some("B"));
        assert_eq!(name_for_crc(0x00FF_EE03).as_deref(), Some("C"));
    }
}
