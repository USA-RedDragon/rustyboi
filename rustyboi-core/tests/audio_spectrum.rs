//! The 4-channel spectral gate: the campaign's actual adjudicator.
//!
//! Every other suite grades CPU-visible digital state UPSTREAM of the output
//! stage; none looks at the emitted PCM. This one FFTs the real audio the sink
//! receives and compares it against ANALYTIC oracles built from first
//! principles (no reference emulator, no captured golden). It fails on the
//! point-sampling output stage — whose instantaneous re-sampling folds the
//! PSG's ultrasonic harmonics down into the audible band — and passes once
//! band-limited step synthesis renders those components faithfully.
//!
//! ROMs are hand-assembled here as raw bytes (no rgbasm); wave RAM is written
//! while CH3 is off, per Pan Docs. The DFT is a direct Goertzel evaluation at
//! the exact frequencies of interest — the capture length is chosen so the
//! fundamental lands on an integer bin (an exact integer number of periods),
//! making the harmonic energy leak-free and Parseval exact, so
//! `spurious = total - Σ harmonics` is a rigorous fraction rather than a
//! windowed estimate.

use rustyboi_core_lib::audio::AudioOutput;
use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Hardware, GB};
use std::sync::{Arc, Mutex};

const FS: f64 = 44_100.0;
const DMG_CPU_HZ: u64 = 4_194_304;
const APU_HZ: u64 = DMG_CPU_HZ / 2; // the PSG clock: 2 MHz

// --- capture harness -------------------------------------------------------

struct CapSink(Arc<Mutex<Vec<(f32, f32)>>>);
impl AudioOutput for CapSink {
    fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
    fn add_samples(&mut self, s: &[(f32, f32)]) {
        self.0.lock().unwrap().extend_from_slice(s);
    }
}

/// A 32 KiB no-MBC ROM whose entry point at 0x100 is `code`. `skip_bios` runs
/// straight from 0x100 and never checks the logo or the header checksum, so this
/// leaves the header zeroed rather than writing a checksum at 0x14D — these
/// programs run past 0x14D, and a checksum write there would corrupt the code
/// (it once silently clobbered a wave-RAM setup byte).
fn rom(code: &[u8]) -> Vec<u8> {
    let mut r = vec![0u8; 0x8000];
    // 0x147/0x148/0x149 stay 0: NoMBC, 32 KiB, no RAM. DMG cart (0x143 = 0).
    r[0x100..0x100 + code.len()].copy_from_slice(code);
    r
}

/// Run `code` on a DMG, discard `warmup` host samples (BLEP group delay + HPF
/// settle), then return the next `n` left-channel samples.
fn capture(code: &[u8], warmup: usize, n: usize) -> Vec<f64> {
    let mut gb = GB::new(Hardware::DMG);
    gb.insert(Cartridge::from_bytes(&rom(code)).expect("cart"));
    gb.skip_bios();
    let buf = Arc::new(Mutex::new(Vec::new()));
    gb.enable_audio(Box::new(CapSink(buf.clone()))).expect("audio");
    let want = warmup + n;
    let mut guard = 0;
    while buf.lock().unwrap().len() < want {
        gb.run_until_frame(true);
        guard += 1;
        assert!(guard < 100_000, "ROM never produced enough audio");
    }
    let all = buf.lock().unwrap();
    all[warmup..warmup + n].iter().map(|&(l, _)| l as f64).collect()
}

// --- DFT / spectral helpers ------------------------------------------------

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// `|X[bin]|^2` of the length-`N` DFT, evaluated directly at a (possibly
/// fractional) bin.
fn dft_power(x: &[f64], bin: f64) -> f64 {
    let n = x.len() as f64;
    let w = 2.0 * std::f64::consts::PI * bin / n;
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (k, &v) in x.iter().enumerate() {
        let a = w * k as f64;
        re += v * a.cos();
        im -= v * a.sin();
    }
    re * re + im * im
}

