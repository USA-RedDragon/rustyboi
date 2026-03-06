use std::time::Instant;

use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Frame, Hardware, GB};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: bench <rom.zip|rom.gb|rom.gbc> [frames]");
        std::process::exit(1);
    }
    let path = &args[1];
    let frames: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(20000);

    // Read the (possibly zipped) ROM bytes in-memory; from_bytes handles zip
    // extraction and does NOT touch sidecar .sav files.
    let bytes = std::fs::read(path).expect("read ROM file");
    let cart = Cartridge::from_bytes(&bytes).expect("load ROM");
    let hardware = if cart.supports_cgb() {
        Hardware::CGB
    } else {
        Hardware::DMG
    };

    let mut gb = GB::new(hardware);
    gb.insert(cart);
    gb.skip_bios();

    // Warm-up: let the game boot past logo/menu into gameplay-ish workload.
    for _ in 0..600 {
        gb.run_until_frame(false);
    }

    let probe = std::env::var("RB_PROBE").is_ok();
    let mut ds_frames = 0u64;
    let mut audio_frames = 0u64;

    let start = Instant::now();
    let mut checksum: u64 = 0;
    for _ in 0..frames {
        let (frame, _bp) = gb.run_until_frame(false);
        if probe {
            if gb.read_memory(0xFF4D) & 0x80 != 0 {
                ds_frames += 1;
            }
            if gb.read_memory(0xFF26) & 0x80 != 0 {
                audio_frames += 1;
            }
        }
        // Fold a couple bytes into a checksum so the frame work can't be
        // optimized away, and so we can sanity-check determinism.
        let (b0, bm) = match &frame {
            Frame::Monochrome(b) => (b[0], b[b.len() / 2]),
            Frame::Color(b) => (b[0], b[b.len() / 2]),
        };
        checksum = checksum.wrapping_add(b0 as u64);
        checksum ^= (bm as u64) << 1;
    }
    let elapsed = start.elapsed();

    if probe {
        let cart_type = gb.read_memory(0x0147);
        println!(
            "  PROBE cart_type=0x{:02X} hw={:?} ds={:.0}% audio_on={:.0}%",
            cart_type,
            hardware,
            100.0 * ds_frames as f64 / frames as f64,
            100.0 * audio_frames as f64 / frames as f64,
        );
    }

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
