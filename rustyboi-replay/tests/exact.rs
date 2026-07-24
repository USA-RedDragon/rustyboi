//! Byte-exact core <-> RBA2 round-trip: the executable form of the shared-Renderer
//! design's central claim.
//!
//! The core mixes its live audio through `rustyboi_mix::Renderer`
//! (`SynthBox::finalize` returns `renderer.render(kernel, &rec)` for the very
//! `rec` it also pushes to the tap), and the RBA2 decoder replays the encoded
//! taps through a fresh `Renderer::new(model)` — literally the same code. So for
//! any deterministic run, `decode(encode(live tap))` MUST equal the live mixed
//! output bit-for-bit. This test asserts that f32 `to_bits()` equality across all
//! three analog families (DMG, CGB/MGB, AGB), driving a stress ROM through the
//! corners the design calls out: NR50/NR51 flips mid-tone, a DAC off->on cycle
//! (NR12 -> 0 then back + re-trigger), DIV ($FF04) writes across the frame
//! sequencer, and — on CGB/AGB — a double-speed switch.
//!
//! Exactness here is structural (one renderer, no duplicated math), so a green
//! run is the point; a red one would mean the core and decoder have forked.

use rustyboi_core_lib::audio::AudioOutput;
use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Hardware, GB};
use rustyboi_replay::{AnalogModel, AudioDecoder, AudioEncoder, FPS_DEN, FPS_NUM};
use std::sync::{Arc, Mutex};

/// Sink that keeps every mixed `(f32, f32)` pair the core emits — the live output
/// the round-trip must reproduce.
struct CapSink(Arc<Mutex<Vec<(f32, f32)>>>);
impl AudioOutput for CapSink {
    fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
    fn add_samples(&mut self, s: &[(f32, f32)]) {
        self.0.lock().unwrap().extend_from_slice(s);
    }
}

/// A 32 KiB no-MBC ROM whose entry at 0x100 is `code`. `skip_bios` runs straight
/// from 0x100 and checks neither the logo nor the header checksum, so the code is
/// free to run through the (zeroed = NOP) header region. 0x143 = 0x80 marks the
/// cart CGB-compatible so CGB/AGB run in CGB mode (KEY1 double-speed available);
/// on DMG the same cart runs in monochrome fallback.
fn rom(code: &[u8]) -> Vec<u8> {
    let mut r = vec![0u8; 0x8000];
    r[0x143] = 0x80;
    r[0x100..0x100 + code.len()].copy_from_slice(code);
    r
}

/// `LD A,val ; LDH (reg),A` — write `val` to `0xFF00 + reg`.
fn ldh(reg: u8, val: u8) -> [u8; 4] {
    [0x3E, val, 0xE0, reg]
}

/// A busy-wait of `count` iterations (`LD DE,count ; DEC DE ; LD A,D ; OR E ;
/// JR NZ,-5`), ~8 cc each — enough to space register writes across the tone.
fn delay16(count: u16) -> Vec<u8> {
    let [lo, hi] = count.to_le_bytes();
    vec![0x11, lo, hi, 0x1B, 0x7A, 0xB3, 0x20, 0xFB]
}

