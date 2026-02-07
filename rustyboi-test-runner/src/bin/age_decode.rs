//! Diagnostic: run an age-test-roms numerical test to its `LD B,B` freeze and
//! decode the result screen from the tilemap at 0x9800. The age framework
//! prints TEST_RESULTS rows as hex with mismatching bytes in inverse font
//! (tile index +64 high nibble / +192 low nibble), so the decoded screen shows
//! exactly which measured bytes differ from the test's expected table.
//! Usage: age_decode <rom> <dmg|cgb|cgbe|agb> [max_frames]

use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Hardware, GB};
use std::fs;

const CYCLES_PER_FRAME: u32 = 70224;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rom_path = &args[1];
    let hw = match args[2].as_str() {
        "dmg" => Hardware::DMG,
        "cgb" => Hardware::CGB,
        "cgbe" => Hardware::CGBE,
        "agb" => Hardware::AGB,
        other => panic!("unknown mode {other}"),
    };
    let max_frames: u32 = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(120);

    let rom_data = fs::read(rom_path).expect("read ROM");
    let cart = Cartridge::from_bytes(&rom_data).expect("load ROM");
    let mut gb = GB::new(hw);
    gb.insert(cart);
    gb.skip_bios();

    let mut done = false;
    let mut total: u64 = 0;
    let budget = max_frames as u64 * CYCLES_PER_FRAME as u64;
    while total < budget {
        let pc = gb.get_cpu_registers().pc;
        if gb.read_memory(pc) == 0x40 {
            done = true;
            break;
        }
        let (_bp, c) = gb.step_instruction(false);
        total += c as u64;
    }
    let r = gb.get_cpu_registers();
    println!(
        "done_marker={} pc={:04X} B={:02X} C={:02X} D={:02X} E={:02X} H={:02X} L={:02X}",
        done, r.pc, r.b, r.c, r.d, r.e, r.h, r.l
    );
    // Decode tilemap rows. Tile index = ascii-32 + style, style in
    // {0 plain, 64 inverse, 128 left, 192 left-inverse}; inverse = MISMATCH.
    for row in 0..18u16 {
        let mut text = String::new();
        let mut marks = String::new();
        let mut any = false;
        for col in 0..20u16 {
            let idx = gb.read_memory(0x9800 + row * 32 + col);
            let ch = (idx & 63) + 32;
            let c = if (32..96).contains(&ch) { ch as char } else { '?' };
            let bad = idx & 64 != 0; // inverse or left-inverse font
            text.push(c);
            marks.push(if bad && c != ' ' { '^' } else { ' ' });
            if c != ' ' {
                any = true;
            }
            if bad && c != ' ' {}
        }
        if any {
            println!("{:2} |{}|", row, text);
            if marks.trim() != "" {
                println!("   |{}|", marks);
            }
        }
    }

    // Optional raw dump: locate `ld bc,TIMING_RESULTS; ld de,EXPECTED; ld hl,..`
    // (01 ll hh 11 ll hh 21 ll hh with bc/hl in WRAM, de in ROM) in the ROM
    // image and print actual (WRAM) vs expected (ROM) with diff markers.
    // Usage: age_decode <rom> <mode> <frames> raw <total_bytes> <bytes_per_line>
    if args.get(4).map(|s| s.as_str()) == Some("raw") {
        let total: usize = args[5].parse().unwrap();
        let per_line: usize = args[6].parse().unwrap();
        let rom = &rom_data;
        let mut found = None;
        for i in 0..rom.len().saturating_sub(9) {
            if rom[i] == 0x01 && rom[i + 3] == 0x11 && rom[i + 6] == 0x21 {
                let bc = u16::from_le_bytes([rom[i + 1], rom[i + 2]]);
                let de = u16::from_le_bytes([rom[i + 4], rom[i + 5]]);
                let hl = u16::from_le_bytes([rom[i + 7], rom[i + 8]]);
                if (0xC000..0xE000).contains(&bc) && de < 0x8000 && (0xC000..0xE000).contains(&hl) {
                    found = Some((bc, de));
                    break;
                }
            }
        }
        let Some((bc, de)) = found else {
            println!("raw: pattern not found");
            return;
        };
        println!("raw: TIMING_RESULTS={bc:04X} EXPECTED={de:04X}");
        let mut off = 0usize;
        while off < total {
            let n = per_line.min(total - off);
            let mut act = String::new();
            let mut exp = String::new();
            let mut mark = String::new();
            let mut bad = false;
            for k in 0..n {
                let a = gb.read_memory(bc + (off + k) as u16);
                let e = rom[de as usize + off + k];
                act.push_str(&format!("{a:02X} "));
                exp.push_str(&format!("{e:02X} "));
                mark.push_str(if a != e { "^^ " } else { "   " });
                if a != e {
                    bad = true;
                }
            }
            let tag = if bad { "DIFF" } else { "    " };
            println!("L{:03} act {act}", off / per_line);
            if bad {
                println!("     exp {exp}");
                println!("     {tag} {mark}");
            }
            off += n;
        }
    }
}