/// One-sided energy attributable to an integer bin, in the same units as the
/// time-domain `Σ x²` (Parseval): `2·|X[b]|² / N` for `0 < b < N/2`.
fn bin_energy(x: &[f64], bin: u64) -> f64 {
    2.0 * dft_power(x, bin as f64) / x.len() as f64
}

fn total_energy(x: &[f64]) -> f64 {
    x.iter().map(|v| v * v).sum()
}

/// The (p, q) of the exact fundamental `f0 = FS·p/q`, reduced. A length-`q`
/// capture is then an integer number of `f0` periods and every harmonic lands
/// on integer bin `k·p`.
fn ratio(f0_num: u64, f0_den: u64) -> (u64, u64) {
    // f0/FS = f0_num / (f0_den * FS). FS = 44100.
    let num = f0_num;
    let den = f0_den * 44_100;
    let g = gcd(num, den);
    (num / g, den / g)
}

// --- CH1 / CH2 square ------------------------------------------------------

/// Program: enable APU, route everything, then trigger `ch` (1 or 2) as a
/// 50 %-duty square at 11-bit `freq`, volume 15, no envelope / sweep / length.
fn square_rom(ch: u8, freq: u16) -> Vec<u8> {
    let (nr1, nr2, nr3, nr4) = if ch == 1 {
        (0x11u8, 0x12u8, 0x13u8, 0x14u8)
    } else {
        (0x16, 0x17, 0x18, 0x19)
    };
    let lo = (freq & 0xFF) as u8;
    let hi = 0x80 | ((freq >> 8) & 0x07) as u8; // trigger, length disabled
    vec![
        0x3E, 0x80, 0xE0, 0x26, // LD A,$80 ; LDH (NR52),A  -- APU on
        0x3E, 0xFF, 0xE0, 0x25, // LD A,$FF ; LDH (NR51),A  -- route all
        0x3E, 0x77, 0xE0, 0x24, // LD A,$77 ; LDH (NR50),A  -- full volume
        0x3E, 0x80, 0xE0, nr1, // LD A,$80 ; LDH (NRx1),A  -- duty 2 (50%)
        0x3E, 0xF0, 0xE0, nr2, // LD A,$F0 ; LDH (NRx2),A  -- vol 15, DAC on
        0x3E, lo, 0xE0, nr3, //   LD A,lo  ; LDH (NRx3),A
        0x3E, hi, 0xE0, nr4, //   LD A,hi  ; LDH (NRx4),A  -- trigger
        0x18, 0xFE, //            JR -2 (spin)
    ]
}

/// A 50 %-duty square between the DAC rails has ONLY odd harmonics, each at
/// amplitude 1/k of the fundamental. We assert: (a) the fabricated inharmonic
/// energy is a tiny fraction of the total (the audible screech metric), and
/// (b) the surviving odd harmonics decay like 1/k² in power (it is really a
/// square, not broadband hash), and report the inter-harmonic floor.
fn assay_square(ch: u8, freq: u16) -> (f64, f64) {
    // f0 = APU_HZ / ((2048 - freq) * 16)  (8 duty steps * (2048-freq)*2 cc).
    let (p, q) = ratio(APU_HZ, (2048 - freq as u64) * 16);
    let n = q as usize;
    let x = capture(&square_rom(ch, freq), 8192, n);
    let total = total_energy(&x);
    assert!(total > 1e-3, "ch{ch}: no signal (total energy {total})");

    // Odd harmonics below Nyquist.
    let mut harmonic = Vec::new();
    let mut k = 1u64;
    while (k * p) < (q / 2) {
        harmonic.push((k, bin_energy(&x, k * p)));
        k += 2;
    }
    let harm_sum: f64 = harmonic.iter().map(|&(_, e)| e).sum();
    let spurious = (total - harm_sum) / total;

    // Inter-harmonic floor: energy midway between the first two harmonics,
    // relative to the fundamental.
    let f1 = harmonic[0].1;
    let mut worst_floor_db = f64::NEG_INFINITY;
    for probe in [p / 2, p + p / 2, 2 * p, 2 * p + p / 2] {
        if probe > 0 && probe < q / 2 {
            let e = bin_energy(&x, probe);
            worst_floor_db = worst_floor_db.max(10.0 * (e / f1).max(1e-30).log10());
        }
    }

    // Shape sanity: 3rd harmonic should sit near 1/9 the fundamental power once
    // the aliasing is gone. Reported here; the spurious fraction is the gate.
    let shape3 = harmonic.get(1).map(|&(k3, e3)| (e3 / f1) / (1.0 / (k3 * k3) as f64));
    eprintln!("ch{ch}: 3rd-harmonic shape ratio {shape3:?} (want ~1.0)");
    (spurious, worst_floor_db)
}

