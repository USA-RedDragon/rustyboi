//! Super Game Boy **system border** extraction from the SNES-side SGB cartridge
//! firmware (`sgb1.sfc` / `sgb2.sfc`).
//!
//! A real Super Game Boy shows its own built-in border until the running game
//! replaces it with CHR_TRN + PCT_TRN. That artwork lives in the SNES program
//! ROM, not in the GB boot ROM, so rustyboi can only show it when the user
//! supplies their own firmware dump. **No artwork is embedded here**: this
//! module is pure parsing code, and every byte of the border comes from the
//! file the user points us at (the same principle as
//! `Mmio::seed_rocket_boot_logo`).
//!
//! # SGB1 compression ("SGB1-LZ")
//!
//! SGB1 stores its border tileset and tilemap compressed; the format was
//! recovered from the firmware's own decompressor at **`$01:D6BB`**, which the
//! loader stub at `$01:8128` calls with a far source pointer in direct page
//! `$C0`/`$C2`.
//!
//! ```text
//! header (8 bytes):
//!   +0  u16le  control word     -- the escape token, chosen per blob
//!   +2  u16le  N                -- source cursor limit: tokens are read from
//!                                  src index 8 while (src < N + 8)
//!   +4  u16le  decompressed size -- NOT read by the firmware; we use it as an
//!                                  integrity check
//!   +6  u16le  first output word -- always emitted literally
//!
//! then out_idx = 2, src_idx = 8; while src_idx < N + 8:
//!   w = u16le at src[src_idx]
//!   w != control -> emit w literally,                    src_idx += 2
//!   w == control -> 5-byte token: CTRL(2) OFF(2) COUNT(1)
//!                   COUNT == 0 -> emit the control word itself (escape)
//!                   else copy COUNT *words* from absolute byte offset OFF
//!                        in the OUTPUT produced so far,  src_idx += 5
//! ```
//!
//! Copies are forward and word-at-a-time, so a back-reference may legally
//! overlap the region currently being written — that overlap *is* the run
//! expansion (offset 0 / count 15 right after one literal replicates it 15
//! times), so it must not be turned into a bulk copy.
//!
//! Note the 5-byte token makes the source stream fall out of 16-bit alignment;
//! that is intentional in the original and reproduced here.
//!
//! SGB2 patches bank `$01` to jump into extra code at file `0x40000+` which
//! loads its *own*, **uncompressed** border, so only SGB1 needs the decoder.
//!
//! # Palettes
//!
//! Both firmwares DMA 512 bytes straight into SNES CGRAM (all 256 colours).
//! The border tilemaps do *not* follow the PCT_TRN convention of only ever
//! selecting BG palettes 4-7: SGB1's map uses palette fields **0 and 4** and
//! SGB2's uses **0, 4 and 5**. We therefore hand out the full 8 BG palettes
//! (CGRAM colours 0..127) and let the compositor use the tilemap's real 3-bit
//! palette field. See [`SgbBorder::pals`].

/// Length of the canonical SGB1 program-ROM dump (256 KiB).
pub const SGB1_FIRMWARE_LEN: usize = 0x0004_0000;
/// Length of the canonical SGB2 program-ROM dump (512 KiB).
pub const SGB2_FIRMWARE_LEN: usize = 0x0008_0000;
/// CRC32 of the canonical SGB1 program-ROM dump.
pub(crate) const SGB1_FIRMWARE_CRC32: u32 = 0x8A4A_174F;
/// CRC32 of the canonical SGB2 program-ROM dump.
pub(crate) const SGB2_FIRMWARE_CRC32: u32 = 0xCB17_6E45;

/// Border tile store handed to `Sgb`: 256 SNES 4bpp tiles x 32 bytes. Neither
/// firmware fills all of it (SGB1 uses 109 tiles, SGB2 128), so the tail is
/// zero — colour index 0 is transparent, so unused tiles simply never draw.
pub(crate) const BORDER_TILES_LEN: usize = 0x2000;
/// Border tilemap: 32x32 LE16 entries (the compositor draws the top 28 rows).
pub(crate) const BORDER_MAP_LEN: usize = 0x800;
/// Colours handed out: SNES BG palettes 0-7, 16 colours each.
pub(crate) const BORDER_PAL_COLORS: usize = 128;

