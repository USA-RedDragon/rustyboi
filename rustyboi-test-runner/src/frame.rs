use flate2::read::ZlibDecoder;
use rustyboi_core_lib::gb::Frame;
use rustyboi_core_lib::ppu::FRAMEBUFFER_SIZE;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

pub const GB_WIDTH: usize = 160;
pub const GB_HEIGHT: usize = 144;
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const RGB_MASK: u32 = 0xF8F8F8;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameMismatch {
    pub differing_pixels: usize,
    pub first_x: usize,
    pub first_y: usize,
    pub max_x: usize,
    pub max_y: usize,
    pub actual: u32,
    pub expected: u32,
}

impl FrameMismatch {
    pub fn describe(&self) -> String {
        format!(
            "{} differing pixels; first mismatch at ({}, {}): actual #{:06X}, expected #{:06X}; bounds x={}..{}, y={}..{}",
            self.differing_pixels,
            self.first_x,
            self.first_y,
            self.actual & 0xFFFFFF,
            self.expected & 0xFFFFFF,
            self.first_x,
            self.max_x,
            self.first_y,
            self.max_y
        )
    }
}

const GLYPHS: [[&str; 8]; 16] = [
    [
        "........",
        ".#######",
        ".#.....#",
        ".#.....#",
        ".#.....#",
        ".#.....#",
        ".#.....#",
        ".#######",
    ],
    [
        "........",
        "....#...",
        "....#...",
        "....#...",
        "....#...",
        "....#...",
        "....#...",
        "....#...",
    ],
    [
        "........",
        ".#######",
        ".......#",
        ".......#",
        ".#######",
        ".#......",
        ".#......",
        ".#######",
    ],
    [
        "........",
        ".#######",
        ".......#",
        ".......#",
        "..######",
        ".......#",
        ".......#",
        ".#######",
    ],
    [
        "........",
        ".#.....#",
        ".#.....#",
        ".#.....#",
        ".#######",
        ".......#",
        ".......#",
        ".......#",
    ],
    [
        "........",
        ".#######",
        ".#......",
        ".#......",
        ".######.",
        ".......#",
        ".......#",
        ".######.",
    ],
    [
        "........",
        ".#######",
        ".#......",
        ".#......",
        ".#######",
        ".#.....#",
        ".#.....#",
        ".#######",
    ],
    [
        "........",
        ".#######",
        ".......#",
        "......#.",
        ".....#..",
        "....#...",
        "...#....",
        "...#....",
    ],
    [
        "........",
        "..#####.",
        ".#.....#",
        ".#.....#",
        "..#####.",
        ".#.....#",
        ".#.....#",
        "..#####.",
    ],
    [
        "........",
        ".#######",
        ".#.....#",
        ".#.....#",
        ".#######",
        ".......#",
        ".......#",
        ".#######",
    ],
    [
        "........",
        "....#...",
        "..#...#.",
        ".#.....#",
        ".#######",
        ".#.....#",
        ".#.....#",
        ".#.....#",
    ],
    [
        "........",
        ".######.",
        ".#.....#",
        ".#.....#",
        ".######.",
        ".#.....#",
        ".#.....#",
        ".######.",
    ],
    [
        "........",
        "..#####.",
        ".#.....#",
        ".#......",
        ".#......",
        ".#......",
        ".#.....#",
        "..#####.",
    ],
    [
        "........",
        ".######.",
        ".#.....#",
        ".#.....#",
        ".#.....#",
        ".#.....#",
        ".#.....#",
        ".######.",
    ],
    [
        "........",
        ".#######",
        ".#......",
        ".#......",
        ".#######",
        ".#......",
        ".#......",
        ".#######",
    ],
    [
        "........",
        ".#######",
        ".#......",
        ".#......",
        ".#######",
        ".#......",
        ".#......",
        ".#......",
    ],
];

pub fn normalize_frame(frame: Frame) -> Vec<u32> {
    match frame {
        Frame::Monochrome(data) => data
            .iter()
            .map(|pixel| match pixel {
                0 => 0xFFFFFF,
                1 => 0xAAAAAA,
                2 => 0x555555,
                _ => 0x000000,
            })
            .collect(),
        Frame::Color(data) => data
            .chunks_exact(3)
            .map(|chunk| {
                ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | chunk[2] as u32
            })
            .collect(),
    }
}

