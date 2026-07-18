//! Frame -> RGB/PNG/base64/HTML helpers shared by the `movie` and `sweep`
//! bins (this directory is not auto-binned; each bin pulls it in via
//! `#[path = "shared/imaging.rs"]`).

use rustyboi_core_lib::gb::Frame;

/// Palette for mapping DMG shade indices (0-3) to RGB. `Color` frames already
/// carry RGB888 and ignore this.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MonoPalette {
    /// The DMG's green LCD tint — closest to an original Game Boy's screen.
    DmgGreen,
    /// Neutral grayscale = the SGB default system palette (0x7FFF/0x56B5/0x294A/
    /// 0x0000 expanded linearly). The Super Game Boy has no LCD: it maps the DMG
    /// shades through SNES palette RAM and drives them to a TV, so a game that
    /// sends no PAL command shows this gray ramp, NOT green. Also the closest
    /// match for the Pocket's (MGB) black-and-white LCD.
    Gray,
}

/// GB pixel buffer -> RGB888 using the DMG green palette (the historical default
/// for the DMG-oriented tools). Prefer [`frame_rgb_pal`] when the target
/// hardware matters (e.g. SGB output is not green).
pub fn frame_rgb(frame: &Frame) -> Vec<u8> {
    frame_rgb_pal(frame, MonoPalette::DmgGreen)
}

/// GB pixel buffer -> RGB888 with an explicit monochrome palette. `Color` frames
/// are passed through verbatim (the core already applied the CGB/SGB palette and
/// any LCD correction).
pub fn frame_rgb_pal(frame: &Frame, mono: MonoPalette) -> Vec<u8> {
    let lut: [[u8; 3]; 4] = match mono {
        MonoPalette::DmgGreen => [
            [0xE0, 0xF8, 0xD0],
            [0x88, 0xC0, 0x70],
            [0x34, 0x68, 0x56],
            [0x08, 0x18, 0x20],
        ],
        MonoPalette::Gray => [[255, 255, 255], [172, 172, 172], [82, 82, 82], [0, 0, 0]],
    };
    match frame {
        Frame::Monochrome(data) => data
            .iter()
            .flat_map(|&p| lut[(p as usize).min(3)])
            .collect(),
        Frame::Color(data) => data.to_vec(),
    }
}

/// RGB888 -> PNG (stored-deflate zlib, color type 2). No external deps.
pub fn encode_rgb_png(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        let mut crc = 0xFFFF_FFFFu32;
        for &b in kind.iter().chain(data) {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xEDB8_8320 & (0u32.wrapping_sub(crc & 1)));
            }
        }
        png.extend_from_slice(&(!crc).to_be_bytes());
    }
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB
    chunk(&mut png, b"IHDR", &ihdr);
    let mut raw = Vec::with_capacity((width as usize * 3 + 1) * height as usize);
    for row in rgb.chunks(width as usize * 3) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut idat = vec![0x78, 0x01];
    for (i, block) in raw.chunks(0xFFFF).enumerate() {
        idat.push(((i + 1) * 0xFFFF >= raw.len()) as u8);
        idat.extend_from_slice(&(block.len() as u16).to_le_bytes());
        idat.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        idat.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    idat.extend_from_slice(&((b << 16) | a).to_be_bytes());
    chunk(&mut png, b"IDAT", &idat);
    chunk(&mut png, b"IEND", &[]);
    png
}

/// RGB888 -> lossless WebP (pure-Rust `image-webp`, VP8L). Used by the sweep's
/// gallery stills; these few-color, flat-region frames compress far below the
/// stored-deflate PNG. Falls back to the PNG encoder if encoding ever fails.
#[allow(dead_code)]
pub fn encode_rgb_webp(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    match image_webp::WebPEncoder::new(&mut out).encode(rgb, width, height, image_webp::ColorType::Rgb8) {
        Ok(()) => out,
        Err(_) => encode_rgb_png(width, height, rgb),
    }
}

// Shared by `movie` (embeds frames as data URIs); `sweep`'s gallery links
// screenshots by relative path, so its copy of this module leaves it unused.
#[allow(dead_code)]
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
