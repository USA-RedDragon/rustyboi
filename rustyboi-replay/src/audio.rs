use std::sync::Arc;

#[cfg(feature = "encode")]
use crate::stream::{brotli_compress_into, write_varint};
use crate::stream::{src_byte, src_take, src_varint, DecodeError, Source};
use rustyboi_mix::{AnalogModel, BlepKernel, Renderer, SampleRecord, NO_TRANSITION, PHASES};

// RBA2: a recording of the APU's OUTPUT as the shared per-sample `SampleRecord`
// contract — per channel, the settled post-DAC level plus the sub-sample phase
// of the LAST level-changing edge in the sample window; plus (nr50, nr51,
// enabled) at the window's end. The decoder reconstructs the EXACT core output
// by running the very same `rustyboi_mix::Renderer` the core now uses live
// (band-limited BLEP step synthesis -> DAC-off fade -> `mix_stereo` -> output
// high-pass), so no APU is needed client-side and no arithmetic is duplicated
// to get there. This REPLACES RBA1, whose decoder ran only `mix_stereo` and so
// reproduced an APPROXIMATE mix — pre-fade and pre-high-pass — never byte-equal
// to what the core actually played.
//
// Container (little-endian):
// ```text
//   magic   "RBA2"      4 bytes
//   rate    u32         sample rate (44100)
//   fps_num u32         fps_den u32     (video frame rate, for frame<->sample)
//   samples u32
//   flags   u8          bits0-1 = AnalogModel (0 Dmg, 1 CgbMgb, 2 Agb); rest 0
//   <brotli stream>     11 planes, in order:
//                         ch1..ch4 level  (f32 palette)
//                         ch1..ch4 phase  (u8 palette: 0..P-1 and 0xFF sentinel)
//                         nr50, nr51, enabled  (u8 palettes)
// ```
// Every plane keeps RBA1's exact machinery: u16 palette_len, palette entries
// (f32le for levels, u8 for phases/regs/enabled), u32 run_count, then run_count
// idx varints followed by run_count run-length varints (SoA — measured better
// than interleaving), the whole stream brotli q10/lgwin22, content-addressed
// dedup upstream. The four level planes and three register planes are therefore
// byte-for-byte an RBA1 encoding; RBA2 adds only the four phase planes. A phase
// plane's palette is {0..P-1, 0xFF = NO_TRANSITION}: the sentinel dominates
// (only a channel that actually stepped this sample carries a phase), so its
// runs RLE to almost nothing and each real transition costs about 6 bits.
//
// ZERO backwards compatibility (by directive): there is no RBA1 decode path.
// The magic bump exists solely as a corruption/sanity check (`BadMagic`); CI
// regenerates every gallery artifact wholesale.

const AUDIO_MAGIC: [u8; 4] = *b"RBA2";
pub const AUDIO_RATE: u32 = 44_100;

/// The `AnalogModel` occupies bits 0-1 of the header flags (it subsumes RBA1's
/// lone AGB bit). Encoded explicitly rather than via `as u8` so the wire value
/// never silently tracks a change in the enum's declaration order.
#[cfg(feature = "encode")]
fn model_flag(model: AnalogModel) -> u8 {
    match model {
        AnalogModel::Dmg => 0,
        AnalogModel::CgbMgb => 1,
        AnalogModel::Agb => 2,
    }
}

/// The decode side of [`model_flag`]. An unknown low-nibble is a malformed file.
fn model_from_flags(flags: u8) -> Result<AnalogModel, DecodeError> {
    match flags & 0b11 {
        0 => Ok(AnalogModel::Dmg),
        1 => Ok(AnalogModel::CgbMgb),
        2 => Ok(AnalogModel::Agb),
        _ => Err(DecodeError::Malformed),
    }
}

/// Reject a phase symbol outside the recorded palette `{0..P-1, 0xFF}` before it
/// reaches the renderer, which would otherwise index its residual table out of
/// bounds. `P` is read from [`rustyboi_mix::PHASES`] so a later fallback to a
/// coarser phase resolution is a one-constant change.
fn checked_phase(p: u8) -> Result<u8, DecodeError> {
    if (p as usize) < PHASES || p == NO_TRANSITION {
        Ok(p)
    } else {
        Err(DecodeError::Malformed)
    }
}

