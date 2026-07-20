//! Extract the ROM image from a `.zip` container.
//!
//! `Cartridge::from_bytes` already unzips when building the machine, but the
//! session must unzip too — otherwise `original_rom` (used for game
//! identification, cheat-DB lookup, ROM patching, and the rom id) would hold the
//! archive bytes instead of the ROM, so a zipped game runs but can't be
//! identified.

use std::io::{Cursor, Read};

/// If `bytes` is a zip, return the contained ROM (a `.gb`/`.gbc`/`.sgb` entry, or
/// else the largest file); otherwise return `bytes` unchanged. A malformed or
/// unsupported archive falls back to the raw bytes so the cartridge loader
/// surfaces the error.
pub(crate) fn extract_rom(bytes: &[u8]) -> Vec<u8> {
    if bytes.len() < 4 || &bytes[..4] != b"PK\x03\x04" {
        return bytes.to_vec();
    }
    extract_from_zip(bytes).unwrap_or_else(|| bytes.to_vec())
}

fn extract_from_zip(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
    const EXTS: [&str; 3] = [".gb", ".gbc", ".sgb"];
    let mut pick: Option<usize> = None;
    let mut largest = (0usize, 0u64);
    for i in 0..archive.len() {
        let f = archive.by_index(i).ok()?;
        if f.is_dir() {
            continue;
        }
        if EXTS.iter().any(|e| f.name().to_lowercase().ends_with(e)) {
            pick = Some(i);
            break;
        }
        if f.size() > largest.1 {
            largest = (i, f.size());
        }
    }
    let idx = pick.unwrap_or(largest.0);
    let mut f = archive.by_index(idx).ok()?;
    let mut data = Vec::with_capacity(f.size() as usize);
    f.read_to_end(&mut data).ok()?;
    (!data.is_empty()).then_some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_zip_passes_through() {
        let rom = vec![0xABu8; 0x200];
        assert_eq!(extract_rom(&rom), rom);
    }

    #[test]
    fn malformed_zip_falls_back_to_raw() {
        // Has the zip magic but is not a valid archive → return the bytes as-is
        // so the cartridge loader reports a clear error rather than silently
        // dropping the ROM.
        let fake = b"PK\x03\x04not a real zip".to_vec();
        assert_eq!(extract_rom(&fake), fake);
    }

    /// Build an in-memory zip from `(name, contents)` entries. Names ending in
    /// `/` are written as directory entries. Uses Stored (no compression) so the
    /// test needs no codec feature.
    fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use zip::write::SimpleFileOptions;
        use zip::CompressionMethod;
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for (name, contents) in entries {
            if let Some(dir) = name.strip_suffix('/') {
                w.add_directory(dir, opts).unwrap();
            } else {
                w.start_file(*name, opts).unwrap();
                std::io::Write::write_all(&mut w, contents).unwrap();
            }
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn extension_match_is_extracted() {
        let rom = vec![0x11u8; 0x400];
        let zip = make_zip(&[("Game.gb", &rom)]);
        assert_eq!(extract_rom(&zip), rom);
    }

    #[test]
    fn non_rom_extension_falls_back_to_largest_file() {
        // No .gb/.gbc/.sgb entry, so the largest file wins regardless of name.
        let small = vec![0x22u8; 0x10];
        let big = vec![0x33u8; 0x800];
        let zip = make_zip(&[("readme.txt", &small), ("data.bin", &big)]);
        assert_eq!(extract_rom(&zip), big);
    }

    #[test]
    fn first_extension_match_wins_over_later_roms() {
        let first = vec![0x44u8; 0x100];
        let second = vec![0x55u8; 0x900]; // larger, but later — extension match still wins
        let zip = make_zip(&[("a.gbc", &first), ("b.gb", &second)]);
        assert_eq!(extract_rom(&zip), first);
    }

    #[test]
    fn directory_entries_are_skipped() {
        // A directory named like a ROM must not be picked; the real file inside is.
        let rom = vec![0x66u8; 0x200];
        let zip = make_zip(&[("roms.gb/", b""), ("roms.gb/game.gb", &rom)]);
        assert_eq!(extract_rom(&zip), rom);
    }

    #[test]
    fn empty_only_file_falls_back_to_passthrough() {
        // The single entry is a zero-byte file: no extension match, largest is
        // empty, so extraction yields None and the raw archive passes through.
        let zip = make_zip(&[("empty.dat", b"")]);
        assert_eq!(extract_rom(&zip), zip);
    }
}