#[cfg(test)]
pub fn matches_hex_output(frame: &[u32], expected: &str) -> bool {
    hex_output_mismatch(frame, expected).is_none()
}

pub fn hex_output_mismatch(frame: &[u32], expected: &str) -> Option<String> {
    let mut matched_glyphs = 0;

    for (tile_index, character) in expected.chars().enumerate() {
        let Some(glyph_index) = character.to_digit(16) else {
            break;
        };

        if (tile_index + 1) * 8 > GB_WIDTH {
            return Some(format!("hex output {expected} is too wide for the framebuffer"));
        }

        if let Some(mismatch) = tile_mismatch(frame, tile_index, glyph_index as usize) {
            return Some(format!(
                "hex output {expected} mismatch at tile {tile_index} ({character}); {}",
                mismatch.describe()
            ));
        }

        matched_glyphs += 1;
    }

    if matched_glyphs == 0 {
        Some(format!("hex output {expected} did not contain any hex digits"))
    } else {
        None
    }
}

#[cfg(test)]
pub fn frame_buffers_equal(left: &[u32], right: &[u32]) -> bool {
    frame_buffer_mismatch(left, right).is_none()
}

pub fn frame_buffer_mismatch(left: &[u32], right: &[u32]) -> Option<FrameMismatch> {
    if left.len() != FRAMEBUFFER_SIZE || right.len() != FRAMEBUFFER_SIZE {
        return Some(FrameMismatch {
            differing_pixels: FRAMEBUFFER_SIZE,
            first_x: 0,
            first_y: 0,
            max_x: GB_WIDTH - 1,
            max_y: GB_HEIGHT - 1,
            actual: left.first().copied().unwrap_or(0),
            expected: right.first().copied().unwrap_or(0),
        });
    }

    collect_mismatch(left.iter().copied().zip(right.iter().copied()))
}

/// Layout comparison that is invariant under a consistent 1:1 recoloring.
/// Some reference screenshots were captured on an emulator whose palette differs
/// from rustyboi's hardware-correct one — e.g. a DMG-compat cart rendered in
/// "DMG green" (scxly-cgb), or a CGB compat shade off by one bit (mbc3-tester,
/// where rustyboi's #7BFF31 is the boot-ROM-correct value vs the ref's #7BFF4A).
/// The pixel LAYOUT is what such tests measure, not the exact palette. This
/// passes if there is a consistent 1:1 mapping between actual and expected
/// colors across EVERY pixel — so a genuine layout error (a color that maps two
/// ways, i.e. a localized wrong pixel) still fails and cannot be laundered.
pub fn frame_buffer_mismatch_recolor(actual: &[u32], expected: &[u32]) -> Option<FrameMismatch> {
    if actual.len() != FRAMEBUFFER_SIZE || expected.len() != FRAMEBUFFER_SIZE {
        return frame_buffer_mismatch(actual, expected);
    }
    use std::collections::HashMap;
    let mut fwd: HashMap<u32, u32> = HashMap::new();
    let mut rev: HashMap<u32, u32> = HashMap::new();
    for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
        let a = a & RGB_MASK;
        let e = e & RGB_MASK;
        let bad = fwd.insert(a, e).is_some_and(|prev| prev != e)
            || rev.insert(e, a).is_some_and(|prev| prev != a);
        if bad {
            return Some(FrameMismatch {
                differing_pixels: 1,
                first_x: i % GB_WIDTH,
                first_y: i / GB_WIDTH,
                max_x: i % GB_WIDTH,
                max_y: i / GB_WIDTH,
                actual: a,
                expected: e,
            });
        }
    }
    None
}

pub fn audio_matches(samples: &[(f32, f32)], audible: bool) -> bool {
    let all_same = samples
        .first()
        .map(|first| samples.iter().all(|sample| sample == first))
        .unwrap_or(true);

    if audible { !all_same } else { all_same }
}

pub fn read_png_rgb(path: &Path) -> Result<Vec<u32>, String> {
    let data = fs::read(path).map_err(|error| format!("failed to read PNG: {error}"))?;
    decode_png_rgba(&data)
}

