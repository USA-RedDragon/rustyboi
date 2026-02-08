//! Standalone grading harness for public GB/GBC test suites (c-sp layout) that
//! the name-based `rustyboi-test-runner` discovery does not pair. Reuses
//! `rustyboi_core_lib` to run each ROM and grades by one of several methods:
//!   - png      : compare final framebuffer to an 8-bit-RGBA reference PNG
//!   - mooneye  : run to `LD B,B` and check the Fibonacci magic registers
//!   - mem      : run a frame budget, read a memory byte, compare to expected
//!   - memauto  : gbmicrotest convention (FF80==1 pass, else FF81 exp / FF82 act)
//!   - serial   : scan blargg serial output for "Passed"/"Failed"
//!
//! Manifest: one test per line, fields separated by `|`:
//!   <id>|<mode>|<grading>|<rom_path>|<arg>
//! where <arg> is the reference-PNG path (png), `addr=expected` (mem), or empty.
//! CGB color conversion uses Linear, which is bucket-equivalent to the c-sp
//! `(X<<3)|(X>>2)` shift formula under the 0xF8 mask used for comparison.

use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Frame, Hardware, GB};
use rustyboi_core_lib::ppu::{CgbColorConversion, FRAMEBUFFER_SIZE};
use std::fs;
use std::io::Read;
use std::path::Path;

const CYCLES_PER_FRAME: u32 = 70224;
const MAX_CYCLES_UNTIL_LCD_FRAME: u32 = CYCLES_PER_FRAME * 64;
const RGB_MASK: u32 = 0xF8F8F8;

fn hw(mode: &str) -> Hardware {
    match mode {
        "dmg" => Hardware::DMG,
        "cgb" => Hardware::CGB,
        "agb" => Hardware::AGB,
        other => panic!("unknown mode {other}"),
    }
}

fn make_gb(mode: &str, rom: &Path) -> Result<GB, String> {
    let rom_data = fs::read(rom).map_err(|e| format!("read ROM: {e}"))?;
    let cart = Cartridge::from_bytes(&rom_data).map_err(|e| format!("load ROM: {e}"))?;
    let mut gb = GB::new(hw(mode));
    gb.insert(cart);
    // Optional real boot ROM (RB_HARNESS_BIOS=<dir>); else synthetic skip_bios.
    let used_bios = if let Ok(dir) = std::env::var("RB_HARNESS_BIOS") {
        let file = match mode {
            "dmg" => "dmg_boot.bin",
            "agb" => "cgb_agb_boot.bin",
            _ => "cgb_boot.bin",
        };
        let p = Path::new(&dir).join(file);
        if p.exists() && gb.load_bios(&p.to_string_lossy()).is_ok() {
            gb.run_boot_rom();
            true
        } else {
            false
        }
    } else {
        false
    };
    if !used_bios {
        gb.skip_bios();
    }
    if mode != "dmg" {
        gb.set_cgb_color_conversion(CgbColorConversion::Linear);
    }
    Ok(gb)
}

fn normalize_frame(frame: Frame) -> Vec<u32> {
    match frame {
        Frame::Monochrome(data) => data
            .iter()
            .map(|p| match p {
                0 => 0xFFFFFF,
                1 => 0xAAAAAA,
                2 => 0x555555,
                _ => 0x000000,
            })
            .collect(),
        Frame::Color(data) => data
            .chunks_exact(3)
            .map(|c| ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32)
            .collect(),
    }
}

/// Run up to `frames` LCD frames, stopping early once a `LD B,B` (0x40) is about
/// to execute (the c-sp/mealybug/mooneye done marker). Returns the last frame.
fn run_frames_until_ldbb(gb: &mut GB, frames: usize) -> Result<Vec<u32>, String> {
    let mut last: Option<Vec<u32>> = None;
    let mut done = false;
    for fi in 0..frames {
        // Step instruction-by-instruction within the frame so we can catch the
        // LD B,B marker; fall back to the frame boundary otherwise.
        let mut cyc = 0u32;
        loop {
            let pc = gb.get_cpu_registers().pc;
            if gb.read_memory(pc) == 0x40 {
                done = true;
                break;
            }
            let (_bp, c) = gb.step_instruction(false);
            cyc += c;
            if gb.get_ppu_debug_info().0.frame_ready() {
                break;
            }
            if cyc >= MAX_CYCLES_UNTIL_LCD_FRAME {
                return Err(format!("frame {fi} timeout"));
            }
        }
        last = Some(normalize_frame(gb.get_current_frame()));
        if done {
            break;
        }
    }
    last.ok_or_else(|| "no frame".to_string())
}

