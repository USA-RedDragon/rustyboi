//! Dev harness for the Game Boy Printer: drive a ROM headlessly with scripted
//! joypad input, a printer on the link port, and dump screen frames plus every
//! captured print as PNGs. Fully deterministic (frame-keyed input, no wall
//! clock), so a script is a reproducible print repro.
//!
//! Usage:
//!   printer-drive --rom <path[.zip]> [--mode dmg|cgb] [--frames N]
//!                 [--input "120:START;126:;240:A+B;246:"] [--out DIR]
//!                 [--screens N]   # dump the screen every N frames
//!
//! Input script: `frame:BUTTONS` entries separated by `;`. BUTTONS is a
//! `+`-separated list of A,B,START,SELECT,UP,DOWN,LEFT,RIGHT (empty =
//! release everything). Events apply at that frame's start.

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

/// Encode an RGB888 frame as a PNG (stored-deflate zlib, like the core's
/// grayscale encoder but color type 2).
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

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rom = arg_value(&args, "--rom").expect("--rom <path> required");
    let mode = arg_value(&args, "--mode").unwrap_or_else(|| "cgb".into());
    let frames: usize = arg_value(&args, "--frames")
        .map(|v| v.parse().expect("bad --frames"))
        .unwrap_or(600);
    let out = PathBuf::from(arg_value(&args, "--out").unwrap_or_else(|| ".".into()));
    let screens: usize = arg_value(&args, "--screens")
        .map(|v| v.parse().expect("bad --screens"))
        .unwrap_or(0);
    let script = arg_value(&args, "--input").unwrap_or_default();
    let events = parse_script(&script);

    let hardware = match mode.as_str() {
        "dmg" => Hardware::DMG,
        "cgb" => Hardware::CGB,
        other => panic!("unknown mode {other:?}"),
    };
    std::fs::create_dir_all(&out).expect("create out dir");

    let mut gb = GB::new(hardware);
    gb.insert(Cartridge::load(&rom).expect("load ROM"));
    gb.skip_bios();
    gb.attach_printer();

    let mut next_event = 0usize;
    let mut prints = 0usize;
    for frame_idx in 0..frames {
        while let Some(e) = events.get(next_event) {
            if e.frame > frame_idx {
                break;
            }
            gb.set_input_state(e.buttons);
            next_event += 1;
        }
        let (frame, _) = gb.run_until_frame(false);
        if screens > 0 && (frame_idx + 1) % screens == 0 {
            let rgb = frame_rgb(&frame);
            let path = out.join(format!("screen-{:05}.png", frame_idx + 1));
            std::fs::write(&path, encode_rgb_png(160, 144, &rgb)).expect("write screen");
        }
        for sheet in gb.take_printer_sheets() {
            prints += 1;
            let path = out.join(format!("print-{prints}.png"));
            std::fs::write(&path, sheet.to_png()).expect("write print");
            println!(
                "frame {}: print {} ({}x{}, sheets={} margins={:02X} palette={:02X} exposure={:02X}) -> {}",
                frame_idx + 1,
                prints,
                sheet.width,
                sheet.height,
                sheet.sheets,
                sheet.margins,
                sheet.palette,
                sheet.exposure,
                path.display()
            );
        }
    }
    // Final screen for scripting iteration.
    let frame = gb.get_current_frame();
    let rgb = frame_rgb(&frame);
    std::fs::write(out.join("screen-final.png"), encode_rgb_png(160, 144, &rgb))
        .expect("write final screen");
    println!("done: {prints} print(s), final screen at {}", out.join("screen-final.png").display());
}
