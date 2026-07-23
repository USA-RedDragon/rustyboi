use crate::memory::boxed_filled;
use crate::memory::mmio;
use crate::memory::Addressable;
use crate::ppu::fetcher;
use crate::ppu::stat_irq;
use serde::{Deserialize, Serialize};

pub const LCD_CONTROL: u16 = 0xFF40;
pub const LCD_STATUS: u16 = 0xFF41;
pub const LY: u16 = 0xFF44;
pub const SCY: u16 = 0xFF42;
pub const SCX: u16 = 0xFF43;
pub const LYC: u16 = 0xFF45;
pub const BGP: u16 = 0xFF47;
pub const OBP0: u16 = 0xFF48; // Object Palette 0 Data
pub const OBP1: u16 = 0xFF49; // Object Palette 1 Data
pub const WY: u16 = 0xFF4A;  // Window Y Position
pub const WX: u16 = 0xFF4B;  // Window X Position

pub const FRAMEBUFFER_SIZE: usize = 160 * 144;

/// Super Game Boy composited output dimensions (SNES 256x224 with the GB
/// screen centered at (48, 40)). See `Ppu::sgb_composited_frame`.
pub const SGB_FRAME_WIDTH: usize = 256;
pub const SGB_FRAME_HEIGHT: usize = 224;
pub const SGB_FRAME_SIZE: usize = SGB_FRAME_WIDTH * SGB_FRAME_HEIGHT;

/// The grayscale ramp `Sgb` powers on with (the SGB boot palette). Only used
/// as the composited centre's shades when no SGB system palette applies, i.e.
/// the user explicitly chose `Grayscale`.
pub(crate) const SGB_BOOT_SHADES: [u16; 4] = [0x7FFF, 0x56B5, 0x294A, 0x0000];

/// Convert an SGB/CGB RGB555 color word (bits: r=0-4, g=5-9, b=10-14) to RGB888
/// using the linear 5-bit->8-bit scaling the emulator uses for CGB `Linear`.
pub(crate) fn rgb555_to_rgb888(color: u16) -> (u8, u8, u8) {
    let r = color & 0x1F ;
    let g = (color >> 5) & 0x1F ;
    let b = (color >> 10) & 0x1F ;
    (((r * 255) / 31) as u8, ((g * 255) / 31) as u8, ((b * 255) / 31) as u8)
}

/// The GB screen's window within the 256x224 SGB frame: 160x144 at (48, 40).
pub const SGB_WINDOW_X: std::ops::Range<usize> = 48..208;
pub const SGB_WINDOW_Y: std::ops::Range<usize> = 40..184;

/// The SGB border artwork with the GB screen cut out of it, as two RGBA8
/// layers — what a caller that draws its own live screen composites around.
/// Both are screen-independent, so identical artwork produces identical bytes.
///
/// Stacking order, matching hardware (border pixels with a non-zero 4bpp index
/// draw OVER the GB picture): `ring` behind, the caller's screen, then
/// `overlay` in front.
pub struct SgbBorderLayers {
    /// 256x224: the backdrop and every border pixel OUTSIDE the screen window,
    /// with the whole window left at alpha 0.
    pub ring: Box<[u8; SGB_FRAME_SIZE * 4]>,
    /// 160x144 in window-local coordinates (the (48, 40) origin subtracted):
    /// the border pixels that intrude INTO the screen window, alpha 0
    /// elsewhere. `None` when the border does not intrude at all, which is the
    /// common case — then there is no overlay layer to draw.
    pub overlay: Option<Box<[u8; 160 * 144 * 4]>>,
}

/// Lossless serde codec for the fixed-size framebuffers. Savestates (rewind
/// ring, quicksaves) carry all four framebuffers; the rewind ring captures one
/// every frame on battery-powered mobile devices, so this must be a single
/// linear pass with no entropy/deflate coding. A GB frame holds very few
/// distinct colors (DMG: 4 shades; CGB: at most 64 palette entries live), so
/// this is a palette-index codec: it collects the distinct colors (a byte
/// buffer read as 3-byte RGB triples) in one pass, then emits a palette plus one
/// index per pixel — 1 byte/pixel when <=256 colors (INDEXED8, the real-frame
/// case) or 2 bytes/pixel when <=65536 (INDEXED16). Any trailing bytes that
/// don't fill a triple ride along raw. It then picks the smallest of a handful
/// of cheap linear encodings — Solid (one repeated color, the all-zero unused
/// pair), Indexed8/Indexed16 (palette + indices), byte-level Rle (which still
/// wins on the DMG shade buffers: 1-byte-per-pixel data with long horizontal
/// runs), or Raw — so every buffer is never larger than the old RLE encoding,
/// while the high-entropy CGB color frame that bloated RLE now falls to
/// INDEXED8. No entropy/deflate coding, no per-pixel allocation; runs only at
/// save/load, so the render hot path is untouched. (Kept named `fb_rle` so the
/// framebuffer field attributes are unchanged.)
mod fb_rle {
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::HashMap;

    // The codec picks the smallest arm per buffer. `Solid` is the blank/constant
    // fast path (an unused mode's all-zero pair). `Rle` is byte-level runs, kept
    // because it still beats palette-indexing on the DMG shade buffers. The
    // serialize side borrows byte runs; the deserialize side owns them. Both
    // shapes share one bincode layout (tag 0 = Solid, 1 = Indexed8,
    // 2 = Indexed16, 3 = Rle, 4 = Raw).
    #[derive(Serialize)]
    enum EncodedRef<'a> {
        Solid {
            color: [u8; 3],
            tail: &'a serde_bytes::Bytes,
        },
        Indexed8 {
            palette: Vec<[u8; 3]>,
            indices: Vec<u8>,
            tail: &'a serde_bytes::Bytes,
        },
        Indexed16 {
            palette: Vec<[u8; 3]>,
            indices: Vec<u16>,
            tail: &'a serde_bytes::Bytes,
        },
        Rle(Vec<(u8, u32)>),
        Raw(&'a serde_bytes::Bytes),
    }

    #[derive(Deserialize)]
    enum EncodedOwned {
        Solid {
            color: [u8; 3],
            tail: serde_bytes::ByteBuf,
        },
        Indexed8 {
            palette: Vec<[u8; 3]>,
            indices: Vec<u8>,
            tail: serde_bytes::ByteBuf,
        },
        Indexed16 {
            palette: Vec<[u8; 3]>,
            indices: Vec<u16>,
            tail: serde_bytes::ByteBuf,
        },
        Rle(Vec<(u8, u32)>),
        Raw(serde_bytes::ByteBuf),
    }

    // bincode framing constants: an enum tag is a u32 (4 bytes); a Vec/byte
    // length prefix is a u64 (8 bytes). Used only to compare candidate arm sizes.
    const TAG: usize = 4;
    const LEN: usize = 8;

    pub(super) fn serialize<S: Serializer, const N: usize>(
        buf: &[u8; N],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let pixels = N / 3;
        let tail_bytes = &buf[pixels * 3..];
        let tail = serde_bytes::Bytes::new(tail_bytes);

        // Pass A: byte-level run list, abandoned the moment it can't beat raw so
        // the run vector never grows past N/5 entries.
        let mut runs: Vec<(u8, u32)> = Vec::new();
        let mut rle_dead = false;
        for &b in buf.iter() {
            match runs.last_mut() {
                Some((v, c)) if *v == b => *c += 1,
                _ => {
                    if runs.len() * 5 >= N {
                        rle_dead = true;
                        break;
                    }
                    runs.push((b, 1));
                }
            }
        }

        // Pass B: intern each RGB triple into the palette, recording its index.
        // Palette growth is the only allocation, bounded by distinct-color count;
        // the per-pixel lookup allocates nothing.
        let mut lut: HashMap<[u8; 3], u32> = HashMap::new();
        let mut palette: Vec<[u8; 3]> = Vec::new();
        let mut indices: Vec<u32> = Vec::with_capacity(pixels);
        for p in 0..pixels {
            let color = [buf[p * 3], buf[p * 3 + 1], buf[p * 3 + 2]];
            let idx = *lut.entry(color).or_insert_with(|| {
                let i = palette.len() as u32;
                palette.push(color);
                i
            });
            indices.push(idx);
        }
        let ncol = palette.len();

        // Byte cost of every applicable arm; MAX marks an inapplicable one.
        let solid_cost = if ncol == 1 { TAG + 3 + LEN + tail_bytes.len() } else { usize::MAX };
        let idx8_cost = if ncol <= 256 {
            TAG + LEN + ncol * 3 + LEN + pixels + LEN + tail_bytes.len()
        } else {
            usize::MAX
        };
        let idx16_cost = if ncol <= 65536 {
            TAG + LEN + ncol * 3 + LEN + pixels * 2 + LEN + tail_bytes.len()
        } else {
            usize::MAX
        };
        let rle_cost = if rle_dead { usize::MAX } else { TAG + LEN + runs.len() * 5 };
        let raw_cost = TAG + LEN + N;
        let best = solid_cost.min(idx8_cost).min(idx16_cost).min(rle_cost).min(raw_cost);

        // Prefer the smallest; ties fall to the earlier arm here, which is fine —
        // correctness is identical, only the encoded size is being minimized.
        if best == solid_cost {
            EncodedRef::Solid { color: palette[0], tail }.serialize(s)
        } else if best == idx8_cost {
            EncodedRef::Indexed8 {
                palette,
                indices: indices.iter().map(|&i| i as u8).collect(),
                tail,
            }
            .serialize(s)
        } else if best == idx16_cost {
            EncodedRef::Indexed16 {
                palette,
                indices: indices.iter().map(|&i| i as u16).collect(),
                tail,
            }
            .serialize(s)
        } else if best == rle_cost {
            EncodedRef::Rle(runs).serialize(s)
        } else {
            EncodedRef::Raw(serde_bytes::Bytes::new(buf)).serialize(s)
        }
    }

    // Rebuild the flat buffer from palette + indices (+ tail), validating every
    // index and the final length so a corrupt state fails loudly rather than
    // silently truncating.
    fn expand<const N: usize, I: Copy + Into<u32>>(
        palette: &[[u8; 3]],
        indices: &[I],
        tail: &[u8],
    ) -> Result<Box<[u8; N]>, &'static str> {
        if indices.len() * 3 + tail.len() != N {
            return Err("framebuffer index length mismatch");
        }
        let mut buf = vec![0u8; N];
        for (p, &i) in indices.iter().enumerate() {
            let color = palette.get(i.into() as usize).ok_or("framebuffer index out of range")?;
            buf[p * 3..p * 3 + 3].copy_from_slice(color);
        }
        buf[indices.len() * 3..].copy_from_slice(tail);
        Ok(buf.into_boxed_slice().try_into().unwrap_or_else(|_| unreachable!()))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>, const N: usize>(
        d: D,
    ) -> Result<Box<[u8; N]>, D::Error> {
        match EncodedOwned::deserialize(d)? {
            EncodedOwned::Solid { color, tail } => {
                let pixels = N / 3;
                if tail.len() != N - pixels * 3 {
                    return Err(D::Error::custom("framebuffer solid tail length mismatch"));
                }
                let mut buf = vec![0u8; N];
                for px in buf[..pixels * 3].chunks_mut(3) {
                    px.copy_from_slice(&color);
                }
                buf[pixels * 3..].copy_from_slice(&tail);
                Ok(buf.into_boxed_slice().try_into().unwrap_or_else(|_| unreachable!()))
            }
            EncodedOwned::Raw(bytes) => {
                if bytes.len() != N {
                    return Err(D::Error::custom("framebuffer raw length mismatch"));
                }
                Ok(bytes
                    .into_vec()
                    .into_boxed_slice()
                    .try_into()
                    .unwrap_or_else(|_| unreachable!()))
            }
            EncodedOwned::Indexed8 { palette, indices, tail } => {
                expand::<N, u8>(&palette, &indices, &tail).map_err(D::Error::custom)
            }
            EncodedOwned::Indexed16 { palette, indices, tail } => {
                expand::<N, u16>(&palette, &indices, &tail).map_err(D::Error::custom)
            }
            EncodedOwned::Rle(runs) => {
                let mut buf = vec![0u8; N];
                let mut i = 0usize;
                for (v, c) in runs {
                    for _ in 0..c {
                        if i >= N {
                            return Err(D::Error::custom("framebuffer RLE overflow"));
                        }
                        buf[i] = v;
                        i += 1;
                    }
                }
                if i != N {
                    return Err(D::Error::custom("framebuffer RLE underflow"));
                }
                Ok(buf.into_boxed_slice().try_into().unwrap_or_else(|_| unreachable!()))
            }
        }
    }
}

#[cfg(test)]
mod fb_rle_tests {
    use serde::{Deserialize, Serialize};

    // Wraps a fixed-size framebuffer with the codec, mirroring how the PPU's
    // `Box<[u8; N]>` framebuffer fields opt into `fb_rle`. Round-trip through
    // bincode (the real savestate format) and assert byte-exact restore.
    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Fb<const N: usize>(#[serde(with = "super::fb_rle")] Box<[u8; N]>);

    fn roundtrip<const N: usize>(buf: [u8; N]) -> Vec<u8> {
        let bytes = bincode::serialize(&Fb(Box::new(buf))).unwrap();
        let back: Fb<N> = bincode::deserialize(&bytes).unwrap();
        assert_eq!(*back.0, buf, "framebuffer did not round-trip byte-exact");
        bytes
    }

    // A realistic RGB frame: 160x144 pixels drawn from a tiny CGB-like palette.
    // The codec must land on INDEXED8 and beat the raw 3-bytes/pixel buffer.
    #[test]
    fn dmg_like_four_color_frame_uses_indexed8_and_shrinks() {
        const N: usize = 160 * 144 * 3;
        let palette = [[224, 248, 208], [136, 192, 112], [52, 104, 86], [8, 24, 32]];
        let mut buf = [0u8; N];
        for (p, px) in buf.chunks_mut(3).enumerate() {
            px.copy_from_slice(&palette[(p * 7 + p / 160) % 4]);
        }
        let enc = roundtrip(buf);
        assert!(enc.len() < N, "indexed frame must be smaller than raw, got {} vs {N}", enc.len());
        // 1 byte/pixel + a 4-entry palette + a little framing: comfortably ~1/3.
        assert!(enc.len() < N / 2, "four-color frame should be well under half raw, got {}", enc.len());
    }

    #[test]
    fn empty_buffer_round_trips() {
        roundtrip([0u8; 0]);
    }

    #[test]
    fn single_color_frame_uses_solid_and_is_tiny() {
        // The blank/constant case (an unused mode's all-zero pair, or any solid
        // fill) -> one Solid color, a handful of bytes regardless of size.
        let enc = roundtrip([0x42u8; 160 * 144 * 3]);
        assert!(enc.len() < 64, "solid frame must cost a few bytes, got {}", enc.len());
    }

    #[test]
    fn blank_all_zero_frame_is_tiny() {
        // The dominant real case: the unused framebuffer pair is all-zero.
        let enc = roundtrip([0u8; 160 * 144 * 3]);
        assert!(enc.len() < 64, "blank frame must cost a few bytes, got {}", enc.len());
    }

    #[test]
    fn high_entropy_many_color_frame_round_trips() {
        // Many distinct triples but still <=256 colors -> INDEXED8.
        let mut buf = [0u8; 300 * 3];
        for (p, px) in buf.chunks_mut(3).enumerate() {
            let v = (p % 200) as u8;
            px.copy_from_slice(&[v, v.wrapping_add(1), v.wrapping_add(2)]);
        }
        roundtrip(buf);
    }

    #[test]
    fn length_not_multiple_of_three_round_trips() {
        // Two trailing bytes must ride along raw and restore byte-exact.
        let mut buf = [0u8; 3 * 5 + 2];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (i * 17 + 3) as u8;
        }
        roundtrip(buf);
    }

    #[test]
    fn over_256_distinct_colors_forces_indexed16() {
        // 400 distinct colors (> u8 palette limit) spread across 4000 pixels, so
        // the 2-byte indices still beat raw -> INDEXED16. Encode the tag directly
        // to prove which arm fired, then round-trip.
        let mut buf = [0u8; 4000 * 3];
        for (p, px) in buf.chunks_mut(3).enumerate() {
            let c = p % 400;
            px.copy_from_slice(&[(c >> 8) as u8, (c & 0xFF) as u8, 0]);
        }
        let enc = bincode::serialize(&Fb(Box::new(buf))).unwrap();
        assert_eq!(&enc[0..4], &[2, 0, 0, 0], "expected INDEXED16 tag (2)");
        assert!(enc.len() < buf.len(), "INDEXED16 must still beat raw here, got {}", enc.len());
        roundtrip(buf);
    }

    // Pins the on-wire shape independent of any fixture: an all-zero 4-pixel
    // buffer -> tag 0 (Solid) + the [0,0,0] color + an empty tail (len u64). If
    // the codec enum or bincode defaults ever drift, this fails with no
    // ROM/fixture involved.
    #[test]
    fn wire_shape_pinned_solid() {
        let enc = bincode::serialize(&Fb(Box::new([0u8; 12]))).unwrap();
        let mut expected = vec![0, 0, 0, 0]; // tag 0 = Solid
        expected.extend_from_slice(&[0, 0, 0]); // the [0,0,0] color
        expected.extend_from_slice(&0u64.to_le_bytes()); // tail len = 0
        assert_eq!(enc, expected);
    }

    // The Indexed8 wire shape: two colors A/B alternating over sixteen pixels.
    // Each pixel differs from its neighbour so byte-RLE needs one run per byte and
    // loses, and there are enough pixels that 1-byte indices beat the 3-byte raw
    // bytes -> tag 1 + palette (len u64 + two triples) + indices (len u64 +
    // sixteen bytes) + empty tail.
    #[test]
    fn wire_shape_pinned_indexed8() {
        let a = [10u8, 20, 30];
        let b = [40u8, 50, 60];
        let mut buf = [0u8; 48];
        for (p, px) in buf.chunks_mut(3).enumerate() {
            px.copy_from_slice(if p % 2 == 0 { &a } else { &b });
        }
        let enc = bincode::serialize(&Fb(Box::new(buf))).unwrap();
        let mut expected = vec![1, 0, 0, 0]; // tag 1 = Indexed8
        expected.extend_from_slice(&2u64.to_le_bytes()); // palette len = 2
        expected.extend_from_slice(&[10, 20, 30, 40, 50, 60]); // colors 0 and 1
        expected.extend_from_slice(&16u64.to_le_bytes()); // indices len = 16
        expected.extend_from_slice(&[0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1]);
        expected.extend_from_slice(&0u64.to_le_bytes()); // tail len = 0
        assert_eq!(enc, expected);
    }

