//! Headless-browser coverage for the wasm-only rewind path the host tests can't
//! reach: `Emulator::rewind_step` → `Session::rewind` → `present` (RGBA blit).
//! This is the worker's hold-to-rewind loop (`worker.js`: while Backspace is
//! held, `if (emu.rewind_step()) postFrameAndState()`), driven directly.
//!
//! Run: `wasm-pack test --headless --chrome rustyboi-web`

#![cfg(target_arch = "wasm32")]

mod common;

use rustyboi_web::Emulator;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

// Fill the rewind buffer, then hammer `rewind_step` past exhaustion and resume
// forward play — several rounds — exactly like holding/releasing Backspace. A
// panic anywhere in the wasm rewind/present path aborts the module and fails
// the test; this is the reproduction harness for the reported "rewind crashes
// the emulator" bug.
#[wasm_bindgen_test]
async fn rewind_hold_loop_does_not_crash() {
    let mut emu = Emulator::create().await.expect("Emulator::create");
    let rom = common::test_rom();
    let _ = emu.load_rom("test.gb", &rom);
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