/// Convert one 0xRRGGBB pixel to an 8-bit grayscale value EXACTLY as PIL's
/// `Image.convert("L")` does. PIL uses fixed-point ITU-R 601-2 luma, not the
/// naive `round(R*299/1000 + G*587/1000 + B*114/1000)`: the two disagree by ±1
/// near the .5 boundary (verified against PIL 12.x). The shootout's
/// `util.compareImage` runs `convert("L")` on both images before diffing, so we
/// must match PIL bit-for-bit or a ±1 luma error could flip the diff-50 verdict.
fn pil_luminance(rgb: u32) -> u8 {
    let r = (rgb >> 16) & 0xFF ;
    let g = (rgb >> 8) & 0xFF ;
    let b = rgb & 0xFF ;
    // PIL C: L24(rgb) = (r*19595 + g*38470 + b*7471 + 0x8000) >> 16
    ((r * 19595 + g * 38470 + b * 7471 + 0x8000) >> 16) as u8
}

/// Shootout-exact screenshot grading. Replicates GBEmulatorShootout
/// `util.compareImage`: convert both framebuffers to PIL "L" grayscale, take the
/// per-pixel absolute difference, and PASS iff the maximum difference is <= 50
/// (the shootout fails on any histogram bucket whose value is strictly > 50).
/// Returns `None` on a pass, or `Some(describe)` on a mismatch (with the worst
/// pixel and its grayscale values, for diagnostics).
pub fn shootout_mismatch(actual: &[u32], expected: &[u32]) -> Option<FrameMismatch> {
    if actual.len() != FRAMEBUFFER_SIZE || expected.len() != FRAMEBUFFER_SIZE {
        return Some(FrameMismatch {
            differing_pixels: FRAMEBUFFER_SIZE,
            first_x: 0,
            first_y: 0,
            max_x: GB_WIDTH - 1,
            max_y: GB_HEIGHT - 1,
            actual: actual.first().copied().unwrap_or(0),
            expected: expected.first().copied().unwrap_or(0),
        });
    }

    let mut worst_diff = 0u8;
    let mut over_threshold = 0usize;
    let mut first: Option<FrameMismatch> = None;
    for (index, (&a, &e)) in actual.iter().zip(expected.iter()).enumerate() {
        let la = pil_luminance(a);
        let le = pil_luminance(e);
        let diff = la.abs_diff(le);
        if diff > worst_diff {
            worst_diff = diff;
        }
        if diff > 50 {
            over_threshold += 1;
            let m = first.get_or_insert(FrameMismatch {
                differing_pixels: 0,
                first_x: index % GB_WIDTH,
                first_y: index / GB_WIDTH,
                max_x: index % GB_WIDTH,
                max_y: index / GB_WIDTH,
                actual: a,
                expected: e,
            });
            m.max_x = m.max_x.max(index % GB_WIDTH);
            m.max_y = m.max_y.max(index / GB_WIDTH);
        }
    }

    first.map(|mut m| {
        m.differing_pixels = over_threshold;
        m
    })
}

pub fn write_ppm(path: &Path, frame: &[u32]) -> Result<(), String> {
    if frame.len() != FRAMEBUFFER_SIZE {
        return Err(format!(
            "expected {FRAMEBUFFER_SIZE} pixels, got {}",
            frame.len()
        ));
    }

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create artifact directory: {error}"))?;
        }

    let mut file = fs::File::create(path)
        .map_err(|error| format!("failed to create PPM {}: {error}", path.display()))?;
    write!(file, "P6\n{GB_WIDTH} {GB_HEIGHT}\n255\n")
        .map_err(|error| format!("failed to write PPM header: {error}"))?;

    for pixel in frame {
        let bytes = [
            ((pixel >> 16) & 0xFF) as u8,
            ((pixel >> 8) & 0xFF) as u8,
            (pixel & 0xFF) as u8,
        ];
        file.write_all(&bytes)
            .map_err(|error| format!("failed to write PPM pixels: {error}"))?;
    }

    Ok(())
}

