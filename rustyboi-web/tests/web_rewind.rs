//! Headless-browser coverage for the wasm-only rewind path the host tests can't
//! reach: `Emulator::rewind_step` → `Session::rewind` → `present` (RGBA blit).
//! This is the worker's hold-to-rewind loop (`worker.js`: while Backspace is
//! held, `if (emu.rewind_step()) postFrameAndState()`), driven directly.
//!
//! Run: `wasm-pack test --headless --chrome rustyboi-web`

#![cfg(target_arch = "wasm32")]

use rustyboi_web::Emulator;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

// A small real ROM so frames actually advance and the rewind buffer fills with
// distinct states.
const ROM: &[u8] = include_bytes!("../../gb-test-roms/gbmicrotest/win9_b.gb");

// Fill the rewind buffer, then hammer `rewind_step` past exhaustion and resume
// forward play — several rounds — exactly like holding/releasing Backspace. A
// panic anywhere in the wasm rewind/present path aborts the module and fails
// the test; this is the reproduction harness for the reported "rewind crashes
// the emulator" bug.
#[wasm_bindgen_test]
async fn rewind_hold_loop_does_not_crash() {
    let mut emu = Emulator::create().await.expect("Emulator::create");
    let _ = emu.load_rom("win9_b.gb", ROM);
    assert!(emu.has_rom(), "ROM should be loaded");

    // Run forward to populate the rewind buffer (default interval/depth).
    for _ in 0..180 {
        let _ = emu.run_frame();
    }

    for _round in 0..4 {
        // Step back until the buffer is exhausted (worker calls this per frame
        // while held). `rewind_step` returns false when there's nothing left.
        let mut steps = 0u32;
        while emu.rewind_step() {
            steps += 1;
            assert!(steps < 10_000, "rewind_step never returned false");
        }
        // Resume forward play, refilling the buffer for the next round.
        for _ in 0..60 {
            let _ = emu.run_frame();
        }
    }
}

// Rewinding with no ROM loaded must be a no-op, never a panic (worker can post
// SetRewind before a ROM is chosen).
#[wasm_bindgen_test]
async fn rewind_without_rom_is_noop() {
    let mut emu = Emulator::create().await.expect("Emulator::create");
    assert!(!emu.has_rom());
    for _ in 0..10 {
        assert!(!emu.rewind_step(), "rewind_step must be false with no ROM");
    }
}

// The worker's control path end-to-end: each control command arrives as a
// serde-JSON `UiAction` string (exactly what the main thread posts) and is
// applied via `Session::apply`. Every representative action must apply and let
// the emulator keep running — no panic, no wasm trap. Fast-forward and
// frame-advance (Tab / backslash) are the ones the report flagged, so they lead.
#[wasm_bindgen_test]
async fn worker_applies_ui_actions_without_crashing() {
    let mut emu = Emulator::create().await.expect("Emulator::create");
    let _ = emu.load_rom("win9_b.gb", ROM);
    assert!(emu.has_rom());

    // JSON is the wire form of `UiAction`: unit variants are bare strings,
    // payload variants are `{"Variant": value}`.
    let actions = [
        "\"ToggleFastForward\"",
        "\"ToggleFastForward\"", // toggle back off
        "\"FrameAdvance\"",
        "\"TogglePause\"",
        "\"TogglePause\"",
        "\"ToggleSgbBorder\"",
        "\"ToggleSgbBorder\"",
        "{\"SetVolume\":50}",
        "{\"SetHardware\":\"Dmg\"}",
        "\"Restart\"",
        "\"Quicksave\"",
        "\"Quickload\"",
        "{\"AddCheat\":\"00A-B7F\"}",
        "\"GetCheats\"",
    ];
    for json in actions {
        emu.apply_action(json).unwrap_or_else(|e| panic!("apply_action({json}) failed: {e:?}"));
        let _ = emu.run_frame();
    }
}