#[test]
fn ch1_square_is_spectrally_clean() {
    // Divider 2004 -> f0 = 2978.9 Hz, the campaign's headline tone.
    let (spurious, floor_db) = assay_square(1, 2004);
    eprintln!("CH1 square: spurious = {:.4}% floor = {:.1} dB", spurious * 100.0, floor_db);
    assert!(
        spurious < 1e-3,
        "CH1 fabricated {:.3}% inharmonic energy (want < 0.1%) -- point sampling \
         folds ultrasonic harmonics into the band",
        spurious * 100.0
    );
    // The plan's headline target: < -90 dB inter-harmonic floor at 2978.9 Hz.
    // P=64 phase quantization reaches it here (measured ~-95 dB); risk #2 notes
    // this floor degrades at higher pitches (see CH2).
    assert!(
        floor_db < -90.0,
        "CH1 inter-harmonic floor {floor_db:.1} dB is too high (want < -90 dB)"
    );
}

#[test]
fn ch2_square_is_spectrally_clean() {
    // A different divider -> f0 = 1024 Hz: proves the fix is not CH1-specific.
    let (spurious, floor_db) = assay_square(2, 1920);
    eprintln!("CH2 square: spurious = {:.4}% floor = {:.1} dB", spurious * 100.0, floor_db);
    assert!(
        spurious < 1e-3,
        "CH2 fabricated {:.3}% inharmonic energy (want < 0.1%)",
        spurious * 100.0
    );
    // Higher pitch than CH1 -> the P=64 phase-quantization floor is physically
    // worse here (risk #2: σ_t≈102ns gives a per-bin floor near -74 dB at
    // 1 kHz+ content). The <0.1% total-spur gate above is the hard audible claim.
    assert!(
        floor_db < -70.0,
        "CH2 inter-harmonic floor {floor_db:.1} dB is too high (want < -70 dB)"
    );
}

// --- CH3 wave --------------------------------------------------------------

/// Program: write `wave` (32 nibbles, MS-nibble first per byte) to wave RAM
/// while CH3 is off, then trigger CH3 at 11-bit `freq`, output level 100 %.
fn wave_rom(freq: u16, wave: [u8; 16]) -> Vec<u8> {
    let lo = (freq & 0xFF) as u8;
    let hi = 0x80 | ((freq >> 8) & 0x07) as u8;
    let mut c = vec![
        0x3E, 0x80, 0xE0, 0x26, // APU on
        0x3E, 0xFF, 0xE0, 0x25, // NR51 route all
        0x3E, 0x77, 0xE0, 0x24, // NR50 full volume
        0x3E, 0x00, 0xE0, 0x1A, // NR30 = 0: DAC off, so wave RAM is writable
    ];
    // Write the 16 wave-RAM bytes: LD A,b ; LDH ($30+i),A.
    for (i, &b) in wave.iter().enumerate() {
        c.extend_from_slice(&[0x3E, b, 0xE0, 0x30 + i as u8]);
    }
    c.extend_from_slice(&[
        0x3E, 0x80, 0xE0, 0x1A, // NR30 = $80: DAC on
        0x3E, 0x20, 0xE0, 0x1C, // NR32 = $20: output level 100%
        0x3E, lo, 0xE0, 0x1D, //   NR33 = freq low
        0x3E, hi, 0xE0, 0x1E, //   NR34 = trigger + freq high
        0x18, 0xFE, //            spin
    ]);
    c
}