    // The DMG shade buffers (1 byte/pixel, long horizontal runs) must still pick
    // the byte-level Rle arm and stay at or below their raw size.
    #[test]
    fn run_heavy_mono_buffer_prefers_rle() {
        let mut buf = [0u8; 300];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = (i / 30) as u8; // ten long runs of thirty bytes each
        }
        let enc = bincode::serialize(&Fb(Box::new(buf))).unwrap();
        assert_eq!(&enc[0..4], &[3, 0, 0, 0], "expected Rle tag (3)");
        assert!(enc.len() < buf.len(), "run-heavy buffer must beat raw, got {}", enc.len());
        roundtrip(buf);
    }
}

#[cfg(test)]
mod color_tests {
    use super::{rgb555_to_rgb888, ColorCorrection, Ppu};

    // Pack a 5-5-5 color into the (low, high) palette byte pair the PPU stores.
    fn bytes(r: u16, g: u16, b: u16) -> (u8, u8) {
        let w = r | (g << 5) | (b << 10);
        ((w & 0xFF) as u8, (w >> 8) as u8)
    }

    #[test]
    fn rgb555_expands_endpoints_and_channels() {
        assert_eq!(rgb555_to_rgb888(0x0000), (0, 0, 0));
        assert_eq!(rgb555_to_rgb888(0x7FFF), (255, 255, 255));
        assert_eq!(rgb555_to_rgb888(0x001F), (255, 0, 0)); // r = 31
        assert_eq!(rgb555_to_rgb888(0x03E0), (0, 255, 0)); // g = 31
        assert_eq!(rgb555_to_rgb888(0x7C00), (0, 0, 255)); // b = 31
        assert_eq!(rgb555_to_rgb888(0x000F), (123, 0, 0)); // r = 15 -> 15*255/31
    }

    #[test]
    fn cgb_linear_matches_the_naive_expansion() {
        let mut ppu = Ppu::new();
        ppu.set_cgb_color_conversion(ColorCorrection::Linear);
        let (lo, hi) = bytes(31, 0, 0);
        assert_eq!(ppu.cgb_color_to_rgb(lo, hi, false), (255, 0, 0));
        let (lo, hi) = bytes(31, 31, 31);
        assert_eq!(ppu.cgb_color_to_rgb(lo, hi, false), (255, 255, 255));
    }

    #[test]
    fn cgb_lcd_applies_the_correction_curve() {
        let mut ppu = Ppu::new();
        ppu.set_cgb_color_conversion(ColorCorrection::Lcd);
        // White under the LCD curve is (248,248,248), NOT the linear (255,255,255).
        let (lo, hi) = bytes(31, 31, 31);
        assert_eq!(ppu.cgb_color_to_rgb(lo, hi, false), (248, 248, 248));
        // Pure red picks up the curve's inter-channel bleed.
        let (lo, hi) = bytes(31, 0, 0);
        assert_eq!(ppu.cgb_color_to_rgb(lo, hi, false), (201, 0, 46));
        // Black stays black on both curves.
        let (lo, hi) = bytes(0, 0, 0);
        assert_eq!(ppu.cgb_color_to_rgb(lo, hi, false), (0, 0, 0));
    }

    #[test]
    fn agb_lcd_uses_the_gba_curve_only_on_agb() {
        let mut ppu = Ppu::new();
        ppu.set_cgb_color_conversion(ColorCorrection::Lcd);
        // The GBA screen is dim and warm: white is not (255,255,255) or the CGB
        // (248,248,248) — it's the ares/byuu curve's ~(246,238,242).
        let (lo, hi) = bytes(31, 31, 31);
        assert_eq!(ppu.cgb_color_to_rgb(lo, hi, true), (246, 238, 242));
        // Black is black; the curve only applies when is_agb is set.
        let (lo, hi) = bytes(0, 0, 0);
        assert_eq!(ppu.cgb_color_to_rgb(lo, hi, true), (0, 0, 0));
        assert_ne!(
            ppu.cgb_color_to_rgb(bytes(31, 0, 0).0, bytes(31, 0, 0).1, true),
            ppu.cgb_color_to_rgb(bytes(31, 0, 0).0, bytes(31, 0, 0).1, false),
        );
    }

    #[test]
    fn mono_shades_are_model_and_correction_aware() {
        use crate::gb::{mono_shades, Hardware};
        let gray = [[255, 255, 255], [170, 170, 170], [85, 85, 85], [0, 0, 0]];
        // Default palette per model: DMG→Green, MGB→Pocket, SGB→Grayscale.
        // Linear = the raw base palette; LCD = its screen-rendered variant.
        assert_eq!(mono_shades(Hardware::DMG, ColorCorrection::Linear)[0], [0x9B, 0xBC, 0x0F]);
        assert_eq!(mono_shades(Hardware::DMG, ColorCorrection::Lcd)[0], [224, 248, 208]);
        assert_eq!(mono_shades(Hardware::MGB, ColorCorrection::Linear)[0], [0xC4, 0xCF, 0xA1]);
        assert_eq!(mono_shades(Hardware::MGB, ColorCorrection::Lcd)[0], [194, 206, 147]);
        // SGB has no LCD → neutral grey either way.
        assert_eq!(mono_shades(Hardware::SGB, ColorCorrection::Linear), gray);
        assert_eq!(mono_shades(Hardware::SGB, ColorCorrection::Lcd), gray);
    }
}

// OAM constants
pub(crate) const OAM_SPRITE_COUNT: usize = 40; // 40 sprites total in OAM
pub(crate) const OAM_BYTES_PER_SPRITE: usize = 4; // 4 bytes per sprite
pub(crate) const MAX_SPRITES_PER_LINE: usize = 10; // Maximum 10 sprites per scanline

pub(in crate::ppu) const DMG_PIXEL_TRANSFER_ARM_DOT: u128 = 80;
pub(in crate::ppu) const CGB_PIXEL_TRANSFER_ARM_DOT: u128 = 82;
pub(in crate::ppu) const DMG_PIXEL_TRANSFER_WARMUP: u8 = 4;
pub(in crate::ppu) const CGB_PIXEL_TRANSFER_WARMUP: u8 = 2;
// Serde default for `frames_since_enable`: a savestate captured mid-run has an
// already-resynced panel, so restore to the "displays normally" value (>= 2).
fn frames_since_enable_default() -> u8 { 2 }
// Mode-3 dot penalty for a window starting on this line (the hardware window draw-start penalty).
pub(in crate::ppu) const WIN_M3_PENALTY: i32 = 6;
// Offset (dots) between the renderer's scheduled mode-0 transition and the
// event-model mode-0 STAT IRQ fire time. Tuned against the suite.
pub(in crate::ppu) const M0IRQ_OFFSET: i64 = -3;
// Mode-2 STAT IRQ fires this many dots relative to the schedule formula; the
// renderer-timed render tests need it earlier. Swept against the suite.
pub(in crate::ppu) const M2IRQ_OFFSET: i64 = -1;
// First-line-after-enable DMG single-speed mode-0 STAT IRQ correction (dots).
// On the first frame after the LCD turns on there is no prior mode-2 scan; the
// DMG first-frame arm (DMG_FIRST_FRAME_ARM_DOT=85) lands the line-0 m0 IRQ three
// master-cc late versus hardware. The ly0_m0irq / frame0_m0irq_count brackets
// (read-PC-calibrated to the exact m0 fire) place the true fire 3 dots earlier;
// every scx (0..3) is uniformly +3. Scoped to DMG SS first line so the
// steady-state m0/m2 IRQ schedule (the m0int/m2int canaries) is untouched.
pub(in crate::ppu) const M0IRQ_DMG_FIRST_FRAME_OFFSET: i64 = -3;
// Absolute-clock offset attributed to an FF41/FF45 register write. The write
// hook fires after the store but before this M-cycle's dots tick, so the
// renderer's current dot is already `abs_cc` (the M-cycle start), matching
// the write resolving at its access cc, before the M-cycle's +4 tick. No
// extra bias is needed at single speed. Swept against the full suite (0 beats
// the former -1 by 32 net).
pub(in crate::ppu) const WRITE_CC_OFFSET: i64 = 0;

// Sentinel for "no pending wy2 update".
pub(in crate::ppu) fn wy2_disabled() -> u64 { u64::MAX }

// Dots into a line before which the window-Y comparator's line input is not
// yet valid, so a scheduled re-check cannot match (see `run_wy_recheck`).
pub(in crate::ppu) const WY_RECHECK_LY_VALID_DOT: u128 = 3;

// Line-tail dot from which a CGB PPU's raw line counter -- the window-Y
// comparator's line input in single speed -- already reads the NEXT line (see
// `wy_comparator_ly`).
pub(in crate::ppu) const CGB_WY_RAW_LY_INC_DOT: u128 = 450;
fn pnow_disabled() -> u64 { u64::MAX }
fn win_y_pos_init() -> u8 { 0xFF }

// Mid-mode-3 register-write commit delays (dots, relative to the write cc) and
// render-phase offsets.
pub(in crate::ppu) const M0IRQ_SCX2_CGB_OFFSET: i64 = -1;
// DMG window bus-glitch (wg_apply): dots from the LCDC write's register commit
// to the VRAM address-line transition. (The renderer's absorbed pre-window
// sprite stall is read from the live SpriteFetchRec, not a constant.)
pub(in crate::ppu) const WG_TRANSITION_DELAY: u64 = 4;

/// Machine configuration for a CPU VRAM/OAM access-window query.
#[derive(Clone, Copy)]
pub(crate) struct AccessEnv {
    pub is_cgb: bool,
    pub(crate) cgb_de: bool,
    pub(crate) double_speed: bool,
    /// True when the access is issued by a HALT-woken CGB-native/AGB stream (the
    /// same `halt_woken_m3_read` population the STAT resolver keys on). Such reads
    /// land on the CPU M-cycle grid (re-phased to the waking IRQ edge), not the
    /// free-running dot grid the OAM-read boundaries are otherwise tuned to.
    pub(crate) halt_woken: bool,
}

pub(in crate::ppu) const WY1_DELAY: i64 = 2;
pub(in crate::ppu) const WY2_DELAY_CGB: i64 = 7;
pub(in crate::ppu) const WY2_DELAY_DMG: i64 = 4;
pub(in crate::ppu) const SCY_DELAY: i64 = 2;
pub(in crate::ppu) const WXEN_COMMIT_DELAY: i64 = 3;
pub(in crate::ppu) const WYTRIG_COMMIT_DELAY: i64 = 3;
pub(in crate::ppu) const GETSTAT_OFF_DS: i64 = -1;

// A tile-column index the real grid can never produce (`(spx-grid0) >> 3` is
// always an integer, never a half-step), used to mark "no column charged yet"
// so the first object of a fresh grid always pays the leading rate.
pub(in crate::ppu) const SPRITE_TILE_NONE: i32 = 1;
fn sprite_prev_tile_default() -> i32 { SPRITE_TILE_NONE }


/// Mode-3 dot cost of the per-line objects, as the fetcher pays it while walking
/// the BG tile grid from xpos 0 to `target_x`.
///
/// Hardware cost model: every visible object costs a flat 6 dots. On top of that,
/// the FIRST object encountered in a given 8-pixel tile column earns a leading
/// bonus of `5 - dist` dots, where `dist` is how many pixels past that column's
/// left edge the object sits — but only while `dist < 5` (an object landing in
/// the last 3 pixels of a column, or any later object sharing that column, pays
/// the flat 6). Equivalently the leading object costs `max(11 - dist, 6)`.
///
/// A window opening mid-line splits the objects at `nwx`: objects at `spx <= nwx`
/// walk the BG grid, and the post-window objects restart on a fresh grid rooted
/// at `nwx + 1` with no column yet charged.
///
/// `sprite_xs` MUST be sorted ascending by spx. `scx` is `SCX & 7`. `nwx` is the
/// window X split point (0xFF when no window starts this line). `target_x` is
/// `lcd_hres + 7 = 167`. `obj_enabled` follows LCDC.1 (always on for CGB).
/// Returns the total object cost in dots.
pub(in crate::ppu) fn sprite_tile_walk_cost(
    sprite_xs: &[i32],
    scx: i32,
    nwx: i32,
    target_x: i32,
    obj_enabled: bool,
) -> i32 {
    if !obj_enabled || sprite_xs.is_empty() {
        return 0;
    }
    // Tile-column origin at the mode-3 start (xpos 0): the grid edge sits `scx&7`
    // pixels back, so column boundaries fall at (8 - scx) mod 8. `discard` is the
    // fine-scroll pixel count dropped before xpos 0 (capped at 5), which shifts
    // only the very first object's column offset.
    let grid0 = (8 - scx).rem_euclid(8);
    let discard = scx.min(5);
    let column_of = |spx: i32, origin: i32| (spx - origin) >> 3;
    let mut cost = 0i32;
    let mut idx = 0usize;

    // The leading object at xpos 0 measures its offset from the discard phase
    // rather than the grid; when it lands in the first 5 discarded pixels it pays
    // the leading rate and is consumed here.
    let lead = sprite_xs[0];
    if discard + lead < 5 && lead <= nwx && lead <= target_x {
        cost += 6 + (5 - (discard + lead));
        idx += 1;
    }

    // Charge each remaining object: flat 6, plus the leading bonus for the first
    // object seen in each tile column. `seed_col` is the column already "charged"
    // when the walk begins (the xpos-0 column for the BG grid, none after a
    // window split).
    let charge = |xs: &[i32], idx: &mut usize, max_spx: i32, origin: i32,
                  seed_col: i32, cost: &mut i32| {
        let mut prev_col = seed_col;
        while *idx < xs.len() && xs[*idx] <= max_spx {
            let spx = xs[*idx];
            let col = column_of(spx, origin);
            let dist = (spx - origin).rem_euclid(8);
            if col != prev_col && dist < 5 {
                *cost += 6 + (5 - dist);
            } else {
                *cost += 6;
            }
            prev_col = col;
            *idx += 1;
        }
    };

    let bg_seed = column_of(0, grid0);
    if nwx < target_x {
        charge(sprite_xs, &mut idx, nwx, grid0, bg_seed, &mut cost);
        charge(sprite_xs, &mut idx, target_x, nwx + 1, SPRITE_TILE_NONE, &mut cost);
    } else {
        charge(sprite_xs, &mut idx, target_x, grid0, bg_seed, &mut cost);
    }

    cost
}

// DMG mid-mode-3 OBJ-enable toggle: dots from the write hook to the first
// pixel pop gated by the new LCDC.1.
// Base documented in Pan Docs: LCDC — OBJ enable is toggleable mid-frame ("toggling
// mid-scanline might have funky results on DMG? Investigation needed") and OBJ size
// mid-mode-3 "leaks"/artifacts. The exact apply latency (dots) is not documented —
// from mealybug-tearoom-tests refs.
pub(in crate::ppu) const OBJEN_APPLY_DOTS: u128 = 2;
// CGB (DMG-compat-on-CGB) mid-mode-3 OBJ-enable toggle commits one dot later
// than DMG-CPU silicon (the CGB PPU's pixel gate samples LCDC.1 a dot further out).
pub(in crate::ppu) const OBJEN_APPLY_DOTS_CGB: u128 = 3;
// DMG mid-mode-3 OBJ-size toggle: dots from the write hook to the fetcher
// seeing the new LCDC.2. A group-2 sprite whose HIGH byte reads exactly one dot
// after the apply splits its row addressing: low byte 8x8, high byte 8x16.
pub(in crate::ppu) const OBJSIZE_APPLY_DOTS: u128 = 1;
// Dots BEFORE the end of a sprite's fetch stall at which its tile-data LOW and
// HIGH bytes are read (object fetch: low at end-3, high at end-1).
pub(in crate::ppu) const OBJ_READ_LOW_BACK: u128 = 3;
pub(in crate::ppu) const OBJ_READ_HIGH_BACK: u128 = 1;
// CGB (DMG-compat-on-CGB) object fetch: the two tile-data bytes' size-sample
// dots sit 3 dots earlier within the stall than on DMG-CPU silicon (the CGB
// PPU begins the object tile-data fetch earlier relative to the stall end).
// A mid-mode-3 LCDC.2 toggle straddling the fetch therefore splits the row
// addressing at end-6 (LOW) / end-3 (HIGH).
pub(in crate::ppu) const OBJ_READ_LOW_BACK_CGB: u128 = 6;
pub(in crate::ppu) const OBJ_READ_HIGH_BACK_CGB: u128 = 3;

// Within line 153 (the last VBlank line) the LY register is held at 153 only
// briefly; after this many dots it reads 0, even though the line itself
// continues until dot 455. This matches the hardware LYC-compare-LY threshold
// (line time - 6 in single speed) and makes the LYC=LY interrupt for LY=0
// fire one line earlier than a naive end-of-line transition would suggest.
pub(in crate::ppu) const LINE_153_LY_ZERO_DOT: u128 = 6;

// Sprite attribute flags (from byte 3 of sprite data)
#[derive(Clone, Copy, Serialize, Deserialize)]
pub(crate) struct SpriteAttributes {
    pub priority: bool,    // 0 = above BG, 1 = behind BG colors 1-3
    pub y_flip: bool,      // 0 = normal, 1 = vertically mirrored
    pub x_flip: bool,      // 0 = normal, 1 = horizontally mirrored
    pub palette: bool,     // 0 = OBP0, 1 = OBP1 (DMG compatibility)
    pub raw: u8,           // Raw attribute byte for CGB palette access
}

impl SpriteAttributes {
    pub(crate) fn from_byte(byte: u8) -> Self {
        SpriteAttributes {
            priority: (byte & 0x80) != 0,
            y_flip: (byte & 0x40) != 0,
            x_flip: (byte & 0x20) != 0,
            palette: (byte & 0x10) != 0,
            raw: byte,
        }
    }
}

// Sprite data structure
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct Sprite {
    pub y: u8,
    pub x: u8,
    pub tile_index: u8,
    pub(crate) attributes: SpriteAttributes,
    pub(crate) oam_index: u8, // For priority resolution
}