fn tile_mismatch(frame: &[u32], tile_index: usize, glyph_index: usize) -> Option<FrameMismatch> {
    if frame.len() != FRAMEBUFFER_SIZE {
        return Some(FrameMismatch {
            differing_pixels: FRAMEBUFFER_SIZE,
            first_x: 0,
            first_y: 0,
            max_x: GB_WIDTH - 1,
            max_y: GB_HEIGHT - 1,
            actual: frame.first().copied().unwrap_or(0),
            expected: 0,
        });
    }

    let glyph = &GLYPHS[glyph_index];
    let mut first_mismatch = None;
    let mut differing_pixels = 0;

    for (y, row) in glyph.iter().enumerate() {
        for (x, expected) in row.as_bytes().iter().enumerate() {
            let frame_index = y * GB_WIDTH + tile_index * 8 + x;
            let expected = if *expected == b'#' { 0 } else { RGB_MASK };
            let actual = frame[frame_index];

            if ((actual ^ expected) & RGB_MASK) != 0 {
                differing_pixels += 1;
                first_mismatch.get_or_insert(FrameMismatch {
                    differing_pixels: 0,
                    first_x: tile_index * 8 + x,
                    first_y: y,
                    max_x: tile_index * 8 + x,
                    max_y: y,
                    actual,
                    expected,
                });
                if let Some(mismatch) = &mut first_mismatch {
                    mismatch.max_x = mismatch.max_x.max(tile_index * 8 + x);
                    mismatch.max_y = mismatch.max_y.max(y);
                }
            }
        }
    }

    first_mismatch.map(|mut mismatch| {
        mismatch.differing_pixels = differing_pixels;
        mismatch
    })
}

fn collect_mismatch(pixels: impl Iterator<Item = (u32, u32)>) -> Option<FrameMismatch> {
    let mut first_mismatch = None;
    let mut differing_pixels = 0;

    for (index, (actual, expected)) in pixels.enumerate() {
        if ((actual ^ expected) & RGB_MASK) != 0 {
            differing_pixels += 1;
            first_mismatch.get_or_insert(FrameMismatch {
                differing_pixels: 0,
                first_x: index % GB_WIDTH,
                first_y: index / GB_WIDTH,
                max_x: index % GB_WIDTH,
                max_y: index / GB_WIDTH,
                actual,
                expected,
            });
            if let Some(mismatch) = &mut first_mismatch {
                mismatch.max_x = mismatch.max_x.max(index % GB_WIDTH);
                mismatch.max_y = mismatch.max_y.max(index / GB_WIDTH);
            }
        }
    }

    first_mismatch.map(|mut mismatch| {
        mismatch.differing_pixels = differing_pixels;
        mismatch
    })
}

/// Decode a 160x144 PNG to one packed 0xRRGGBB per pixel. Supports the c-sp
/// reference formats (color types 0=grayscale, 2=RGB, 3=palette, 6=RGBA at bit
/// depths 1/2/4/8) in addition to the original 8-bit-RGBA Gambatte references.
/// Non-interlaced only; alpha is dropped (the comparison mask ignores it).
fn decode_png_rgba(data: &[u8]) -> Result<Vec<u32>, String> {
    if data.len() < PNG_SIGNATURE.len() || &data[..PNG_SIGNATURE.len()] != PNG_SIGNATURE {
        return Err("not a PNG file".to_string());
    }

    let mut width = None;
    let mut height = None;
    let mut bit_depth = 8u8;
    let mut color_type = 6u8;
    let mut palette: Vec<u32> = Vec::new();
    let mut idat = Vec::new();
    let mut offset = PNG_SIGNATURE.len();

    while offset + 8 <= data.len() {
        let length = read_be_u32(&data[offset..offset + 4]) as usize;
        offset += 4;
        let chunk_type = &data[offset..offset + 4];
        offset += 4;

        if offset + length + 4 > data.len() {
            return Err("truncated PNG chunk".to_string());
        }

        let chunk_data = &data[offset..offset + length];
        offset += length;
        offset += 4;

        match chunk_type {
            b"IHDR" => {
                if chunk_data.len() != 13 {
                    return Err("invalid PNG IHDR length".to_string());
                }

                let image_width = read_be_u32(&chunk_data[0..4]) as usize;
                let image_height = read_be_u32(&chunk_data[4..8]) as usize;
                bit_depth = chunk_data[8];
                color_type = chunk_data[9];
                let compression = chunk_data[10];
                let filter = chunk_data[11];
                let interlace = chunk_data[12];

                if image_width != GB_WIDTH || image_height != GB_HEIGHT {
                    return Err(format!(
                        "expected {GB_WIDTH}x{GB_HEIGHT} PNG, got {image_width}x{image_height}"
                    ));
                }
                if compression != 0 || filter != 0 || interlace != 0 {
                    return Err("only non-interlaced PNGs are supported".to_string());
                }
                if !matches!(color_type, 0 | 2 | 3 | 6) || !matches!(bit_depth, 1 | 2 | 4 | 8) {
                    return Err(format!(
                        "unsupported PNG color type {color_type} / bit depth {bit_depth}"
                    ));
                }

                width = Some(image_width);
                height = Some(image_height);
            }
            b"PLTE" => {
                palette = chunk_data
                    .chunks_exact(3)
                    .map(|c| ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32)
                    .collect();
            }
            b"IDAT" => idat.extend_from_slice(chunk_data),
            b"IEND" => break,
            _ => {}
        }
    }

    let width = width.ok_or_else(|| "missing PNG IHDR".to_string())?;
    let height = height.ok_or_else(|| "missing PNG IHDR".to_string())?;

    // Samples per pixel for the channel layout (alpha dropped at read time).
    let channels = match color_type {
        0 | 3 => 1,
        2 => 3,
        6 => 4,
        _ => unreachable!(),
    };
    // Bytes per pixel for filtering (rounded up to 1 for sub-byte depths).
    let bits_per_pixel = channels * bit_depth as usize;
    let bytes_per_pixel = bits_per_pixel.div_ceil(8).max(1);
    let stride = (width * bits_per_pixel).div_ceil(8);
    let expected_raw_len = (stride + 1) * height;

    let mut decoder = ZlibDecoder::new(&idat[..]);
    let mut raw = Vec::new();
    decoder
        .read_to_end(&mut raw)
        .map_err(|error| format!("failed to decompress PNG IDAT: {error}"))?;

    if raw.len() != expected_raw_len {
        return Err(format!(
            "expected {expected_raw_len} decompressed PNG bytes, got {}",
            raw.len()
        ));
    }

    let unfiltered = unfilter_rows(&raw, stride, bytes_per_pixel, height)?;
    samples_to_rgb(
        &unfiltered,
        width,
        height,
        stride,
        bit_depth,
        color_type,
        channels,
        &palette,
    )
}