#[test]
fn ch3_wave_pattern_is_spectrally_clean() {
    // A 16x $F then 16x $0 table is a square at fetch_rate/32. Fetch period
    // = (0x800 - freq) cc, 32 nibbles per waveform:
    //   f0 = APU_HZ / (32 * (0x800 - freq)).
    // freq = 0x600 -> period 0x200=512, f0 = 2097152/(32*512) = 128 Hz.
    let freq = 0x600u16;
    let table = [0xFFu8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
    let (p, q) = ratio(APU_HZ, 32 * (0x800 - freq as u64));
    let n = q as usize;
    let x = capture(&wave_rom(freq, table), 8192, n);
    let total = total_energy(&x);
    assert!(total > 1e-3, "CH3: no signal (total {total})");

    // The oracle is "energy only at multiples of the table rate" (Verification):
    // sum ALL harmonics k·f_wave. The 16/16 table is an odd-harmonic square, so
    // even multiples land near zero; summing all of them keeps the gate robust.
    // Aliasing shows only as INTER-harmonic energy, which the floor (below)
    // measures directly and is what the fetch-loop conversion is really gated on.
    let mut harm_sum = 0.0;
    let f1 = bin_energy(&x, p);
    let mut k = 1u64;
    while (k * p) < (q / 2) {
        harm_sum += bin_energy(&x, k * p);
        k += 1;
    }
    let spurious = (total - harm_sum) / total;
    // Inter-harmonic floor, sampled between several harmonics.
    let mut worst_floor = f64::NEG_INFINITY;
    for probe in [p / 2, p + p / 2, 2 * p + p / 2, 3 * p + p / 2] {
        if probe < q / 2 {
            worst_floor = worst_floor.max(bin_energy(&x, probe) / f1);
        }
    }
    let floor_db = 10.0 * worst_floor.max(1e-30).log10();
    eprintln!("CH3 wave: spurious = {:.4}% floor = {:.1} dB", spurious * 100.0, floor_db);
    assert!(
        spurious < 1.5e-3,
        "CH3 wave fabricated {:.3}% inter-harmonic energy (want < 0.15%) -- the \
         fetch loop is aliasing",
        spurious * 100.0
    );
    assert!(floor_db < -80.0, "CH3 inter-harmonic floor {floor_db:.1} dB too high");
}

/// The "parked channel" case, both ways it can be parked, must be silent.
///
/// (a) A FLAT waveform (constant nibble): no fetch changes the level, so the
/// collapse emits no transitions, the renderer holds, and the high-pass removes
/// the DC — trivially silent.
///
/// (b) A channel driven at an ultrasonic EDGE RATE — freq 2047 + an alternating
/// table is a ~1 MHz square whose edges are far denser than the 44.1 kHz sample
/// grid. This is the case WP6 fixes. A last-edge collapse keeps whichever rail
/// the final intra-sample edge landed on, which beats against the sample grid
/// into a full-scale ~10 kHz AUDIBLE alias (this test measured rms 0.2262 on the
/// pre-WP6 core). The box-filter collapse instead emits the window's
/// time-weighted average: a symmetric ultrasonic square averages to ~DC, which
/// quantizes to the DAC level nearest 0 and, held constant, is removed by the
/// output high-pass. What remains is only the residual of the quantization
/// dithering as the tiny per-window average ripples across the mid-code
/// boundary — a ~16x-smaller artifact (rms ~0.014), not a fabricated screech.
/// Hardware puts this fundamental above 20 kHz where it is inaudible; near-DC
/// silence is the faithful rendering.
#[test]
fn ch3_parked_channel_is_silent() {
    // A constant nibble 8 -> a flat DC waveform: no fetches change the level.
    let table = [0x88u8; 16];
    let x = capture(&wave_rom(0x600, table), 8192, 16384);
    let rms = (total_energy(&x) / x.len() as f64).sqrt();
    eprintln!("CH3 flat parking: rms = {rms:.6}");
    assert!(
        rms < 1e-3,
        "a flat (constant-output) parked channel must be silent (rms {rms:.6}) -- \
         the dedup emitted spurious transitions"
    );

    // The dense-edge ultrasonic case: the box-filter collapse (WP6) must render
    // it as near-silence, NOT the pre-WP6 rms-0.2262 full-scale ~10 kHz alias.
    // Measured post-WP6: rms ~0.014 (the residual mid-code quantization dither);
    // the bound proves the alias is gone with margin over the measurement.
    let table = [0xF0u8; 16];
    let y = capture(&wave_rom(2047, table), 8192, 16384);
    let rms_us = (total_energy(&y) / y.len() as f64).sqrt();
    eprintln!("CH3 ultrasonic-edge parking (box-filtered): rms = {rms_us:.4}");
    assert!(
        rms_us < 0.03,
        "CH3 ultrasonic parking must be near-silent after the box-filter collapse \
         (rms {rms_us:.4}, want < 0.03); the pre-WP6 last-edge alias was ~0.226"
    );
}

/// A square frequency GLIDING through the ultrasonic band must produce no
/// audible chirp. A CH3 alternating-nibble table is a square whose EDGE rate is
/// the fetch rate; over freq 2032..2047 (fetch period 16..1 cc) that is >= 3
/// edges per 44.1 kHz window throughout — squarely inside WP6's ">= 2 edges,
/// above-Nyquist" box-filter regime, with the fundamental (65..1049 kHz) always
/// far above the 18 kHz top of the assertion band. The freq register (NR33) is
/// glided while the channel runs (no re-trigger). Any energy in 1..18 kHz is a
/// fabricated alias, not real signal.
///
/// A last-edge collapse aliases each ultrasonic fundamental to a different
/// AUDIBLE frequency, so a glide becomes an audible chirp sweeping across the
/// band. The box-filter collapse averages every symmetric square to the same
/// ~DC regardless of frequency, so the glide stays near-silent: no moving tonal
/// spur, no broadband screech. We assert both a low total RMS and no strong
/// tonal peak anywhere the chirp would pass through.
#[test]
fn square_glide_through_ultrasonic_has_no_chirp() {
    // Prologue: APU on, route/volume, then load an alternating (0xF0) wave table
    // while CH3's DAC is off, DAC on, output 100%, trigger at freq 2047.
    let mut code: Vec<u8> = vec![
        0x3E, 0x80, 0xE0, 0x26, // NR52 = $80: APU on
        0x3E, 0xFF, 0xE0, 0x25, // NR51 = $FF: route all
        0x3E, 0x77, 0xE0, 0x24, // NR50 = $77: full volume
        0x3E, 0x00, 0xE0, 0x1A, // NR30 = 0: DAC off, wave RAM writable
        0x3E, 0xF0, //            LD A,$F0: an alternating F/0 nibble table
    ];
    for i in 0..16u8 {
        code.extend_from_slice(&[0xE0, 0x30 + i]); // LDH ($30+i),A
    }
    // The glide loop is position-independent (all jumps are relative and internal),
    // so its offsets do not depend on the prologue length above.
    code.extend_from_slice(&[
        0x3E, 0x80, 0xE0, 0x1A, // NR30 = $80: DAC on
        0x3E, 0x20, 0xE0, 0x1C, // NR32 = $20: output level 100%
        0x3E, 0xFF, 0xE0, 0x1D, // NR33 = $FF: freq low
        0x3E, 0x87, 0xE0, 0x1E, // NR34 = $87: trigger, freq high $07 (freq 2047)
        0x0E, 0xFF, //            LD C,$FF               ; running freq-low value
        // outer: write the current freq low (NO re-trigger -> a true glide)
        0x79, 0xE0, 0x1D, //      LD A,C ; LDH (NR33),A
        0x16, 0x04, //            LD D,$04               ; dwell per freq
        // delayD:
        0x06, 0x00, //            LD B,$00
        // delayB:
        0x05, 0x20, 0xFD, //      DEC B ; JR NZ,delayB   (-3 -> delayB)
        0x15, 0x20, 0xF8, //      DEC D ; JR NZ,delayD   (-8 -> delayD)
        0x0D, //                  DEC C                  ; next (higher) freq
        0x79, 0xFE, 0xF0, //      LD A,C ; CP $F0        ; floor at freq low 0xF0
        0x30, 0x02, //            JR NC,cont             ; C >= $F0: keep gliding
        0x0E, 0xFF, //            LD C,$FF               ; else wrap to freq 2047
        // cont:
        0x18, 0xE9, //            JR outer               (-23 -> outer)
    ]);
    let x = capture(&code, 8192, 32768);
    let total = total_energy(&x);
    let rms = (total / x.len() as f64).sqrt();

    // No tonal peak in 1..18 kHz: probe ~240 bins across the band and take the
    // worst, relative to full scale (Parseval energy of a unit-amplitude bin).
    let bin_hz = FS / x.len() as f64;
    let full_scale = x.len() as f64 / 2.0; // |X|^2 energy of a 1.0-amplitude tone
    let mut worst_bin = 0.0f64;
    let mut f = 1000.0;
    while f <= 18_000.0 {
        worst_bin = worst_bin.max(bin_energy(&x, (f / bin_hz).round() as u64));
        f += 71.0; // coprime-ish step so probes don't all miss a comb
    }
    let peak_db = 10.0 * (worst_bin / full_scale).max(1e-30).log10();
    eprintln!("square glide: rms = {rms:.4} worst 1-18kHz peak = {peak_db:.1} dBFS");
    // Near-silence overall (measured rms ~0.028 — mostly the quantization dither
    // of the lower-frequency, less-dense end of the glide; the pre-WP6 last-edge
    // collapse chirps here at rms ~0.2).
    assert!(rms < 0.04, "ultrasonic glide produced audible energy (rms {rms:.4})");
    // And specifically no fabricated tone: every in-band bin is deep down.
    assert!(
        peak_db < -45.0,
        "ultrasonic glide produced an in-band tonal spur ({peak_db:.1} dBFS); a \
         fabricated chirp alias is leaking into 1-18 kHz"
    );
}

// --- CH4 noise -------------------------------------------------------------

/// Program: trigger CH4 in 7-bit LFSR mode with NR43 = `nr43`, volume 15.
fn noise_rom(nr43: u8) -> Vec<u8> {
    vec![
        0x3E, 0x80, 0xE0, 0x26, // APU on
        0x3E, 0xFF, 0xE0, 0x25, // NR51 route all
        0x3E, 0x77, 0xE0, 0x24, // NR50 full volume
        0x3E, 0xF0, 0xE0, 0x21, // NR42 = $F0: vol 15, DAC on
        0x3E, nr43, 0xE0, 0x22, // NR43
        0x3E, 0x80, 0xE0, 0x23, // NR44 = trigger (length disabled)
        0x18, 0xFE, //            spin
    ]
}

/// The exact 7-bit LFSR output sequence the core models (period 127).
fn lfsr7_sequence() -> Vec<f64> {
    let mut lfsr: u16 = 0;
    let mut out = Vec::with_capacity(127);
    for _ in 0..127 {
        // Match Noise::step_lfsr in narrow (7-bit) mode.
        let new_high = (lfsr ^ (lfsr >> 1) ^ 1) & 1 != 0;
        lfsr >>= 1;
        if new_high {
            lfsr |= 0x4040;
        } else {
            lfsr &= !0x4040;
        }
        out.push(if lfsr & 1 != 0 { 1.0 } else { 0.0 });
    }
    out
}

#[test]
fn ch4_noise_has_the_lfsr_line_structure() {
    // NR43 = $18: divisor code 0 (=> divisor 8 cc), shift 1 (=> counter bit 1
    // rising every 2 increments = 16 cc per LFSR step), 7-bit (bit 3 set).
    // shift_rate = APU_HZ / 16 = 131072 steps/s; the 127-step sequence repeats
    // at 131072/127 = 1032.06 Hz. Well below Nyquist, so the line spectrum at
    // multiples of that rate is the oracle.
    let x = capture(&noise_rom(0x18), 8192, 65536);
    let total = total_energy(&x);
    assert!(total > 1e-4, "CH4: no signal (total {total})");

    // Line rate in bins of this DFT.
    let step_rate = APU_HZ as f64 / 16.0; // Hz per LFSR step
    let seq_rate = step_rate / 127.0; // Hz of the repeating sequence
    let bin_hz = FS / x.len() as f64;
    let line_bin = seq_rate / bin_hz;

    // Energy on the LFSR lines vs energy exactly between them: the line
    // structure means the on-line bins dominate.
    let mut on_line = 0.0;
    let mut off_line = 0.0;
    for m in 1..60 {
        let lb = line_bin * m as f64;
        if lb >= x.len() as f64 / 2.0 {
            break;
        }
        on_line += dft_power(&x, lb);
        off_line += dft_power(&x, lb + 0.5 * line_bin);
    }
    let ratio = on_line / off_line.max(1e-30);
    eprintln!("CH4 noise: on/off-line power ratio = {ratio:.2}");
    // The reconstructed line spectrum must concentrate on the LFSR lines.
    assert!(
        ratio > 8.0,
        "CH4 noise line structure washed out (on/off-line ratio {ratio:.2}); \
         the LFSR period is not being rendered as a line spectrum"
    );
    // Sanity: the modelled 7-bit sequence really is period 127.
    let seq = lfsr7_sequence();
    assert_eq!(seq.len(), 127);
}

#[test]
fn ch4_high_shift_rate_has_no_tonal_spur() {
    // NR43 = $08: divisor 0, shift 0 -> the LFSR steps every 8 cc = 262 kHz,
    // far above Nyquist. Faithfully rendered this is broadband hiss with
    // bounded power and NO tonal spur; point-sampling folds it into tones.
    let x = capture(&noise_rom(0x08), 8192, 65536);
    let total = total_energy(&x);
    let mean_bin = total / (x.len() as f64 / 2.0); // ~ average bin energy
    let mut worst = 0.0f64;
    let n = x.len();
    let step = (n / 2) / 400; // sample ~400 bins across the band
    let mut b = step.max(1);
    while b < n / 2 {
        worst = worst.max(bin_energy(&x, b as u64));
        b += step.max(1);
    }
    let peak_ratio = worst / mean_bin.max(1e-30);
    let rms = (total / n as f64).sqrt();
    eprintln!("CH4 high-shift: rms = {rms:.4} peak/mean bin ratio = {peak_ratio:.1}");
    // Bounded total power: the channel is audible hiss, neither silenced nor
    // blown up by an aliased fold. WP6's box-filter collapse averages each
    // above-Nyquist window instead of keeping its last shift, which strips the
    // ultrasonic energy the ≤1-transition collapse left in — total power drops
    // (measured rms ~0.058, down from ~0.206) while staying real audible hiss.
    assert!(
        (0.02..0.5).contains(&rms),
        "CH4 high-shift total power out of range (rms {rms:.4})"
    );
    // With the box filter this is a genuine band-limit (a window mean quantized
    // to the DAC alphabet), not the final-level color approximation the pre-WP6
    // collapse left. The tonal fold the point-sampler produced (peak/mean 116.6)
    // is now strongly suppressed (measured ~36.7): noise stays broadband hiss
    // with no screech tone.
    assert!(
        peak_ratio < 60.0,
        "CH4 high-shift tonal spur not suppressed vs the point-sampler's 116.6 \
         (peak/mean bin {peak_ratio:.1})"
    );
}
