//! Headless ROM+savestate repro harness for rendering-glitch investigation.
//!
//! Usage:
//!   glitch --rom <rom-or-zip> --state <savestate.rustyboisave> \
//!          [--frames N] [--out DIR] [--dump-all]
//!
//! Loads the savestate, reattaches the ROM, runs N frames with no input,
//! prints a per-frame FNV-1a hash of the RGB framebuffer, and dumps PNGs
//! (all frames with --dump-all, otherwise only frames whose hash differs
//! from both neighbors' majority run).

#[path = "shared/imaging.rs"]
#[allow(dead_code)]
mod imaging;

use clap::Parser;
use imaging::{encode_rgb_png, frame_rgb};
use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::GB;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    rom: String,
    #[arg(long)]
    state: String,
    #[arg(long, default_value_t = 600)]
    frames: u32,
    #[arg(long, default_value = "glitch-out")]
    out: String,
    #[arg(long, default_value_t = false)]
    dump_all: bool,
    /// Comma-separated frame indices whose post-frame VRAM (both banks) to dump.
    #[arg(long, default_value = "")]
    vram_frames: String,
}

fn fnv1a(data: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn main() {
    let args = Args::parse();
    let rom_container = std::fs::read(&args.rom).expect("read rom");
    let rom = Cartridge::extract_rom_bytes(&rom_container).expect("extract rom");
    let state = std::fs::read(&args.state).expect("read state");
    let mut gb = GB::from_state_bytes(&state).expect("load state");
    if gb.cartridge_needs_rom() {
        assert!(gb.reattach_rom(&rom), "state carried no cartridge");
    }
    std::fs::create_dir_all(&args.out).expect("mkdir out");

    let vram_frames: Vec<u32> = args
        .vram_frames
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().expect("frame index"))
        .collect();

    let mut frames: Vec<(u64, Vec<u8>)> = Vec::with_capacity(args.frames as usize);
    for i in 0..args.frames {
        let (frame, _) = gb.run_until_frame(false);
        let rgb = frame_rgb(&frame);
        frames.push((fnv1a(&rgb), rgb));
        let r = |a: u16| gb.read_memory(a);
        eprintln!(
            "frame {i:04} LCDC={:02x} STAT={:02x} SCY={:02x} SCX={:02x} WY={:02x} WX={:02x} VBK={:02x} BGPI={:02x} HDMA5={:02x} LY={:02x}",
            r(0xFF40), r(0xFF41), r(0xFF42), r(0xFF43), r(0xFF4A), r(0xFF4B),
            r(0xFF4F), r(0xFF68), r(0xFF55), r(0xFF44)
        );
        if vram_frames.contains(&i) {
            let mut v = Vec::with_capacity(0x4000);
            for bank in 0..2u8 {
                for addr in 0x8000..0xA000u32 {
                    v.push(gb.read_vram_bank(bank, addr as u16));
                }
            }
            std::fs::write(format!("{}/vram{i:04}.bin", args.out), v).expect("write vram");
        }
    }

    for (i, (h, rgb)) in frames.iter().enumerate() {
        let prev = i.checked_sub(1).map(|p| frames[p].0);
        let next = frames.get(i + 1).map(|n| n.0);
        let spike = prev.is_some_and(|p| p != *h) && next.is_some_and(|n| n != *h);
        println!("frame {i:04} {h:016x}{}", if spike { " SPIKE" } else { "" });
        if args.dump_all || spike {
            let png = encode_rgb_png(160, 144, rgb);
            std::fs::write(format!("{}/f{i:04}.png", args.out), png).expect("write png");
        }
    }
}
