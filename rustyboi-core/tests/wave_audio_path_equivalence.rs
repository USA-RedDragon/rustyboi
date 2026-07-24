//! Risk #10 guard at unit scale: the observing wave fetch-loop (audio ON) must
//! leave BYTE-IDENTICAL CPU-visible state to the analytic wave jump (audio OFF).
//!
//! The library audio baseline is regenerated with audio ON (`emulate` collects
//! audio, so CH3 runs its fetch loop), while all 28 hardware suites and `bench`
//! run audio OFF (CH3 takes the analytic jump). The two production gates thus
//! exercise DIFFERENT wave code, each self-consistent — so a CPU-visible fork
//! between them (a dropped wave-RAM read quirk, a misplaced PCM34 nibble, a
//! length-expiry timing slip) would be baked into the regenerated baseline
//! unseen. The full-library version of this cross-check is WP5(2): `sweep run
//! --no-audio` diffed against the audio-on baseline's video `hash_all` columns.
//!
//! This is the same assertion at the scale of a `cargo test`: run a handful of
//! audio-active ROMs BOTH ways for N frames and require the VIDEO frame hashes to
//! match frame-for-frame. blargg's dmg_sound / cgb_sound are the strongest
//! available discriminators — they render pass/fail results derived from APU
//! reads (NR52 channel-status bits, the DMG wave-RAM read quirk, and on CGB the
//! PCM12/PCM34 output nibbles), all of which are exactly the CPU-visible state
//! the wave-path fork could disturb.
//!
//! ROMs live under `../gb-test-roms` (present in CI and after `make setup`); the
//! test skips gracefully per ROM when absent, like `savestate_golden`.

use rustyboi_core_lib::audio::AudioOutput;
use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Hardware, GB};

/// A sink that discards samples. Its only job is to put the APU into the
/// observing mode that `run_until_frame(true)` selects — the wave fetch loop.
struct DiscardSink;
impl AudioOutput for DiscardSink {
    fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
    fn add_samples(&mut self, _s: &[(f32, f32)]) {}
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn frame_fnv(rgb: &[u8]) -> u64 {
    let mut h = FNV_OFFSET;
    for &b in rgb {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Run `rom` for `frames` frames and return the per-frame FNV hash of the RGB
/// framebuffer. `audio` selects the wave code path: ON = observing fetch loop,
/// OFF = analytic jump. No input is injected, so both runs are deterministic and
/// identical in everything except which wave path executes.
fn frame_hashes(rom: &[u8], hardware: Hardware, frames: usize, audio: bool) -> Vec<u64> {
    let mut gb = GB::new(hardware);
    gb.insert(Cartridge::from_bytes(rom).expect("cartridge"));
    gb.skip_bios();
    if audio {
        gb.enable_audio(Box::new(DiscardSink)).expect("enable audio");
    }
    let mut out = Vec::with_capacity(frames);
    for _ in 0..frames {
        let (frame, _bp) = gb.run_until_frame(audio);
        out.push(frame_fnv(frame.rgb()));
    }
    out
}

/// The audio-active ROMs to cross-check, with the model each is written for.
/// dmg_sound is a DMG cart (exercises the DMG wave-RAM read quirk); cgb_sound is
/// run on CGB (exercises PCM12/PCM34 and the CGB wave-RAM access rules).
const CASES: &[(&str, Hardware)] = &[
    ("../gb-test-roms/blargg/dmg_sound/dmg_sound.gb", Hardware::DMG),
    ("../gb-test-roms/blargg/cgb_sound/cgb_sound.gb", Hardware::CGB),
];

/// ~13 s of emulated time: long enough to reach the wave-RAM / PCM sub-tests,
/// short enough to keep two full runs per ROM cheap.
const FRAMES: usize = 800;

#[test]
fn wave_on_off_video_hashes_match() {
    let mut ran = 0usize;
    for &(path, hardware) in CASES {
        let Ok(rom) = std::fs::read(path) else {
            eprintln!("skipping {path}: not present");
            continue;
        };
        ran += 1;
        let on = frame_hashes(&rom, hardware, FRAMES, true);
        let off = frame_hashes(&rom, hardware, FRAMES, false);
        assert_eq!(
            on.len(),
            off.len(),
            "{path}: frame counts differ ({} vs {})",
            on.len(),
            off.len()
        );
        for (f, (a, b)) in on.iter().zip(off.iter()).enumerate() {
            assert_eq!(
                a, b,
                "{path} [{hardware:?}]: audio-on and audio-off VIDEO frame hashes \
                 diverged at frame {f} ({a:016x} vs {b:016x}) — the observing wave \
                 fetch loop forked CPU-visible state from the analytic jump (risk #10)"
            );
        }
        eprintln!("{path} [{hardware:?}]: {FRAMES} frames, wave on/off video-identical");
    }
    if ran == 0 {
        eprintln!(
            "wave_on_off_video_hashes_match: no gb-test-roms present; skipped \
             (the channel-level equivalence tests in audio::controller still cover \
             the invariant, and CI runs this with the ROMs)"
        );
    }
}
