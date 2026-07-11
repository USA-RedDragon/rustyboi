use std::time::Instant;

use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Hardware, GB};

#[path = "shared/masher.rs"]
mod masher;
use masher::masher;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: bench <rom.zip|rom.gb|rom.gbc> [frames] [--drive]");
        std::process::exit(1);
    }
    let path = &args[1];
    // --drive feeds the deterministic gameplay masher each frame — used by the
    // PGO profiling workload so the profile reflects real in-game emulation
    // (input handling, sprites, window, HDMA), not an idle title screen. Pure
    // emulation otherwise: no per-frame full-framebuffer hashing to skew it.
    let drive = args.iter().any(|a| a == "--drive");
    let frames: usize = args
        .get(2)
        .filter(|s| !s.starts_with("--"))
        .and_then(|s| s.parse().ok())
        .unwrap_or(20000);
    // Read the (possibly zipped) ROM bytes in-memory; from_bytes handles zip
    // extraction and does NOT touch sidecar .sav files.
    let bytes = std::fs::read(path).expect("read ROM file");
    // Per-ROM masher seed from a cheap FNV of the file bytes (stable, no sha dep).
    let seed = bytes.iter().take(4096).fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3)
    });
    let cart = Cartridge::from_bytes(&bytes).expect("load ROM");
    let hardware = if cart.supports_cgb() {
        Hardware::CGB
    } else {
        Hardware::DMG
    };

    let mut gb = GB::new(hardware);
    gb.insert(cart);
    gb.skip_bios();

    // Frame index spans warm-up + measured so --drive's masher hits its
    // title-clearing phase (0..600) then gameplay (600+).
    let mut fi = 0usize;
    // Warm-up: let the game boot past logo/menu into gameplay-ish workload.
    for _ in 0..600 {
        if drive {
            gb.set_input_state(masher(fi, seed));
        }
        gb.run_until_frame(false);
        fi += 1;
    }

    let start = Instant::now();
    let mut checksum: u64 = 0;
    for _ in 0..frames {
        if drive {
            gb.set_input_state(masher(fi, seed));
        }
        fi += 1;
        let (frame, _bp) = gb.run_until_frame(false);
        // Fold a couple bytes into a checksum so the frame work can't be
        // optimized away, and so we can sanity-check determinism.
        let b = frame.rgb();
        let (b0, bm) = (b[0], b[b.len() / 2]);
        checksum = checksum.wrapping_add(b0 as u64);
        checksum ^= (bm as u64) << 1;
    }
    let elapsed = start.elapsed();

    let fps = frames as f64 / elapsed.as_secs_f64();
    let ns_per_frame = elapsed.as_nanos() as f64 / frames as f64;
    println!(
        "{:<50} {:>8} frames  {:>9.1} fps  {:>10.0} ns/frame  chk={:016x}",
        std::path::Path::new(path)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        frames,
        fps,
        ns_per_frame,
        checksum
    );
}
