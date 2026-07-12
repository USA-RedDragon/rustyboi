//! `.rbr` — the video recording: a global palette plus a temporal-delta stream
//! of framebuffers.
//!
//! Container (all integers little-endian):
//! ```text
//!   magic   "RBR3"        4 bytes
//!   width   u16           height u16
//!   flags   u8            bit0: index width (0 = u8, 1 = u16)
//!                         bit1: raw payloads packed 4px/byte (palette <= 4)
//!   fps_num u32           fps_den u32     (GB rate 4194304 / 70224)
//!   frames  u32
//!   pal_len u32           then pal_len * 3 bytes RGB888
//!   <brotli stream>       brotli of exactly `frames` frame blocks
//! ```
//! A frame block is one mode byte then its payload — XOR deltas vs the previous
//! frame (raw / sparse / zero), intra (RLE / raw), or motion-compensated
//! (mvx, mvy from the recorded SCX/SCY delta, then a sparse residual). A static
//! screen is one byte (mode 2); scrolling collapses to the motion residual; the
//! brotli layer then catches cross-frame redundancy no per-frame arm can see
//! (~30x measured on palette-animation games, x0.67 vs deflate overall).

#[cfg(feature = "encode")]
use std::collections::HashMap;
use std::sync::Arc;

#[cfg(feature = "encode")]
use crate::stream::{brotli_compress_into, write_varint};
use crate::stream::{apply_sparse_src, src_byte, src_take, src_varint, DecodeError, Source};
#[cfg(feature = "encode")]
use crate::{FPS_DEN, FPS_NUM};

const MAGIC: [u8; 4] = *b"RBR3";
const FLAG_WIDE: u8 = 1 << 0;
/// Raw (full-buffer) payloads are bit-packed 4 pixels/byte. Only ever set for
/// <=4-color clips (measured -10..21% there; 4-bit packing measured WORSE, so
/// 2-bit is the only packed form). Sparse/RLE/motion payloads stay byte-wide.
const FLAG_PACKED: u8 = 1 << 1;

// Per frame the encoder picks the smallest of these. The first three are deltas
// against the previous frame (cheap when little changed); the last two are intra
// (self-contained) — crucial for motion/scrolling, where the XOR delta is
// high-entropy noise but the frame's own index buffer still has long flat runs.
const MODE_DELTA_RAW: u8 = 0; // XOR: literal delta bytes
const MODE_DELTA_SPARSE: u8 = 1; // XOR: (skip, lit)* ops
const MODE_ZERO: u8 = 2; // identical to previous frame
const MODE_INTRA_RLE: u8 = 3; // replace: (count, value)* run-length of the frame
const MODE_INTRA_RAW: u8 = 4; // replace: literal index bytes
// Motion-compensated: predict = previous frame shifted by the emulator's exact
// per-frame scroll (SCX/SCY), then a sparse residual XOR. Collapses background
// scrolling — the case a plain XOR delta turns into whole-frame noise.
const MODE_MOTION: u8 = 5; // i8 mvx, i8 mvy, then (skip, lit)* residual

// ---- encoder -------------------------------------------------------------

/// Streams RGB888 frames in, emits a `.rbr` blob. Interns a global palette as it
/// goes and holds each frame's palette indices (u16) until [`Encoder::finish`].
#[cfg(feature = "encode")]
pub struct Encoder {
    width: u16,
    height: u16,
    pixels: usize,
    palette: Vec<[u8; 3]>,
    lut: HashMap<[u8; 3], u16>,
    indices: Vec<u16>, // frame-major, `pixels` per frame
    scroll: Vec<(u8, u8)>, // per-frame (SCX, SCY) for motion compensation
    frames: u32,
}

#[cfg(feature = "encode")]
impl Encoder {
    /// `width * height` must be > 0. Panics otherwise (a caller bug, not input).
    pub fn new(width: u16, height: u16) -> Self {
        let pixels = width as usize * height as usize;
        assert!(pixels > 0, "replay: zero-sized frame");
        Self {
            width,
            height,
            pixels,
            palette: Vec::new(),
            lut: HashMap::new(),
            indices: Vec::new(),
            scroll: Vec::new(),
            frames: 0,
        }
    }