/// Expand the unfiltered scanline bytes into one 0xRRGGBB per pixel.
#[allow(clippy::too_many_arguments)]
fn samples_to_rgb(
    bytes: &[u8],
    width: usize,
    height: usize,
    stride: usize,
    bit_depth: u8,
    color_type: u8,
    channels: usize,
    palette: &[u32],
) -> Result<Vec<u32>, String> {
    let mut out = Vec::with_capacity(width * height);
    let max = (1u32 << bit_depth) - 1 ;
    for y in 0..height {
        let row = &bytes[y * stride..y * stride + stride];
        for x in 0..width {
            let pixel = if bit_depth == 8 {
                let base = x * channels;
                match color_type {
                    0 => {
                        let g = row[base] as u32;
                        (g << 16) | (g << 8) | g
                    }
                    2 | 6 => {
                        ((row[base] as u32) << 16)
                            | ((row[base + 1] as u32) << 8)
                            | row[base + 2] as u32
                    }
                    3 => *palette
                        .get(row[base] as usize)
                        .ok_or("palette index out of range")?,
                    _ => unreachable!(),
                }
            } else {
                // Sub-byte sample (grayscale or palette index), MSB-first.
                let bit = x * bit_depth as usize;
                let byte = row[bit / 8];
                let shift = 8 - bit_depth as usize - (bit % 8);
                let sample = ((byte >> shift) as u32) & max;
                match color_type {
                    0 => {
                        // Scale the grayscale sample to 8-bit.
                        let g = (sample * 255 / max) & 0xFF;
                        (g << 16) | (g << 8) | g
                    }
                    3 => *palette
                        .get(sample as usize)
                        .ok_or("palette index out of range")?,
                    _ => return Err("sub-byte depth only valid for grayscale/palette".into()),
                }
            };
            out.push(pixel);
        }
    }
    Ok(out)
}

fn unfilter_rows(
    raw: &[u8],
    stride: usize,
    bytes_per_pixel: usize,
    height: usize,
) -> Result<Vec<u8>, String> {
    let mut output = vec![0; stride * height];

    for y in 0..height {
        let raw_offset = y * (stride + 1);
        let filter = raw[raw_offset];
        let scanline = &raw[raw_offset + 1..raw_offset + 1 + stride];
        let output_offset = y * stride;

        for x in 0..stride {
            let left = if x >= bytes_per_pixel {
                output[output_offset + x - bytes_per_pixel]
            } else {
                0
            };
            let up = if y > 0 {
                output[output_offset + x - stride]
            } else {
                0
            };
            let up_left = if y > 0 && x >= bytes_per_pixel {
                output[output_offset + x - stride - bytes_per_pixel]
            } else {
                0
            };

            output[output_offset + x] = match filter {
                0 => scanline[x],
                1 => scanline[x].wrapping_add(left),
                2 => scanline[x].wrapping_add(up),
                3 => scanline[x].wrapping_add(((left as u16 + up as u16) / 2) as u8),
                4 => scanline[x].wrapping_add(paeth_predictor(left, up, up_left)),
                _ => return Err(format!("unsupported PNG filter type {filter}")),
            };
        }
    }

    Ok(output)
}