fn frame_mismatch(actual: &[u32], expected: &[u32]) -> Option<(usize, usize, usize, u32, u32)> {
    if actual.len() != FRAMEBUFFER_SIZE || expected.len() != FRAMEBUFFER_SIZE {
        return Some((FRAMEBUFFER_SIZE, 0, 0, 0, 0));
    }
    let mut diff = 0;
    let mut first = None;
    for (i, (&a, &e)) in actual.iter().zip(expected.iter()).enumerate() {
        if ((a ^ e) & RGB_MASK) != 0 {
            diff += 1;
            if first.is_none() {
                first = Some((i % 160, i / 160, a, e));
            }
        }
    }
    first.map(|(x, y, a, e)| (diff, x, y, a, e))
}

// --- minimal PNG decoder (non-interlaced 8-bit RGBA, 160x144) ---
fn read_png_rgb(path: &Path) -> Result<Vec<u32>, String> {
    use flate2::read::ZlibDecoder;
    const SIG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    let data = fs::read(path).map_err(|e| format!("read PNG {}: {e}", path.display()))?;
    if data.len() < 8 || &data[..8] != SIG {
        return Err("not a PNG".into());
    }
    let (mut w, mut h, mut idat, mut off) = (0usize, 0usize, Vec::new(), 8usize);
    let be = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
    while off + 8 <= data.len() {
        let len = be(&data[off..off + 4]);
        let ct = &data[off + 4..off + 8];
        off += 8;
        if off + len + 4 > data.len() {
            return Err("truncated PNG".into());
        }
        let cd = &data[off..off + len];
        off += len + 4;
        match ct {
            b"IHDR" => {
                w = be(&cd[0..4]);
                h = be(&cd[4..8]);
                if w != 160 || h != 144 {
                    return Err(format!("expected 160x144, got {w}x{h}"));
                }
                if cd[8] != 8 || cd[9] != 6 || cd[12] != 0 {
                    return Err("need non-interlaced 8-bit RGBA".into());
                }
            }
            b"IDAT" => idat.extend_from_slice(cd),
            b"IEND" => break,
            _ => {}
        }
    }
    let stride = w * 4;
    let mut raw = Vec::new();
    ZlibDecoder::new(&idat[..])
        .read_to_end(&mut raw)
        .map_err(|e| format!("inflate: {e}"))?;
    if raw.len() != (stride + 1) * h {
        return Err("bad PNG data length".into());
    }
    let mut out = vec![0u8; stride * h];
    let bpp = 4;
    for y in 0..h {
        let ro = y * (stride + 1);
        let f = raw[ro];
        for x in 0..stride {
            let s = raw[ro + 1 + x];
            let left = if x >= bpp { out[y * stride + x - bpp] } else { 0 };
            let up = if y > 0 { out[(y - 1) * stride + x] } else { 0 };
            let ul = if y > 0 && x >= bpp {
                out[(y - 1) * stride + x - bpp]
            } else {
                0
            };
            out[y * stride + x] = match f {
                0 => s,
                1 => s.wrapping_add(left),
                2 => s.wrapping_add(up),
                3 => s.wrapping_add(((left as u16 + up as u16) / 2) as u8),
                4 => s.wrapping_add(paeth(left, up, ul)),
                _ => return Err(format!("bad filter {f}")),
            };
        }
    }
    Ok(out
        .chunks_exact(4)
        .map(|c| ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32)
        .collect())
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (a, b, c) = (a as i16, b as i16, c as i16);
    let p = a + b - c;
    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

/// Run instruction-by-instruction until LD B,B (0x40), with a hard cycle cap.
/// Returns true if the marker was reached.
fn run_until_ldbb(gb: &mut GB, max_cycles: u64) -> bool {
    let mut cyc = 0u64;
    while cyc < max_cycles {
        let pc = gb.get_cpu_registers().pc;
        if gb.read_memory(pc) == 0x40 {
            return true;
        }
        let (_bp, c) = gb.step_instruction(false);
        cyc += c as u64;
    }
    false
}

fn grade_png(mode: &str, rom: &Path, refpng: &Path, frames: usize) -> (bool, String) {
    let mut gb = match make_gb(mode, rom) {
        Ok(g) => g,
        Err(e) => return (false, format!("ERR {e}")),
    };
    // Default: run frames, early-stopping on LD B,B. For ROMs that turn the LCD
    // off and never complete a frame (e.g. oam_bug), RB_FIXED_BUDGET=1 instead
    // runs a flat cycle budget and grabs the last rendered frame.
    let actual = if std::env::var("RB_FIXED_BUDGET").is_ok() {
        let budget = frames as u64 * CYCLES_PER_FRAME as u64;
        let mut cyc = 0u64;
        while cyc < budget {
            let (_bp, c) = gb.step_instruction(false);
            cyc += c as u64;
        }
        normalize_frame(gb.get_current_frame())
    } else {
        match run_frames_until_ldbb(&mut gb, frames) {
            Ok(f) => f,
            Err(e) => return (false, format!("ERR {e}")),
        }
    };
    let expected = match read_png_rgb(refpng) {
        Ok(e) => e,
        Err(e) => return (false, format!("ERR refpng {e}")),
    };
    if let Ok(dir) = std::env::var("RB_DUMP_DIR") {
        let stem = format!(
            "{}_{}",
            rom.file_stem().and_then(|s| s.to_str()).unwrap_or("rom"),
            mode
        );
        let _ = write_ppm(&Path::new(&dir).join(format!("{stem}.actual.ppm")), &actual);
        let _ = write_ppm(&Path::new(&dir).join(format!("{stem}.expected.ppm")), &expected);
    }
    match frame_mismatch(&actual, &expected) {
        None => (true, "ok".into()),
        Some((d, x, y, a, e)) => (
            false,
            format!("{d}px diff; first ({x},{y}) act #{:06X} exp #{:06X}", a & 0xFFFFFF, e & 0xFFFFFF),
        ),
    }
}

fn write_ppm(path: &Path, frame: &[u32]) -> Result<(), String> {
    use std::io::Write;
    let mut s = "P6\n160 144\n255\n".to_string().into_bytes();
    for px in frame {
        s.push(((px >> 16) & 0xFF) as u8);
        s.push(((px >> 8) & 0xFF) as u8);
        s.push((px & 0xFF) as u8);
    }
    let mut f = fs::File::create(path).map_err(|e| e.to_string())?;
    f.write_all(&s).map_err(|e| e.to_string())
}

/// blargg serial grading. blargg ROMs print results to the serial port: each
/// byte is written to SB (FF01) then a transfer is started by writing SC (FF02)
/// with bit7 (start) + bit0 (internal clock). We poll SC per instruction and
/// capture SB on each rising edge of the start bit, reconstructing the text
/// stream, then scan for "Passed"/"Failed". Pass requires "Passed" and no
/// "Failed". `frames` bounds the run (converted to a cycle budget).
fn grade_serial(mode: &str, rom: &Path, frames: usize) -> (bool, String) {
    let mut gb = match make_gb(mode, rom) {
        Ok(g) => g,
        Err(e) => return (false, format!("ERR {e}")),
    };
    let budget = frames as u64 * CYCLES_PER_FRAME as u64;
    let mut cyc = 0u64;
    let mut prev_start = false;
    let mut out: Vec<u8> = Vec::new();
    while cyc < budget {
        let sc = gb.read_memory(0xFF02);
        let start = (sc & 0x80) != 0 && (sc & 0x01) != 0;
        if start && !prev_start {
            out.push(gb.read_memory(0xFF01));
            // Stop early once a verdict has been transmitted.
            let s = String::from_utf8_lossy(&out);
            if s.contains("Passed") || s.contains("Failed") || s.contains("Error") {
                // run a little more to flush, then break
            }
        }
        prev_start = start;
        let (_bp, c) = gb.step_instruction(false);
        cyc += c as u64;
        // Early exit check on accumulated text.
        if out.len() >= 6 {
            let s = String::from_utf8_lossy(&out);
            if s.contains("Passed") {
                return (true, "Passed (serial)".into());
            }
            if s.contains("Failed") || s.contains("Error") {
                let tail: String = s.chars().rev().take(40).collect::<String>().chars().rev().collect();
                return (false, format!("serial verdict: ...{}", tail.replace('\n', " ")));
            }
        }
    }
    let s = String::from_utf8_lossy(&out);
    if s.contains("Passed") {
        (true, "Passed (serial)".into())
    } else if s.is_empty() {
        (false, "no serial output (timeout)".into())
    } else {
        let tail: String = s.chars().rev().take(60).collect::<String>().chars().rev().collect();
        (false, format!("no Passed; tail: {}", tail.replace('\n', " ")))
    }
}

/// blargg memory-protocol grading. blargg ROMs with cart RAM write their result
/// to 0xA000 (0x80 while running, final code on completion: 0x00 == pass) once
/// the signature 0xDE 0xB0 0x61 appears at 0xA001-0xA003. We also capture the
/// ASCII text blargg mirrors into 0xA004.. for the failure detail.
fn grade_blargg_mem(mode: &str, rom: &Path, frames: usize) -> (bool, String) {
    let mut gb = match make_gb(mode, rom) {
        Ok(g) => g,
        Err(e) => return (false, format!("ERR {e}")),
    };
    let budget = frames as u64 * CYCLES_PER_FRAME as u64;
    let mut cyc = 0u64;
    // Read the protocol bytes from the cart RAM backing store (bank 0, offset 0
    // == 0xA000) so the result survives blargg disabling RAM before halting.
    let read_ram = |gb: &GB, off: usize| -> u8 {
        gb.cartridge()
            .map(|c| c.save_ram().get(off).copied().unwrap_or(0xFF))
            .unwrap_or(0xFF)
    };
    // blargg writes the signature first, then sets A000=0x80 while running, then
    // the final code. Only trust a completion once we have actually observed the
    // running marker (0x80), so the uninitialized 0xFF window is not mistaken for
    // a verdict.
    let mut saw_running = false;
    loop {
        let sig = [read_ram(&gb, 1), read_ram(&gb, 2), read_ram(&gb, 3)];
        let status = read_ram(&gb, 0);
        if sig == [0xDE, 0xB0, 0x61] && status == 0x80 {
            saw_running = true;
        }
        if saw_running && sig == [0xDE, 0xB0, 0x61] && status != 0x80 {
            // text region (RAM offset 4..)
            let mut txt = Vec::new();
            for off in 4usize..0x200 {
                let b = read_ram(&gb, off);
                if b == 0 {
                    break;
                }
                txt.push(b);
            }
            let s = String::from_utf8_lossy(&txt);
            let oneline = s.replace('\n', " ");
            if status == 0x00 {
                return (true, format!("Passed (mem) {}", oneline.trim()));
            }
            return (false, format!("code={status:02X}: {}", oneline.trim()));
        }
        if cyc >= budget {
            return (false, "no result signature (timeout)".into());
        }
        let (_bp, c) = gb.step_instruction(false);
        cyc += c as u64;
    }
}

fn grade_mooneye(mode: &str, rom: &Path) -> (bool, String) {
    let mut gb = match make_gb(mode, rom) {
        Ok(g) => g,
        Err(e) => return (false, format!("ERR {e}")),
    };
    // mooneye tests complete quickly; 250M cycles is ~60s of GB time, ample.
    if !run_until_ldbb(&mut gb, 250_000_000) {
        return (false, "no LD B,B (timeout)".into());
    }
    let r = gb.get_cpu_registers();
    let ok = r.b == 3 && r.c == 5 && r.d == 8 && r.e == 13 && r.h == 21 && r.l == 34;
    if ok {
        (true, "ok".into())
    } else {
        (
            false,
            format!(
                "regs B={:02X} C={:02X} D={:02X} E={:02X} H={:02X} L={:02X} (want 03 05 08 0D 15 22)",
                r.b, r.c, r.d, r.e, r.h, r.l
            ),
        )
    }
}

/// gbmicrotest: run a frame budget then check the result protocol. The verdict
/// byte is at 0xFF82 (0x01 pass / 0xFF fail); 0xFF80 is the actual value and
/// 0xFF81 the expected value (informational only). A DMG-CPU-08 suite.
fn grade_memauto(mode: &str, rom: &Path, frames: usize) -> (bool, String) {
    let mut gb = match make_gb(mode, rom) {
        Ok(g) => g,
        Err(e) => return (false, format!("ERR {e}")),
    };
    let budget = frames as u64 * CYCLES_PER_FRAME as u64;
    let mut cyc = 0u64;
    while cyc < budget {
        let (_bp, c) = gb.step_instruction(false);
        cyc += c as u64;
    }
    let r80 = gb.read_memory(0xFF80);
    let r81 = gb.read_memory(0xFF81);
    let r82 = gb.read_memory(0xFF82);
    if r82 == 0x01 {
        (true, "ok".into())
    } else {
        (
            false,
            format!("FF82={r82:02X} (want 01) actual(FF80)={r80:02X} expected(FF81)={r81:02X}"),
        )
    }
}

fn grade_mem(mode: &str, rom: &Path, addr: u16, expected: u8, frames: usize) -> (bool, String) {
    let mut gb = match make_gb(mode, rom) {
        Ok(g) => g,
        Err(e) => return (false, format!("ERR {e}")),
    };
    let budget = frames as u64 * CYCLES_PER_FRAME as u64;
    let mut cyc = 0u64;
    while cyc < budget {
        let (_bp, c) = gb.step_instruction(false);
        cyc += c as u64;
    }
    let got = gb.read_memory(addr);
    if got == expected {
        (true, "ok".into())
    } else {
        (false, format!("[{addr:04X}]={got:02X} want {expected:02X}"))
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: suite_harness <manifest> [frames]");
        std::process::exit(2);
    }
    let manifest = fs::read_to_string(&args[1]).expect("read manifest");
    let frames: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(60);

    let (mut pass, mut fail, mut total) = (0usize, 0usize, 0usize);
    for line in manifest.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split('|').collect();
        if f.len() < 4 {
            eprintln!("SKIP malformed: {line}");
            continue;
        }
        let (id, mode, grading, rom) = (f[0], f[1], f[2], Path::new(f[3]));
        let arg = f.get(4).copied().unwrap_or("");
        total += 1;
        let (ok, detail) = match grading {
            "png" => grade_png(mode, rom, Path::new(arg), frames),
            "serial" => grade_serial(mode, rom, frames),
            "blargg_mem" => grade_blargg_mem(mode, rom, frames),
            "mooneye" => grade_mooneye(mode, rom),
            "memauto" => grade_memauto(mode, rom, frames),
            "mem" => {
                // arg = "ADDR=VAL" in hex
                let parts: Vec<&str> = arg.split('=').collect();
                let addr = u16::from_str_radix(parts[0].trim_start_matches("0x"), 16).unwrap_or(0);
                let val = u8::from_str_radix(parts[1].trim_start_matches("0x"), 16).unwrap_or(0);
                grade_mem(mode, rom, addr, val, frames)
            }
            other => (false, format!("unknown grading {other}")),
        };
        if ok {
            pass += 1;
            println!("PASS {id} [{mode}] {detail}");
        } else {
            fail += 1;
            println!("FAIL {id} [{mode}] {detail}");
        }
    }
    println!("\n=== {pass}/{total} passed ({fail} failed) ===");
}