    /// Append one frame with no motion hint (motion arm inert). `rgb` must be
    /// `width*height*3` bytes of RGB888.
    pub fn push_rgb(&mut self, rgb: &[u8]) {
        self.push_rgb_scroll(rgb, 0, 0);
    }

    /// Append one frame plus the emulator's `SCX`/`SCY` at frame end, so the
    /// encoder can predict background scrolling from the exact scroll delta.
    pub fn push_rgb_scroll(&mut self, rgb: &[u8], scx: u8, scy: u8) {
        assert_eq!(rgb.len(), self.pixels * 3, "replay: wrong frame size");
        self.indices.reserve(self.pixels);
        for px in rgb.chunks_exact(3) {
            let color = [px[0], px[1], px[2]];
            let idx = *self.lut.entry(color).or_insert_with(|| {
                let i = self.palette.len();
                self.palette.push(color);
                // GB can present at most 32768 distinct colours; u16 always fits.
                i as u16
            });
            self.indices.push(idx);
        }
        self.scroll.push((scx, scy));
        self.frames += 1;
    }

    pub fn frames(&self) -> u32 {
        self.frames
    }

    /// Serialize to the `.rbr` container (brotli'd frame stream). Consumes
    /// nothing; callable once the last frame is pushed.
    #[cfg(feature = "encode")]
    pub fn finish(&self) -> Vec<u8> {
        let mut out = self.build_header();
        let stream = self.build_stream();
        brotli_compress_into(&stream, &mut out);
        out
    }

    fn packed(&self) -> bool {
        self.palette.len() <= 4
    }

    fn build_header(&self) -> Vec<u8> {
        let wide = self.palette.len() > 256; // u16 indices vs u8
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        let mut flags = 0u8;
        if wide {
            flags |= FLAG_WIDE;
        }
        if self.packed() {
            flags |= FLAG_PACKED;
        }
        out.push(flags);
        out.extend_from_slice(&FPS_NUM.to_le_bytes());
        out.extend_from_slice(&FPS_DEN.to_le_bytes());
        out.extend_from_slice(&self.frames.to_le_bytes());
        out.extend_from_slice(&(self.palette.len() as u32).to_le_bytes());
        for c in &self.palette {
            out.extend_from_slice(c);
        }
        out
    }

    /// The concatenated frame blocks (pre-compression).
    fn build_stream(&self) -> Vec<u8> {
        let wide = self.palette.len() > 256;
        let idx_bytes = self.pixels * if wide { 2 } else { 1 };
        let mut out = Vec::new();
        let isz = if wide { 2 } else { 1 };
        let (w, h) = (self.width as usize, self.height as usize);
        let mut prev = vec![0u8; idx_bytes];
        let mut cur = vec![0u8; idx_bytes];
        let mut delta = vec![0u8; idx_bytes];
        let mut predicted = vec![0u8; idx_bytes];
        let mut residual = vec![0u8; idx_bytes];
        for f in 0..self.frames as usize {
            let frame = &self.indices[f * self.pixels..(f + 1) * self.pixels];
            index_bytes(frame, wide, &mut cur);
            for i in 0..idx_bytes {
                delta[i] = cur[i] ^ prev[i];
            }
            // Motion arm: predict the previous frame shifted by this frame's exact
            // scroll delta, and encode only the (usually sparse) residual.
            let motion = (f > 0)
                .then(|| {
                    let mvx = self.scroll[f].0.wrapping_sub(self.scroll[f - 1].0) as i8;
                    let mvy = self.scroll[f].1.wrapping_sub(self.scroll[f - 1].1) as i8;
                    (mvx != 0 || mvy != 0).then(|| {
                        shift_predict(&prev, w, h, isz, mvx, mvy, &mut predicted);
                        for i in 0..idx_bytes {
                            residual[i] = cur[i] ^ predicted[i];
                        }
                        (mvx, mvy, encode_sparse(&residual))
                    })
                })
                .flatten();
            write_frame_block(&mut out, &delta, &cur, motion.as_ref(), self.packed());
            std::mem::swap(&mut prev, &mut cur);
        }
        out
    }
}