/// Which firmware image a dump is, decided by length + CRC32.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SgbFirmware {
    /// Original Super Game Boy (SNES/SFC cartridge).
    Sgb1,
    /// Super Game Boy 2 (Japan-only, own crystal, own border artwork).
    Sgb2,
}

/// How one asset is stored in a firmware image. Offsets are into the raw file.
#[derive(Clone, Copy)]
enum Source {
    /// SGB1-LZ blob starting here; its own header says how long the output is.
    Lz(usize),
    /// Uncompressed run of `len` bytes starting here — the length matters, it
    /// is exactly what the firmware's own uploader is asked to transfer.
    Raw { off: usize, len: usize },
}

#[cfg(test)]
impl Source {
    /// File offset this asset starts at. Only the tests need it — the decoder
    /// matches on the variant and uses the offset in place.
    const fn offset(self) -> usize {
        match self {
            Self::Lz(off) | Self::Raw { off, .. } => off,
        }
    }
}

/// Where one firmware keeps its border assets.
struct Layout {
    /// The 4bpp tileset uploaded to VRAM word 0x0000.
    tiles: Source,
    /// The 32x32 tilemap uploaded to VRAM word 0x3C00.
    map: Source,
    /// File offset of the 512-byte CGRAM image.
    cgram: usize,
}

// SGB1: the loader at $01:8128 points the $01:D6BB decompressor at $03:D868
// (tileset -> VRAM word 0x0000) and $03:E261 (tilemap -> VRAM word 0x3C00);
// DMA channel 6 at $01:8394 pushes 512 bytes from $04:EFF1 into CGRAM.
const SGB1_LAYOUT: Layout = Layout {
    tiles: Source::Lz(0x0001_D868),
    map: Source::Lz(0x0001_E261),
    cgram: 0x0002_6FF1,
};

// SGB2: the uploader at $88:CDB8 is fed Y=$1000 X=$0000 from $0B:C2C0 (tileset,
// 128 tiles) and Y=$0800 X=$3C00 from $0A:9800 (tilemap), both uncompressed;
// DMA channel 6 at $88:8228 pushes 512 bytes from $0B:8000 into CGRAM. The Y
// lengths are load-bearing: the bytes after the tileset are the SGB2 menu's
// graphics, not more border.
const SGB2_LAYOUT: Layout = Layout {
    tiles: Source::Raw { off: 0x0005_C2C0, len: 0x1000 },
    map: Source::Raw { off: 0x0005_1800, len: 0x0800 },
    cgram: 0x0005_8000,
};

/// The decoded system border, in exactly the shape `Sgb` stores it.
#[derive(Clone)]
pub(crate) struct SgbBorder {
    /// 4bpp tile data, exactly [`BORDER_TILES_LEN`] bytes (zero-padded).
    pub tiles: Vec<u8>,
    /// Tilemap bytes, exactly [`BORDER_MAP_LEN`].
    pub map: Vec<u8>,
    /// SNES BG palettes 0-7 flattened, exactly [`BORDER_PAL_COLORS`] RGB555
    /// words. Longer than the 64 a PCT_TRN produces *on purpose*: the length
    /// is what tells the compositor the tilemap's palette field is the full
    /// 3 bits rather than the PCT_TRN 4-7 window.
    pub(crate) pals: Vec<u16>,
}

/// Plain CRC32 (same polynomial/convention as the boot-ROM check in `mmio`).
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Identify a firmware dump, or explain why it was rejected.
///
/// Deliberately strict: the border offsets below are hard-coded for these two
/// exact images, so anything else would be read as garbage. This is a
/// user-supplied file, so it gets a real error rather than a guess.
pub fn identify(rom: &[u8]) -> Result<SgbFirmware, String> {
    let (want, kind) = match rom.len() {
        SGB1_FIRMWARE_LEN => (SGB1_FIRMWARE_CRC32, SgbFirmware::Sgb1),
        SGB2_FIRMWARE_LEN => (SGB2_FIRMWARE_CRC32, SgbFirmware::Sgb2),
        other => {
            return Err(format!(
                "SGB firmware has unexpected length {other} \
                 (want {SGB1_FIRMWARE_LEN} SGB1 or {SGB2_FIRMWARE_LEN} SGB2)"
            ));
        }
    };
    let got = crc32(rom);
    if got == want {
        Ok(kind)
    } else {
        Err(format!(
            "SGB firmware CRC mismatch: got 0x{got:08X}, expected 0x{want:08X}"
        ))
    }
}

