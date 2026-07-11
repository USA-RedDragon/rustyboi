//! Dev harness for the Game Boy Camera (POCKET CAMERA $FC + M64282FP):
//! drive the camera ROM headlessly with scripted joypad input, an optional
//! battery `.sav`, and an optional externally-fed sensor image, dumping
//! screen frames as PNGs. Fully deterministic (frame-keyed input, no wall
//! clock), so a script is a reproducible shoot/capture/gallery repro.
//!
//! This bin is also the reference for frontend integration: feed a live
//! webcam/still by calling `Cartridge::set_camera_image(&[u8; 128*112])`
//! (via `GB::cartridge_mut()`) whenever a new source frame is available;
//! captures triggered by the game sample whatever was fed last.
//!
//! Usage:
//!   camera-drive --rom <path[.zip]> [--frames N] [--sav <path>]
//!                [--image <128x112 P5 .pgm | 14336-byte raw>]
//!                [--input "620:START;632:;750:A;762:"]
//!                [--out DIR] [--screens N] [--shots F1,F2,...]

use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Frame, GB, Hardware};
use rustyboi_core_lib::input::ButtonState;
use std::path::PathBuf;

struct Event {
    frame: usize,
    buttons: ButtonState,
}

fn parse_buttons(spec: &str) -> ButtonState {
    let mut b = ButtonState::default();
    for name in spec.split('+').filter(|s| !s.is_empty()) {
        match name.to_ascii_uppercase().as_str() {
            "A" => b.a = true,
            "B" => b.b = true,
            "START" => b.start = true,
            "SELECT" => b.select = true,
            "UP" => b.up = true,
            "DOWN" => b.down = true,
            "LEFT" => b.left = true,
            "RIGHT" => b.right = true,
            other => panic!("unknown button {other:?}"),
        }
    }
    b
}

fn parse_script(script: &str) -> Vec<Event> {
    let mut events: Vec<Event> = script
        .split(';')
        .filter(|s| !s.trim().is_empty())
        .map(|entry| {
            let (frame, buttons) = entry
                .split_once(':')
                .unwrap_or_else(|| panic!("bad input event {entry:?} (want frame:BUTTONS)"));
            Event {
                frame: frame.trim().parse().expect("bad frame number"),
                buttons: parse_buttons(buttons.trim()),
            }
        })
        .collect();
    events.sort_by_key(|e| e.frame);
    events
}

/// Encode an RGB888 frame as a PNG (stored-deflate zlib).
fn encode_rgb_png(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        let mut crc = 0xFFFF_FFFFu32;
        for &b in kind.iter().chain(data) {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xEDB8_8320 & (0u32.wrapping_sub(crc & 1)));
            }
        }
        png.extend_from_slice(&(!crc).to_be_bytes());
    }
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &ihdr);
    let mut raw = Vec::with_capacity((width as usize * 3 + 1) * height as usize);
    for row in rgb.chunks(width as usize * 3) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut idat = vec![0x78, 0x01];
    for (i, block) in raw.chunks(0xFFFF).enumerate() {
        idat.push(((i + 1) * 0xFFFF >= raw.len()) as u8);
        idat.extend_from_slice(&(block.len() as u16).to_le_bytes());
        idat.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        idat.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    idat.extend_from_slice(&(((b << 16) | a).to_be_bytes()));
    chunk(&mut png, b"IDAT", &idat);
    chunk(&mut png, b"IEND", &[]);
    png
}

fn frame_rgb(frame: &Frame) -> Vec<u8> {
    frame.rgb().to_vec()
}

fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Load a sensor image: either a raw 128*112 grayscale blob or a P5 PGM of
/// exactly 128x112 (maxval <= 255).
fn load_sensor_image(path: &str) -> [u8; 128 * 112] {
    let data = std::fs::read(path).expect("read --image file");
    let mut out = [0u8; 128 * 112];
    if data.len() == out.len() {
        out.copy_from_slice(&data);
        return out;
    }
    let text = &data;
    assert!(text.starts_with(b"P5"), "--image must be raw 14336B or P5 PGM");
    // Tokenize the header: magic, width, height, maxval, then raster.
    let mut fields = Vec::new();
    let mut i = 2;
    while fields.len() < 3 && i < text.len() {
        while i < text.len() && (text[i] as char).is_whitespace() {
            i += 1;
        }
        if text[i] == b'#' {
            while i < text.len() && text[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let start = i;
        while i < text.len() && !(text[i] as char).is_whitespace() {
            i += 1;
        }
        fields.push(
            std::str::from_utf8(&text[start..i])
                .unwrap()
                .parse::<usize>()
                .expect("bad PGM header"),
        );
    }
    i += 1; // single whitespace after maxval
    assert_eq!((fields[0], fields[1]), (128, 112), "--image must be 128x112");
    assert!(fields[2] <= 255, "--image must be 8-bit");
    out.copy_from_slice(&data[i..i + 128 * 112]);
    out
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rom = arg_value(&args, "--rom").expect("--rom <path> required");
    let frames: usize = arg_value(&args, "--frames")
        .map(|v| v.parse().expect("bad --frames"))
        .unwrap_or(600);
    let out = PathBuf::from(arg_value(&args, "--out").unwrap_or_else(|| ".".into()));
    let screens: usize = arg_value(&args, "--screens")
        .map(|v| v.parse().expect("bad --screens"))
        .unwrap_or(0);
    let shots: Vec<usize> = arg_value(&args, "--shots")
        .map(|v| v.split(',').map(|s| s.parse().expect("bad shot frame")).collect())
        .unwrap_or_default();
    let script = arg_value(&args, "--input").unwrap_or_default();
    let events = parse_script(&script);

    std::fs::create_dir_all(&out).expect("create out dir");

    // Read the (possibly zipped) ROM in-memory; from_bytes extracts zips
    // without touching disk and never auto-creates sidecar files.
    let bytes = std::fs::read(&rom).expect("read ROM file");
    let mut cart = Cartridge::from_bytes(&bytes).expect("load ROM");
    println!(
        "camera cart: has_camera={} battery={} ram={}KB",
        cart.has_camera(),
        cart.has_battery(),
        cart.save_ram().len() / 1024,
    );
    if let Some(sav) = arg_value(&args, "--sav") {
        cart.attach_save_file(&sav).expect("attach --sav");
        println!("sav attached: {sav}");
    }
    if let Some(image) = arg_value(&args, "--image") {
        cart.set_camera_image(&load_sensor_image(&image));
        println!("sensor image fed: {image}");
    }

    let mut gb = GB::new(Hardware::DMG);
    gb.insert(cart);
    gb.skip_bios();

    let mut next_event = 0usize;
    for frame_idx in 1..=frames {
        while let Some(e) = events.get(next_event) {
            if e.frame > frame_idx {
                break;
            }
            gb.set_input_state(e.buttons);
            next_event += 1;
        }
        let (frame, _) = gb.run_until_frame(false);
        let checkpoint = frame_idx == frames
            || shots.contains(&frame_idx)
            || (screens > 0 && frame_idx % screens == 0);
        if checkpoint {
            let rgb = frame_rgb(&frame);
            println!("frame {frame_idx:>5}: hash={:016x}", fnv1a(&rgb));
            std::fs::write(
                out.join(format!("screen-{frame_idx:05}.png")),
                encode_rgb_png(160, 144, &rgb),
            )
            .expect("write screen");
        }
    }
    println!("done ({frames} frames)");
}