/// Predict `out[x,y] = prev[x+mvx, y+mvy]` (0 where the source is off-frame, i.e.
/// scrolled-in edges), operating on `isz`-byte index cells.
fn shift_predict(prev: &[u8], w: usize, h: usize, isz: usize, mvx: i8, mvy: i8, out: &mut [u8]) {
    out.fill(0);
    let (mvx, mvy) = (mvx as isize, mvy as isize);
    for y in 0..h {
        let sy = y as isize + mvy;
        if sy < 0 || sy >= h as isize {
            continue;
        }
        for x in 0..w {
            let sx = x as isize + mvx;
            if sx < 0 || sx >= w as isize {
                continue;
            }
            let di = (y * w + x) * isz;
            let si = (sy as usize * w + sx as usize) * isz;
            out[di..di + isz].copy_from_slice(&prev[si..si + isz]);
        }
    }
}

/// Pack a frame's u16 indices into the on-wire index bytes (u8 or u16 LE).
#[cfg(feature = "encode")]
fn index_bytes(indices: &[u16], wide: bool, out: &mut [u8]) {
    if wide {
        for (i, &idx) in indices.iter().enumerate() {
            out[i * 2..i * 2 + 2].copy_from_slice(&idx.to_le_bytes());
        }
    } else {
        for (i, &idx) in indices.iter().enumerate() {
            out[i] = idx as u8;
        }
    }
}

/// Emit the smallest encoding for one frame. `delta` is its XOR against the
/// previous frame, `cur` its own index bytes, `motion` an optional
/// `(mvx, mvy, sparse-residual)` from scroll prediction, `packed` whether raw
/// payloads ship 4px/byte. Delta arms win on small changes, intra arms on
/// complex frames, motion on scrolling.
#[cfg(feature = "encode")]
fn write_frame_block(
    out: &mut Vec<u8>,
    delta: &[u8],
    cur: &[u8],
    motion: Option<&(i8, i8, Vec<u8>)>,
    packed: bool,
) {
    if delta.iter().all(|&b| b == 0) {
        out.push(MODE_ZERO);
        return;
    }
    let sparse = encode_sparse(delta);
    let rle = encode_rle(cur);
    let raw_len = if packed { pack2_len(cur.len()) } else { cur.len() };
    // Total cost = 1 mode byte + payload (motion adds 2 mv bytes). Default = the
    // always-applicable intra-raw; smaller candidates win, ties to the earlier.
    let (mut mode, mut cost) = (MODE_INTRA_RAW, 1 + raw_len);
    if 1 + sparse.len() < cost {
        (mode, cost) = (MODE_DELTA_SPARSE, 1 + sparse.len());
    }
    if 1 + rle.len() < cost {
        (mode, cost) = (MODE_INTRA_RLE, 1 + rle.len());
    }
    if 1 + raw_len < cost {
        (mode, cost) = (MODE_DELTA_RAW, 1 + raw_len);
    }
    if let Some((_, _, res)) = motion
        && 3 + res.len() < cost
    {
        (mode, cost) = (MODE_MOTION, 3 + res.len());
    }
    let _ = cost;
    let raw = |buf: &[u8], out: &mut Vec<u8>| {
        if packed {
            out.extend_from_slice(&pack2(buf));
        } else {
            out.extend_from_slice(buf);
        }
    };
    match mode {
        MODE_DELTA_SPARSE => {
            out.push(MODE_DELTA_SPARSE);
            out.extend_from_slice(&sparse);
        }
        MODE_INTRA_RLE => {
            out.push(MODE_INTRA_RLE);
            out.extend_from_slice(&rle);
        }
        MODE_DELTA_RAW => {
            out.push(MODE_DELTA_RAW);
            raw(delta, out);
        }
        MODE_MOTION => {
            let (mvx, mvy, res) = motion.unwrap();
            out.push(MODE_MOTION);
            out.push(*mvx as u8);
            out.push(*mvy as u8);
            out.extend_from_slice(res);
        }
        _ => {
            out.push(MODE_INTRA_RAW);
            raw(cur, out);
        }
    }
}

/// Bytes needed for `n` 2-bit cells at 4 per byte.
fn pack2_len(n: usize) -> usize {
    n.div_ceil(4)
}

