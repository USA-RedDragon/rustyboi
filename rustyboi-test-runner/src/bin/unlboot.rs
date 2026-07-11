//! Boot-validation harness for unlicensed-mapper carts.
//!
//! Loads a ROM (zips are extracted in-memory, never to disk), reports the
//! detected mapper, runs N frames with optional scripted button taps, prints
//! an FNV-1a hash of the frame at each checkpoint, and writes PPM screenshots
//! so bank-switch faults show up as garbage/blank frames.
//!
//! Usage:
//!   unlboot <rom> [frames] [--hw dmg|cgb|auto] [--out DIR]
//!           [--press BTN@FRAME ...] [--detect-only] [--shots F1,F2,...]

use std::fs;
use std::path::PathBuf;

use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Frame, Hardware, GB};
use rustyboi_core_lib::input::ButtonState;

fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn frame_bytes(frame: &Frame) -> Vec<u8> {
    match frame {
        Frame::Monochrome(b) => b.to_vec(),
        Frame::Color(b) => b.to_vec(),
    }
}

fn write_ppm(path: &PathBuf, frame: &Frame) {
    const W: usize = 160;
    const H: usize = 144;
    let mut out = format!("P6\n{W} {H}\n255\n").into_bytes();
    match frame {
        Frame::Monochrome(b) => {
            // 2-bit shade -> grayscale
            for &px in b.iter() {
                let v = match px {
                    0 => 0xFF,
                    1 => 0xAA,
                    2 => 0x55,
                    _ => 0x00,
                };
                out.extend_from_slice(&[v, v, v]);
            }
        }
        Frame::Color(b) => out.extend_from_slice(&b[..]),
    }
    fs::write(path, out).expect("write ppm");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: unlboot <rom> [frames] [--hw dmg|cgb|auto] [--out DIR] [--press BTN@FRAME ...] [--detect-only] [--shots F1,F2,...]");
        std::process::exit(1);
    }
    let path = &args[1];
    let mut frames: usize = 600;
    let mut hw = "auto".to_string();
    let mut out_dir: Option<PathBuf> = None;
    let mut presses: Vec<(String, usize)> = Vec::new();
    let mut detect_only = false;
    let mut shots: Vec<usize> = Vec::new();

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--hw" => {
                hw = args[i + 1].clone();
                i += 2;
            }
            "--out" => {
                out_dir = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--press" => {
                let (btn, frame) = args[i + 1].split_once('@').expect("--press BTN@FRAME");
                presses.push((btn.to_string(), frame.parse().expect("frame number")));
                i += 2;
            }
            "--detect-only" => {
                detect_only = true;
                i += 1;
            }
            "--shots" => {
                shots = args[i + 1]
                    .split(',')
                    .map(|s| s.parse().expect("shot frame"))
                    .collect();
                i += 2;
            }
            other => {
                frames = other.parse().expect("frame count");
                i += 1;
            }
        }
    }

    let bytes = fs::read(path).expect("read ROM file");
    let cart = Cartridge::from_bytes(&bytes).expect("load ROM");
    let name = PathBuf::from(path)
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    println!(
        "{name}: mapper={:?} cgb={:?} battery={}",
        cart.unl_mapper(),
        cart.get_cgb_support(),
        cart.has_battery(),
    );
    if detect_only {
        return;
    }

    let hardware = match hw.as_str() {
        "dmg" => Hardware::DMG,
        "cgb" => Hardware::CGB,
        _ => {
            if cart.supports_cgb() {
                Hardware::CGB
            } else {
                Hardware::DMG
            }
        }
    };
    let mut gb = GB::new(hardware);
    gb.insert(cart);
    gb.skip_bios();

    if let Some(dir) = &out_dir {
        fs::create_dir_all(dir).expect("create out dir");
    }

    let mut last_hash = 0u64;
    for f in 1..=frames {
        // Scripted taps: hold the button for 12 frames from its start frame.
        let mut state = ButtonState::default();
        for (btn, at) in &presses {
            if f >= *at && f < *at + 12 {
                match btn.as_str() {
                    "start" => state.start = true,
                    "select" => state.select = true,
                    "a" => state.a = true,
                    "b" => state.b = true,
                    "up" => state.up = true,
                    "down" => state.down = true,
                    "left" => state.left = true,
                    "right" => state.right = true,
                    _ => {}
                }
            }
        }
        gb.set_input_state(state);

        let (frame, _bp) = gb.run_until_frame(false);
        let is_checkpoint = f == frames || shots.contains(&f) || f % 120 == 0;
        if is_checkpoint {
            let bytes = frame_bytes(&frame);
            let hash = fnv1a(&bytes);
            let blank = bytes.iter().all(|&b| b == bytes[0]);
            println!(
                "  frame {f:>5}: hash={hash:016x}{}{}",
                if blank { " BLANK" } else { "" },
                if hash == last_hash { " (unchanged)" } else { "" },
            );
            last_hash = hash;
            if let Some(dir) = &out_dir
                && (f == frames || shots.contains(&f))
            {
                write_ppm(&dir.join(format!("{name}-f{f}.ppm")), &frame);
            }
        }
    }
}
