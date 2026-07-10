//! Headless-browser coverage for the web worker's file load/save round-trips
//! and for applying EVERY worker-serviceable `UiAction`. All of these are
//! `#[wasm_bindgen] Emulator` methods callable directly (the JS shell's
//! postMessage glue + the DOM download/picker are not exercised here — they're
//! plain JS, out of reach of a Rust test).
//!
//! Run: `wasm-pack test --headless --chrome rustyboi-web`

#![cfg(target_arch = "wasm32")]

mod common;

use rustyboi_session::action::{
    GbcDmgPalette, HardwareChoice, LcdEffect, PaletteChoice, ScalingMode, TextureFilter,
};
use rustyboi_session::{ColorCorrection, InputConfig, UiAction};
use rustyboi_web::Emulator;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

// A savestate produced by `export_state` must re-import cleanly via `load_state`
// — the File → Export / Import savestate round-trip, no DOM needed.
#[wasm_bindgen_test]
async fn savestate_export_import_round_trips() {
    let mut emu = Emulator::create().await.expect("create");
    let _ = emu.load_rom("test.gb", &common::test_rom());
    for _ in 0..30 {
        let _ = emu.run_frame();
    }
    let state = emu.export_state().to_vec();
    assert!(!state.is_empty(), "export_state produced no bytes");
    let reqs = emu.load_state(&state);
    // No panic/trap, and the finisher returns its request array.
    assert!(reqs.length() >= 1, "load_state returned no requests");
}

// Battery SRAM: import bytes, export them back — the `.sav` Import/Export path
// for a battery-backed cartridge (MBC3).
#[wasm_bindgen_test]
async fn battery_import_export_round_trips() {
    let mut emu = Emulator::create().await.expect("create");
    let _ = emu.load_rom("mbc3.gb", &common::test_rom_mbc3());
    let payload = vec![0xABu8; common::MBC3_RAM_BYTES];
    let _ = emu.import_battery(&payload);
    let exported = emu.export_battery().to_vec();
    assert_eq!(exported, payload, "battery SRAM did not round-trip");
}

// RTC blob: a timer cartridge exports a non-empty `.rtc`, and re-importing it
// succeeds.
#[wasm_bindgen_test]
async fn rtc_export_import_round_trips() {
    let mut emu = Emulator::create().await.expect("create");
    let _ = emu.load_rom("mbc3.gb", &common::test_rom_mbc3());
    let rtc = emu.export_rtc().to_vec();
    assert!(!rtc.is_empty(), "timer cart should export an RTC blob");
    let _ = emu.import_rtc(&rtc); // must not panic
}

// A ROM has no battery/RTC → exports are empty (not a crash).
#[wasm_bindgen_test]
async fn plain_rom_has_no_battery_or_rtc() {
    let mut emu = Emulator::create().await.expect("create");
    let _ = emu.load_rom("test.gb", &common::test_rom());
    assert!(emu.export_battery().to_vec().is_empty());
    assert!(emu.export_rtc().to_vec().is_empty());
}

// A no-op IPS patch applies without crashing (header "PATCH" + "EOF", no
// records → the ROM is unchanged).
#[wasm_bindgen_test]
async fn apply_noop_ips_patch_does_not_crash() {
    let mut emu = Emulator::create().await.expect("create");
    let _ = emu.load_rom("test.gb", &common::test_rom());
    let mut ips = b"PATCH".to_vec();
    ips.extend_from_slice(b"EOF");
    let _ = emu.apply_patch(&ips); // returns a request array; must not trap
}

// Attaching the printer and running must not crash `take_prints` (no sheets are
// produced by this ROM, so it returns an empty array).
#[wasm_bindgen_test]
async fn printer_take_prints_does_not_crash() {
    let mut emu = Emulator::create().await.expect("create");
    let _ = emu.load_rom("test.gb", &common::test_rom());
    emu.apply_action("\"TogglePrinter\"").expect("attach printer");
    for _ in 0..10 {
        let _ = emu.run_frame();
    }
    let _ = emu.take_prints(); // array (empty here), no trap
}

/// Every `UiAction` the web main thread forwards to the worker (the exhaustive
/// `serviceable` arm of `webapp::dispatch_action`). Constructed as real values
/// and serialized here so the wire form is exactly what production posts.
fn serviceable_actions() -> Vec<UiAction> {
    use UiAction::*;
    vec![
        TogglePause,
        ToggleRecording,
        StopReplay,
        TogglePrinter,
        TogglePrinter, // toggle back
        Restart,
        ClearError,
        SaveSlot(1),
        LoadSlot(1),
        Quicksave,
        Quickload,
        ToggleFastForward,
        ToggleFastForward,
        FrameAdvance,
        ToggleSgbBorder,
        ToggleTouchControls,
        SetHardware(HardwareChoice::Dmg),
        SetHardware(HardwareChoice::Cgb),
        SetPalette(PaletteChoice::GreenLcd),
        SetGbcDmgPalette(GbcDmgPalette::Auto),
        SetGbcDmgPalette(GbcDmgPalette::Scheme(5)),
        SetColorCorrection(ColorCorrection::Lcd),
        SetRealBootRom(false),
        SetTextureFilter(TextureFilter::Linear),
        SetLcdEffect(LcdEffect::Scanlines),
        SetPrinterScale(4),
        SetTouchOpacity(50),
        SetRewindEnabled(true),
        SetRewindInterval(3),
        SetRewindDepth(42),
        SetVolume(80),
        SetScalingMode(ScalingMode::Stretch),
        SetInputConfig(InputConfig::default()),
        AddCheat("00A-B7F".into()),
        AddCheats(vec!["00A-B7F".into()]),
        RemoveCheat("00A-B7F".into()),
        GetCheats,
        ClearFetchedCheats,
    ]
}

// Apply every worker-serviceable action through the exact JSON wire path the
// main thread uses, and keep running a frame after each. None may trap or return
// an error from `apply_action` (a bad-JSON deserialize would).
#[wasm_bindgen_test]
async fn worker_applies_every_serviceable_action() {
    let mut emu = Emulator::create().await.expect("create");
    let _ = emu.load_rom("test.gb", &common::test_rom());
    for action in serviceable_actions() {
        let json = serde_json::to_string(&action).expect("serialize UiAction");
        emu.apply_action(&json)
            .unwrap_or_else(|e| panic!("apply_action({json}) failed: {e:?}"));
        let _ = emu.run_frame();
    }
}