/// Pack index bytes (values 0..=3) 4 per byte, cell 0 in bits 0-1. XOR commutes
/// with this bitwise layout, so packed deltas apply directly to packed buffers —
/// but the codec packs only at the serialization boundary and keeps its working
/// buffers byte-per-index.
#[cfg(feature = "encode")]
fn pack2(buf: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; pack2_len(buf.len())];
    for (i, &v) in buf.iter().enumerate() {
        out[i / 4] |= (v & 3) << ((i % 4) * 2);
    }
    out
}

/// Inverse of [`pack2`]: expand into `out` (whose length picks the cell count).
fn unpack2(packed: &[u8], out: &mut [u8]) {
    for (i, o) in out.iter_mut().enumerate() {
        *o = (packed[i / 4] >> ((i % 4) * 2)) & 3;
    }
}

/// Byte-level run-length: `(count varint, value)*` covering the whole buffer.
#[cfg(feature = "encode")]
fn encode_rle(buf: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        let v = buf[i];
        let start = i;
        while i < buf.len() && buf[i] == v {
            i += 1;
        }
        write_varint(&mut out, (i - start) as u32);
        out.push(v);
    }
    out
}

/// (skip, lit_len, lit_bytes)* — runs of unchanged (zero) bytes then literal
/// non-zero delta bytes, covering the whole buffer.
#[cfg(feature = "encode")]
fn encode_sparse(delta: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < delta.len() {
        let skip_start = i;
        while i < delta.len() && delta[i] == 0 {
            i += 1;
        }
        write_varint(&mut out, (i - skip_start) as u32);
        let lit_start = i;
        while i < delta.len() && delta[i] != 0 {
            i += 1;
        }
        write_varint(&mut out, (i - lit_start) as u32);
        out.extend_from_slice(&delta[lit_start..i]);
    }
    out
}

/// One-shot convenience: encode a whole clip of RGB888 frames.
#[cfg(feature = "encode")]
pub fn encode(width: u16, height: u16, frames: &[&[u8]]) -> Vec<u8> {
    let mut enc = Encoder::new(width, height);
    for f in frames {
        enc.push_rgb(f);
    }
    enc.finish()
}

/// Sequential decoder: keeps a running index buffer and walks the frame stream,
/// wrapping back to the start after the last frame (built for looping playback).
/// Reads both v1 (raw stream) and v2 (deflated stream, inflated incrementally).
pub struct Decoder {
    width: u16,
    height: u16,
    pixels: usize,
    wide: bool,
    packed: bool,
    idx_bytes: usize,
    fps_num: u32,
    fps_den: u32,
    frames: u32,
    palette: Vec<[u8; 3]>,
    compressed: Arc<[u8]>, // frame stream (shared with `src` for cheap reset)
    src: Source,
    frame: u32,        // index of the next frame to produce
    cur: Vec<u8>,      // running index bytes
    scratch: Vec<u8>,  // motion-prediction target
    packbuf: Vec<u8>,  // unpack target for packed raw payloads
}