/// The stress program. Sets up all four channels, then walks the corner cases the
/// byte-exact claim must survive. When `speed_switch`, it arms KEY1 and STOPs to
/// switch to double speed first (CGB/AGB only — the armed-KEY1 STOP takes the
/// speed-switch path, not the low-power freeze).
fn stress_rom(speed_switch: bool) -> Vec<u8> {
    let mut c = Vec::new();
    if speed_switch {
        c.extend_from_slice(&ldh(0x4D, 0x01)); // KEY1.0 arm
        c.extend_from_slice(&[0x10, 0x00]); // STOP -> double-speed switch
    }
    // APU on, route everything, full master volume.
    c.extend_from_slice(&ldh(0x26, 0x80)); // NR52
    c.extend_from_slice(&ldh(0x25, 0xFF)); // NR51
    c.extend_from_slice(&ldh(0x24, 0x77)); // NR50
    // CH1 square, steady 50% duty at freq 0x700.
    c.extend_from_slice(&ldh(0x11, 0x80)); // NR11 duty 50%
    c.extend_from_slice(&ldh(0x12, 0xF0)); // NR12 vol 15, DAC on
    c.extend_from_slice(&ldh(0x13, 0x00)); // NR13
    c.extend_from_slice(&ldh(0x14, 0x87)); // NR14 trigger
    // CH2 square with an active decreasing envelope (envelope-boundary content).
    c.extend_from_slice(&ldh(0x16, 0x80)); // NR21
    c.extend_from_slice(&ldh(0x17, 0xF3)); // NR22 vol 15, decrease, period 3
    c.extend_from_slice(&ldh(0x18, 0x00)); // NR23
    c.extend_from_slice(&ldh(0x19, 0x86)); // NR24 trigger
    // CH3 wave: program a square table while the DAC is off, then enable.
    c.extend_from_slice(&ldh(0x1A, 0x00)); // NR30 DAC off (wave RAM writable)
    for i in 0..16u8 {
        c.extend_from_slice(&ldh(0x30 + i, 0xF0)); // wave RAM (alternating nibbles)
    }
    c.extend_from_slice(&ldh(0x1A, 0x80)); // NR30 DAC on
    c.extend_from_slice(&ldh(0x1C, 0x20)); // NR32 output level 100%
    c.extend_from_slice(&ldh(0x1D, 0x00)); // NR33
    c.extend_from_slice(&ldh(0x1E, 0x85)); // NR34 trigger
    // CH4 noise, 7-bit LFSR.
    c.extend_from_slice(&ldh(0x21, 0xF0)); // NR42 vol 15, DAC on
    c.extend_from_slice(&ldh(0x22, 0x18)); // NR43
    c.extend_from_slice(&ldh(0x23, 0x80)); // NR44 trigger

    // --- corner cases, spaced across the tone ---
    c.extend_from_slice(&delay16(1500));
    c.extend_from_slice(&ldh(0x25, 0xF0)); // NR51 left only
    c.extend_from_slice(&delay16(1500));
    c.extend_from_slice(&ldh(0x25, 0x0F)); // NR51 right only
    c.extend_from_slice(&delay16(1500));
    c.extend_from_slice(&ldh(0x24, 0x33)); // NR50 quieter
    c.extend_from_slice(&delay16(1500));
    c.extend_from_slice(&ldh(0x24, 0x77)); // NR50 restore
    c.extend_from_slice(&delay16(1500));
    c.extend_from_slice(&ldh(0x25, 0xFF)); // NR51 restore
    c.extend_from_slice(&delay16(1500));
    c.extend_from_slice(&ldh(0x12, 0x00)); // CH1 DAC off (NR12 = 0)
    c.extend_from_slice(&delay16(1500));
    c.extend_from_slice(&ldh(0x12, 0xF0)); // CH1 DAC back on
    c.extend_from_slice(&ldh(0x14, 0x87)); // re-trigger CH1
    c.extend_from_slice(&delay16(1500));
    c.extend_from_slice(&ldh(0x04, 0x00)); // DIV write (resets the frame sequencer)
    c.extend_from_slice(&delay16(1500));
    c.extend_from_slice(&ldh(0x17, 0xF3)); // re-arm CH2 envelope
    c.extend_from_slice(&ldh(0x19, 0x86)); // re-trigger CH2
    c.extend_from_slice(&delay16(1500));
    c.extend_from_slice(&ldh(0x04, 0x00)); // DIV write again
    c.extend_from_slice(&delay16(1500));
    c.extend_from_slice(&[0x18, 0xFE]); // spin
    c
}

