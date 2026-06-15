//! Golden savestate fixtures: committed byte-exact states that pin the
//! savestate wire format. `load(fixture).to_state_bytes()` must equal the
//! fixture byte-for-byte, so any accidental format change (field reorder, codec
//! drift, container swap) fails here loudly — independent of whether the
//! in-memory representation changes (e.g. heap-boxing the big buffers).
//!
//! Two fixtures cover both `fb_rle` framebuffer codec arms and both hardware
//! shapes: dmg_acid2 (DMG shade runs → Rle; no CGB banks) and cgb_acid2
//! (high-entropy RGB → Raw; vram_bank1 + wram_banks populated).
//!
//! Regenerating a fixture is an explicit, reviewed act:
//!   cargo test -p rustyboi-core --test savestate_golden -- --ignored write_golden_fixtures

use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Hardware, GB};
use std::fs;
use std::path::PathBuf;

const CASES: &[(&str, &str, Hardware)] = &[
    (
        "dmg_acid2.state",
        "../gb-test-roms/dmg-acid2/dmg-acid2.gb",
        Hardware::DMG,
    ),
    (
        "cgb_acid2.state",
        "../gb-test-roms/cgb-acid2/cgb-acid2.gbc",
        Hardware::CGB,
    ),
];

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Deterministic state recipe (mirrors gb.rs's mid-frame roundtrip test):
/// boot, settle 30 frames, then stop 2000 instructions into the next frame so
/// the state captures a live mid-render pipeline. No RTC/wall-clock inputs.
fn generate_state(rom_path: &str, hardware: Hardware) -> Option<Vec<u8>> {
    let rom = fs::read(rom_path).ok()?;
    let mut gb = GB::new(hardware);
    gb.insert(Cartridge::from_bytes(&rom).ok()?);
    gb.skip_bios();
    for _ in 0..30 {
        gb.run_until_frame(false);
    }
    for _ in 0..2000 {
        gb.step_instruction(false);
    }
    Some(gb.to_state_bytes().expect("serialize"))
}

/// The wire-format pin: every committed fixture must deserialize, re-serialize
/// to the exact same bytes, and stay stable through a second round-trip.
/// Deliberately on the harness's default (2 MiB) thread: restore must stay
/// stack-cheap (tests/struct_size_guard.rs enforces the layout side).
#[test]
fn golden_fixtures_reserialize_byte_identically() {
    for (name, _, _) in CASES {
        let path = fixture_path(name);
        let fixture = fs::read(&path).unwrap_or_else(|e| {
            panic!(
                "missing golden fixture {} ({e}); generate with: cargo test -p rustyboi-core \
                 --test savestate_golden -- --ignored write_golden_fixtures",
                path.display()
            )
        });

        let mut restored = GB::from_state_bytes(&fixture)
            .unwrap_or_else(|e| panic!("{name}: fixture failed to deserialize: {e}"));
        let reserialized = restored.to_state_bytes().expect("serialize");
        assert_eq!(
            reserialized, fixture,
            "{name}: re-serialization is not byte-identical to the committed fixture"
        );

        let mut second = GB::from_state_bytes(&reserialized).expect("second deserialize");
        assert_eq!(
            second.to_state_bytes().expect("serialize"),
            fixture,
            "{name}: second round-trip drifted"
        );
    }
}

/// Determinism gate: regenerating the state from the ROM must reproduce the
/// committed fixture exactly (runs wherever gb-test-roms is present, e.g. CI
/// after `make setup`; skips gracefully without it).
#[test]
fn golden_fixtures_match_regeneration() {
    for (name, rom, hardware) in CASES {
        let Some(state) = generate_state(rom, *hardware) else {
            eprintln!("skipping {name}: {rom} not present");
            continue;
        };
        let fixture = fs::read(fixture_path(name)).expect("fixture missing (see writer test)");
        assert_eq!(
            state, fixture,
            "{name}: regenerated state differs from the committed fixture"
        );
    }
}

/// Fixture writer — explicit only. Fails loudly if a ROM is missing.
#[test]
#[ignore]
fn write_golden_fixtures() {
    fs::create_dir_all(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"))
        .expect("create fixtures dir");
    for (name, rom, hardware) in CASES {
        let state = generate_state(rom, *hardware)
            .unwrap_or_else(|| panic!("{name}: ROM {rom} not present; cannot generate"));
        let path = fixture_path(name);
        fs::write(&path, &state).expect("write fixture");
        println!("wrote {} ({} bytes)", path.display(), state.len());
    }
}