/// Read a little-endian u16 at `off`, or `None` past the end.
fn le16(data: &[u8], off: usize) -> Option<u16> {
    let hi = *data.get(off + 1)?;
    let lo = *data.get(off)?;
    Some(u16::from(lo) | (u16::from(hi) << 8))
}

/// Decode one SGB1-LZ blob starting at `src[0]` (see the module docs).
///
/// Fully bounds-checked: a truncated or corrupt blob is an error, never a
/// panic and never an unbounded allocation (the header's declared length caps
/// the output).
pub fn decompress(src: &[u8]) -> Result<Vec<u8>, String> {
    let ctrl = le16(src, 0).ok_or("SGB1-LZ blob is shorter than its 8-byte header")?;
    let nsrc = usize::from(le16(src, 2).ok_or("SGB1-LZ blob is truncated in its header")?);
    let declared = usize::from(le16(src, 4).ok_or("SGB1-LZ blob is truncated in its header")?);
    let first = src
        .get(6..8)
        .ok_or("SGB1-LZ blob is truncated before its first literal word")?;

    let mut out = Vec::with_capacity(declared.max(2));
    out.extend_from_slice(first);

    let limit = nsrc + 8;
    let mut i = 8usize;
    while i < limit {
        let w = le16(src, i).ok_or_else(|| format!("SGB1-LZ source truncated at {i:#x}"))?;
        if w != ctrl {
            out.extend_from_slice(&src[i..i + 2]);
            i += 2;
        } else {
            let token = src
                .get(i..i + 5)
                .ok_or_else(|| format!("SGB1-LZ token truncated at {i:#x}"))?;
            let count = usize::from(token[4]);
            if count == 0 {
                // Escape: the control word appears in the payload for real.
                out.extend_from_slice(&token[..2]);
            } else {
                let mut o = usize::from(u16::from_le_bytes([token[2], token[3]]));
                for _ in 0..count {
                    // Overlapping back-references are the run-expansion
                    // mechanism, so this reads `out` as it grows.
                    let (a, b) = match (out.get(o).copied(), out.get(o + 1).copied()) {
                        (Some(a), Some(b)) => (a, b),
                        _ => {
                            return Err(format!(
                                "SGB1-LZ back-reference to {o:#x} past end of output at src {i:#x}"
                            ));
                        }
                    };
                    out.push(a);
                    out.push(b);
                    o += 2;
                }
            }
            i += 5;
        }
        if out.len() > declared {
            return Err(format!(
                "SGB1-LZ output overran its declared size {declared} at src {i:#x}"
            ));
        }
    }

    if out.len() != declared {
        return Err(format!(
            "SGB1-LZ output length {} != declared {declared}",
            out.len()
        ));
    }
    Ok(out)
}

/// Fetch one asset, decompressing it when the layout says it is packed.
fn asset(rom: &[u8], src: Source, what: &str) -> Result<Vec<u8>, String> {
    match src {
        Source::Lz(off) => {
            let blob = rom
                .get(off..)
                .ok_or_else(|| format!("SGB firmware too short for the {what} at {off:#x}"))?;
            decompress(blob).map_err(|e| format!("{what}: {e}"))
        }
        Source::Raw { off, len } => rom
            .get(off..off.saturating_add(len))
            .map(<[u8]>::to_vec)
            .ok_or_else(|| format!("SGB firmware too short for the {what} at {off:#x}")),
    }
}