/// Run `hardware` on the stress ROM for `frames`, collecting BOTH the live mixed
/// output and the per-sample tap in one deterministic pass. The tap is engaged
/// before the first frame — before the renderer's first `finalize` — so its first
/// record is the renderer's first output and a fresh decoder renderer stays in
/// lockstep.
fn run_capture(
    hardware: Hardware,
    speed_switch: bool,
    frames: usize,
) -> (Vec<(f32, f32)>, Vec<rustyboi_replay::SampleRecord>, AnalogModel) {
    let mut gb = GB::new(hardware);
    gb.insert(Cartridge::from_bytes(&rom(&stress_rom(speed_switch))).expect("cart"));
    gb.skip_bios();
    let buf = Arc::new(Mutex::new(Vec::new()));
    gb.enable_audio(Box::new(CapSink(buf.clone()))).expect("audio");
    gb.set_channel_tap(true);
    let model = gb.analog_model();
    for _ in 0..frames {
        gb.run_until_frame(true);
    }
    let live = buf.lock().unwrap().clone();
    let tap = gb.drain_channel_tap();
    (live, tap, model)
}

/// Decode an RBA2 blob back to the full interleaved stereo stream by
/// concatenating every video frame's span (`frame_into` over all frames
/// reproduces the whole recording, per the decoder's contract).
fn decode_all(blob: Vec<u8>) -> Vec<f32> {
    let mut dec = AudioDecoder::new(blob).expect("decode");
    let total = dec.sample_count();
    let mut out: Vec<f32> = Vec::with_capacity(total as usize * 2);
    let mut tmp: Vec<f32> = Vec::new();
    let mut frame = 0u32;
    while (out.len() as u32) / 2 < total {
        dec.frame_into(frame, &mut tmp).expect("frame_into");
        out.extend_from_slice(&tmp);
        frame += 1;
        assert!(frame < total + 2, "frame_into made no progress");
    }
    assert_eq!(out.len() as u32, total * 2, "decoded length != sample_count");
    out
}

/// The whole claim for one machine: live mixed output == decode(encode(tap)),
/// bit-for-bit.
fn assert_byte_exact(hardware: Hardware, speed_switch: bool) {
    let (live, tap, model) = run_capture(hardware, speed_switch, 30);
    assert!(!tap.is_empty(), "{hardware:?}: no audio captured");
    // 1:1 by construction: each `finalize` pushes one record AND emits one pair.
    assert_eq!(
        live.len(),
        tap.len(),
        "{hardware:?}: live samples ({}) != tap records ({})",
        live.len(),
        tap.len()
    );

    let mut enc = AudioEncoder::new();
    enc.set_model(model);
    enc.push(&tap);
    let blob = enc.finish(FPS_NUM, FPS_DEN);
    let decoded = decode_all(blob);
    assert_eq!(decoded.len(), live.len() * 2, "{hardware:?}: decoded length mismatch");

    for (i, &(l, r)) in live.iter().enumerate() {
        assert_eq!(
            l.to_bits(),
            decoded[2 * i].to_bits(),
            "{hardware:?}: L channel diverged at sample {i} ({model:?})"
        );
        assert_eq!(
            r.to_bits(),
            decoded[2 * i + 1].to_bits(),
            "{hardware:?}: R channel diverged at sample {i} ({model:?})"
        );
    }
}

#[test]
fn dmg_live_output_equals_rba2_roundtrip() {
    assert_byte_exact(Hardware::DMG, false);
}

#[test]
fn cgb_live_output_equals_rba2_roundtrip() {
    assert_byte_exact(Hardware::CGB, true);
}

#[test]
fn agb_live_output_equals_rba2_roundtrip() {
    assert_byte_exact(Hardware::AGB, true);
}

/// Guard the premise: the three families really do select distinct analog models
/// (so the test above exercises all three high-pass / mix paths, not one thrice).
#[test]
fn the_three_families_are_distinct_models() {
    let dmg = GB::new(Hardware::DMG).analog_model();
    let cgb = GB::new(Hardware::CGB).analog_model();
    let agb = GB::new(Hardware::AGB).analog_model();
    assert_eq!(dmg, AnalogModel::Dmg);
    assert_eq!(cgb, AnalogModel::CgbMgb);
    assert_eq!(agb, AnalogModel::Agb);
}
