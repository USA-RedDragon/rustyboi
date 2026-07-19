use std::sync::Arc;

#[cfg(feature = "encode")]
use crate::stream::{brotli_compress_into, write_varint};
use crate::stream::{src_byte, src_take, src_varint, DecodeError, Source};

// A recording of the APU's OUTPUT, decomposed per channel — the measured sweet
// spot (x0.483 vs coding the stereo mix): each channel's DAC stream has a tiny
// alphabet (<=16 levels) and its runs aren't broken by the other channels'
// edges. The decoder rebuilds the exact stereo mix with the same arithmetic as
// the core's `get_mixed_output` (NR51 pan masks, NR50 volume, /4) — proven
// byte-exact on real PCM — so no APU is ever needed client-side.
//
// Container (little-endian):
// ```text
//   magic   "RBA1"      4 bytes
//   rate    u32         sample rate (44100)
//   fps_num u32         fps_den u32     (video frame rate, for frame<->sample)
//   samples u32
//   flags   u8          reserved 0
//   <brotli stream>     7 planes: ch1..ch4 (f32 values), nr50, nr51, enabled
// ```
// Each plane: u16 palette_len, palette entries (f32le for channels, u8 for
// regs/enabled), u32 run_count, then run_count idx varints followed by
// run_count run-length varints (SoA — measured better than interleaving).

const AUDIO_MAGIC: [u8; 4] = *b"RBA1";
pub const AUDIO_RATE: u32 = 44_100;

/// One tapped APU sample: pre-mix channel outputs + (nr50, nr51, enabled) —
/// structurally identical to `rustyboi_core_lib::audio::ChannelSample`, mirrored
/// here so this crate stays core-free.
pub type ChannelSample = ([f32; 4], u8, u8, bool);

/// Accumulates tapped channel samples, emits an `.rba` blob.
#[cfg(feature = "encode")]
#[derive(Default)]
pub struct AudioEncoder {
    samples: Vec<ChannelSample>,
}

#[cfg(feature = "encode")]
impl AudioEncoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, samples: &[ChannelSample]) {
        self.samples.extend_from_slice(samples);
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn finish(&self, fps_num: u32, fps_den: u32) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&AUDIO_MAGIC);
        out.extend_from_slice(&AUDIO_RATE.to_le_bytes());
        out.extend_from_slice(&fps_num.to_le_bytes());
        out.extend_from_slice(&fps_den.to_le_bytes());
        out.extend_from_slice(&(self.samples.len() as u32).to_le_bytes());
        out.push(0); // flags
        let mut stream = Vec::new();
        for ch in 0..4 {
            write_plane(&mut stream, self.samples.iter().map(|s| s.0[ch]), |v, o| {
                o.extend_from_slice(&v.to_le_bytes())
            });
        }
        write_plane(&mut stream, self.samples.iter().map(|s| s.1), |v, o| o.push(v));
        write_plane(&mut stream, self.samples.iter().map(|s| s.2), |v, o| o.push(v));
        write_plane(&mut stream, self.samples.iter().map(|s| u8::from(s.3)), |v, o| o.push(v));
        brotli_compress_into(&stream, &mut out);
        out
    }
}

/// RLE one value plane: palette + SoA (idx varints, then run varints).
#[cfg(feature = "encode")]
fn write_plane<T, I, W>(out: &mut Vec<u8>, values: I, write_val: W)
where
    T: Copy + PartialEq,
    I: Iterator<Item = T>,
    W: Fn(T, &mut Vec<u8>),
{
    let mut palette: Vec<T> = Vec::new();
    let mut idx: Vec<u32> = Vec::new();
    let mut runs: Vec<u32> = Vec::new();
    for v in values {
        let pi = match palette.iter().position(|p| *p == v) {
            Some(i) => i as u32,
            None => {
                palette.push(v);
                (palette.len() - 1) as u32
            }
        };
        match (idx.last(), runs.last_mut()) {
            (Some(&last), Some(r)) if last == pi => *r += 1,
            _ => {
                idx.push(pi);
                runs.push(1);
            }
        }
    }
    out.extend_from_slice(&(palette.len() as u16).to_le_bytes());
    for &v in &palette {
        write_val(v, out);
    }
    out.extend_from_slice(&(idx.len() as u32).to_le_bytes());
    for &i in &idx {
        write_varint(out, i);
    }
    for &r in &runs {
        write_varint(out, r);
    }
}

/// One decoded plane with a sequential/seekable cursor.
struct Plane<T> {
    palette: Vec<T>,
    idx: Vec<u32>,
    runs: Vec<u32>,
    run: usize,     // current run index
    within: u32,    // consumed samples of the current run
}