fn paeth_predictor(left: u8, up: u8, up_left: u8) -> u8 {
    let left = left as i16;
    let up = up as i16;
    let up_left = up_left as i16;
    let estimate = left + up - up_left;
    let left_distance = (estimate - left).abs();
    let up_distance = (estimate - up).abs();
    let up_left_distance = (estimate - up_left).abs();

    if left_distance <= up_distance && left_distance <= up_left_distance {
        left as u8
    } else if up_distance <= up_left_distance {
        up as u8
    } else {
        up_left as u8
    }
}

fn read_be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_with_hex(expected: &str) -> Vec<u32> {
        let mut frame = vec![0xFFFFFF; FRAMEBUFFER_SIZE];

        for (tile_index, character) in expected.chars().enumerate() {
            let glyph_index = character.to_digit(16).unwrap() as usize;
            for (y, row) in GLYPHS[glyph_index].iter().enumerate() {
                for (x, pixel) in row.as_bytes().iter().enumerate() {
                    let frame_index = y * GB_WIDTH + tile_index * 8 + x;
                    frame[frame_index] = if *pixel == b'#' { 0 } else { 0xFFFFFF };
                }
            }
        }

        frame
    }

    #[test]
    fn matches_gambatte_hex_tiles() {
        let frame = frame_with_hex("0Af");

        assert!(matches_hex_output(&frame, "0Af"));
        assert!(!matches_hex_output(&frame, "0Ae"));
    }

    #[test]
    fn recolor_layout_accepts_consistent_recoloring_but_not_layout_errors() {
        // A layout of two shades (black/white) laid out identically in both, but
        // the "reference" uses a green palette (the scxly-cgb / mbc3 case).
        let mut actual = vec![0x000000u32; FRAMEBUFFER_SIZE];
        let mut green = vec![0x0F380Fu32; FRAMEBUFFER_SIZE]; // dark green
        for i in (0..FRAMEBUFFER_SIZE).step_by(2) {
            actual[i] = 0xFFFFFF; // white
            green[i] = 0x9BBC0F; // light green
        }
        // Same layout, different palette -> passes (consistent 1:1 recoloring).
        assert!(frame_buffer_mismatch_recolor(&actual, &green).is_none());
        // Exact comparison would (correctly) reject it.
        assert!(frame_buffer_mismatch(&actual, &green).is_some());
        // Flip ONE pixel's shade in the reference -> white must map to BOTH light
        // green and dark green -> inconsistent -> still rejected (not laundered).
        green[0] = 0x0F380F;
        assert!(frame_buffer_mismatch_recolor(&actual, &green).is_some());
    }

    #[test]
    fn frame_comparison_uses_gambatte_rgb_mask() {
        let white = vec![0xFFFFFF; FRAMEBUFFER_SIZE];
        let masked_white = vec![RGB_MASK; FRAMEBUFFER_SIZE];
        let black = vec![0; FRAMEBUFFER_SIZE];

        assert!(frame_buffers_equal(&white, &masked_white));
        assert!(!frame_buffers_equal(&white, &black));
    }

    #[test]
    fn audio_predicates_follow_gambatte_silence_rule() {
        assert!(audio_matches(&[(0.0, 0.0), (0.0, 0.0)], false));
        assert!(!audio_matches(&[(0.0, 0.0), (0.0, 0.0)], true));
        assert!(audio_matches(&[(0.0, 0.0), (0.1, 0.0)], true));
    }

    // Build a minimal 160x144 PNG (the decoder ignores chunk CRCs, so we use 0).
    fn build_png(bit_depth: u8, color_type: u8, plte: &[u8], raw_rows: &[u8]) -> Vec<u8> {
        use flate2::Compression;
        use flate2::write::ZlibEncoder;
        use std::io::Write as _;

        let mut png = PNG_SIGNATURE.to_vec();
        let chunk = |ty: &[u8; 4], data: &[u8], out: &mut Vec<u8>| {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(ty);
            out.extend_from_slice(data);
            out.extend_from_slice(&[0, 0, 0, 0]); // CRC (unchecked by decoder)
        };
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&(GB_WIDTH as u32).to_be_bytes());
        ihdr.extend_from_slice(&(GB_HEIGHT as u32).to_be_bytes());
        ihdr.extend_from_slice(&[bit_depth, color_type, 0, 0, 0]);
        chunk(b"IHDR", &ihdr, &mut png);
        if !plte.is_empty() {
            chunk(b"PLTE", plte, &mut png);
        }
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(raw_rows).unwrap();
        let idat = enc.finish().unwrap();
        chunk(b"IDAT", &idat, &mut png);
        chunk(b"IEND", &[], &mut png);
        png
    }

    #[test]
    fn decodes_palette_2bit_csp_png() {
        // 2-bit palette, 4 colors. Each row packs 4 pixels/byte. 160 px = 40 bytes.
        let plte = [
            0xFF, 0xFF, 0xFF, // index 0 white
            0xAA, 0xAA, 0xAA, // index 1
            0x55, 0x55, 0x55, // index 2
            0x00, 0x00, 0x00, // index 3 black
        ];
        let row_bytes = (GB_WIDTH * 2).div_ceil(8); // 40
        let mut raw = Vec::new();
        for _ in 0..GB_HEIGHT {
            raw.push(0u8); // filter: none
                           // first byte = indices 0,1,2,3 (0b00_01_10_11 = 0x1B), rest = 0.
            raw.push(0x1B);
            raw.extend(std::iter::repeat(0u8).take(row_bytes - 1));
        }
        let png = build_png(2, 3, &plte, &raw);
        let decoded = decode_png_rgba(&png).unwrap();
        assert_eq!(decoded.len(), FRAMEBUFFER_SIZE);
        assert_eq!(decoded[0], 0xFFFFFF); // index 0
        assert_eq!(decoded[1], 0xAAAAAA); // index 1
        assert_eq!(decoded[2], 0x555555); // index 2
        assert_eq!(decoded[3], 0x000000); // index 3
        assert_eq!(decoded[4], 0xFFFFFF); // index 0 again
    }

    #[test]
    fn pil_luminance_matches_pil_convert_l() {
        // Exact PIL "L" values (captured from PIL 12.x Image.convert("L")).
        assert_eq!(pil_luminance(0xFFFFFF), 255);
        assert_eq!(pil_luminance(0x000000), 0);
        assert_eq!(pil_luminance(0xAAAAAA), 170);
        assert_eq!(pil_luminance(0x555555), 85);
        assert_eq!(pil_luminance(0xFF0000), 76);
        assert_eq!(pil_luminance(0x00FF00), 150);
        assert_eq!(pil_luminance(0x0000FF), 29);
        assert_eq!(pil_luminance(0x8040C8), 99);
        assert_eq!(pil_luminance(0x010203), 2);
    }

    #[test]
    fn shootout_grading_passes_within_threshold_fails_beyond() {
        // Two solid fields whose grayscale diff is <= 50 pass; > 50 fails.
        let white = vec![0xFFFFFF; FRAMEBUFFER_SIZE]; // L=255
        let light = vec![0xF0F0F0u32; FRAMEBUFFER_SIZE]; // L=240, diff 15 <= 50
        let dark = vec![0x808080u32; FRAMEBUFFER_SIZE]; // L=128, diff 127 > 50
        assert!(shootout_mismatch(&white, &light).is_none());
        assert!(shootout_mismatch(&white, &dark).is_some());
        // The shootout mask is lenient: a 5-bit-mask-distinct color that is
        // grayscale-close still passes.
        let a = vec![0xF8F8F8u32; FRAMEBUFFER_SIZE]; // L=248
        assert!(shootout_mismatch(&white, &a).is_none()); // diff 7
    }

    #[test]
    fn decodes_grayscale_8bit_csp_png() {
        let row_bytes = GB_WIDTH; // 1 byte/pixel
        let mut raw = Vec::new();
        for _ in 0..GB_HEIGHT {
            raw.push(0u8); // filter: none
            raw.push(0x80); // gray 0x80
            raw.extend(std::iter::repeat(0u8).take(row_bytes - 1));
        }
        let png = build_png(8, 0, &[], &raw);
        let decoded = decode_png_rgba(&png).unwrap();
        assert_eq!(decoded[0], 0x808080);
        assert_eq!(decoded[1], 0x000000);
    }
}