/// Accumulates per-sample [`SampleRecord`]s, emits an RBA2 blob.
#[cfg(feature = "encode")]
#[derive(Default)]
pub struct AudioEncoder {
    samples: Vec<SampleRecord>,
    /// Which analog family the recorded machine is. Constant for a whole
    /// recording (it selects the decoder's fade + high-pass + NR51 semantics),
    /// so it rides in the header flags rather than widening every per-sample
    /// plane's palette.
    model: AnalogModel,
}

#[cfg(feature = "encode")]
impl AudioEncoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the machine's analog family (see [`rustyboi_mix::AnalogModel`]).
    /// Replaces RBA1's `set_agb`: the decoder needs the full model, not just the
    /// AGB bit, to reproduce the DAC-off fade and the model-gated high-pass.
    pub fn set_model(&mut self, model: AnalogModel) {
        self.model = model;
    }

    pub fn push(&mut self, samples: &[SampleRecord]) {
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
        out.push(model_flag(self.model)); // flags: bits0-1 = AnalogModel
        let mut stream = Vec::new();
        for ch in 0..4 {
            write_plane(&mut stream, self.samples.iter().map(|s| s.levels[ch]), |v, o| {
                o.extend_from_slice(&v.to_le_bytes())
            });
        }
        for ch in 0..4 {
            write_plane(&mut stream, self.samples.iter().map(|s| s.phases[ch]), |v, o| o.push(v));
        }
        write_plane(&mut stream, self.samples.iter().map(|s| s.nr50), |v, o| o.push(v));
        write_plane(&mut stream, self.samples.iter().map(|s| s.nr51), |v, o| o.push(v));
        write_plane(&mut stream, self.samples.iter().map(|s| u8::from(s.enabled)), |v, o| o.push(v));
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

/// One decoded plane with a sequential cursor (decode-once reads each fully).
struct Plane<T> {
    palette: Vec<T>,
    idx: Vec<u32>,
    runs: Vec<u32>,
    run: usize,  // current run index
    within: u32, // consumed samples of the current run
}

impl<T: Copy> Plane<T> {
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

/// Decodes an RBA2 blob to interleaved stereo f32.
///
/// Unlike RBA1, this reconstructs the EXACT output the core plays. The whole
/// stream is rendered once, at construction, through the shared
/// [`rustyboi_mix::Renderer`] — literally the same code the core mixes live
/// with, seeded from the header's [`AnalogModel`] — so exactness is structural
/// rather than maintained. The renderer's continuous stages (the band-limited
/// step rings, the DAC-off fade, and the output high-pass) all carry state
/// across samples, which is why the stream is decoded in one forward pass and
/// held in a buffer rather than mixed per seek: filter state at an arbitrary
/// seek target is not reconstructible without decoding everything before it.
///
/// The decoded buffer is ~5.3 MB for a 15 s clip (2 f32 per sample), which is
/// fine — one gallery card plays at a time. `seek_frame` and `frame_into`
/// become trivial slices into it, and their signatures are unchanged so the
/// wasm wrapper and `gallery.js` need no changes.
pub struct AudioDecoder {
    rate: u32,
    fps_num: u32,
    fps_den: u32,
    samples: u32,
    /// The whole recording, decoded once as interleaved stereo (`2 * samples`).
    decoded: Vec<f32>,
    pos: u64, // next sample index (sequential-read cursor for `frame_into`)
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
        let model = model_from_flags(hdr[20])?;
        if rate == 0 || fps_num == 0 || fps_den == 0 {
            return Err(DecodeError::Malformed);
        }
        let compressed: Arc<[u8]> = Arc::from(&bytes[21..]);
        let mut src = Source::new(compressed);
        let f32v = |src: &mut Source| -> Result<f32, DecodeError> {
            let b = src_take(src, 4)?;
            Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        let mut levels = [
            read_plane(&mut src, f32v)?,
            read_plane(&mut src, f32v)?,
            read_plane(&mut src, f32v)?,
            read_plane(&mut src, f32v)?,
        ];
        let mut phases = [
            read_plane(&mut src, src_byte)?,
            read_plane(&mut src, src_byte)?,
            read_plane(&mut src, src_byte)?,
            read_plane(&mut src, src_byte)?,
        ];
        let mut nr50 = read_plane(&mut src, src_byte)?;
        let mut nr51 = read_plane(&mut src, src_byte)?;
        let mut enabled = read_plane(&mut src, src_byte)?;

        // Decode-once: replay every record through the one shared renderer.
        let kernel = BlepKernel::build();
        let mut renderer = Renderer::new(model);
        let mut decoded = Vec::with_capacity(samples as usize * 2);
        for _ in 0..samples {
            let rec = SampleRecord {
                levels: [
                    levels[0].next()?,
                    levels[1].next()?,
                    levels[2].next()?,
                    levels[3].next()?,
                ],
                phases: [
                    checked_phase(phases[0].next()?)?,
                    checked_phase(phases[1].next()?)?,
                    checked_phase(phases[2].next()?)?,
                    checked_phase(phases[3].next()?)?,
                ],
                nr50: nr50.next()?,
                nr51: nr51.next()?,
                enabled: enabled.next()? != 0,
            };
            let (l, r) = renderer.render(&kernel, &rec);
            decoded.push(l);
            decoded.push(r);
        }
        Ok(Self { rate, fps_num, fps_den, samples, decoded, pos: 0 })
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

    /// Position playback at the start of video frame `i`.
    pub fn seek_frame(&mut self, i: u32) {
        self.pos = self.frame_sample(i).min(u64::from(self.samples));
    }

    /// Copy video frame `i`'s span of samples as interleaved stereo into `out`
    /// (cleared first). Frames are consumed sequentially unless `seek_frame`
    /// repositions. Returns the number of stereo pairs.
    pub fn frame_into(&mut self, i: u32, out: &mut Vec<f32>) -> Result<usize, DecodeError> {
        out.clear();
        let start = self.frame_sample(i);
        if start != self.pos {
            self.seek_frame(i);
        }
        let end = self.frame_sample(i + 1).min(u64::from(self.samples));
        let n = end.saturating_sub(self.pos) as usize;
        let (a, b) = ((self.pos * 2) as usize, (end * 2) as usize);
        out.extend_from_slice(&self.decoded[a..b]);
        self.pos = end;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FPS_DEN, FPS_NUM};
    use rustyboi_mix::dac_analog;

    const MODELS: [AnalogModel; 3] = [AnalogModel::Dmg, AnalogModel::CgbMgb, AnalogModel::Agb];

    /// A varied but VALID record stream: two squares at different periods, a
    /// wave-ish ramp with DAC-off (`0.0`) stretches, an LFSR noise channel, a
    /// panning + volume change partway, and a one-sample master-disable. Phases
    /// are `NO_TRANSITION` when a channel holds and a rotating sub-sample
    /// position when it steps — the exact alphabet the encoder must round-trip.
    fn records(n: usize) -> Vec<SampleRecord> {
        let mut recs = Vec::with_capacity(n);
        let mut prev = [f32::NAN; 4]; // force a transition on sample 0
        let mut lfsr: u16 = 0x7fff;
        for i in 0..n {
            let l0 = if (i / 50) % 2 == 0 { dac_analog(0) } else { dac_analog(15) };
            let l1 = if (i / 33) % 2 == 0 { dac_analog(3) } else { dac_analog(12) };
            let l2 = if (i / 400) % 3 == 2 { 0.0 } else { dac_analog(((i / 7) % 16) as u8) };
            // Galois LFSR step per sample -> a genuine noise transition stream.
            let carry = lfsr & 1;
            lfsr >>= 1;
            if carry != 0 {
                lfsr ^= 0xb400;
            }
            let l3 = if lfsr & 1 == 0 { dac_analog(0) } else { dac_analog(15) };
            let levels = [l0, l1, l2, l3];
            let mut phases = [NO_TRANSITION; 4];
            for c in 0..4 {
                if levels[c].to_bits() != prev[c].to_bits() {
                    phases[c] = ((i * 13 + c * 7) % PHASES) as u8;
                }
            }
            let nr50 = if i < n / 2 { 0x77 } else { 0x34 };
            let nr51 = if i < n / 3 { 0xff } else { 0xf1 };
            let enabled = i % 900 != 3;
            recs.push(SampleRecord { levels, phases, nr50, nr51, enabled });
            prev = levels;
        }
        recs
    }

    /// A melodic stream: sparse transitions (two squares + a slow wave), the
    /// common case whose phase planes RLE to almost nothing.
    fn melodic(n: usize) -> Vec<SampleRecord> {
        let mut recs = Vec::with_capacity(n);
        let mut prev = [f32::NAN; 4];
        for i in 0..n {
            let l0 = if (i / 50) % 2 == 0 { dac_analog(2) } else { dac_analog(13) };
            let l1 = if (i / 75) % 2 == 0 { dac_analog(5) } else { dac_analog(10) };
            let l2 = dac_analog(((i / 220) % 16) as u8);
            let levels = [l0, l1, l2, 0.0];
            let mut phases = [NO_TRANSITION; 4];
            for c in 0..4 {
                if levels[c].to_bits() != prev[c].to_bits() {
                    // A near-constant sub-sample phase, as a fixed pitch has.
                    phases[c] = ((i / 50) % 3) as u8;
                }
            }
            recs.push(SampleRecord { levels, phases, nr50: 0x77, nr51: 0xff, enabled: true });
            prev = levels;
        }
        recs
    }

    /// A noise-heavy stream: all four channels driven by fast LFSRs, a
    /// transition nearly every sample with near-random phase symbols — the
    /// incompressible worst case that grounds the no-growth size gate.
    fn noisy(n: usize) -> Vec<SampleRecord> {
        let mut recs = Vec::with_capacity(n);
        let mut lfsr: u32 = 0xace1_2345;
        for _ in 0..n {
            let mut levels = [0.0f32; 4];
            let mut phases = [NO_TRANSITION; 4];
            for c in 0..4 {
                lfsr = lfsr.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                levels[c] = dac_analog(((lfsr >> 12) & 0xf) as u8);
                phases[c] = ((lfsr >> 20) % PHASES as u32) as u8;
            }
            recs.push(SampleRecord { levels, phases, nr50: 0x77, nr51: 0xff, enabled: true });
        }
        recs
    }

    /// A reference render of the same records straight through a fresh renderer
    /// — the byte-exact contract the decoder must reproduce.
    fn reference(recs: &[SampleRecord], model: AnalogModel) -> Vec<f32> {
        let kernel = BlepKernel::build();
        let mut r = Renderer::new(model);
        let mut out = Vec::with_capacity(recs.len() * 2);
        for rec in recs {
            let (l, rr) = r.render(&kernel, rec);
            out.push(l);
            out.push(rr);
        }
        out
    }

    /// Pull the whole stream out of a decoder frame-by-frame, exactly as the
    /// player does.
    fn drain(dec: &mut AudioDecoder) -> Vec<f32> {
        let mut got = Vec::new();
        let mut buf = Vec::new();
        let mut frame = 0u32;
        loop {
            let n = dec.frame_into(frame, &mut buf).unwrap();
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf);
            frame += 1;
        }
        got
    }

    /// Encode -> decode-once -> frame pull bit-equals a reference renderer run
    /// over the same records, on every analog model. This is the local
    /// byte-exact contract (the cross-core version is WP4's `exact.rs`).
    #[test]
    fn round_trip_bit_equals_a_reference_renderer_on_every_model() {
        for model in MODELS {
            let recs = records(30_000);
            let mut enc = AudioEncoder::new();
            enc.set_model(model);
            enc.push(&recs);
            let blob = enc.finish(FPS_NUM, FPS_DEN);

            let mut dec = AudioDecoder::new(blob).unwrap();
            assert_eq!(dec.sample_count(), recs.len() as u32, "{model:?} sample count");
            assert_eq!(dec.sample_rate(), AUDIO_RATE, "{model:?} sample rate");

            let want = reference(&recs, model);
            let got = drain(&mut dec);
            assert_eq!(got.len(), want.len(), "{model:?} length");
            for (k, (a, b)) in got.iter().zip(&want).enumerate() {
                assert_eq!(a.to_bits(), b.to_bits(), "{model:?} interleaved value {k}");
            }
        }
    }

    /// A fresh decoder seeking straight to a frame agrees with a sequential
    /// decoder that walked there, and both agree with the decoded buffer.
    #[test]
    fn seek_matches_sequential() {
        let recs = records(20_000);
        let mut enc = AudioEncoder::new();
        enc.set_model(AnalogModel::CgbMgb);
        enc.push(&recs);
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
        assert!(!a.is_empty(), "frame 19 should carry samples");
    }

    /// Empty recording decodes to zero samples; anything that is not an RBA2
    /// container is rejected without touching the renderer.
    #[test]
    fn empty_and_bad_magic() {
        let blob = AudioEncoder::new().finish(FPS_NUM, FPS_DEN);
        let mut dec = AudioDecoder::new(blob).unwrap();
        assert_eq!(dec.sample_count(), 0);
        let mut out = Vec::new();
        assert_eq!(dec.frame_into(0, &mut out).unwrap(), 0);

        // Too short for even the 21-byte header.
        assert!(matches!(AudioDecoder::new(vec![1, 2, 3]), Err(DecodeError::Truncated)));
        // Right length, wrong magic (a stale RBA1 blob is exactly this case).
        assert!(matches!(
            AudioDecoder::new(b"RBA1................!".to_vec()),
            Err(DecodeError::BadMagic)
        ));
        assert!(matches!(
            AudioDecoder::new(b"XXXX................!".to_vec()),
            Err(DecodeError::BadMagic)
        ));
        // Valid magic + header, but the compressed plane stream is missing.
        let mut hdr = AUDIO_MAGIC.to_vec();
        hdr.extend_from_slice(&AUDIO_RATE.to_le_bytes());
        hdr.extend_from_slice(&FPS_NUM.to_le_bytes());
        hdr.extend_from_slice(&FPS_DEN.to_le_bytes());
        hdr.extend_from_slice(&1u32.to_le_bytes()); // claims one sample
        hdr.push(0); // Dmg
        // A header claiming samples with no plane stream must be rejected (the
        // exact variant depends on brotli's handling of the empty tail), never
        // silently decoded to garbage.
        assert!(AudioDecoder::new(hdr).is_err());
    }

    /// Size discipline: report RBA2's total against an equivalent RBA1-style
    /// encoding (the same level + register planes, no phase planes) on both a
    /// melodic and a noise-heavy stream. No hard assert — the numbers ground
    /// WP5's no-growth gate; the sanity check is only that it round-trips.
    #[test]
    fn size_report_phase_plane_share() {
        for (label, recs) in [("melodic", melodic(44_100)), ("noise", noisy(44_100))] {
            let mut enc = AudioEncoder::new();
            enc.set_model(AnalogModel::CgbMgb);
            enc.push(&recs);
            let blob = enc.finish(FPS_NUM, FPS_DEN);
            let rba2 = blob.len();

            // RBA1-equivalent: the 4 level + 3 register planes only, same
            // machinery, plus the 21-byte header.
            let mut lr = Vec::new();
            for ch in 0..4 {
                write_plane(&mut lr, recs.iter().map(|s| s.levels[ch]), |v, o| {
                    o.extend_from_slice(&v.to_le_bytes())
                });
            }
            write_plane(&mut lr, recs.iter().map(|s| s.nr50), |v, o| o.push(v));
            write_plane(&mut lr, recs.iter().map(|s| s.nr51), |v, o| o.push(v));
            write_plane(&mut lr, recs.iter().map(|s| u8::from(s.enabled)), |v, o| o.push(v));
            let mut lr_c = Vec::new();
            brotli_compress_into(&lr, &mut lr_c);
            let rba1_equiv = 21 + lr_c.len();

            // The four phase planes in isolation, for their marginal cost.
            let mut ph = Vec::new();
            for ch in 0..4 {
                write_plane(&mut ph, recs.iter().map(|s| s.phases[ch]), |v, o| o.push(v));
            }
            let mut ph_c = Vec::new();
            brotli_compress_into(&ph, &mut ph_c);

            let marginal = rba2 as i64 - rba1_equiv as i64;
            let share = 100.0 * marginal as f64 / rba2 as f64;
            println!(
                "[rba2-size:{label}] samples={} RBA2={rba2}B RBA1equiv={rba1_equiv}B \
                 phase_marginal={marginal}B ({share:.1}% of RBA2) \
                 phase_planes_isolated={}B bytes_per_sample={:.3}",
                recs.len(),
                ph_c.len(),
                rba2 as f64 / recs.len() as f64,
            );

            let dec = AudioDecoder::new(blob).unwrap();
            assert_eq!(dec.sample_count(), recs.len() as u32);
        }
    }
}