impl Decoder {
    pub fn new(bytes: Vec<u8>) -> Result<Self, DecodeError> {
        let mut p = 0usize;
        let take = |p: &mut usize, n: usize| -> Result<&[u8], DecodeError> {
            let s = bytes.get(*p..*p + n).ok_or(DecodeError::Truncated)?;
            *p += n;
            Ok(s)
        };
        if take(&mut p, 4)? != MAGIC {
            return Err(DecodeError::BadMagic);
        }
        let u16le = |b: &[u8]| u16::from_le_bytes([b[0], b[1]]);
        let u32le = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let width = u16le(take(&mut p, 2)?);
        let height = u16le(take(&mut p, 2)?);
        let flags = take(&mut p, 1)?[0];
        let wide = flags & FLAG_WIDE != 0;
        let packed = flags & FLAG_PACKED != 0;
        let fps_num = u32le(take(&mut p, 4)?);
        let fps_den = u32le(take(&mut p, 4)?);
        let frames = u32le(take(&mut p, 4)?);
        let pal_len = u32le(take(&mut p, 4)?) as usize;
        let mut palette = Vec::with_capacity(pal_len);
        for _ in 0..pal_len {
            let c = take(&mut p, 3)?;
            palette.push([c[0], c[1], c[2]]);
        }
        let pixels = width as usize * height as usize;
        if pixels == 0 || fps_den == 0 {
            return Err(DecodeError::Malformed);
        }
        // A u8-index stream must fit its palette in one byte; a packed stream
        // must fit it in two bits.
        if !wide && pal_len > 256 {
            return Err(DecodeError::Malformed);
        }
        if packed && (wide || pal_len > 4) {
            return Err(DecodeError::Malformed);
        }
        let idx_bytes = pixels * if wide { 2 } else { 1 };
        let compressed: Arc<[u8]> = Arc::from(&bytes[p..]);
        Ok(Self {
            width,
            height,
            pixels,
            wide,
            packed,
            idx_bytes,
            fps_num,
            fps_den,
            frames,
            palette,
            src: Source::new(compressed.clone()),
            compressed,
            frame: 0,
            cur: vec![0u8; idx_bytes],
            scratch: vec![0u8; idx_bytes],
            packbuf: Vec::new(),
        })
    }

    pub fn width(&self) -> u16 {
        self.width
    }
    pub fn height(&self) -> u16 {
        self.height
    }
    pub fn frame_count(&self) -> u32 {
        self.frames
    }
    pub fn fps_num(&self) -> u32 {
        self.fps_num
    }
    pub fn fps_den(&self) -> u32 {
        self.fps_den
    }

    /// Restart playback from frame 0 without reparsing the header (the brotli
    /// stream restarts from the top of the shared compressed frame data).
    pub fn reset(&mut self) {
        self.src = Source::new(self.compressed.clone());
        self.frame = 0;
        self.cur.iter_mut().for_each(|b| *b = 0);
    }

    /// Decode the next frame into `out` (RGBA8, `width*height*4` bytes), wrapping
    /// to frame 0 after the last. Returns the decoded frame's index.
    pub fn next_into(&mut self, out: &mut [u8]) -> Result<u32, DecodeError> {
        if self.frames == 0 {
            return Err(DecodeError::Malformed);
        }
        if self.frame >= self.frames {
            self.reset();
        }
        self.apply_next_block()?;
        self.expand_rgba(out)?;
        let produced = self.frame;
        self.frame += 1;
        Ok(produced)
    }

    fn apply_next_block(&mut self) -> Result<(), DecodeError> {
        // Destructure so the source (mutable) and the frame buffers (mutable)
        // borrow disjointly.
        let Self { src, cur, scratch, packbuf, idx_bytes, wide, packed, width, height, .. } = self;
        let (idx_bytes, wide, packed) = (*idx_bytes, *wide, *packed);
        let (w, h) = (*width as usize, *height as usize);
        // Raw payloads are 4px/byte when the packed flag is set: read the packed
        // form, expand into `packbuf`, and hand back one unpacked view.
        let raw_len = if packed { pack2_len(idx_bytes) } else { idx_bytes };
        let take_raw = |src: &mut Source, packbuf: &mut Vec<u8>| -> Result<(), DecodeError> {
            packbuf.resize(idx_bytes, 0);
            if packed {
                let payload = src_take(src, raw_len)?;
                unpack2(payload, packbuf);
            } else {
                packbuf.copy_from_slice(src_take(src, raw_len)?);
            }
            Ok(())
        };
        match src_byte(src)? {
            MODE_ZERO => {}
            MODE_DELTA_RAW => {
                take_raw(src, packbuf)?;
                for (c, d) in cur.iter_mut().zip(packbuf.iter()) {
                    *c ^= d;
                }
            }
            MODE_INTRA_RAW => {
                take_raw(src, packbuf)?;
                cur.copy_from_slice(packbuf);
            }
            MODE_INTRA_RLE => {
                let mut i = 0usize;
                while i < idx_bytes {
                    let count = src_varint(src)? as usize;
                    let v = src_byte(src)?;
                    let end = i.checked_add(count).ok_or(DecodeError::Malformed)?;
                    let target = cur.get_mut(i..end).ok_or(DecodeError::Malformed)?;
                    target.fill(v);
                    i = end;
                }
                if i != idx_bytes {
                    return Err(DecodeError::Malformed);
                }
            }
            MODE_DELTA_SPARSE => {
                apply_sparse_src(src, cur)?;
            }
            MODE_MOTION => {
                let mv = src_take(src, 2)?;
                let (mvx, mvy) = (mv[0] as i8, mv[1] as i8);
                let isz = if wide { 2 } else { 1 };
                // predicted = shift(previous frame); residual XORs onto it.
                shift_predict(cur, w, h, isz, mvx, mvy, scratch);
                apply_sparse_src(src, scratch)?;
                std::mem::swap(cur, scratch);
            }
            _ => return Err(DecodeError::Malformed),
        }
        Ok(())
    }

