//! Fast-forward audio contract: a finite FF speed resamples the extra sub-frame
//! audio down to one real-time frame's worth (audible, sped-up pitch), while
//! *uncapped* FF mutes (no fixed ratio to resample to). Regression guard for the
//! "FF is silent" report — proves finite FF stays audible.

use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Hardware, GB};
use rustyboi_core_lib::movie::sha256;

use rustyboi_session::action::UiAction;
use rustyboi_session::config::Config;
use rustyboi_session::input::AbstractInput;
use rustyboi_session::ports::{MemRumble, MemStorage, MemWebcam};
use rustyboi_session::session::{Ports, RunMode, Session};

fn ports() -> Ports {
    Ports {
        storage: Box::new(MemStorage::new()),
        rumble: Box::new(MemRumble::default()),
        webcam: Box::new(MemWebcam::default()),
    }
}

fn rms(a: &[(f32, f32)]) -> f32 {
    if a.is_empty() {
        return 0.0;
    }
    let s: f32 = a.iter().map(|(l, r)| l * l + r * r).sum();
    (s / (a.len() as f32 * 2.0)).sqrt()
}

// A ROM that switches channel-1 on as a continuous 50%-duty square wave (max
// envelope, length disabled) then spins on a tight `JP self`, so every frame —
// forever — produces the same non-zero tone.
fn tone_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100] = 0xC3;
    rom[0x101] = 0x50;
    rom[0x102] = 0x01;
    let prog: &[u8] = &[
        0x3E, 0x80, 0xE0, 0x26, // LDH (NR52),A  power on
        0x3E, 0x77, 0xE0, 0x24, // LDH (NR50),A  volume
        0x3E, 0xFF, 0xE0, 0x25, // LDH (NR51),A  all to both
        0x3E, 0x80, 0xE0, 0x11, // LDH (NR11),A  duty 50%
        0x3E, 0xF0, 0xE0, 0x12, // LDH (NR12),A  env vol 15, no decay
        0x3E, 0x00, 0xE0, 0x13, // LDH (NR13),A  freq lo
        0x3E, 0x87, 0xE0, 0x14, // LDH (NR14),A  freq hi + trigger
        0xC3, 0x6C, 0x01, //       JP 0x016C  (this instruction: tight self-loop)
    ];
    rom[0x150..0x150 + prog.len()].copy_from_slice(prog);
    let mut checksum: u8 = 0;
    for &b in &rom[0x134..0x14D] {
        checksum = checksum.wrapping_sub(b).wrapping_sub(1);
    }
    rom[0x14D] = checksum;
    rom
}

fn tone_session() -> Session {
    let bytes = tone_rom();
    let cart = Cartridge::from_bytes(&bytes).expect("load tone rom");
    let mut gb = GB::new(Hardware::DMG);
    gb.insert(cart);
    gb.skip_bios();
    let cfg = Config { hardware: Hardware::DMG, ..Default::default() };
    Session::with_gb(Box::new(gb), cfg, ports(), sha256(&bytes))
}

#[test]
fn finite_fast_forward_audio_stays_audible() {
    let mut s = tone_session();

    // Warm up so the tone is running.
    let mut normal = 0.0;
    for _ in 0..30 {
        normal = rms(&s.run_frame(AbstractInput::none()).audio);
    }
    assert!(normal > 0.05, "sanity: tone ROM should be audible at normal speed (got {normal})");

    // Fast-forward at the default 4×: still audible, and resampled to about one
    // real-time frame's worth of samples (not 4× the backlog).
    s.apply(UiAction::ToggleFastForward, 0);
    assert!(matches!(s.mode(), RunMode::FastForward(4)));
    let out = s.run_frame(AbstractInput::none());
    assert!(!out.audio.is_empty(), "FF must still produce audio");
    assert!(rms(&out.audio) > 0.05, "FF audio must stay audible (got {})", rms(&out.audio));
    assert!(
        (600..900).contains(&out.audio.len()),
        "FF audio resampled to ~one real-time frame (~738), got {}",
        out.audio.len()
    );
}

#[test]
fn uncapped_fast_forward_is_muted() {
    let mut s = tone_session();
    for _ in 0..30 {
        let _ = s.run_frame(AbstractInput::none());
    }
    s.set_fast_forward_factor(0); // uncapped
    s.apply(UiAction::ToggleFastForward, 0);
    assert!(s.config().ff_uncapped());
    let out = s.run_frame(AbstractInput::none());
    assert!(out.audio.is_empty(), "uncapped FF mutes audio (no fixed resample ratio)");
}