// Live mode-3 per-sprite fetch record (parallel to `sprites_on_line`, same
// index space as `next_sprite_fetch_index`). Tracks whether the live walk
// actually fetched a sprite this line and at which dot its stall armed, so the
// DMG mid-mode-3 LCDC.1/LCDC.2 toggle model can resolve per-sprite semantics:
// - a sprite whose x-match dot passed while OBJ was disabled never fetches
// (skipped: no pixels, no stall — hardware skips the object process on DMG);
// - a sprite whose fetch is IN PROGRESS when OBJ is disabled aborts (hardware
// aborts the in-progress object fetch): the remaining stall dots are refunded and
// the sprite's pixels never reach the line;
// - a fetched sprite's two tile-data bytes each sample LCDC.2 (OBJ size) at
// their own fetch dot (arm + penalty - 3 / - 1), so a mid-fetch size toggle
// can split the row addressing between the LOW and HIGH bytes.
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub(in crate::ppu) enum SpriteFetchPhase {
    Pending,
    Fetched,
    Aborted,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub(in crate::ppu) struct SpriteFetchRec {
    pub(in crate::ppu) phase: SpriteFetchPhase,
    // Dot (self.ticks) the sprite's x-match landed. For left-clipped sprites
    // (OAM x < 8) the hardware match happens (8 - x) dots before the pixel
    // pipeline head reaches column 0, during the first-tile prologue; the
    // recorded tick carries that adjustment so the byte-fetch dots line up.
    pub(in crate::ppu) arm_tick: u128,
    pub(in crate::ppu) penalty: u8,
}

impl Default for SpriteFetchRec {
    fn default() -> Self {
        SpriteFetchRec { phase: SpriteFetchPhase::Pending, arm_tick: 0, penalty: 0 }
    }
}

// One mid-mode-3 BG tile captured for the CGB-compat up-pulse LCDC.4 train
// line-end re-resolve (see bg_tile_buf / cgb_train_reresolve).
#[derive(Clone, Copy, Serialize, Deserialize, Default)]
pub(in crate::ppu) struct CapturedBgTile {
    pub(in crate::ppu) n: u64,      // fetcher tile index from line start
    pub(in crate::ppu) tn: u8,      // latched tile number
    pub(in crate::ppu) first_x: u8, // display column of this tile's first (leftmost) pixel
    pub(in crate::ppu) y: u8,       // BG pixel row (ly + scy) & 0xFF for the tile-line lookup
    // Live (partial-journal) per-plane tile-data-select bits as drawn.
    // Diagnostic only on the BG path (the re-resolve recomputes both plane
    // bytes from the complete journal and re-plots per column); the WINDOW
    // analog still keys its split-tile repair on them.
    pub(in crate::ppu) live_low_tds: bool,
    pub(in crate::ppu) live_high_tds: bool,
}

// One mid-mode-3 WINDOW tile captured for the CGB-compat up-pulse LCDC.4 train
// line-end re-resolve (see win_tile_buf / win_train_reresolve). Window tiles are
// uniform (no per-plane split, no tile-index-as-data glitch), so a single
// tile-data-select sample per tile suffices.
#[derive(Clone, Copy, Serialize, Deserialize, Default)]
pub(in crate::ppu) struct CapturedWinTile {
    pub(in crate::ppu) n: u64,      // fetcher tile index from line start
    pub(in crate::ppu) tn: u8,      // latched tile number
    pub(in crate::ppu) first_x: u8, // display column of this tile's first (leftmost) pixel
    pub(in crate::ppu) y: u8,       // window internal line (win_y_pos) — the tile-line lookup row
    // Live per-plane tile-data-select bits as drawn. Window tiles are UNIFORM on
    // hardware (the base is latched once per tile), but rustyboi's per-substep
    // resolution can split them when a journal edge falls between the LOW (k=1)
    // and HIGH (k=2) reads — the mixed $8000/$8800 read the re-resolve corrects.
    pub(in crate::ppu) live_low_tds: bool,
    pub(in crate::ppu) live_high_tds: bool,
}

// Lazy per-line OAM Y/X snapshot: the sprite scan samples OAM position-by-
// position across mode 2, and this reproduces which positions have been sampled
// as of any given cc so a mid-scan OAM/DMA/size change is caught at the right
// position. Structure follows the standard walk-since-last-update model.
// Holds a lazily-sampled 80-byte snapshot of the OAM Y/X positions (`buf`,
// even=Y odd=X) plus the per-sprite large-size flag (`lsbuf`). The snapshot is
// advanced by `update(cc)`, which walks OAM positions up to
// `the pos-cycle conversion at cc = (line cycles(cc) + 1) % 456`, copying from the source. The
// source is the real OAM normally, but reads as 0xFF for the whole window of an
// active OAM-DMA (hardware points the OAM read at the cartridge's disabled RAM).
// `change(cc)` (on CPU OAM writes and at DMA start/end) caps the next walk via
// `last_change`. The per-line sprite list is built from `buf` at mode-2-END.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct OamReader {
    // posbuf_: Y at even index, X at odd index, for each of 40 sprites.
    #[serde(with = "serde_bytes")]
    pub(in crate::ppu) buf: [u8; 2 * OAM_SPRITE_COUNT],
    // lsbuf_: per-sprite large-size flag.
    #[serde(with = "self::bool40", default = "scan_slot_large_default")]
    lsbuf: [bool; OAM_SPRITE_COUNT],
    // lu_: cc of the last update (the position-walk anchor), in PPU `abs_cc`.
    pub(in crate::ppu) lu: u64,
    // last-change: position-walk cap (0xFF == no pending change).
    last_change: u8,
    // large-sprites source: live LCDC OBJ-size bit, latched into lsbuf on the walk.
    pub(in crate::ppu) large_src: bool,
    pub(in crate::ppu) cgb: bool,
    // Whether the source currently reads 0xFF (active OAM-DMA window).
    pub(in crate::ppu) src_disabled: bool,
    // OAM "bus retention" ghost, latched at the OAM-DMA start edge: on hardware
    // the mode-2 scan cannot read OAM while an OAM-DMA runs, and the Y/X bus
    // retains the last pair actually read (on hardware the OAM
    // Y/X bus only updates while no DMA is
    // active, but the object check still runs against the stale bus). Positions
    // walked inside the DMA window sample this pair instead of open-bus 0xFF
    // (ashiepaws strikethrough: the line-68 scan ghosts entry 39's (0x54, 79)
    // pair, re-matching the bar sprite the in-flight DMA is erasing).
    pub(in crate::ppu) ghost: (u8, u8),
    // Which sprite slots currently hold a ghost-sampled pair (vs a real OAM
    // sample). Ghost slots read their tile/attributes from the live
    // progressively-written OAM (`ppu_read_oam_live`) instead of the CPU view
    // (0xFF during DMA) — on hardware that fetch sees the in-flight DMA data.
    #[serde(with = "self::bool40", default = "scan_slot_large_default")]
    pub(in crate::ppu) ghost_slot: [bool; OAM_SPRITE_COUNT],
}

const OAM_POS_CYCLES: u32 = (2 * OAM_SPRITE_COUNT) as u32; // 80

// Sub-M-cycle correction (in single-speed dots) between the cc at which the PPU
// step observes the OAM-DMA window edge and the master cc hardware fires
// OAM-DMA start/OAM-DMA end at. Calibrated against the late_sp*x/y `_1`/`_2` and
// `_ds_1`/`_ds_2` bracket pairs.
pub(in crate::ppu) const OAMDMA_CHANGE_CC_OFFSET: u32 = 3;

fn scan_slot_large_default() -> [bool; OAM_SPRITE_COUNT] {
    [false; OAM_SPRITE_COUNT]
}

// serde derive stops at 32-element arrays; the per-sprite `[bool; 40]` flags
// (OAM_SPRITE_COUNT == 40) are packed into a u64 bitmask for savestates.
pub(crate) mod bool40 {
    use super::OAM_SPRITE_COUNT;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(crate) fn serialize<S: Serializer>(v: &[bool; OAM_SPRITE_COUNT], s: S) -> Result<S::Ok, S::Error> {
        let mut mask: u64 = 0;
        for (i, &b) in v.iter().enumerate() {
            if b {
                mask |= 1u64 << i;
            }
        }
        mask.serialize(s)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<[bool; OAM_SPRITE_COUNT], D::Error> {
        let mask = u64::deserialize(d)?;
        let mut out = [false; OAM_SPRITE_COUNT];
        for (i, b) in out.iter_mut().enumerate() {
            *b = (mask & (1u64 << i)) != 0;
        }
        Ok(out)
    }
}

impl Default for OamReader {
    fn default() -> Self {
        OamReader {
            buf: [0; 2 * OAM_SPRITE_COUNT],
            lsbuf: [false; OAM_SPRITE_COUNT],
            lu: 0,
            last_change: 0xFF,
            large_src: false,
            cgb: false,
            src_disabled: false,
            ghost: (0xFF, 0xFF),
            ghost_slot: [false; OAM_SPRITE_COUNT],
        }
    }
}

impl OamReader {
    fn changed(&self) -> bool {
        self.last_change != 0xFF
    }

    // the pos-cycle conversion: line cycles(cc)+1 wrapped to [0, 456).
    //
    // `cc` may be a past update cc (`self.lu`) lying on the PREVIOUS line relative
    // to `lc`'s anchor — rustyboi updates the OAM snapshot sparsely (only at
    // change/the event dispatch), so `lu` can trail the current line by up to ~one line
    // without the >=1-line full-resample (controller `update`) firing. The raw
    // `456 - ((time - cc) >> ds)` then goes negative and the u64 subtraction
    // overflow-panics in debug (silently wraps in release). Compute it signed and
    // reduce modulo the line length — the hardware unsigned wrap — so the position
    // stays in [0,456). Byte-identical to the old `if v>=456 {v-=456}` whenever
    // `cc` is within the current line (`dots` in 1..=456).
    fn to_pos_cycles(cc: u64, lc: &stat_irq::LyCounter) -> u32 {
        let dots = (lc.time.wrapping_sub(cc) >> lc.ds as u32) as i64;
        let raw = stat_irq::LCD_CYCLES_PER_LINE as i64 - dots + 1;
        raw.rem_euclid(stat_irq::LCD_CYCLES_PER_LINE as i64) as u32
    }

    // Re-seed the snapshot from the current OAM.
    pub(in crate::ppu) fn reset(&mut self, oam: &[u8; 2 * OAM_SPRITE_COUNT], cgb: bool) {
        self.cgb = cgb;
        self.large_src = false;
        self.src_disabled = false;
        self.ghost = (0xFF, 0xFF);
        self.ghost_slot = [false; OAM_SPRITE_COUNT];
        self.lu = 0;
        self.last_change = 0xFF;
        self.lsbuf = [self.large_src; OAM_SPRITE_COUNT];
        self.buf.copy_from_slice(oam);
    }

    // Seed the OAM snapshot at LCD enable (holds inactive until the post-enable window ends).
    pub(in crate::ppu) fn enable_display(&mut self, cc: u64, ds: bool) {
        self.buf = [0; 2 * OAM_SPRITE_COUNT];
        self.lsbuf = [false; OAM_SPRITE_COUNT];
        self.ghost = (0xFF, 0xFF);
        self.ghost_slot = [false; OAM_SPRITE_COUNT];
        self.lu = cc + ((OAM_POS_CYCLES as u64) << ds as u32) + 1;
        self.last_change = OAM_POS_CYCLES as u8;
    }

    // Latch the OAM-bus retention ghost at the OAM-DMA start edge. Called right
    // after the edge `change(cc)` capped the walk (`last_change`): the pair at
    // the last position the walk sampled before the cap is what the hardware
    // Y/X bus still holds when the DMA takes the OAM away from the scan. A cap
    // at/before position 1 means no pair was sampled on this line yet, so the
    // bus holds the previous line's final OAM read.
    //
    // `line_has_fetches`: whether the line whose reads the bus last saw had any
    // visible sprites. The Y bus is ALSO written by every mode-3 sprite
    // tile/flags fetch (on hardware the OAM Y bus latches the fetched tile byte),
    // so on a line that fetched sprites the retained value is a tile byte —
    // effectively never a matching Y — not the scan pair. Model that clobber as
    // an invisible ghost. It applies when the window opens outside the scan
    // walk (cap at 80: this line's fetches; cap before 2: the previous line's);
    // a mid-scan window start (2..80) retains the just-scanned pair, fetches
    // not yet run (late_sp39y_2 vs ashiepaws strikethrough, whose
    // DMA-start line renders no sprites so the scan pair survives to the next
    // line's walk).
    pub(in crate::ppu) fn capture_ghost(&mut self, line_has_fetches: bool) {
        let cap = (self.last_change as usize).min(2 * OAM_SPRITE_COUNT);
        if !(2..2 * OAM_SPRITE_COUNT).contains(&cap) && line_has_fetches {
            self.ghost = (0xFF, 0xFF);
        } else {
            let p = if cap >= 2 {
                (cap - 1) & !1
            } else {
                2 * OAM_SPRITE_COUNT - 2
            };
            self.ghost = (self.buf[p], self.buf[p + 1]);
        }
    }

    // Incremental OAM Y/X snapshot walk. `oam_y`/`oam_x` for sprite `i` are read
    // lazily via the closure (real OAM when enabled, 0xFF when DMA-disabled).
    pub(in crate::ppu) fn update(&mut self, cc: u64, lc: &stat_irq::LyCounter, oam_pos: &[u8; 2 * OAM_SPRITE_COUNT]) {
        if cc <= self.lu {
            return;
        }
        // Full-line-or-more elapsed since the last update: hardware walks the
        // whole 80-position buffer (distance = 2*lcd_num_oam_entries). Because
        // rustyboi updates sparsely (only at change/the event dispatch, not per access),
        // `the pos-cycle conversion(lu)` can underflow when lu is multiple lines old; do the
        // full re-sample explicitly from pos 0 so every position is refreshed
        // (sampling the disabled source if a DMA spans this whole window — which
        // it cannot for >1 line, so this is the steady-state/post-enable refresh).
        if self.changed()
            && ((cc - self.lu) >> lc.ds as u32) >= stat_irq::LCD_CYCLES_PER_LINE as u64
        {
            for i in 0..OAM_SPRITE_COUNT {
                self.lsbuf[i] = self.large_src;
                if self.src_disabled {
                    // OAM-DMA window: the scan's Y/X bus retains its pre-DMA
                    // pair (see `capture_ghost`), it does not read open-bus.
                    self.buf[2 * i] = self.ghost.0;
                    self.buf[2 * i + 1] = self.ghost.1;
                    self.ghost_slot[i] = true;
                } else {
                    self.buf[2 * i] = oam_pos[2 * i];
                    self.buf[2 * i + 1] = oam_pos[2 * i + 1];
                    self.ghost_slot[i] = false;
                }
            }
            self.last_change = 0xFF;
            self.lu = cc;
            return;
        }
        if self.changed() {
            let lulc = Self::to_pos_cycles(self.lu, lc);
            let mut pos = lulc.min(OAM_POS_CYCLES);

            // Distance to walk: from `pos` (the line cycle of the last update) to
            // `cclc` (now), within a single line (the >= 1-line case is handled
            // above). Mirrors the hardware OAM-reader update.
            let cclc = Self::to_pos_cycles(cc, lc);
            let mut distance = cclc.min(OAM_POS_CYCLES).wrapping_sub(pos)
                .wrapping_add(if cclc < lulc { OAM_POS_CYCLES } else { 0 });

            {
                let lcg = self.last_change as u32;
                let target = lcg.wrapping_sub(pos)
                    .wrapping_add(if lcg <= pos { OAM_POS_CYCLES } else { 0 });
                if target <= distance {
                    distance = target;
                    self.last_change = 0xFF;
                }
            }

            let mut d = distance;
            while d > 0 {
                d -= 1;
                if pos & 1 == 0 {
                    if pos == OAM_POS_CYCLES {
                        pos = 0;
                    }
                    if self.cgb {
                        self.lsbuf[(pos / 2) as usize] = self.large_src;
                    }
                    // During an OAM-DMA window the walk samples the retained
                    // Y/X bus pair (`ghost`), not open-bus 0xFF.
                    let (y, x) = if self.src_disabled {
                        (self.ghost.0, self.ghost.1)
                    } else {
                        (oam_pos[pos as usize], oam_pos[pos as usize + 1])
                    };
                    self.ghost_slot[(pos / 2) as usize] = self.src_disabled;
                    self.buf[pos as usize] = y;
                    self.buf[pos as usize + 1] = x;
                } else {
                    let cur = self.lsbuf[(pos / 2) as usize];
                    self.lsbuf[(pos / 2) as usize] = (cur && self.cgb) || self.large_src;
                }
                pos += 1;
            }
        }
        self.lu = cc;
    }