    fn expand_rgba(&self, out: &mut [u8]) -> Result<(), DecodeError> {
        if out.len() < self.pixels * 4 {
            return Err(DecodeError::Malformed);
        }
        for px in 0..self.pixels {
            let idx = if self.wide {
                u16::from_le_bytes([self.cur[px * 2], self.cur[px * 2 + 1]]) as usize
            } else {
                self.cur[px] as usize
            };
            let c = self.palette.get(idx).ok_or(DecodeError::Malformed)?;
            let o = px * 4;
            out[o] = c[0];
            out[o + 1] = c[1];
            out[o + 2] = c[2];
            out[o + 3] = 0xFF;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Render a synthetic RGB frame: a flat background with a moving 8x8 block, so
    // successive frames differ only in a small region (exercises the sparse arm).
    fn frame(w: usize, h: usize, t: usize) -> Vec<u8> {
        let mut f = vec![0u8; w * h * 3];
        for (i, px) in f.chunks_exact_mut(3).enumerate() {
            let (x, y) = (i % w, i / w);
            let inside = x >= t % (w - 8) && x < t % (w - 8) + 8 && (4..12).contains(&y);
            px.copy_from_slice(if inside { &[224, 32, 32] } else { &[15, 56, 15] });
        }
        f
    }

    fn rgba_of(dec: &mut Decoder, want: u32) -> Vec<u8> {
        let mut out = vec![0u8; dec.width() as usize * dec.height() as usize * 4];
        loop {
            let got = dec.next_into(&mut out).unwrap();
            if got == want {
                return out;
            }
        }
    }

    #[test]
    fn round_trip_is_pixel_exact() {
        let (w, h) = (160usize, 144usize);
        let frames: Vec<Vec<u8>> = (0..30).map(|t| frame(w, h, t)).collect();
        let refs: Vec<&[u8]> = frames.iter().map(|f| f.as_slice()).collect();
        let blob = encode(w as u16, h as u16, &refs);

        let mut dec = Decoder::new(blob).unwrap();
        assert_eq!(dec.frame_count(), 30);
        assert_eq!((dec.width(), dec.height()), (160, 144));

        let mut out = vec![0u8; w * h * 4];
        for (t, src) in frames.iter().enumerate() {
            let got = dec.next_into(&mut out).unwrap();
            assert_eq!(got, t as u32);
            // Every decoded pixel equals the source RGB with opaque alpha.
            for (p, s) in out.chunks_exact(4).zip(src.chunks_exact(3)) {
                assert_eq!(&p[..3], s, "frame {t} pixel mismatch");
                assert_eq!(p[3], 0xFF);
            }
        }
    }

    #[test]
    fn loops_back_to_frame_zero() {
        let (w, h) = (32u16, 32u16);
        let frames: Vec<Vec<u8>> = (0..5).map(|t| frame(w as usize, h as usize, t)).collect();
        let refs: Vec<&[u8]> = frames.iter().map(|f| f.as_slice()).collect();
        let mut dec = Decoder::new(encode(w, h, &refs)).unwrap();
        let first = rgba_of(&mut dec, 0);
        // Advance a full loop; frame 0 must decode identically the second time.
        for _ in 0..5 {
            let mut o = vec![0u8; w as usize * h as usize * 4];
            dec.next_into(&mut o).unwrap();
        }
        assert_eq!(rgba_of(&mut dec, 0), first);
    }

    #[test]
    fn round_trip_motion_scroll() {
        // Vertical stripes scrolling left 1px/frame, with a matching SCX ramp so
        // the motion arm predicts almost the whole frame (residual = the 1px
        // revealed edge). Verifies the motion decode path is byte-exact.
        let (w, h) = (64usize, 48usize);
        let mk = |t: usize| {
            let mut f = vec![0u8; w * h * 3];
            for (i, px) in f.chunks_exact_mut(3).enumerate() {
                let stripe = ((i % w + t) % 16) < 8;
                px.copy_from_slice(if stripe { &[200, 200, 200] } else { &[20, 60, 20] });
            }
            f
        };
        let frames: Vec<Vec<u8>> = (0..20).map(mk).collect();
        let mut enc = Encoder::new(w as u16, h as u16);
        for (t, f) in frames.iter().enumerate() {
            enc.push_rgb_scroll(f, t as u8, 0); // SCX += 1 each frame
        }
        let blob = enc.finish();
        // Motion should make this far smaller than storing intra frames.
        assert!(blob.len() < w * h * 20 / 4, "motion clip not compact: {}", blob.len());

        let mut dec = Decoder::new(blob).unwrap();
        let mut out = vec![0u8; w * h * 4];
        for (t, src) in frames.iter().enumerate() {
            dec.next_into(&mut out).unwrap();
            for (p, s) in out.chunks_exact(4).zip(src.chunks_exact(3)) {
                assert_eq!(&p[..3], s, "motion frame {t} mismatch");
            }
        }
    }

    #[test]
    fn static_clip_is_tiny() {
        // 100 identical frames: header + palette + one keyframe + 99 zero bytes.
        let (w, h) = (160u16, 144u16);
        let f = vec![90u8; w as usize * h as usize * 3]; // one solid colour
        let refs: Vec<&[u8]> = (0..100).map(|_| f.as_slice()).collect();
        let blob = encode(w, h, &refs);
        // 99 unchanged frames cost 1 byte each; nowhere near raw (100*69120).
        assert!(blob.len() < 1000, "static clip bloated: {} bytes", blob.len());
        let mut dec = Decoder::new(blob).unwrap();
        let mut out = vec![0u8; w as usize * h as usize * 4];
        for _ in 0..100 {
            dec.next_into(&mut out).unwrap();
        }
        assert_eq!(&out[..4], &[90, 90, 90, 0xFF]);
    }

    #[test]
    fn four_color_clips_pack_and_round_trip() {
        // A DMG-like clip (4 colors) must set the packed flag and stay
        // pixel-exact through the packed raw payload path.
        let (w, h) = (160usize, 144usize);
        let pal = [[15u8, 56, 15], [48, 98, 48], [139, 172, 15], [155, 188, 15]];
        // High-entropy frames force the raw arms (RLE/sparse would win on
        // structured content and dodge the packed path).
        let mk = |t: usize| -> Vec<u8> {
            let mut f = vec![0u8; w * h * 3];
            for (i, px) in f.chunks_exact_mut(3).enumerate() {
                let x = (i * 2654435761 + t * 97) & 0xFFFF;
                px.copy_from_slice(&pal[(x ^ (x >> 7)) & 3]);
            }
            f
        };
        let frames: Vec<Vec<u8>> = (0..8).map(mk).collect();
        let refs: Vec<&[u8]> = frames.iter().map(|f| f.as_slice()).collect();
        let blob = encode(w as u16, h as u16, &refs);
        assert_eq!(blob[8] & FLAG_PACKED, FLAG_PACKED, "packed flag missing");

        let mut dec = Decoder::new(blob).unwrap();
        let mut out = vec![0u8; w * h * 4];
        for (t, src) in frames.iter().enumerate() {
            dec.next_into(&mut out).unwrap();
            for (p, s) in out.chunks_exact(4).zip(src.chunks_exact(3)) {
                assert_eq!(&p[..3], s, "packed frame {t} mismatch");
            }
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(Decoder::new(vec![0u8; 3]), Err(DecodeError::Truncated)));
        assert!(matches!(Decoder::new(b"NOPE....".to_vec()), Err(DecodeError::BadMagic)));
    }
}