impl<T: Copy> Plane<T> {
    fn seek(&mut self, mut sample: u64) {
        self.run = 0;
        self.within = 0;
        while self.run < self.runs.len() && sample >= u64::from(self.runs[self.run]) {
            sample -= u64::from(self.runs[self.run]);
            self.run += 1;
        }
        self.within = sample as u32;
    }

    fn next(&mut self) -> Result<T, DecodeError> {
        let r = *self.runs.get(self.run).ok_or(DecodeError::Truncated)?;
        let v = self.palette[self.idx[self.run] as usize];
        self.within += 1;
        if self.within >= r {
            self.run += 1;
            self.within = 0;
        }
        Ok(v)
    }
}

fn read_plane<T: Copy, F: Fn(&mut Source) -> Result<T, DecodeError>>(
    src: &mut Source,
    read_val: F,
) -> Result<Plane<T>, DecodeError> {
    let pl = src_take(src, 2)?;
    let pal_len = u16::from_le_bytes([pl[0], pl[1]]) as usize;
    let mut palette = Vec::with_capacity(pal_len);
    for _ in 0..pal_len {
        palette.push(read_val(src)?);
    }
    let rc = src_take(src, 4)?;
    let run_count = u32::from_le_bytes([rc[0], rc[1], rc[2], rc[3]]) as usize;
    let mut idx = Vec::with_capacity(run_count);
    for _ in 0..run_count {
        let i = src_varint(src)?;
        if i as usize >= pal_len {
            return Err(DecodeError::Malformed);
        }
        idx.push(i);
    }
    let mut runs = Vec::with_capacity(run_count);
    for _ in 0..run_count {
        runs.push(src_varint(src)?);
    }
    Ok(Plane { palette, idx, runs, run: 0, within: 0 })
}

/// Decodes an `.rba` to interleaved stereo f32, one video frame's span at a
/// time, reproducing `get_mixed_output`'s arithmetic exactly.
pub struct AudioDecoder {
    rate: u32,
    fps_num: u32,
    fps_den: u32,
    samples: u32,
    chans: [Plane<f32>; 4],
    nr50: Plane<u8>,
    nr51: Plane<u8>,
    enabled: Plane<u8>,
    pos: u64, // next sample index
}