    // Cap the snapshot walk at the current position (an OAM change point).
    pub(in crate::ppu) fn change(&mut self, cc: u64, lc: &stat_irq::LyCounter, oam_pos: &[u8; 2 * OAM_SPRITE_COUNT]) {
        self.update(cc, lc, oam_pos);
        self.last_change = (Self::to_pos_cycles(self.lu, lc).min(OAM_POS_CYCLES)) as u8;
    }
}

pub(crate) enum LCDCFlags {
    BGDisplay = 1<<0,
    SpriteDisplayEnable = 1<<1,
    SpriteSize = 1<<2,
    BGTileMapDisplaySelect = 1<<3,
    BGWindowTileDataSelect = 1<<4,
    WindowDisplayEnable = 1<<5,
    WindowTileMapDisplaySelect = 1<<6,
    DisplayEnable = 1<<7,
}

// Test one LCDC bit in an arbitrary LCDC byte. The `Ppu::lcdc_has` method below
// covers the common `self.lcdc.reg` case; this free form is for the sites that
// deliberately test a DIFFERENT byte (a fetcher-latched `lcdc_state.lcdc`, a
// pre-write `old_lcdc`, the OR-read glitch's second LCDC), where silently
// substituting the live register would change behaviour.
#[inline]
pub(crate) fn lcdc_has(lcdc: u8, f: LCDCFlags) -> bool {
    (lcdc & (f as u8)) != 0
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum State {
    OAMSearch,
    PixelTransfer,
    HBlank,
    VBlank,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchDebugEventKind {
    TileNumber,
    TileDataLow,
    TileDataHigh,
    PushToFifo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FetchDebugEvent {
    pub kind: FetchDebugEventKind,
    pub ppu_ticks: u128,
    pub x: u8,
    pub ly: u8,
    pub fifo_size: usize,
    pub tile_index: u8,
    pub tile_num: u8,
    pub tile_attributes: u8,
    pub tile_line: u8,
    pub addr: Option<u16>,
    pub value: Option<u8>,
    pub lcdc: u8,
    pub tile_index_is_tile_data: bool,
    pub fetching_window: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelDebugEvent {
    pub ppu_ticks: u128,
    pub x: u8,
    pub ly: u8,
    pub bg_pixel_idx: u8,
    pub rgb: [u8; 3],
    pub lcdc: u8,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ppu) enum PendingLcdcEventKind {
    TileDataSelectOnly,
    Full,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ppu) struct PendingLcdcEvent {
    pub(in crate::ppu) cycles_remaining: u32,
    pub(in crate::ppu) base_value: u8,
    pub(in crate::ppu) value: u8,
    pub(in crate::ppu) kind: PendingLcdcEventKind,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColorCorrection {
    #[default]
    Linear,
    Lcd,
}

/// The PPU's raw frame, before the presentation palette is applied: either DMG
/// shade indices (0..=3) for a monochrome model, or already-corrected RGB888 for
/// a colour model (CGB/AGB) or a colorized SGB. Internal to the core — the GB
/// converts it to the unified always-RGB [`crate::gb::Frame`] using the DMG
/// palette + colour correction, while the shade indices remain available
/// (correction-independent) via [`Ppu::dmg_shade_frame`] for the test suite.
pub(crate) enum RenderedFrame {
    Monochrome(Box<[u8; FRAMEBUFFER_SIZE]>),
    Color(Box<[u8; FRAMEBUFFER_SIZE * 3]>),
}

/// Speed-switch / STOP-bridge sub-dot corrections: the residual phase the
/// whole-dot DS<->SS bridge cannot express, accumulated per switch.
#[derive(Serialize, Deserialize, Clone, Default)]
pub(in crate::ppu) struct SpeedPhase {
    // After a DS->SS speed switch the 3-dot stop bridge lands the LY counter one
    // master-cc higher than hardware (the DS half-dot the whole-dot bridge can't
    // express), so the closed-form `+1` the LY counter correction in `m0_time_exact`
    // over-corrects by 1. Set on the DS->SS switch, cleared at the next LCD
    // enable / LY reset.
    #[serde(default)]
    pub(in crate::ppu) lytime_no_plus1: bool,
    // Set when an SS->DS speed switch executes DURING mode 3. Across the switch
    // The hardware re-anchored LY-counter time (on a speed change) sits ~5 DS-dots
    // (10 cc) ahead of rustyboi's bridged renderer line phase for the FF44 (LY)
    // read's LY-register anticipation window. Consumed ONLY by `get_ly_reg_at_cc`
    // (not the STAT/mode-0 time predictor, which is already correct). Cleared at the
    // next LCD enable / LY reset, like `lytime_no_plus1`.
    #[serde(default)]
    pub(in crate::ppu) ssds_mode3_ly_advance: bool,
    // Frame boundaries completed since `ssds_mode3_ly_advance` was last set. The
    // mode-3-switch the LY counter re-anchor is a phase artifact local to the frames
    // right after the switch; once several frame wraps re-settle the phase it no
    // longer applies. Reset to 0 when the flag is set.
    #[serde(default)]
    pub(in crate::ppu) ssds_mode3_frames: u8,
    // Cumulative NON-mode-3 (OAM/HBlank) DS->SS speed-switch count for the LY-read
    // sub-dot phase accumulator (the hardware speed-change half-dot re-anchor,
    // applied per switch). rustyboi's whole-dot DS->SS bridge folds the integer part;
    // the residual half-dot per switch accumulates and its parity shifts the post-STOP
    // LY-register boundary read one sub-dot. Mode-3 DS->SS switches carry their residual
    // through the `stat_phase_carry` path instead, so they are excluded here.
    #[serde(default)]
    pub(in crate::ppu) dsss_ly_phase_count: u32,
    // Total DS->SS switch count (INCLUDING mode-3) for the early-frame anticipation
    // narrowing. Mode-3 DS->SS switches carry their sub-dot through the STAT-phase
    // carry for the glitch-dot resolution, but the anticipation-window WIDTH of an
    // early-frame read still tracks the full switch parity (extra mode-3 switches
    // flip the narrow-window parity).
    #[serde(default)]
    pub(in crate::ppu) dsss_ly_total_count: u32,
    // Set when an SS->DS speed switch executes during PixelTransfer (mode 3) and
    // the bridge dropped 2 dots (see `stop_bridge_advance`). If a subsequent
    // DS->SS switch follows (the double-switch speedchange{2..5} families), that
    // bridge restores the 2 dots so the net renderer advance matches the
    // single-switch base family's tuning. Cleared by the compensating DS->SS
    // switch or at the next LCD enable / LY reset.
    #[serde(default)]
    pub(in crate::ppu) sc_mode3_pullback_pending: bool,
    // Running count of DS->SS-during-mode3 STOP switches. The reference
    // the speed-change re-anchor is `now -= 1` (HALF an SS dot) per DS->SS
    // switch; the whole-dot bridge rounds each to 0, accumulating a missing HALF
    // dot per switch. `floor(count/2)` extra STAT-only carry dots (via
    // `stat_phase_carry`) reproduce that accumulated half-dot shift on the
    // STAT/line phase WITHOUT moving the render latch.
    #[serde(default)]
    pub(in crate::ppu) dsss_mode3_stop_count: u32,
    // Accumulated STAT-phase carry in master-cc (`1<<ds` per `stat_phase_carry`
    // dot). The carry advances the
    // STAT/line phase (line_cycle/abs_cc) so the STAT/m2-enable observables shift,
    // but the pixel-fetcher render latch must stay anchored to its ORIGINAL
    // position. The CPU VRAM/OAM/cgbp access-visibility gate (`ppu_blocks` via
    // `render_carry_skew`) SUBTRACTS this skew from the access cc so a store still
    // resolves against the un-carried fetcher mode-3 lock window — the decoupling
    // that lets the odd STAT-phase shift land without moving the render latch.
    #[serde(default)]
    pub(in crate::ppu) render_carry_skew_cc: i64,
    // Sub-PPU-dot parity (0/1) of the currently-resolving CPU register write at
    // double speed. Set by the bus just before the FF4x write hooks run.
    #[serde(skip, default)]
    pub(in crate::ppu) write_subdot: u8,
}

/// Presentation + debug sinks: the double-buffered framebuffers the frontend
/// reads, the panel-persistence state that decides what a skipped post-enable
/// frame shows, and the opt-in fetch/pixel debug event journals.
#[derive(Serialize, Deserialize, Clone)]
pub(in crate::ppu) struct FrameOut {
    #[serde(with = "fb_rle")]
    pub(in crate::ppu) fb_a: Box<[u8; FRAMEBUFFER_SIZE]>,
    #[serde(with = "fb_rle")]
    pub(in crate::ppu) fb_b: Box<[u8; FRAMEBUFFER_SIZE]>,
    /// SGB MASK_EN Freeze latch: the DMG shade frame captured at the first
    /// frame boundary after the freeze engaged, shown instead of the live
    /// frame until the mask clears (games hide their *_TRN transfer screens
    /// behind this). None when not frozen.
    #[serde(default)]
    pub(in crate::ppu) sgb_freeze_fb: Option<Vec<u8>>,
    #[serde(with = "fb_rle")]
    pub(in crate::ppu) color_fb_a: Box<[u8; FRAMEBUFFER_SIZE * 3]>, // RGB color framebuffer
    #[serde(with = "fb_rle")]
    pub(in crate::ppu) color_fb_b: Box<[u8; FRAMEBUFFER_SIZE * 3]>, // RGB color framebuffer
    pub(in crate::ppu) have_frame: bool,
    // First-frame-after-LCD-enable display blanking. On real hardware the panel
    // has not resynced for the first frame produced after LCDC.7 0->1, so it shows
    // the LCD-off "whiter than white" blank instead of that frame's pixels.
    // `frames_since_enable` counts completed frames since the last enable (saturating);
    // get_frame presents blank until it reaches 2 (one full frame after enable has
    // been displayed). Seeded to 2 so a skip_bios boot (LCD already on, no enable
    // edge observed) — and a savestate from a running system — displays normally.
    #[serde(default = "frames_since_enable_default")]
    pub(in crate::ppu) frames_since_enable: u8,
    // CGB panel persistence. The skipped first frame after an LCDC.7 enable is
    // not driven to the panel; the panel keeps showing whatever it last showed,
    // and decays to the "whiter than white" blank when the drive countdown
    // (SameBoy `frame_repeat_countdown`, measured on CGB-E: 144*456*2 + 3640
    // 8 MHz cycles, AGB 5982; re-armed at the START of every VBlank line
    // 144-152, run down in real time even with the LCD off) expires before the
    // skipped frame's own VBlank entry. The 144-line budget spans that render,
    // so an off may only last ~1820 4 MHz cc (just under 4 lines, AGB 2991)
    // measured from the start of the VBlank line it begins on. The EA CGB
    // middleware (Madden/NHL 2000, Men in Black) flips its double-buffered
    // tilemap every ~7 frames via a 2.5-line LCD off/on inside VBlank;
    // blanking those skipped frames (the pre-fix behavior) strobed the menu
    // white at ~9 Hz where hardware shows a seamless image. `last_drive_cc`
    // is the master cc of the last driven VBlank line start;
    // `panel_holds_image` is true once a frame has actually been DISPLAYED
    // (not blanked), so a panel that never displayed anything (power-on,
    // little-things-gb `firstwhite`'s one-frame enables) still blanks. Both
    // are serde(skip): savestate bytes stay identical, and a restored state
    // falls back to the blank for at most one frame.
    #[serde(skip, default)]
    pub(in crate::ppu) last_drive_cc: u64,
    #[serde(skip, default)]
    pub(in crate::ppu) panel_holds_image: bool,
    // Latched at the skipped frame's VBlank entry (the repeat decision samples
    // the drive window BEFORE that entry re-arms it, exactly as SameBoy checks
    // `frame_repeat_countdown` before re-arming); applied at frame completion.
    #[serde(skip, default)]
    pub(in crate::ppu) repeat_skip_pending: bool,
    #[serde(skip, default)]
    pub(in crate::ppu) fetch_debug_events_enabled: bool,
    #[serde(skip, default)]
    pub(in crate::ppu) fetch_debug_events: Vec<FetchDebugEvent>,
    #[serde(skip, default)]
    pub(in crate::ppu) pixel_debug_events: Vec<PixelDebugEvent>,
}

// `Box<[u8; N]>` has no `Default`, and `frames_since_enable` must power on at
// 2 (see the field's comment), so this cannot be derived.
impl Default for FrameOut {
    fn default() -> Self {
        FrameOut {
            fb_a: boxed_filled(0),
            fb_b: boxed_filled(0),
            sgb_freeze_fb: None,
            color_fb_a: boxed_filled(0),
            color_fb_b: boxed_filled(0),
            have_frame: false,
            frames_since_enable: frames_since_enable_default(),
            last_drive_cc: 0,
            panel_holds_image: false,
            repeat_skip_pending: false,
            fetch_debug_events_enabled: false,
            fetch_debug_events: Vec::new(),
            pixel_debug_events: Vec::new(),
        }
    }
}

/// The PPU-visible LCDC byte and everything that gates when a write to it
/// becomes visible: the quantized pending-commit queue plus the exact-cc
/// latches the per-substep consumers read instead of the queue.
#[derive(Serialize, Deserialize, Clone, Default)]
pub(in crate::ppu) struct LcdcState {
    #[serde(default)]
    pub(in crate::ppu) reg: u8,
    #[serde(default)]
    pub(in crate::ppu) cgb_tile_index_is_tile_data: bool,
    #[serde(default)]
    pub(in crate::ppu) pending_lcdc_events: Vec<PendingLcdcEvent>,
    // Exact-cc latch for a mid-mode-3 CGB LCDC bit4 (BGWindowTileDataSelect)
    // toggle. The per-dot pending-event queue quantizes the bit4 commit to a
    // dot boundary, which at double speed lands the change one BG-fetch substep
    // late (the change should split a tile between its TileDataLow and
    // TileDataHigh fetches, but the dot model applies it a substep too late).
    // Record the exact abs_cc at which the change becomes visible (`write_cc + 2`
    // PPU dots) and let the fetcher consult it per-substep. (commit_cc, new_lcdc, old_lcdc).
    #[serde(default)]
    pub(in crate::ppu) lcdc_b4_exact: Option<(u64, u8, u8)>,
    // Exact-cc window-enable (LCDC bit 5) toggle for the window-enable master checkpoints.
    // rustyboi's pending_lcdc_events commit the window bit one PPU dot before
    // the hardware LCDC write taking effect at cc+2 (the queue runs through one
    // step_lcdc_events on the write dot). That 1-dot-early commit is harmless to
    // the renderer/STAT resolve but mis-orders the lc450/lc454 window-enable master checkpoints
    // against a window-enable write whose hardware commit (`write_cc + 2`) lands
    // exactly on the checkpoint dot: hardware runs the window-enable master event
    // BEFORE the LCDC write resolves, so the checkpoint sees the OLD window bit. We
    // record the write's master-cc commit (`write_cc + 2`) and the bit's old/new
    // values; `update_window_y_latch` reads the window-enable bit as-of the
    // checkpoint cc through this. (commit_master_cc, new_win_bit, old_win_bit).
    #[serde(default)]
    pub(in crate::ppu) we_win_bit_exact: Option<(u64, bool, bool)>,
}

/// Mid-mode-3 VRAM address-bus glitch journals plus the fetch-grid anchors they
/// are resolved against. Every member is per-line scratch, cleared at each
/// mode-3 arm.
#[derive(Serialize, Deserialize, Clone, Default)]
pub(in crate::ppu) struct BusGlitch {
    // CGB tile-index-is-tile-data glitch targets (the hardware tile-select glitch).
    // Each falling mid-mode-3 LCDC.4 write records the single BG data read
    // (target_cc, target_k) that lands in the write's 1-T-cycle glitch window and
    // therefore returns the tile index instead of a VRAM byte. Resolved per fetch
    // substep in `tidxtd_quirk_at_fetch`. Cleared at each mode-3 arm.
    #[serde(default)]
    pub(in crate::ppu) tidxtd_glitch: Vec<(u64, u8)>,
    // DMG window bus-glitch journal: each mid-mode-3 LCDC write that toggles
    // bit 6 (window map select) or bit 4 (tile data select) records
    // (transition_cc, old_lcdc, new_lcdc) — the abs_cc at which the new address
    // lines reach the VRAM bus. Window fetch reads are re-evaluated against it
    // at their reconstructed hardware dots (see wg_apply). Cleared at each
    // mode-3 arm.
    #[serde(default)]
    pub(in crate::ppu) wg_hist: Vec<(u64, u8, u8)>,
    // Whether this line's bus-glitch journals resolve with the CGB-compat
    // rules (DMG cart on CGB hardware, single speed) instead of the DMG ones.
    // Latched at mode-3 arm.
    #[serde(default)]
    pub(in crate::ppu) wg_cgb: bool,
    // The undelayed window-restart TileNumber dot (abs_cc) for the current
    // line's window — the hardware fetch-grid origin F. None when the window
    // did not start through the x==0 restart path this line (the glitch model
    // is scoped to it) or when the pre-window sprite configuration is outside
    // the single-sprite case.
    #[serde(default)]
    pub(in crate::ppu) wg_anchor_cc: Option<u64>,
    // Hardware pre-window delay D_pre from an offscreen-left sprite (OAM X<=7)
    // fetched before the window restart. 0 when none.
    #[serde(default)]
    pub(in crate::ppu) wg_dpre: u64,
    // The line's first BG TileNumber read dot (abs_cc) — the hardware BG
    // fetch-grid origin for bg_wg_apply / the SCY journal. Recorded at the
    // tile-0 TileNumber substep; None before it or on lines that never fetch
    // BG. Cleared at each mode-3 arm.
    #[serde(default)]
    pub(in crate::ppu) bg_anchor_cc: Option<u64>,
    // The same origin in line-relative dots (`ticks`), recorded on every
    // model (bg_anchor_cc is DMG-only). The BG fetch grid reaches display
    // column C at `bg_anchor_dot + 8 + C`; the CGB WE-off revert column
    // resolves against that grid. Cleared at each mode-3 arm.
    #[serde(default)]
    pub(in crate::ppu) bg_anchor_dot: Option<u128>,
    // DMG mid-mode-3 SCY write journal: (transition_cc, old, new) — the abs_cc
    // at which the new map-row / tile-line address bits reach the VRAM bus.
    // BG fetch reads resolve SCY against it at their reconstructed hardware
    // dots (see bg_wg_apply). Cleared at each mode-3 arm.
    #[serde(default)]
    pub(in crate::ppu) bg_scy_hist: Vec<(u64, u8, u8)>,
    // DMG mid-mode-3 SCX write journal: (write_cc, old, new). The BG tile-map
    // column resolves SCX against it at the tile's reconstructed hardware
    // TileNumber dot (see bg_wg_apply / m3_scx_high_5_bits). Cleared each M3 arm.
    #[serde(default)]
    pub(in crate::ppu) bg_scx_hist: Vec<(u64, u8, u8)>,
    // Capture-phase mid-mode-3 BG tile buffer (CGB-compat up-pulse LCDC.4 train
    // re-resolve). Each BG tile pushed to the FIFO during mode 3 records the
    // context needed to re-resolve its tile-data-select bits against the
    // COMPLETE wg_hist journal at line-end and re-plot: (fetch index n, tile
    // number, first display column, tile-row y (0..255)). Reset each mode-3 arm.
    #[serde(default)]
    pub(in crate::ppu) bg_tile_buf: Vec<CapturedBgTile>,
    // Capture-phase mid-mode-3 WINDOW tile buffer (CGB-compat up-pulse LCDC.4
    // train re-resolve; the window analog of bg_tile_buf). See win_train_reresolve.
    #[serde(default)]
    pub(in crate::ppu) win_tile_buf: Vec<CapturedWinTile>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Ppu {
    pub(in crate::ppu) fetcher: fetcher::Fetcher,
    pub(in crate::ppu) disabled: bool,
    pub(in crate::ppu) state: State,
    pub(in crate::ppu) ticks: u128,
    pub(in crate::ppu) x: u8,

    // Sprite data for current scanline
    pub(in crate::ppu) sprites_on_line: Vec<Sprite>,
    pub(in crate::ppu) current_oam_sprite_index: usize, // Current sprite being checked during OAM search
    // Lazy OAM Y/X snapshot. Drives sprite
    // visibility so an OAM-DMA overlapping mode-2 retroactively zeroes positions
    // sampled inside the DMA-disabled window. Fed by `oam_change`/`oam_update`.
    // Serialized so a mid-session savestate round-trips the sprite snapshot.
    // Was `#[serde(skip)]`: a restored machine then scanned an all-zero Y/X
    // buffer and dropped every sprite (the rewind "sprites vanish" bug). A
    // legacy state lacking this key loads the default (empty) snapshot.
    #[serde(default)]
    pub(in crate::ppu) oam_reader: OamReader,
    // Tracks the previous-dot OAM-DMA "writing" state so the PPU can fire the
    // OamReader `change` (source toggle) on DMA start/end edges.
    #[serde(default)]
    pub(in crate::ppu) prev_dma_writing: bool,
    // Set once the OamReader has been seeded for the current LCD-on session.
    #[serde(default)]
    pub(in crate::ppu) oam_reader_seeded: bool,
    // Per-slot OBJ size recorded by the incremental mode-2 scan, reused by the
    // snapshot rebuild so the calibrated size-latch timing is preserved.
    // Serialized so the current line's per-sprite height round-trips a savestate.
    #[serde(with = "self::bool40", default = "scan_slot_large_default")]
    pub(in crate::ppu) scan_slot_large: [bool; OAM_SPRITE_COUNT],
    #[serde(default)]
    pub(in crate::ppu) next_sprite_fetch_index: usize,
    // Tile number `(spx - first-tile xpos) & -8` of the most recently charged
    // sprite in the live mode-3 walk. Sprites sharing a tile with this one cost
    // a flat 6 (only the first sprite per BG tile gets the leading rate), matching
    // The previous sprite's tile number, tracked while accumulating mode-3 sprite cost.
    // Reset to SPRITE_TILE_NONE at M3 start and on window draw-start.
    #[serde(default = "sprite_prev_tile_default")]
    pub(in crate::ppu) m3_sprite_prev_tile: i32,
    // Tick at which the most-recently-fetched sprite's stall was armed (the dot
    // `next_sprite_fetch_index` last advanced, and the first stall dot was consumed).
    // Hardware charges that sprite's `max(11-dist,6)` stall
    // dots one at a time as `p.cycles` counts down, so a mid-mode-3 OBJ-disable
    // refunds only the not-yet-counted-down remainder of the in-progress sprite:
    // `cost - (ticks - this + 1)` (see `remaining_sprite_cost`).
    #[serde(default)]
    pub(in crate::ppu) m3_last_sprite_commit_tick: u128,
    #[serde(default)]
    pub(in crate::ppu) sprite_fetch_stall: u8,
    #[serde(default)]
    pub(in crate::ppu) pixel_transfer_warmup: u8,
    // Fetcher cadence counter, decoupled from absolute self.ticks so that
    // sprite-fetch stall dots do not flip the fetcher's even/odd phase.
    // Reset to 0 on every OAMSearch -> PixelTransfer transition.
    #[serde(default)]
    pub(in crate::ppu) fetcher_cadence_tick: u8,

    // Window state tracking
    // The hardware `window Y position`: the window's internal Y line, incremented by 1 ONLY
    // at the moment the window actually begins drawing on a line (the mode-3-start window checkpoint /
    // pixel output draw-start), NOT per-line whenever ly > wy. Initialized to 0xFF
    // at frame start so the first window-draw line yields window Y position == 0. The
    // fetcher uses this (masked) for the window tile row / tile line.
    #[serde(default = "win_y_pos_init")]
    pub(in crate::ppu) win_y_pos: u8,
    // The `win_draw_start` bit of the window-draw state. On DMG, when WX matches
    // at xpos == 166 (lcd_hres+6) the window cannot draw this line (the line
    // ends first) but ARMS: win_draw_start is set and survives into the next
    // line, where the mode-3-start window checkpoint activates the window from x==0 (the window-Y increment) even
    // though WX is unchanged. Set during a line, consumed at the next line's
    // M3 start. CGB never arms this way (handled by pixel output's !cgb guard).
    #[serde(default)]
    pub(in crate::ppu) win_draw_start: bool,
    // Set at this line's M3 start (the window checkpoint) when win_draw_start was armed
    // from the previous line and the window is enabled: the window draws from
    // x==0 this line regardless of WX. Consumed by the PixelTransfer window
    // start at x==0.
    #[serde(default)]
    pub(in crate::ppu) win_draw_started_at_x0: bool,
    // The `win_draw_started` bit of the window-draw state: persists across lines
    // once the window has begun drawing this frame, until a WE-off / display
    // disable / frame end clears it. Distinct from `window_started_this_line`
    // (per-line). Mirrors the hardware pixel-output distinction between starting
    // the window now vs re-arming an already-started window: the FIRST WX==166 match with the
    // window not yet drawing starts it on that very line (the window-Y increment, no visible
    // pixels), so the next line draws with window Y position one higher than an arm-only
    // path would give. Needed by the DMG wxA6 cluster.
    #[serde(default)]
    pub(in crate::ppu) win_draw_started: bool,
    pub(in crate::ppu) window_y_triggered: bool,   // Whether WY condition was met this frame
    pub(in crate::ppu) window_started_this_line: bool, // Whether window started rendering on current scanline
    // A CGB mid-line WE-off cleared `window_started_this_line` on a line that
    // HAD already restarted the fetcher in window mode. The restart's FIFO
    // shortfall outlives the disable, so the mode-3 end still has to let the
    // renderer run on (image-only) to x==160 instead of cutting the line off
    // at the closed-form mode-0 boundary. Cleared per line.
    #[serde(default)]
    pub(in crate::ppu) win_weoff_deferred_tail: bool,
    // Dot (within-line `ticks`) at which the window began drawing this line.
    // The StartWindowDraw mode-3 penalty becomes non-refundable once the
    // pipeline advances WIN_M3_PENALTY dots past this; used by the late_disable
    // read-at-cc recompute to decide whether a mid-M3 window-disable keeps the
    // window-inclusive mode-0 time or reverts to the no-window length.
    pub(in crate::ppu) win_start_dot: Option<u128>,
    // Predicted within-line `ticks` at which the window WILL begin drawing this
    // line, computed at M3 arm from WX/SCX when a window is scheduled. Used only
    // on DMG to resolve the disable-AT-window-start boundary race: the LCDC-write
    // hook fires during the CPU's store, one step before the PixelTransfer code
    // that latches `win_start_dot`, so a disable landing on the exact start dot
    // sees `window_started_this_line == false` even though the StartWindowDraw
    // penalty is already committed. The late_disable_N cluster brackets this:
    // disable strictly before the start dot refunds (mode 0), at/after keeps
    // (mode 3). `None` when no window is scheduled this line.
    #[serde(default)]
    pub(in crate::ppu) predicted_win_start_dot: Option<u128>,
    // Set once a late-WX mid-window refund has been applied this line, so a
    // second WX write does not refund twice.
    pub(in crate::ppu) win_wx_penalty_resolved: bool,
    // Set once a mid-mode-3 WX-write window-ENABLE has been resolved this line
    // (penalty added or determined not-applicable), so the WX != arm-WX
    // pre-window-start condition does not re-enter and null the schedule on the
    // following dots.
    #[serde(default)]
    pub(in crate::ppu) win_wx_enable_resolved: bool,

    // STAT interrupt state tracking
    // True for the first scanline after LCDC.7 transitions 0 -> 1. On real
    // hardware this line has no Mode 2 phase: STAT reports mode 0 until M3
    // begins, no Mode 2 STAT IRQ fires, and M3 starts later than usual
    // (dot 85 on DMG / 86 on CGB instead of 80 / 82).
    #[serde(default)]
    pub(in crate::ppu) first_line_after_enable: bool,
    // The hardware OAM-reader lookup-until (`lu`) boundary for `inactive-after-enable(cc) = cc < lu`:
    // the master cc until which, right after an LCD enable, the STAT resolve suppresses
    // mode 2/3 (reports mode 0). Seeded at enable to `enable_cc + (80<<ds) + 1`.
    #[serde(default)]
    pub(in crate::ppu) display_enable_inactive_until: u64,
    // True once we've zeroed FF44 partway through line 153 and before the
    // line itself ends. Used to gate the end-of-frame transition and the
    // LY=0 Mode 2 pretrigger (both of which originally checked LY==153).
    #[serde(default)]
    pub(in crate::ppu) line_153_ly_zeroed: bool,
    // Number of BG pixels discarded so far for SCX fine-scroll alignment at
    // the start of Mode 3 (while x == 0). Faithful to the hardware mode-3-start fine-scroll
    // per-dot loop: each dot, the LIVE `scx % 8` is re-read; if we have not
    // yet discarded that many pixels we pop one and consume the dot, else we
    // begin output. A mid-M3 SCX write therefore changes both the discard
    // count and (because the BG tile column re-reads SCX live) the fetched
    // tile-map column. Reset to 0 at every M3 arm.
    #[serde(default)]
    pub(in crate::ppu) m3_pixels_discarded: u8,
    // Fine-scroll discard target latched at M3 start (the mode-3-start fine-scroll phase
    // samples `scx % 8` when the loop first runs, at M3 start, before the
    // mode-2 STAT handler's mid-M3 SCX write lands). Reading SCX live in the
    // pop loop samples it too late (after FIFO latency), capturing the
    // already-written value and over-discarding. -1 = not yet latched.
    #[serde(default)]
    pub(in crate::ppu) m3_discard_target: i8,
    // Dot at which the current line's M3 (PixelTransfer) was armed. xpos in
    // The mode-3-start fine-scroll loop xpos == ticks - this. Used to re-read SCX at the
    // same early M3 dots hardware samples, so a mid-discard SCX write moves the
    // break target without the FIFO-warmup latency over-reading later writes.
    #[serde(default)]
    pub(in crate::ppu) m3_arm_dot: u128,
    // DMG window-startup fetch phase anchor: the trigger dot of a mid-line
    // window start. Hardware restarts the fetcher ON the trigger dot
    // (TileNumber dots t..t+1, data-low t+2..t+3, data-high t+4..t+5, push at
    // t+6), so the first window pixel pops exactly 6 dots after the trigger
    // regardless of the global fetch parity. While set, the fetch cadence is
    // (ticks - anchor) % 2 == 0 instead of ticks % 2 == 0; cleared at the first
    // window tile's PushToFIFO (the FIFO's 8-pixel slack absorbs the re-sync to
    // global parity). DMG-only; CGB keeps its decoupled fetcher_cadence_tick.
    #[serde(default)]
    pub(in crate::ppu) win_fetch_anchor: Option<u128>,
    // DMG WX 1..6 immediate window start: on hardware the WX comparator matches
    // during the discard prologue at position WX-7, so the window activates
    // chop = (7-WX) dots EARLIER than the WX=7 (position 0) start, and the
    // remaining chop prologue discard pops (1 per dot, from the freshly pushed
    // window tile) chop its leading pixels. Net: the first VISIBLE window pixel
    // appears at the same dot as a WX=7 start (the earlier activation exactly
    // cancels the extra discards) but the CONTENT starts at window pixel chop
    // (e.g. a 3-px left chop at WX=4) and the fetch pipeline runs chop dots
    // ahead (no FIFO underrun after the chopped tile). Emulated by running
    // the first chop dots' worth of fetch substeps on the trigger dot
    // (win_fetch_anchor = ticks - chop) and pacing the chop pops 1/dot in the
    // x==0 prologue. Remaining chop pops this line; reset at M3 arm.
    #[serde(default)]
    pub(in crate::ppu) win_first_tile_chop: u8,
    // The hardware "window is being fetched" state: true from a window activation until the
    // first FIFO pop that follows it (chop/discard pops count). The reactivation
    // insert below is suppressed while set — the activation's own first tile
    // must not self-insert.
    #[serde(default)]
    pub(in crate::ppu) win_being_fetched: bool,
    // DMG window "reactivation zero pixel" (the hardware BG-pixel insert): when the
    // WX comparator matches AGAIN while the window is already active (a mid-
    // mode-3 WX rewrite), and the match dot coincides with a window tile's
    // nametable-read fetch dot with the FIFO holding exactly 8 pixels, the PPU
    // renders one color-0 pixel WITHOUT popping the FIFO — inserting a pixel
    // that shifts the rest of the line right by one (an every-8-rows diagonal at
    // x = WX-7). Consumed by the next draw_fifo_pixel.
    // Pan Docs: Window mid-frame behavior — https://gbdev.io/pandocs/Window.html
    #[serde(default)]
    pub(in crate::ppu) insert_bg_pixel: bool,
    // DMG per-dot visibility history of LCDC.5 (window enable) inside mode 3,
    // shifted at the top of each PixelTransfer dot: [k] = the visible bit k
    // dots ago ([0] = current dot). Our visible bit (self.lcdc.reg, via the
    // 2-dot pending-event commit) turns OFF 2 dots before and ON 1 dot
    // before the hardware-visible bit, so an 8-cycle WE-off pulse spans 9
    // visible OFF dots. Three taps (each successive window row shifts the WE
    // pulse one dot along the window-trigger/fetch grid):
    // - WX comparator (trigger + prologue paths): we[2] INSTEAD of the live
    // bit — hardware needs WE asserted on the check dot and the one
    // before (an 8-cycle pulse blocks 9 comparator dots);
    // - window fetcher TileNumber kill: OFF at BOTH [3] and [4] (hardware
    // samples WE one dot delayed at its T1 dot, one dot before our TN);
    // - WE-off zero-pixel insert: we[2] at the tile-boundary pop dot.
    // Seeded at M3 arm.
    #[serde(default)]
    pub(in crate::ppu) we_dot_hist: [bool; 5],
    // Display-x values at which a pushed BG/window tile's FIRST pixel will
    // pop (the hardware push-at-empty dots, where the WE-off zero-pixel insert
    // glitch is evaluated). Queued at PushToFIFO, consumed at the pop of
    // that x; at most two tiles are in flight.
    #[serde(default)]
    pub(in crate::ppu) we_glitch_tile_starts: [Option<u8>; 2],
    // DMG WE-off insert glitch, discard-prologue variant: the line's FIRST
    // push-at-empty boundary sits at position -(SCX&7) — INSIDE the
    // fine-scroll discard prologue — and the comparator match there is
    // WX == position + 7, i.e. WX == 7 - (SCX&7). The inserted color-0 pixel
    // is itself swallowed by the prologue: one discard dot pops it instead of
    // a real BG pixel, so one extra leading BG pixel survives and the visible
    // line shifts right by one with NO on-screen glitch pixel. Set at the
    // push dot, consumed by the first discard pop; reset at M3 arm.
    #[serde(default)]
    pub(in crate::ppu) we_glitch_discard_insert: bool,
    // The hardware window-pixel-insertion-disable glitch: a WE-off LCDC write
    // landing while a window tile fetch is in flight (win_being_fetched)
    // suppresses the WE-off zero-pixel insert for the REST of the line.
    // Reset at M3 arm.
    #[serde(default)]
    pub(in crate::ppu) we_insert_suppressed: bool,
    // Which WE tap the window TileNumber kill samples (see we_dot_hist).
    // A MID-LINE window restart runs its fetch on the trigger-anchored grid,
    // where the hardware tile-number dot sits one dot BEFORE our TN step
    // (tap [3]); a LINE-BEGIN (mode-3-start window checkpoint) window runs on the global fetch
    // grid where they coincide (tap [2]).
    #[serde(default)]
    pub(in crate::ppu) win_kill_tap_late: bool,
    // One-shot latch for the DMG WX=0 + SCX&7>0 window-activation quirk: the
    // window activates one T-cycle later than the plain x==0 start. Set on the
    // would-be trigger dot (which becomes a dead dot: no pop, no activation);
    // the trigger then fires on the next dot. Reset at M3 arm.
    #[serde(default)]
    pub(in crate::ppu) win_wx0_delayed: bool,
    // DMG mid-line WX comparator deferral: the hardware comparator samples WX
    // through the end of the CPU store's M-cycle, so a match seen with the OLD
    // WX on the store's commit dot must NOT start the window (a WX==LY match can
    // lose to a WX restore landing on that very dot). Arm (trigger dot, matched wx) on the
    // exact x+7==wx match; commit one dot later iff WX still reads the matched
    // value, with a one-substep catch-up so the restart timing is byte-identical
    // to the immediate start for a stable WX. Cleared at M3 arm.
    #[serde(default)]
    pub(in crate::ppu) dmg_wx_trigger_pending: Option<(u128, u8)>,
    // scx%8 sampled at M3 arm, used by the closed-form mode-0 schedule's
    // discard prefix. If the live f1 break resolves to a different count, the
    // schedule is nudged by the difference so M3 ends at the right dot.
    #[serde(default)]
    pub(in crate::ppu) m3_arm_scx: u8,
    // Full SCX (all 8 bits) sampled at M3 arm. The first BG tile in the FIFO is
    // fetched from column (arm_scx / 8). If a mid-M3 SCX write moves the f1 break
    // to a different tile column (the mode-3-start fine-scroll phase re-reads SCX live at
    // its case-0 tile fetch), the already-queued first tile is stale and the
    // FIFO must be refetched from the new column. -1 = not yet armed this line.
    #[serde(default)]
    pub(in crate::ppu) m3_arm_scx_full: i16,
    // WX snapshot taken when the closed-form mode-0 schedule was computed; a
    // mid-mode-3 WX change before the window starts invalidates the schedule.
    pub(in crate::ppu) m3_scheduled_wx: u8,
    // window_will_start() result at schedule time; a mid-mode-3 WY write that
    // flips it (late WY==ly) invalidates the schedule.
    #[serde(default)]
    pub(in crate::ppu) m3_scheduled_win: bool,
    // OBJ-size (LCDC bit2) value used by the mode-2 OAM scan, latched one scan
    // slot behind the live LCDC. The hardware OAM scanner latches the per-OAM
    // entry size (`lsbuf_[pos/2]`) when that entry's OAM slot is read; a mid-mode-2
    // size write only affects entries scanned strictly AFTER the write commits.
    // Refreshed from the live LCDC after each scan slot so a write landing within
    // a slot's window applies to the next slot (the late_sizechange 1-cc boundary).
    #[serde(default)]
    pub(in crate::ppu) scan_obj_size_large: bool,
    // Exact-cc OBJ-size (LCDC bit2) latch for the mode-2 OAM scan (PoC extension
    // of the SCX f1 / LCDC-bit4 pattern). A mid-mode-2 sprite-size write goes
    // through the pending_lcdc_events queue (a 2-dot quantized self.lcdc.reg commit)
    // AND the per-slot `scan_obj_size_large` snapshot lags one slot, which on the
    // late_sizechange* tests pushes the change one OAM slot too late: the sprite
    // whose 8x16-only y-range straddles the line is scanned with the stale 8x8
    // size and dropped, so mode-0 time (and the boundary FF41 STAT read) resolves the
    // wrong mode. The hardware OAM scanner latches each entry's size at that
    // entry's OAM-read cc; record the exact abs_cc at which the bit2 change
    // becomes visible (`write_cc + 2*cgb`, an LCDC write taking effect at cc+2 on hardware) and let
    // each scan slot sample bit2 as-of its OWN abs_cc. (apply_cc, old_large,
    // new_large); apply_cc == wy2_disabled() means no pending change.
    #[serde(default = "wy2_disabled")]
    pub(in crate::ppu) objsize_apply_cc: u64,
    #[serde(default)]
    pub(in crate::ppu) objsize_prev_large: bool,
    #[serde(default)]
    pub(in crate::ppu) objsize_new_large: bool,
    // Absolute `ticks` dot at which Mode 3 -> Mode 0 (HBlank) fires. Computed
    // at M3 arm from a cycle-exact mode-3 length formula (matching hardware) and
    // drives the FF41 mode bits + mode-0 STAT IRQ, replacing the x==160 trigger.
    #[serde(default)]
    pub(in crate::ppu) scheduled_mode0_dot: Option<u128>,
    // The hardware `mode-0 (HBlank) time` for the current line, in MASTER-cc units: the absolute clock at
    // which the predicted mode-3 -> mode-0 transition occurs, equal to
    // the xpos-167 advance time `now_at_arm + (m3_len << ds)`. Captured at M3
    // arm (master_cc + m3_len<<ds). The CPU's FF41 read resolves mode 3 iff
    // `access_cc + 2 < m0_time_master` (the hardware STAT resolve); the mode-0 STAT IRQ
    // fires one xpos earlier (the xpos-166 advance time `mode-0 time - (1<<ds)`).
    // None when no closed-form dot is available (window / first line).
    #[serde(default)]
    pub(in crate::ppu) m0_time_master: Option<u64>,
    // Master-cc anchor at which CGB palette RAM (FF69/FF6B) becomes INACCESSIBLE
    // for the current line (the hardware CGB-palette-accessible window: blocked once
    // `line cycles(cc) + ds >= 80`). Captured at M3 arm from the same master_cc /
    // m3_arm_dot the m0_time_master uses, so the cgbp begin boundary resolves at
    // the CPU's access cc rather than the renderer dot (whose pre/post-tick phase
    // differs between the read and write paths). None when no closed-form M3 arm
    // exists (first line after enable). Paired with `m0_time_master` for the end.
    #[serde(default)]
    pub(in crate::ppu) cgbp_block_start_cc: Option<u64>,
    // The CPU-visible mode-0 (HBlank) start dot is computed on demand by
    // `reported_mode0_dot_value` from the closed-form `scheduled_mode0_dot` plus
    // a per-phase early-report nudge. It is decoupled from the live pixel
    // pipeline's actual M3 termination, driving ONLY the FF41 mode bits read back
    // by the CPU and the mode-0 STAT IRQ arm, so it can report mode 0 a few dots
    // EARLIER than the renderer drains its FIFO (hardware computes the reported
    // mode from the closed-form mode-3 length, not from the pixel-pump
    // termination) without ever hanging M3. This flag latches once that report
    // has fired for the current line, so the later live termination does not
    // re-drive the mode bits or re-fire the STAT check.
    #[serde(default)]
    pub(in crate::ppu) mode0_reported_this_line: bool,

    // Latched once `render_full_line` has produced the current visible line's
    // framebuffer, so the closed-form line render runs at most once per line.
    // Reset at the start of each line (mode-2 entry).
    #[serde(default)]
    pub(in crate::ppu) line_rendered_this_line: bool,

    // DMG wx==166 pixel output-at-xpos166 runs once at the mode-3 -> HBlank
    // transition; this guards against the two transition call sites both firing
    // it on the same line. Reset at M3 start. See apply_dmg_wxa6_lineend_windraw.
    #[serde(default)]
    pub(in crate::ppu) wxa6_lineend_applied: bool,

    // Event-scheduled STAT/mode/LYC IRQ model. `abs_cc` is a monotonic absolute
    // dot clock; `line_cycle` (0..455) tracks position within the current 456-dot
    // line. Together they reproduce the reference `the LY counter` (`time` = abs_cc
    // when LY next increments).
    #[serde(default)]
    pub(in crate::ppu) abs_cc: u64,
    // LCD-enable anchor (the hardware PPU-clock base): the master cc value at which
    // the PPU dot-clock `abs_cc` was last re-based. The PPU's machine-cycle clock
    // is `master_cc - p_now` (both advance 1/T-cycle), so `p_now` folds the PPU
    // onto the single master cc. Re-anchored on LCD enable / LY-write reset, and
    // on every speed change / STOP bridge where the master cc and the PPU's
    // render-dot accumulation diverge in count. DISABLED sentinel until first
    // enable, where it is seeded so the derived value equals the accumulator.
    #[serde(default = "pnow_disabled")]
    pub(in crate::ppu) p_now: u64,
    pub(in crate::ppu) speed: SpeedPhase,
    // The hardware `wy2`: WY delayed by `6 - double_speed` cc after a write.
    // Event-scheduled against the write cc; consumed by the window-Y gate so
    // the M3-length predictor / window-start see the delayed value.
    #[serde(default)]
    pub(in crate::ppu) wy2: u8,
    // Absolute clock at which a pending wy2 update applies; DISABLED when none.
    #[serde(default = "wy2_disabled")]
    pub(in crate::ppu) wy2_apply_cc: u64,
    // The WY value to latch into wy2 when wy2_apply_cc arrives.
    #[serde(default)]
    pub(in crate::ppu) wy2_pending: u8,
    // The delayed WY value the window-enable master checkpoints read: updated at
    // `cc + 1 + cgb` after a write (`update(cc + 1 + cgb)` in `WY change`).
    // Distinct from `wy2` (the per-line gate value), which is delayed further.
    #[serde(default = "win_y_pos_init")]
    pub(in crate::ppu) wy1: u8,
    #[serde(default = "wy2_disabled")]
    pub(in crate::ppu) wy1_apply_cc: u64,
    // Absolute clock of a pending on-write WY==LY re-comparison (hardware's
    // scheduled window-Y check). A WY or LCDC store re-runs the comparator a
    // few cc later instead of waiting for the next per-line checkpoint, so a
    // WY value that is only briefly equal to the current line still arms the
    // window. DISABLED when none; never armed once the latch is already set.
    //
    #[serde(default = "wy2_disabled")]
    pub(in crate::ppu) wy_recheck_cc: u64,
    #[serde(default)]
    pub(in crate::ppu) wy1_pending: u8,
    // Delayed SCY/SCX visible to the BG fetcher during mode 3. A mid-M3 write to
    // FF42/FF43 resolves in mmio immediately (CPU readback is live), but the
    // fetcher sees the new value only after `scy/scx_apply_cc` (write-side analog
    // of the wy1/wy2 delayed-apply latches). Steady-state these equal the live
    // register, so non-write rendering is unaffected.
    #[serde(default)]
    pub(in crate::ppu) scy_delayed: u8,
    #[serde(default = "wy2_disabled")]
    pub(in crate::ppu) scy_apply_cc: u64,
    #[serde(default)]
    pub(in crate::ppu) scy_pending: u8,
    #[serde(default)]
    pub(in crate::ppu) scx_delayed: u8,
    #[serde(default = "wy2_disabled")]
    pub(in crate::ppu) scx_apply_cc: u64,
    #[serde(default)]
    pub(in crate::ppu) scx_pending: u8,
    // Exact-cc f1-discard SCX latch. On hardware the SCX change becomes visible at
    // `cc + 2*cgb` (before the SCX write itself resolves), so on CGB the new SCX is only
    // visible to the f1 fine-scroll discard 2 PPU cc after the write's cc. The
    // f1 loop reads SCX as-of its dot's exact abs_cc through this latch instead
    // of the immediate register, so a mid-discard SCX write lands on the
    // correct f1 iteration without shifting the steady-state discard timing.
    #[serde(default)]
    pub(in crate::ppu) scx_prev_f1: u8, // value in effect before the pending write
    #[serde(default = "wy2_disabled")]
    pub(in crate::ppu) scx_f1_apply_cc: u64, // abs_cc at which scx_pending becomes visible to f1
    #[serde(default)]
    pub(in crate::ppu) scx_f1_new: u8,
    // sub-cc column lever. A mid-mode-3 SCX write applies to the BG
    // column fetcher at `write_cc + 2*cgb` (on hardware the SCX change becomes visible at
    // `cc + 2*cgb`, before the SCX write resolves), evaluated against the cc at which a fetched tile's pixels are
    // PLOTTED (the fetcher leads the display by the FIFO depth). A tile whose
    // first plotted pixel is at/before the apply cc keeps the OLD scx; after it
    // uses NEW. These persist for the whole line (unlike scx_apply_cc which
    // resets on apply) so the fetcher can choose per-tile. `subcc_scx_apply_cc`
    // == disabled when no write is pending this line.
    #[serde(default = "wy2_disabled")]
    pub(in crate::ppu) subcc_scx_apply_cc: u64,
    #[serde(default)]
    pub(in crate::ppu) subcc_scx_old: u8,
    #[serde(default)]
    pub(in crate::ppu) subcc_scx_new: u8,
    // Armed by a mid-mode-3 SCX write while a BG tile is in flight (column
    // already committed under the OLD scx, not yet pushed). The next PushToFifo
    // re-keys that single tile to the NEW scx column iff it plots after the
    // apply cc, then disarms. Exactly one tile per write can straddle.
    #[serde(default)]
    pub(in crate::ppu) subcc_rekey_armed: bool,
    // First-tile (f1) prologue straddle: a mid-mode-3 SCX write that lands while
    // x==0 (the discard prologue, before any pixel has plotted) but AFTER the
    // first displayed tile has already been queued into the FIFO. The tile still
    // in flight (the 2nd displayed tile) latched its column under the OLD scx one
    // dot before the write; on hardware it plots well after the write so
    // its column comes from the NEW scx. The first queued tile (already pushed)
    // keeps the OLD scx. Re-keys exactly that one in-flight tile on its next
    // PushToFifo. DMG single-speed only (the CGB/DS prologue uses the
    // m3_arm_scx_full re-fetch path above).
    #[serde(default)]
    pub(in crate::ppu) prologue_rekey_armed: bool,
    // First-line (LY=0) sprite-shifted straddle (CGB SS, gap==1): on the line
    // after LCD-enable the fetcher runs a different warmup/dispatch phase, so a
    // left-edge sprite-fetch dot shifts the OLD->NEW scx boundary one tile later
    // than on LY>=1. The per-dot fetcher already read the NEW scx for that tile
    // (one tile too early), so when set the next PushToFifo reverts the 8
    // just-pushed entries back to the OLD-scx column.
    #[serde(default)]
    pub(in crate::ppu) subcc_revert_next_old: bool,
    // Two-tile DS straddle (CGB double-speed, low-X sprite): at DS a mid-mode-3
    // SCX write straddles TWO display tiles because the sprite-fetch dot shifts
    // the BG fetch phase one tile while the DS FIFO carries an extra tile. Both
    // straddle tiles must render under the OLD scx at their plot column shifted
    // back one tile (xpos-8). The first (in-flight) tile is rekeyed at the DS
    // flip; this flag rekeys the SECOND tile (fetched NEXT under the NEW scx) on
    // its push back to the OLD-scx column at its own xpos-8.
    #[serde(default)]
    pub(in crate::ppu) ds_straddle_next_old: bool,
    // abs_cc at which the most recent BG TileNumber latch happened (the fetch
    // cc of the tile currently in flight). The armed straddle tile's column was
    // committed at this cc; the rekey compares it to the write's apply cc.
    #[serde(default)]
    pub(in crate::ppu) subcc_last_tn_cc: u64,
    // First line after enable: the SCX value the fine-scroll discard prefix
    // actually samples (the mode-3-start fine-scroll phase reads SCX once at the M3-start
    // dot). A mid-discard SCX write (write_cc + 2*cgb visible) only counts if
    // it lands at/before that sample dot, which sits `prev_scx % 8` dots past
    // M3-arm. `compute_m3_length_win` uses this override (when set) instead of
    // the live register so the late-enable + SCX mode-0 time matches hardware.
    #[serde(default)]
    pub(in crate::ppu) first_line_scx_override: Option<u8>,
    #[serde(default)]
    pub(in crate::ppu) line_cycle: u32,
    #[serde(default)]
    pub(in crate::ppu) internal_ly_val: u8,
    #[serde(default)]
    pub(in crate::ppu) sched_lycirq: u64,
    #[serde(default)]
    pub(in crate::ppu) sched_m1irq: u64,
    #[serde(default)]
    pub(in crate::ppu) sched_m2irq: u64,
    #[serde(default)]
    pub(in crate::ppu) sched_m0irq: u64,
    #[serde(default)]
    pub(in crate::ppu) sched_oneshot_statirq: u64,
    // Remaining mode-3 fast dots: while > 0 (and the state is still
    // PixelTransfer), `step` skips its preamble — the STAT dispatch (no
    // event can come due inside the budget), the l154 window check (LY
    // bounded), the pending LY-write take and LYC=LY rewrite (both only
    // change on CPU writes, which invalidate the budget via
    // `invalidate_fast_span`), and the window-Y latch (its checkpoints lie
    // outside mode-3 ticks). Recomputed lazily from `sched_min` slack;
    // zeroed by `stat_sched_touched` and by any >= 0xFE00 bus write.
    // Not serialized: deserializes to 0 = full preamble.
    #[serde(skip)]
    #[serde(default)]
    pub(in crate::ppu) fast_dots_left: u32,
    // Post-invalidation hold: after any fast-span invalidation (a >=0xFE00
    // bus write or a delayed LCDC commit), run the FULL preamble for this
    // many further dots before the budget may recompute — the one-shot
    // mid-mode-3 write-detection checks (WX/WY/window-enable m0 adjustments)
    // must observe the change on the dot it lands, and delayed LCDC commits
    // land a few dots after the write that invalidated.
    #[serde(skip)]
    #[serde(default)]
    pub(in crate::ppu) fast_hold: u8,
    // Cached lower bound of the 9 scheduled STAT/apply event slots consumed by
    // `dispatch_stat_events`, so the per-dot fast bail is a single compare
    // instead of a 9-way min. Invariant: always <= the true minimum (0 =
    // "dirty, recompute"). Refreshed at the end of every slow dispatch and
    // zeroed by `stat_sched_touched()` at every site that can LOWER a slot.
    // Deliberately NOT serialized: deserializes to 0 = dirty = safe.
    #[serde(skip)]
    #[serde(default)]
    pub(in crate::ppu) sched_min: u64,
    // Set when the m1 event flagged VBlank this frame so the render-machine
    // ly143->144 transition does NOT re-flag it (hardware has a single VBlank
    // source: the m1 event). Cleared when the m1 event re-arms for the next frame.
    #[serde(default)]
    pub(in crate::ppu) m1_vblank_fired: bool,
    // DMG "line 154" STAT-write glitch (gbmicrotest stat_write_glitch_l154_d):
    // when the CPU writes FF41 (STAT) at the frame-wrap boundary (the LY 153->0
    // exit of VBlank, into the first line of the new frame) a hardware glitch on
    // the shared VBlank/STAT interrupt path clears the still-pending VBlank IF
    // bit (bit 0). Real DMG-CPU-08 reads IF=0xE0 there; a naive sticky-bit model
    // (like the pre-fix renderer) reads 0xE1. Armed at the VBlank->OAM
    // frame-wrap, disarmed a few dots into line 0/1 so a normal mid-frame STAT
    // write never clears a legitimately-pending VBlank IRQ. DMG-only.
    #[serde(default)]
    pub(in crate::ppu) l154_vblank_glitch_window: bool,
    #[serde(default)]
    pub(in crate::ppu) lyc_irq: stat_irq::LycIrq,
    #[serde(default)]
    pub(in crate::ppu) mstat_irq: stat_irq::MStatIrq,
    #[serde(default)]
    pub(in crate::ppu) stat_reg_committed: u8,

    // DMG palette registers delayed by one dot. A BGP/OBP write during mode 3
    // is resolved by the CPU before the four PPU dots of the write M-cycle are
    // stepped, but on hardware the new palette only affects the pixel one dot
    // after the write lands. The renderer resolves palettes at pixel shift-out
    // from these delayed copies; each are refreshed to the live register at the
    // end of every dot, yielding the one-dot apply latency.
    #[serde(default)]
    pub(in crate::ppu) bgp_delayed: u8,
    #[serde(default)]
    pub(in crate::ppu) obp0_delayed: u8,
    #[serde(default)]
    pub(in crate::ppu) obp1_delayed: u8,
    // DMG mid-mode-3 BGP sub-M-cycle phase hold. `on_bgp_write` fires at the write
    // M-cycle START, but the store's bus-write lands a phase-dependent number of dots
    // later; for a write whose `master_cc % 4` is non-zero the new value must not reach
    // `bgp_delayed` until `bgp_defer_countdown` more dot-refreshes have passed. The old
    // (pre-write) value is held in `bgp_defer_hold` meanwhile. Phase-0 writes set
    // countdown 0 and are byte-identical to the plain one-dot latch. See `on_bgp_write`.
    #[serde(default)]
    pub(in crate::ppu) bgp_defer_hold: u8,
    #[serde(default)]
    pub(in crate::ppu) bgp_defer_countdown: u8,

    pub(in crate::ppu) out: FrameOut,
    pub(in crate::ppu) lcdc: LcdcState,
    pub(in crate::ppu) wg: BusGlitch,
    // Per-line LCDC.0 (BG-enable) plot history for the per-pixel renderer.
    // The per-dot draw is flushed in bursts (the
    // mode-0 time flush draws all remaining FIFO pixels at one cc), so a live
    // `self.lcdc.reg & 1` read applies the final BG-enable to every flushed column
    // and a mid-mode-3 LCDC.0 toggle (BG off then on within pixel transfer) is
    // lost. Hardware instead reads `lcdc & bg_enable` live as the fetcher walks
    // tiles, so each plotted column sees the BG-enable bit in effect at its own
    // plot position. We record the BG-enable changes during this line's mode 3
    // as (boundary_col, bgen) entries — columns >= boundary_col see the new bit.
    // The first entry (boundary_col == 0) seeds the value at mode-3 start.
    // Empty/single-entry => no mid-line toggle => identical to the live read.
    #[serde(default)]
    pub(in crate::ppu) bgen_history: Vec<(u64, bool)>,
    // DMG per-dot OBJ-enable (LCDC.1) history. Hardware gates each sprite pixel
    // on OBJ-enable AT THAT PIXEL'S pop dot (hardware's pixel-render step
    // reads LCDC.1 live per popped pixel), so a mid-mode-3 disable/enable
    // covers an exact dot span — which maps to columns THROUGH the stall
    // schedule (a column popping late because of a sprite stall resolves the
    // gate at its actual pop dot). Entries are (apply_tick, enabled); pops at
    // ticks >= apply_tick see the new bit. Seeded at mode-3 entry (tick 0);
    // single-entry == no toggle == the live-read fast path.
    #[serde(default)]
    pub(in crate::ppu) objen_history: Vec<(u128, bool)>,
    // DMG per-dot OBJ-size (LCDC.2) history: (apply_tick, large). The sprite
    // fetcher samples LCDC.2 independently at each tile-data byte's own fetch
    // dot (hardware recomputes the object line address for the low AND high
    // byte), so a mid-fetch toggle splits the row addressing between bytes.
    // Seeded at mode-3 entry (apply_tick 0).
    #[serde(default)]
    pub(in crate::ppu) objsize_dot_history: Vec<(u128, bool)>,
    // Per-sprite live fetch records, parallel to `sprites_on_line` (see
    // `SpriteFetchRec`). Rebuilt (all Pending) at mode-3 entry.
    #[serde(default)]
    pub(in crate::ppu) sprite_fetch_recs: Vec<SpriteFetchRec>,
    // Per-line BGP / OBP0 / OBP1 plot history for the per-pixel renderer, mirroring
    // `bgen_history`. A mid-mode-3 write to BGP (FF47) / OBP0 (FF48) / OBP1 (FF49)
    // takes effect at the exact pixel being drawn `MID_M3_PAL_LATENCY` dots later
    // (the DMG palette-RAM pipeline latency). The per-dot draw is flushed in
    // bursts at mode-0 time, so a single
    // live `mmio.read(BGP)` snapshot would apply the final value to every flushed
    // column. We record each mid-mode-3 change as a (boundary_col, value) entry —
    // columns >= boundary_col see the new value — and resolve per displayed column.
    // The first entry (boundary 0) seeds the value at mode-3 start; with no mid-line
    // write the history is a single seed and resolves to that value for the whole
    // line (identical to the previous `bgp_delayed` steady-state read).
    #[serde(default)]
    pub(in crate::ppu) bgp_history: Vec<(u64, u8)>,
    #[serde(default)]
    pub(in crate::ppu) obp0_history: Vec<(u64, u8)>,
    #[serde(default)]
    pub(in crate::ppu) obp1_history: Vec<(u64, u8)>,
    // DMG dot-keyed OBP histories: (apply_tick, value). The OBP register is
    // sampled as each sprite pixel pops out of the OAM FIFO, so the correct
    // key is the pixel's POP DOT — the column mapping breaks whenever a sprite
    // stall delays the pops (e.g. a pixel at column 8 popping at dot 111 must see
    // a write that applied at dot 105, even though the write's column boundary was
    // 9). On stall-free lines this
    // is exactly equivalent to the column model (columns pop 1/dot). It also
    // subsumes the old off-left-edge column-0 forcing: left-clipped sprites'
    // pixels pop early, before any mid-mode-3 write applies.
    #[serde(default)]
    pub(in crate::ppu) obp0_dot_history: Vec<(u128, u8)>,
    #[serde(default)]
    pub(in crate::ppu) obp1_dot_history: Vec<(u128, u8)>,
    // Dot-keyed BGP history for the CGB / DMG-compat BG color path. A mid-mode-3
    // BGP write applies at `ticks + latency` (a DOT), and each BG pixel is colored
    // at its own pop dot — which is delayed by any sprite-fetch stall between the
    // write and that column. Sampling by pop-dot (not display column) makes the
    // stall absorption exact for both the on-stall write and a pre-stall write
    // whose target column is pushed past the stall. The column-keyed `bgp_history`
    // remains the DMG-hardware path.
    #[serde(default)]
    pub(in crate::ppu) bgp_dot_history: Vec<(u128, u8)>,
    // DMG mid-mode-3 BGP-write "glitch". On real DMG hardware a
    // CPU write to BGP (FF47) during mode 3 can collide with the PPU's palette read for
    // the pixel being pushed at that dot: the register is mid-transition and the pixel is
    // looked up through the bitwise OR of the old and new BGP bytes (`old | new`) rather
    // than either settled value. When old and new differ in a color slot the OR sets
    // extra shade bits, darkening that one pixel — the "black spike" bracketing each
    // mid-line palette band (e.g. old=$41,new=$42 -> $43, so a color-0 pixel reads shade
    // 3 / black; when old|new==old the spike is invisible). It is a TWO-WRITE collision
    // (see `bgp_writes`), so a lone or loosely-spaced write shows no spike. CGB uses
    // true-color palette RAM and shows no such collapse, so this is DMG-gated. The two
    // fields below drive it, both reset at mode-3 start:
    // Per-column BG color index (0-3) of the pixel drawn at each display column this
    // line, or -1 where a sprite won the mix / the column is undrawn. Recorded by the
    // per-dot DMG draw and read by `resolve_bgp_spikes` to re-map the glitched columns
    // through the OR palette at mode-3 end. 160 entries, reset each line.
    #[serde(default)]
    pub(in crate::ppu) line_bg_idx: Vec<i8>,
    // Every mid-mode-3 BGP write on the current line, as (abs_cc, display_col, old|new).
    // The DMG palette-latch glitch is a TWO-WRITE interaction: a write spikes its own
    // pixel only when it has a neighboring mid-mode-3 write within the tight SET/RESTORE
    // cadence (`BGP_SPIKE_CADENCE_CC`, ~12-dot pairs). A single write, or one loosely
    // spaced (one write per line, or 60-148 dots apart), does NOT collide and shows no
    // spike. The gate is
    // resolved at mode-3 end (all writes known) by `resolve_bgp_spikes`, which paints the
    // glitch straight into the framebuffer. Reset at mode-3 start.
    #[serde(default)]
    pub(in crate::ppu) bgp_writes: Vec<(u64, u8, u8)>,
    // Last mode-2 (OAM scan) BGP write (cc, value), carried across the mode-3-arm
    // bgp_writes clear and injected as a neighbor-only spike entry at mode-3 entry
    // (see on_bgp_write / the arm seed). None once consumed or if no mode-2 write.
    #[serde(default)]
    pub(in crate::ppu) bgp_mode2_pending: Option<(u64, u8)>,
    #[serde(default)]
    pub(in crate::ppu) cgb_color_conversion: ColorCorrection,
}

impl Default for Ppu {
    fn default() -> Self {
        Self::new()
    }
}

impl Ppu {
    pub fn new() -> Self {
        Ppu {
            fetcher: fetcher::Fetcher::new(),
            disabled: true,
            state: State::OAMSearch,
            ticks: 0,
            x: 0,
            sprites_on_line: Vec::new(),
            current_oam_sprite_index: 0,
            oam_reader: OamReader::default(),
            prev_dma_writing: false,
            oam_reader_seeded: false,
            scan_slot_large: [false; OAM_SPRITE_COUNT],
            next_sprite_fetch_index: 0,
            m3_sprite_prev_tile: SPRITE_TILE_NONE,
            m3_last_sprite_commit_tick: 0,
            sprite_fetch_stall: 0,
            pixel_transfer_warmup: 0,
            fetcher_cadence_tick: 0,
            win_y_pos: 0xFF,
            win_draw_start: false,
            win_draw_started_at_x0: false,
            win_draw_started: false,
            window_y_triggered: false,
            win_start_dot: None,
            predicted_win_start_dot: None,
            win_wx_penalty_resolved: false,
            win_wx_enable_resolved: false,
            window_started_this_line: false,
            win_weoff_deferred_tail: false,
            first_line_after_enable: false,
            display_enable_inactive_until: 0,
            line_153_ly_zeroed: false,
            m3_pixels_discarded: 0,
            m3_discard_target: -1,
            m3_arm_scx_full: -1,
            m3_arm_dot: 0,
            win_fetch_anchor: None,
            win_first_tile_chop: 0,
            win_being_fetched: false,
            insert_bg_pixel: false,
            we_dot_hist: [true; 5],
            we_glitch_tile_starts: [None; 2],
            we_glitch_discard_insert: false,
            we_insert_suppressed: false,
            win_kill_tap_late: false,
            win_wx0_delayed: false,
            dmg_wx_trigger_pending: None,
            m3_arm_scx: 0,
            m3_scheduled_wx: 0,
            m3_scheduled_win: false,
            scan_obj_size_large: false,
            objsize_apply_cc: wy2_disabled(),
            objsize_prev_large: false,
            objsize_new_large: false,
            scheduled_mode0_dot: None,
            m0_time_master: None,
            speed: SpeedPhase::default(),
            cgbp_block_start_cc: None,
            mode0_reported_this_line: false,
            line_rendered_this_line: false,
            wxa6_lineend_applied: false,
            bgen_history: Vec::new(),
            objen_history: Vec::new(),
            objsize_dot_history: Vec::new(),
            sprite_fetch_recs: Vec::new(),
            obp0_dot_history: Vec::new(),
            obp1_dot_history: Vec::new(),
            bgp_dot_history: Vec::new(),
            bgp_history: Vec::new(),
            obp0_history: Vec::new(),
            obp1_history: Vec::new(),
            line_bg_idx: vec![-1; 160],
            bgp_writes: Vec::new(),
            bgp_mode2_pending: None,
            abs_cc: 0,
            p_now: pnow_disabled(),
            wy2: 0,
            wy2_apply_cc: wy2_disabled(),
            wy2_pending: 0,
            wy1: 0xFF,
            wy1_apply_cc: wy2_disabled(),
            wy_recheck_cc: wy2_disabled(),
            wy1_pending: 0,
            scy_delayed: 0,
            scy_apply_cc: wy2_disabled(),
            scy_pending: 0,
            scx_delayed: 0,
            scx_apply_cc: wy2_disabled(),
            scx_pending: 0,
            scx_prev_f1: 0,
            scx_f1_apply_cc: wy2_disabled(),
            scx_f1_new: 0,
            subcc_scx_apply_cc: wy2_disabled(),
            subcc_scx_old: 0,
            subcc_scx_new: 0,
            subcc_rekey_armed: false,
            prologue_rekey_armed: false,
            subcc_revert_next_old: false,
            ds_straddle_next_old: false,
            subcc_last_tn_cc: 0,
            first_line_scx_override: None,
            line_cycle: 0,
            internal_ly_val: 0,
            sched_lycirq: stat_irq::DISABLED_TIME,
            sched_m1irq: stat_irq::DISABLED_TIME,
            sched_m2irq: stat_irq::DISABLED_TIME,
            sched_m0irq: stat_irq::DISABLED_TIME,
            sched_oneshot_statirq: stat_irq::DISABLED_TIME,
            sched_min: 0,
            fast_dots_left: 0,
            fast_hold: 0,
            m1_vblank_fired: false,
            l154_vblank_glitch_window: false,
            lyc_irq: stat_irq::LycIrq::default(),
            mstat_irq: stat_irq::MStatIrq::default(),
            stat_reg_committed: 0,
            bgp_delayed: 0,
            obp0_delayed: 0,
            obp1_delayed: 0,
            bgp_defer_hold: 0,
            bgp_defer_countdown: 0,
            out: FrameOut::default(),
            lcdc: LcdcState::default(),
            wg: BusGlitch::default(),
            cgb_color_conversion: ColorCorrection::Lcd,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn set_cgb_color_conversion(&mut self, conversion: ColorCorrection) {
        self.cgb_color_conversion = conversion;
    }

    pub(crate) fn cgb_color_conversion(&self) -> ColorCorrection {
        self.cgb_color_conversion
    }

    pub(crate) fn sync_lcdc_from_mmio(&mut self, mmio: &mmio::Mmio) {
        self.set_lcdc_visible(mmio.read(LCD_CONTROL), mmio.is_cgb_features_enabled(), mmio.is_double_speed_mode());
        self.lcdc.pending_lcdc_events.clear();
    }

    /// Seed the post-boot PPU frame phase for `skip_bios`. The real boot ROM
    /// leaves the LCD enabled and the PPU deep into a frame; the hardware initial
    /// state sets `video cycles = 144*456 + 164` (CGB) /
    /// `153*456 + 396` (DMG) — i.e. the game starts in VBlank at LY=144 (CGB) or
    /// LY=153 (DMG), NOT at a fresh LY=0 OAM search. Mirror that here so the very
    /// first instruction's LY/STAT reads (display_startstate tests) match real
    /// hardware. Must run after LCDC=0x91 and `sync_lcdc_from_mmio`.
    pub(crate) fn set_post_bios_state(&mut self, mmio: &mut mmio::Mmio, dmg0: bool) {
        // LCD must be on for this to apply (skip_bios writes LCDC=0x91 first).
        if !self.lcdc_has(LCDCFlags::DisplayEnable) {
            return;
        }
        let cgb = mmio.is_cgb_features_enabled();
        // Post-boot LCD phase (dots into the frame): cgb ? 144*456+164+agb*4 :
        // 153*456+396. AGB's post-boot video phase leads CGB's by 4 dots.
        let agb_off = if mmio.is_agb() { 4 } else { 0 };
        // The DMG0 boot ROM hands off ~9 scanlines earlier in the frame than
        // DMG-ABC/MGB: mooneye boot_hwio-dmg0 reads FF44/FF41 at fixed offsets
        // into its unrolled compare loop (FF41 at ~4528 T, FF44 at ~4712 T past
        // the 0x150 handoff) and asserts LY=0x01 with STAT mode 3, whereas
        // boot_hwio-dmgABCmgb asserts LY=0x0A / mode 0 at the same offsets. Both
        // hand off in VBlank; the live PPU then advances into the next frame so
        // the loop samples line 1 (dmg0) vs line 10 (dmgABC). The DMG0 video cycles
        // that lands the FF41 read mid-mode-3 on line 1 and the FF44 read still on
        // line 1 is 145*456+198; the wide (~170-dot) window around it makes the
        // exact CPU read sub-phase irrelevant. Non-DMG0 keeps the hardware 153*456+396.
        let video_cycles: u32 = if cgb {
            144 * stat_irq::LCD_CYCLES_PER_LINE + 164 + agb_off
        } else if dmg0 {
            145 * stat_irq::LCD_CYCLES_PER_LINE + 198
        } else {
            153 * stat_irq::LCD_CYCLES_PER_LINE + 396
        };
        let ly = (video_cycles / stat_irq::LCD_CYCLES_PER_LINE) as u8;
        let line_cycle = video_cycles % stat_irq::LCD_CYCLES_PER_LINE;

        self.disabled = false;
        self.internal_ly_val = ly;
        self.line_cycle = line_cycle;
        self.ticks = line_cycle as u128;
        // Both LY=144 (CGB) and LY=153 (DMG) land in VBlank.
        self.state = State::VBlank;
        self.first_line_after_enable = false;

        // On line 153 the LY *register* flips to 0 early (at dot
        // LINE_153_LY_ZERO_DOT), well before the line itself ends. The DMG
        // post-boot phase (LY=153, line cycle=396) is past that dot, so the
        // register already reads 0 and the LYC=0 coincidence has already armed.
        // Mirror that transient state so the first FF44/FF41 read matches.
        let line_153_zeroed =
            ly == (stat_irq::LCD_LINES_PER_FRAME as u8 - 1) && line_cycle >= LINE_153_LY_ZERO_DOT as u32;
        self.line_153_ly_zeroed = line_153_zeroed;
        let ly_reg = if line_153_zeroed { 0 } else { ly };

        // Anchor the dot-clock origin: abs_cc = 0 at the post-boot instant so
        // ly_counter().time mirrors the hardware LY-counter reset to (video cycles, cc)
        // with cc as the origin. p_now = master_cc keeps abs_cc = master_cc -
        // p_now consistent; the first step() folds abs_cc -> 1 and advances
        // line_cycle by one dot.
        self.abs_cc = 0;
        self.p_now = mmio.master_cc();
        self.speed.lytime_no_plus1 = false;
        self.speed.ssds_mode3_ly_advance = false;

        // Publish LY and the VBlank STAT mode (FF41 mode bits = 1).
        mmio.write_ly_from_ppu(ly_reg);
        Self::set_lcd_status_mode(mmio, 1);
        // LYC=LY coincidence flag against the *register* LY (0 on the line-153
        // transient). LYC defaults to 0, so CGB (LY=144) clears it and DMG
        // (LY register 0) sets it.
        let lyc = mmio.read(LYC);
        if lyc == ly_reg {
            mmio.write_lcd_status_from_ppu(mmio.read(LCD_STATUS) | (1 << 2));
        } else {
            mmio.write_lcd_status_from_ppu(mmio.read(LCD_STATUS) & !(1 << 2));
        }

        // Seed the event-scheduled STAT/LYC IRQ clocks for the running frame.
        self.scy_delayed = mmio.read(SCY);
        self.scy_apply_cc = wy2_disabled();
        self.scx_delayed = mmio.read(SCX);
        self.scx_apply_cc = wy2_disabled();
        self.wy2 = mmio.read(WY);
        self.wy2_apply_cc = wy2_disabled();
        self.wy1 = mmio.read(WY);
        self.wy1_apply_cc = wy2_disabled();
        self.stat_reg_committed = mmio.read(LCD_STATUS);
        // The LYC/STAT interrupt machinery follows the LCD-controller silicon,
        // which is CGB whenever the hardware is CGB-like — even for a DMG cart in
        // DMG-compatibility mode (hardware gates the LYC IRQ on the console-is-CGB signal, which
        // is the CGB-console signal, not cart CGB-feature support). Use hardware
        // is-CGB, not `is_cgb_features_enabled()`.
        self.lyc_irq.set_cgb(mmio.is_cgb());
        self.lyc_irq.seed(mmio.read(LCD_STATUS), lyc);
        self.mstat_irq.seed(mmio.read(LCD_STATUS), lyc);
        self.reschedule_all_stat_events(mmio);
        self.sched_m0irq = stat_irq::DISABLED_TIME;
        self.sched_oneshot_statirq = stat_irq::DISABLED_TIME;
    }

    /// True while the renderer is in pixel transfer (mode 3) — consumer: the
    /// bus's sticky mid-m3 LCDC-writer marker (CGB halt-exit stall scoping).
    pub(crate) fn in_pixel_transfer(&self) -> bool {
        self.state == State::PixelTransfer
    }

    // ---- Event-scheduled STAT IRQ model (hardware model) ----










    /// Register a NON-mode-3 (OAM/HBlank) DS->SS speed switch for the LY-read
    /// sub-dot phase accumulator. The hardware speed-change handling applies a
    /// half-dot re-anchor on every DS->SS switch; the whole-dot DS->SS bridge folds
    /// the integer part, and mode-3 switches carry their residual through the
    /// `stat_phase_carry` (p_now) path. This tracks the OAM/HBlank switches that have
    /// no such carry: their accumulated parity determines whether a post-STOP boundary
    /// LY read lands one sub-dot early (anticipated) or late (stale).
    pub(crate) fn bump_dsss_ly_phase(&mut self) {
        self.speed.dsss_ly_phase_count += 1;
    }
    /// Register any DS->SS switch (including mode-3) for the total-parity accumulator.
    pub(crate) fn bump_dsss_ly_total(&mut self) {
        self.speed.dsss_ly_total_count += 1;
    }




















    // Resolve a dot-keyed DMG palette history at pop dot `tick`.
    pub(in crate::ppu) fn pal_at_tick(hist: &[(u128, u8)], tick: u128, fallback: u8) -> u8 {
        if hist.len() <= 1 {
            return hist.last().map(|&(_, v)| v).unwrap_or(fallback);
        }
        let mut val = hist[0].1;
        for &(apply_tick, v) in hist.iter() {
            if apply_tick <= tick {
                val = v;
            } else {
                break;
            }
        }
        val
    }


    // Resolve a column-keyed DMG palette history at display column `sx`: the last
    // entry whose boundary column is <= `sx` wins. Single-seed history (the common
    // no-mid-write case) always returns the seed. Mirrors `bgen_at`.
    pub(in crate::ppu) fn pal_at(hist: &[(u64, u8)], sx: u8, fallback: u8) -> u8 {
        Self::pal_at_col(hist, sx as u64, fallback)
    }

    // As `pal_at` but with an arbitrary sample column (the DMG sprite OBP path may
    // force column 0 for off-left-edge sprites rather than the pixel's own column).
    fn pal_at_col(hist: &[(u64, u8)], sample_col: u64, fallback: u8) -> u8 {
        if hist.len() <= 1 {
            return hist.last().map(|&(_, v)| v).unwrap_or(fallback);
        }
        let mut val = hist[0].1;
        for &(boundary_col, v) in hist.iter() {
            if boundary_col <= sample_col {
                val = v;
            } else {
                break;
            }
        }
        val
    }


    pub fn step(&mut self, mmio: &mut mmio::Mmio) {
        if self.disabled {
            if mmio.read(LCD_CONTROL)&(LCDCFlags::DisplayEnable as u8) != 0 {
                self.enter_lcd_enabled(mmio);
            } else {
                return;
            }
        } else if self.lcdc.reg&(LCDCFlags::DisplayEnable as u8) == 0 {
            self.enter_lcd_disabled(mmio);
            return;
        }

        // Mode-3 preamble fast path: while the budget holds (see
        // `fast_dots_left`), every piece gated on `!fast` below is a proven
        // no-op for this dot.
        let fast = if matches!(self.state, State::PixelTransfer) {
            if self.fast_hold > 0 {
                self.fast_hold -= 1;
                self.fast_dots_left = 0;
            } else if self.fast_dots_left == 0 {
                self.fast_dots_left = self.mode3_fast_budget(mmio);
            }
            if self.fast_dots_left > 0 {
                self.fast_dots_left -= 1;
                true
            } else {
                false
            }
        } else {
            false
        };

        // Fire any scheduled STAT IRQ events that have come due at this dot,
        // then advance the clean event clock by one dot (phase-locked with the
        // renderer's 456-dot line).
        if !fast {
            self.dispatch_stat_events(mmio);
        }
        // Fold the PPU dot-clock onto the master cc. `p_now` is the LCD-enable
        // anchor such that the PPU machine-cycle clock is `master_cc - p_now`
        // (the hardware PPU-clock base); the master cc advances `1<<ds` per render dot
        // within a speed epoch, so the derived clock advances exactly as the old
        // accumulator did. `p_now` is seeded at enable and re-based on the speed
        // change / STOP bridge (where the master cc and render-dot counts diverge).
        self.abs_cc = mmio.master_cc().wrapping_sub(self.p_now);
        self.line_cycle += 1;
        if self.line_cycle >= stat_irq::LCD_CYCLES_PER_LINE {
            self.line_cycle = 0;
            self.internal_ly_val += 1;
            if self.internal_ly_val as u32 >= stat_irq::LCD_LINES_PER_FRAME {
                self.internal_ly_val = 0;
            }
        }
        // Disarm the "line 154" STAT-write VBlank-IF glitch window once the new
        // frame has advanced a few dots past the LY 0->1 boundary. The glitch is
        // observed only for a FF41 write straddling that boundary (gbmicrotest
        // stat_write_glitch_l154_d: internal_ly==1, line_cycle 0); keeping the
        // window this narrow guarantees a normal mid-frame STAT write never
        // clears a legitimately-pending VBlank IRQ.
        if !fast
            && self.l154_vblank_glitch_window
            && (self.internal_ly_val > 1
                || (self.internal_ly_val == 1 && self.line_cycle > 4))
        {
            self.l154_vblank_glitch_window = false;
        }

        // Drive the lazy OAM sprite snapshot:
        // fire `change(cc)` on OAM-DMA window edges (source toggle) and on CPU
        // OAM writes, mirroring the hardware OAM-DMA start / OAM-DMA end / OAM change events.
        self.process_oam_reader_events(mmio);

        // LYC=LY compare uses an "effective LY" that anticipates the
        // next-line value in the last 2 dots of any line (matches the hardware
        // `the LYC-compare-LY calc` `time-to-next-LY <= 2` threshold). Line 153's earlier
        // ly=0 transient is handled separately in Phase D by writing FF44
        // directly, so this anticipation only fires on lines 0..=152.
        if !fast {
            let effective_ly = self.effective_ly_for_lyc_compare(mmio);
            if mmio.ppu_io_reg(LYC) == effective_ly {
                mmio.write_lcd_status_from_ppu(mmio.lcd_status_reg() | (1 << 2)); // Set the LYC=LY flag
            } else {
                mmio.write_lcd_status_from_ppu(mmio.lcd_status_reg() & !(1 << 2)); // Clear the LYC=LY flag
            }
        }

        // hardware-style window-Y (window-enable master) latch. The trigger is sticky for
        // the frame and is evaluated at three points: ly0 mode-2 start
        // (wy==0), and near each line's end at the prior-to-LY-inc (ly==wy)
        // and after-LY-inc (ly+1==wy) cycles. This catches late WY writes that
        // land in the small window between these checks.
        if !fast {
            self.update_window_y_latch(mmio);
        }

        match self.state {
            State::OAMSearch => self.step_mode2(mmio),
            State::PixelTransfer => self.step_mode3_dot(mmio, fast),
            State::HBlank => {
                if self.step_hblank(mmio) {
                    return;
                }
            },
            State::VBlank => {
                if self.step_vblank(mmio) {
                    return;
                }
            },
        }
        // Latch the live DMG palette registers for use one dot from now. A
        // mid-mode-3 write lands before this dot's pixel push (the CPU resolves
        // the write before stepping the M-cycle's four dots), so resolving from
        // last dot's snapshot gives the one-dot apply latency hardware shows.
        // A late-sub-M-cycle-phase write (`on_bgp_write`) holds the old value for
        // `bgp_defer_countdown` more dots before the live register is picked up.
        if self.bgp_defer_countdown > 0 {
            self.bgp_defer_countdown -= 1;
            self.bgp_delayed = self.bgp_defer_hold;
        } else {
            self.bgp_delayed = mmio.ppu_io_reg(BGP);
        }
        self.obp0_delayed = mmio.ppu_io_reg(OBP0);
        self.obp1_delayed = mmio.ppu_io_reg(OBP1);
        self.ticks += 1;
    }

    /// Push the BG fetcher's current VRAM data-bus address to the bus for the
    /// OAM-DMA-source conflict model. Called once per dot after `step`. The lock is
    /// active only while the PPU is in PixelTransfer (mode 3) and the LCD is on —
    /// the only window in which the fetcher drives VRAM. Outside it a VRAM-source
    /// OAM-DMA read sees true VRAM (the clean HBlank/mode-0 identity window).
    pub(crate) fn update_dma_fetcher_bus(&self, mmio: &mut mmio::Mmio) {
        let lcd_on = !self.disabled && self.lcdc_has(LCDCFlags::DisplayEnable);
        let locked = lcd_on && self.state == State::PixelTransfer;
        let (addr, bank) = self.fetcher.last_vram_bus();
        mmio.set_fetcher_vram_bus(addr, bank, locked);

        // DMG mode-2 fetcher-prefetch onset (see `Mmio::set_dmg_prefetch_bus`). On
        // DMG the BG fetcher's first tile-NUMBER fetch begins one M-cycle (4 dots)
        // before the mode-3 lock, so a VRAM-source OAM-DMA M-cycle in the last
        // mode-2 M-cycle already conflicts on the first tilemap address. Publish
        // that predicted address for the 4-dot window preceding the normal-line
        // mode-3 arm. CGB is unaffected (its AND lock at mode-3 entry already
        // byte-matches its dumps). Skipped on the first line after enable (no
        // mode-2 phase / different arm geometry).
        let prefetch = lcd_on
            && !mmio.is_cgb_features_enabled()
            && self.state == State::OAMSearch
            && !self.first_line_after_enable
            && self.ticks + 4 >= DMG_PIXEL_TRANSFER_ARM_DOT
            && self.ticks < DMG_PIXEL_TRANSFER_ARM_DOT;
        if prefetch {
            // First BG tile-number address for this line (display column 0):
            // tilemap_base + ((ly + scy)/8 % 32)*32 + (scx/8 % 32).
            let map_base: u16 = if self.lcdc_has(LCDCFlags::BGTileMapDisplaySelect) {
                0x9C00
            } else {
                0x9800
            };
            let scy = mmio.read(SCY) as u16;
            let scx = mmio.read(SCX) as u16;
            let bg_y = self.internal_ly_val as u16 + scy;
            let map_y = (bg_y / 8) & 0x1F;
            let map_x = (scx / 8) & 0x1F;
            let map_addr = map_base + (map_y * 32 + map_x);
            mmio.set_dmg_prefetch_bus(map_addr, true);
        } else {
            mmio.set_dmg_prefetch_bus(0, false);
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::registers;
    use crate::memory::Addressable;

    // The previous mode-2 STAT pretrigger unit tests were removed: the Mode-2
    // STAT IRQ is now delivered by the event-scheduled model (see `stat_irq` and
    // `dispatch_stat_events`), validated end-to-end by the hardware hwtest suite
    // (m2int/m2enable/miscmstatirq clusters), not the old per-dot pretrigger.

    #[test]
    fn cgb_lcdc_enabled_write_applies_tile_data_before_full_lcdc() {
        let mut mmio = mmio::Mmio::new();
        mmio.set_cgb_features_enabled(true);

        let old_lcdc = LCDCFlags::DisplayEnable as u8
            | LCDCFlags::SpriteDisplayEnable as u8
            | LCDCFlags::SpriteSize as u8
            | LCDCFlags::BGWindowTileDataSelect as u8;
        let new_lcdc = LCDCFlags::DisplayEnable as u8
            | LCDCFlags::BGDisplay as u8
            | LCDCFlags::SpriteDisplayEnable as u8
            | LCDCFlags::SpriteSize as u8
            | LCDCFlags::BGTileMapDisplaySelect as u8;

        mmio.write(LCD_CONTROL, old_lcdc);
        let mut ppu = Ppu::new();
        ppu.sync_lcdc_from_mmio(&mmio);
        ppu.handle_lcdc_write(new_lcdc, &mmio);

        ppu.step_lcdc_events(&mmio);
        assert_eq!(ppu.lcdc.reg & (LCDCFlags::BGWindowTileDataSelect as u8), 0);
        assert_eq!(ppu.lcdc.reg & (LCDCFlags::BGDisplay as u8), 0);
        assert_eq!(ppu.lcdc.reg & (LCDCFlags::BGTileMapDisplaySelect as u8), 0);
        assert!(ppu.lcdc.cgb_tile_index_is_tile_data);

        ppu.step_lcdc_events(&mmio);
        assert_eq!(ppu.lcdc.reg, new_lcdc);
        assert_ne!(ppu.lcdc.reg & (LCDCFlags::BGDisplay as u8), 0);
        assert!(!ppu.lcdc.cgb_tile_index_is_tile_data);
    }

    // The DMG "line 154" STAT-write VBlank-IF glitch is a PPU-line phenomenon:
    // line 154 only exists while the LCD is actively scanning, so the glitch
    // cannot occur with the LCD disabled. When a game turns the LCD off while the
    // glitch window is armed, the window freezes armed (the PPU stops before
    // disarming it); a later FF41 write must NOT clear a still-pending VBlank IF.
    // Without the `!disabled` gate this stranded Alfred Chicken (Europe) (Beta),
    // which HALTs on the post-boot pending VBlank immediately after LCD-off.
    #[test]
    fn stat_write_with_lcd_off_keeps_pending_vblank_if() {
        let mut mmio = mmio::Mmio::new(); // DMG: CGB features off, has the bug
        let mut ppu = Ppu::new();
        ppu.disabled = true;
        ppu.l154_vblank_glitch_window = true;
        mmio.request_interrupt(registers::InterruptFlag::VBlank);
        let vblank = registers::InterruptFlag::VBlank as u8;
        assert_ne!(mmio.snapshot_serial_read(registers::INTERRUPT_FLAG) & vblank, 0);
        mmio.write(LCD_STATUS, 0x40); // arms ff41_write_pending
        ppu.on_stat_register_write(&mut mmio);
        assert_ne!(
            mmio.snapshot_serial_read(registers::INTERRUPT_FLAG) & vblank,
            0,
            "STAT write with the LCD off must not clear the pending VBlank IF"
        );
    }

    // CGB panel-persistence decay (SameBoy `frame_repeat_countdown`). Re-homed
    // from the dropped first-party ROM `ppu/lcd_enable_repeat_decay`: the skipped
    // post-LCD-enable frame REPEATS the last displayed image while the panel's
    // drive countdown is still live, but a LONG LCD-off (past the countdown) has
    // decayed the panel to the "whiter than white" blank instead of repeating.
    // The modeled boundary is `panel_recently_driven`'s window, 144*456 + 3640/2
    // = 67484 cc at CGB single speed: an off of exactly that many cc from the last
    // driven VBlank-line anchor still repeats, one cc more blanks. Pins the modeled
    // boundary only (no hardware-portability claim).
    #[test]
    fn cgb_skipped_frame_repeats_within_window_then_blanks_after_long_off() {
        const CGB_WINDOW: u64 = 144 * 456 + 3640 / 2; // 67484 cc, single speed

        // The RGB byte the skipped frame presents for an off `diff` cc past the
        // last driven anchor. 0x12 == the retained image (repeat), 0xFF == blank.
        fn skipped_frame_fill(diff_from_anchor: u64) -> u8 {
            let mut mmio = mmio::Mmio::new();
            mmio.set_cgb_features_enabled(true);
            // Park the master clock well past the window so the anchor never
            // underflows, then anchor the last drive `diff` cc in the past.
            let now = 4 * CGB_WINDOW;
            mmio.bulk_advance_idle(now);

            let mut ppu = Ppu::new();
            ppu.out.color_fb_b.fill(0x12); // a distinctive, non-white retained image
            ppu.out.panel_holds_image = true;
            ppu.out.last_drive_cc = now - diff_from_anchor;
            // The skipped post-enable frame: LCD on, but fewer than two frames
            // displayed since the enable edge, so `blank_panel` takes the
            // persistence path rather than the normal framebuffer.
            ppu.disabled = false;
            ppu.out.frames_since_enable = 1;

            match ppu.get_frame(&mmio) {
                RenderedFrame::Color(fb) => {
                    let fill = fb[0];
                    assert!(fb.iter().all(|&b| b == fill), "frame is not uniformly filled");
                    fill
                }
                RenderedFrame::Monochrome(_) => panic!("CGB must render a color frame"),
            }
        }

        // Short off: exactly at the countdown boundary -> repeat the last image.
        assert_eq!(
            skipped_frame_fill(CGB_WINDOW),
            0x12,
            "in-window skip must repeat the last image"
        );
        // Long off: one cc past the countdown -> the panel has decayed to blank.
        assert_eq!(
            skipped_frame_fill(CGB_WINDOW + 1),
            0xFF,
            "past-window skip must blank (CGB white)"
        );
    }
}