/// Extract the power-on system border from a firmware dump.
///
/// The tileset is zero-padded up to [`BORDER_TILES_LEN`] so it drops straight
/// into `Sgb`'s CHR_TRN-shaped store; the padding tiles are all colour 0, i.e.
/// fully transparent, and no tilemap entry references them anyway.
pub(crate) fn extract_border(rom: &[u8]) -> Result<SgbBorder, String> {
    let layout = match identify(rom)? {
        SgbFirmware::Sgb1 => &SGB1_LAYOUT,
        SgbFirmware::Sgb2 => &SGB2_LAYOUT,
    };

    let mut tiles = asset(rom, layout.tiles, "border tileset")?;
    if tiles.len() > BORDER_TILES_LEN {
        return Err(format!(
            "SGB border tileset is {} bytes, more than the {BORDER_TILES_LEN}-byte store",
            tiles.len()
        ));
    }
    tiles.resize(BORDER_TILES_LEN, 0);

    let map = asset(rom, layout.map, "border tilemap")?;
    if map.len() != BORDER_MAP_LEN {
        return Err(format!(
            "SGB border tilemap is {} bytes, want {BORDER_MAP_LEN}",
            map.len()
        ));
    }

    let cgram = rom
        .get(layout.cgram..layout.cgram + BORDER_PAL_COLORS * 2)
        .ok_or("SGB firmware too short for its CGRAM image")?;
    // Mask bit 15 exactly like the PCT_TRN path: SNES CGRAM is 15-bit.
    let pals = cgram
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]) & 0x7FFF)
        .collect();

    Ok(SgbBorder { tiles, map, pals })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic SGB1-LZ blob so the decoder is exercised without any
    /// firmware present.
    fn blob(ctrl: u16, first: u16, body: &[u8], declared: u16) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&ctrl.to_le_bytes());
        v.extend_from_slice(&(body.len() as u16).to_le_bytes());
        v.extend_from_slice(&declared.to_le_bytes());
        v.extend_from_slice(&first.to_le_bytes());
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn literals_pass_through() {
        let b = blob(0x0002, 0xAAAA, &[0x11, 0x22, 0x33, 0x44], 6);
        assert_eq!(decompress(&b).unwrap(), vec![0xAA, 0xAA, 0x11, 0x22, 0x33, 0x44]);
    }

    #[test]
    fn zero_count_token_escapes_the_control_word() {
        // A single token with COUNT = 0 emits the control word as a literal.
        let b = blob(0x0002, 0xAAAA, &[0x02, 0x00, 0x00, 0x00, 0x00], 4);
        assert_eq!(decompress(&b).unwrap(), vec![0xAA, 0xAA, 0x02, 0x00]);
    }

    #[test]
    fn back_reference_may_overlap_the_write_cursor() {
        // Offset 0 / count 3 right after the first literal replicates it: this
        // is the run expansion, and would break under a non-overlapping copy.
        let b = blob(0x0002, 0xBEEF, &[0x02, 0x00, 0x00, 0x00, 0x03], 8);
        assert_eq!(decompress(&b).unwrap(), [0xEF, 0xBE].repeat(4));
    }

    #[test]
    fn declared_length_mismatch_is_an_error() {
        let b = blob(0x0002, 0xAAAA, &[0x11, 0x22], 99);
        assert!(decompress(&b).is_err());
    }

    #[test]
    fn malformed_blobs_error_without_panicking() {
        assert!(decompress(&[]).is_err());
        assert!(decompress(&[0, 0, 0, 0, 0, 0, 0]).is_err());
        // N runs past the end of the buffer.
        assert!(decompress(&blob(0x0002, 0, &[0x11, 0x22], 4)[..9]).is_err());
        // Back-reference beyond anything produced so far.
        assert!(decompress(&blob(0x0002, 0xAAAA, &[0x02, 0x00, 0xFF, 0xFF, 0x01], 4)).is_err());
        // Token header truncated mid-token.
        assert!(decompress(&blob(0x0002, 0xAAAA, &[0x02, 0x00, 0x00], 4)).is_err());
    }

    #[test]
    fn identify_rejects_unknown_images() {
        assert!(identify(&[]).is_err());
        assert!(identify(&vec![0u8; SGB1_FIRMWARE_LEN]).is_err());
        assert!(identify(&vec![0u8; SGB2_FIRMWARE_LEN]).is_err());
        assert!(identify(&vec![0u8; 1024]).is_err());
        assert!(extract_border(&vec![0u8; 1024]).is_err());
    }

    /// The two canonical dumps identify as themselves — the gate every frontend
    /// (desktop probe, browser file picker) runs before installing a picked
    /// file. Skips silently when the user has no dumps.
    #[test]
    fn identify_accepts_the_two_real_dumps() {
        let dumps = super::firmware_test::dumps();
        if dumps.is_empty() {
            return;
        }
        assert_eq!(identify(&dumps[0]).unwrap(), SgbFirmware::Sgb1);
        assert_eq!(identify(&dumps[1]).unwrap(), SgbFirmware::Sgb2);
        // A dump padded/truncated by even one byte is not that image.
        let mut long = dumps[0].clone();
        long.push(0);
        assert!(identify(&long).is_err());
        assert!(identify(&dumps[0][..dumps[0].len() - 1]).is_err());
    }

    /// Truncating a real firmware at any point must be rejected cleanly (no
    /// panic, no out-of-bounds), never silently produce a border.
    #[test]
    fn truncated_firmware_is_rejected_without_panicking() {
        for rom in super::firmware_test::dumps() {
            for cut in [0usize, 1, 8, 0x1D868, 0x1E000, 0x26FF0, rom.len() - 1] {
                let short = &rom[..cut.min(rom.len())];
                assert!(
                    extract_border(short).is_err(),
                    "truncation to {cut} was not rejected"
                );
            }
            // Same length, one flipped byte: the CRC gate must catch it.
            let mut bent = rom.clone();
            bent[0x1D868] ^= 0xFF;
            assert!(extract_border(&bent).is_err(), "bit-flip was not rejected");
        }
    }

    /// Decode the real firmware (when the user has it) and pin the shape of
    /// what comes out. Skips silently when the dumps are absent, mirroring
    /// `cgb_compat_palette::tables_match_cgb_boot_bin`.
    #[test]
    fn border_extracts_from_real_firmware() {
        let dumps = super::firmware_test::dumps();
        if dumps.is_empty() {
            return;
        }
        // (expected highest tile id referenced by the 32x32 map, palette fields)
        let expected: [(u16, &[u16]); 2] = [(0x6C, &[0, 4]), (0x40, &[0, 4, 5])];
        for (rom, (max_tile, pal_fields)) in dumps.iter().zip(expected) {
            let b = extract_border(rom).expect("border extracts");
            assert_eq!(b.tiles.len(), BORDER_TILES_LEN);
            assert_eq!(b.map.len(), BORDER_MAP_LEN);
            assert_eq!(b.pals.len(), BORDER_PAL_COLORS);
            assert!(b.pals.iter().all(|c| *c & 0x8000 == 0), "CGRAM is 15-bit");

            let entries: Vec<u16> = b
                .map
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            let hi = entries.iter().map(|e| e & 0x3FF).max().unwrap();
            assert_eq!(hi, max_tile, "highest referenced tile id");
            let mut used: Vec<u16> = entries.iter().map(|e| (e >> 10) & 7).collect();
            used.sort_unstable();
            used.dedup();
            assert_eq!(used, pal_fields, "palette fields used by the tilemap");
            // Every referenced tile must have real pixel data behind it.
            assert!(usize::from(hi) * 32 + 32 <= b.tiles.len());
        }
    }

    /// The strong self-check called out in the format notes: each compressed
    /// blob declares its own output size at header +4.
    #[test]
    fn compressed_blobs_match_their_declared_length() {
        let dumps = super::firmware_test::dumps();
        if dumps.is_empty() {
            return;
        }
        let rom = &dumps[0]; // only SGB1 compresses its border
        for (off, what) in [
            (SGB1_LAYOUT.tiles.offset(), "tileset"),
            (SGB1_LAYOUT.map.offset(), "tilemap"),
        ] {
            let declared = usize::from(le16(rom, off + 4).unwrap());
            let out = decompress(&rom[off..]).unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(out.len(), declared, "{what} length == header +4");
        }
        // And the sizes the border model depends on.
        assert_eq!(decompress(&rom[SGB1_LAYOUT.tiles.offset()..]).unwrap().len(), 3488);
        assert_eq!(decompress(&rom[SGB1_LAYOUT.map.offset()..]).unwrap().len(), 2048);
    }
}

/// Test-only firmware loader. The dumps are the user's own property and are
/// git-ignored, so every firmware-backed test is skip-if-absent.
#[cfg(test)]
pub(crate) mod firmware_test {
    /// `[sgb1, sgb2]`, or empty if either dump is missing. Searched relative to
    /// the crate directory's parent (the workspace root's `bios/`).
    pub(crate) fn dumps() -> Vec<Vec<u8>> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        let mut out = Vec::new();
        for name in ["bios/sgb1.sfc", "bios/sgb2.sfc"] {
            match std::fs::read(root.join(name)) {
                Ok(d) => out.push(d),
                Err(_) => return Vec::new(),
            }
        }
        out
    }
}
