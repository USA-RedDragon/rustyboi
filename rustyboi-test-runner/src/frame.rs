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

pub fn write_ppm(path: &Path, frame: &[u32]) -> Result<(), String> {
    if frame.len() != FRAMEBUFFER_SIZE {
        return Err(format!(
            "expected {FRAMEBUFFER_SIZE} pixels, got {}",
            frame.len()
        ));
    }

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create artifact directory: {error}"))?;
        }
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
            first_mismatch.get_or_insert_with(|| FrameMismatch {
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

fn decode_png_rgba(data: &[u8]) -> Result<Vec<u32>, String> {
    if data.len() < PNG_SIGNATURE.len() || &data[..PNG_SIGNATURE.len()] != PNG_SIGNATURE {
        return Err("not a PNG file".to_string());
    }

    let mut width = None;
    let mut height = None;
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
                let bit_depth = chunk_data[8];
                let color_type = chunk_data[9];
                let compression = chunk_data[10];
                let filter = chunk_data[11];
                let interlace = chunk_data[12];

                if image_width != GB_WIDTH || image_height != GB_HEIGHT {
                    return Err(format!(
                        "expected {GB_WIDTH}x{GB_HEIGHT} PNG, got {image_width}x{image_height}"
                    ));
                }
                if bit_depth != 8 || color_type != 6 || compression != 0 || filter != 0 || interlace != 0 {
                    return Err("only non-interlaced 8-bit RGBA PNGs are supported".to_string());
                }

                width = Some(image_width);
                height = Some(image_height);
            }
            b"IDAT" => idat.extend_from_slice(chunk_data),
            b"IEND" => break,
            _ => {}
        }
    }

    let width = width.ok_or_else(|| "missing PNG IHDR".to_string())?;
    let height = height.ok_or_else(|| "missing PNG IHDR".to_string())?;
    let stride = width * 4;
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

    let rgba = unfilter_rgba_rows(&raw, width, height)?;
    Ok(rgba
        .chunks_exact(4)
        .map(|chunk| ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | chunk[2] as u32)
        .collect())
}

fn unfilter_rgba_rows(raw: &[u8], width: usize, height: usize) -> Result<Vec<u8>, String> {
    let bytes_per_pixel = 4;
    let stride = width * bytes_per_pixel;
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
}