impl AudioDecoder {
    pub fn new(bytes: Vec<u8>) -> Result<Self, DecodeError> {
        let hdr = bytes.get(..21).ok_or(DecodeError::Truncated)?;
        if hdr[..4] != AUDIO_MAGIC {
            return Err(DecodeError::BadMagic);
        }
        let u32le = |o: usize| u32::from_le_bytes([hdr[o], hdr[o + 1], hdr[o + 2], hdr[o + 3]]);
        let rate = u32le(4);
        let fps_num = u32le(8);
        let fps_den = u32le(12);
        let samples = u32le(16);
        if rate == 0 || fps_num == 0 || fps_den == 0 {
            return Err(DecodeError::Malformed);
        }
        let compressed: Arc<[u8]> = Arc::from(&bytes[21..]);
        let mut src = Source::new(compressed);
        let f32v = |src: &mut Source| -> Result<f32, DecodeError> {
            let b = src_take(src, 4)?;
            Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        let chans = [
            read_plane(&mut src, f32v)?,
            read_plane(&mut src, f32v)?,
            read_plane(&mut src, f32v)?,
            read_plane(&mut src, f32v)?,
        ];
        let nr50 = read_plane(&mut src, src_byte)?;
        let nr51 = read_plane(&mut src, src_byte)?;
        let enabled = read_plane(&mut src, src_byte)?;
        Ok(Self { rate, fps_num, fps_den, samples, chans, nr50, nr51, enabled, pos: 0 })
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    pub fn sample_count(&self) -> u32 {
        self.samples
    }

    /// First sample index of video frame `i` (frame<->sample arithmetic; the
    /// concatenation over all frames reproduces the full stream exactly).
    fn frame_sample(&self, i: u32) -> u64 {
        u64::from(i) * u64::from(self.rate) * u64::from(self.fps_den) / u64::from(self.fps_num)
    }

    /// Position playback at the start of video frame `i` (O(runs)).
    pub fn seek_frame(&mut self, i: u32) {
        let s = self.frame_sample(i).min(u64::from(self.samples));
        for c in &mut self.chans {
            c.seek(s);
        }
        self.nr50.seek(s);
        self.nr51.seek(s);
        self.enabled.seek(s);
        self.pos = s;
    }

    /// Decode video frame `i`'s span of samples as interleaved stereo into
    /// `out` (cleared first). Frames must be consumed sequentially unless
    /// `seek_frame` repositions. Returns the number of stereo pairs.
    pub fn frame_into(&mut self, i: u32, out: &mut Vec<f32>) -> Result<usize, DecodeError> {
        out.clear();
        let start = self.frame_sample(i);
        if start != self.pos {
            self.seek_frame(i);
        }
        let end = self.frame_sample(i + 1).min(u64::from(self.samples));
        let n = end.saturating_sub(self.pos) as usize;
        out.reserve(n * 2);
        for _ in 0..n {
            let chs = [
                self.chans[0].next()?,
                self.chans[1].next()?,
                self.chans[2].next()?,
                self.chans[3].next()?,
            ];
            let nr50 = self.nr50.next()?;
            let nr51 = self.nr51.next()?;
            let en = self.enabled.next()? != 0;
            let (l, r) = mix(chs, nr50, nr51, en);
            out.push(l);
            out.push(r);
        }
        self.pos = end;
        Ok(n)
    }
}

/// The core's `get_mixed_output` arithmetic, bit-for-bit: conditional adds in
/// channel order, then master volume `(vol+1)/8`, then /4 — f32 op order
/// matters for exact reconstruction (proven byte-exact on real PCM).
fn mix(chs: [f32; 4], nr50: u8, nr51: u8, enabled: bool) -> (f32, f32) {
    if !enabled {
        return (0.0, 0.0);
    }
    let mut left = 0.0f32;
    let mut right = 0.0f32;
    for (i, &ch) in chs.iter().enumerate() {
        if nr51 & (1 << (i + 4)) != 0 {
            left += ch;
        }
        if nr51 & (1 << i) != 0 {
            right += ch;
        }
    }
    left *= ((nr50 >> 4) & 7) as f32 + 1.0;
    left /= 8.0;
    right *= (nr50 & 7) as f32 + 1.0;
    right /= 8.0;
    (left / 4.0, right / 4.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FPS_DEN, FPS_NUM};

    // Synthetic APU tap: square-ish channels at different periods, a panning
    // flip and a volume change partway, one disabled stretch.
    fn tap(n: usize) -> Vec<ChannelSample> {
        (0..n)
            .map(|i| {
                let chs = [
                    if (i / 50) % 2 == 0 { 0.6f32 } else { -0.6 },
                    if (i / 33) % 2 == 0 { 0.25 } else { -0.25 },
                    (i / 200 % 4) as f32 * 0.1,
                    if i % 7 == 0 { 0.9 } else { -0.13 },
                ];
                let nr50 = if i < n / 2 { 0x77 } else { 0x34 };
                let nr51 = if i < n / 3 { 0xFF } else { 0xF1 };
                (chs, nr50, nr51, i % 900 != 3)
            })
            .collect()
    }

    #[test]
    fn audio_round_trip_is_sample_exact() {
        let samples = tap(30_000);
        let mut enc = AudioEncoder::new();
        enc.push(&samples);
        let blob = enc.finish(FPS_NUM, FPS_DEN);
        let mut dec = AudioDecoder::new(blob).unwrap();
        assert_eq!(dec.sample_count(), 30_000);

        let mut out = Vec::new();
        let mut got = 0usize;
        let mut frame = 0u32;
        while got < samples.len() {
            let n = dec.frame_into(frame, &mut out).unwrap();
            assert!(n > 0, "stalled at frame {frame}");
            for (k, pair) in out.chunks_exact(2).enumerate() {
                let (chs, nr50, nr51, en) = samples[got + k];
                let (l, r) = mix(chs, nr50, nr51, en);
                assert_eq!((pair[0], pair[1]), (l, r), "sample {} mismatch", got + k);
            }
            got += n;
            frame += 1;
        }
        assert_eq!(got, samples.len());
    }

    #[test]
    fn audio_seek_matches_sequential() {
        let samples = tap(20_000);
        let mut enc = AudioEncoder::new();
        enc.push(&samples);
        let blob = enc.finish(FPS_NUM, FPS_DEN);

        let mut seq = AudioDecoder::new(blob.clone()).unwrap();
        let mut a = Vec::new();
        for f in 0..20 {
            seq.frame_into(f, &mut a).unwrap();
        }
        // `a` now holds frame 19; a fresh decoder seeking straight there must agree.
        let mut skp = AudioDecoder::new(blob).unwrap();
        let mut b = Vec::new();
        skp.frame_into(19, &mut b).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn audio_empty_and_garbage() {
        let blob = AudioEncoder::new().finish(FPS_NUM, FPS_DEN);
        let mut dec = AudioDecoder::new(blob).unwrap();
        let mut out = Vec::new();
        assert_eq!(dec.frame_into(0, &mut out).unwrap(), 0);
        assert!(matches!(AudioDecoder::new(vec![1, 2, 3]), Err(DecodeError::Truncated)));
        assert!(matches!(
            AudioDecoder::new(b"NOPE................!".to_vec()),
            Err(DecodeError::BadMagic)
        ));
    }}
