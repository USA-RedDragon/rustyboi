//! `harness` — the by-hand dev harnesses in one multitool bin. Each former
//! standalone bin is a subcommand keeping its exact CLI shape and output
//! format:
//!
//!   harness sramdump <rom> <out.bin> [frames] [dmg|cgb]
//!       Run a ROM for N frames and dump cartridge save RAM to a file.
//!
//!   harness glitch --rom <rom-or-zip> --state <savestate.rustyboisave>
//!                  [--frames N] [--out DIR] [--dump-all] [--vram-frames F1,F2,...]
//!       ROM+savestate repro for rendering-glitch investigation: loads the
//!       savestate, reattaches the ROM, prints a per-frame FNV-1a hash of the
//!       RGB framebuffer, and dumps PNGs (all frames with --dump-all,
//!       otherwise only single-frame hash spikes) plus optional VRAM dumps.
//!
//!   harness unlboot <rom> [frames] [--hw dmg|cgb|auto] [--out DIR]
//!                   [--press BTN@FRAME ...] [--detect-only] [--shots F1,F2,...]
//!       Boot validation for unlicensed-mapper carts: reports the detected
//!       mapper, prints frame hashes at checkpoints, and writes PPM
//!       screenshots so bank-switch faults show up as garbage/blank frames.
//!
//!   harness camera-drive --rom <path[.zip]> [--frames N] [--sav <path>]
//!                        [--image <128x112 P5 .pgm | 14336-byte raw>]
//!                        [--input SCRIPT] [--out DIR] [--screens N]
//!                        [--shots F1,F2,...]
//!       Drive the Game Boy Camera (POCKET CAMERA $FC + M64282FP) headlessly
//!       with scripted joypad input, an optional battery `.sav`, and an
//!       optional externally-fed sensor image, dumping screen frames as PNGs.
//!       Also the reference for frontend integration: feed a live webcam/still
//!       via `Cartridge::set_camera_image(&[u8; 128*112])` (through
//!       `GB::cartridge_mut()`) whenever a new source frame is available.
//!
//!   harness printer-drive --rom <path[.zip]> [--mode dmg|cgb] [--frames N]
//!                         [--input SCRIPT] [--out DIR] [--screens N]
//!       Drive a ROM headlessly with a printer on the link port and dump
//!       screen frames plus every captured print as PNGs.
//!
//! Input SCRIPT is the shared `frame:BUTTONS` DSL (see shared/script.rs).
//! Everything is fully deterministic (frame-keyed input, no wall clock), so a
//! script is a reproducible repro. Unlike the old standalone bins, unknown
//! flags are rejected and `--help` prints the subcommand's usage.

use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{GB, Hardware};
use rustyboi_core_lib::input::ButtonState;
use std::path::PathBuf;
use std::process::ExitCode;

use rustyboi_test_runner_lib::imaging::{encode_rgb_png, fnv1a, frame_rgb, write_ppm};
use rustyboi_test_runner_lib::script;

const USAGE_SRAMDUMP: &str = "harness sramdump <rom> <out.bin> [frames] [dmg|cgb]";
const USAGE_GLITCH: &str = "harness glitch --rom <rom-or-zip> --state <savestate.rustyboisave> \
                            [--frames N] [--out DIR] [--dump-all] [--vram-frames F1,F2,...]";
const USAGE_UNLBOOT: &str = "harness unlboot <rom> [frames] [--hw dmg|cgb|auto] [--out DIR] \
                             [--press BTN@FRAME ...] [--detect-only] [--shots F1,F2,...]";
const USAGE_CAMERA: &str = "harness camera-drive --rom <path[.zip]> [--frames N] [--sav <path>] \
                            [--image <128x112 P5 .pgm | 14336-byte raw>] [--input SCRIPT] \
                            [--out DIR] [--screens N] [--shots F1,F2,...]";
const USAGE_PRINTER: &str = "harness printer-drive --rom <path[.zip]> [--mode dmg|cgb] [--frames N] \
                             [--input SCRIPT] [--out DIR] [--screens N]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(String::as_str);
    let rest = &args[args.len().min(2)..];
    type Cmd = fn(&[String]) -> Result<(), String>;
    let (usage, run): (&str, Cmd) = match sub {
        Some("sramdump") => (USAGE_SRAMDUMP, cmd_sramdump),
        Some("glitch") => (USAGE_GLITCH, cmd_glitch),
        Some("unlboot") => (USAGE_UNLBOOT, cmd_unlboot),
        Some("camera-drive") => (USAGE_CAMERA, cmd_camera_drive),
        Some("printer-drive") => (USAGE_PRINTER, cmd_printer_drive),
        _ => {
            eprintln!(
                "usage:\n  {USAGE_SRAMDUMP}\n  {USAGE_GLITCH}\n  {USAGE_UNLBOOT}\n  \
                 {USAGE_CAMERA}\n  {USAGE_PRINTER}"
            );
            return ExitCode::from(2);
        }
    };
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: {usage}");
        return ExitCode::SUCCESS;
    }
    match run(rest) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// shared strict CLI parser (one stack for every subcommand)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Cli {
    positionals: Vec<String>,
    /// `--flag value` occurrences in order (flags may repeat, e.g. `--press`).
    values: Vec<(String, String)>,
    switches: Vec<String>,
}

/// Parse `args` against the subcommand's declared flags. Unlike the old
/// standalone bins' ad-hoc parsers, an undeclared `--flag` is an error instead
/// of being silently ignored (a typo used to silently change semantics).
fn parse_cli(args: &[String], value_flags: &[&str], switch_flags: &[&str]) -> Result<Cli, String> {
    let mut cli = Cli::default();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a.starts_with("--") {
            if value_flags.contains(&a) {
                let v = args.get(i + 1).ok_or_else(|| format!("{a} requires a value"))?;
                cli.values.push((a.to_string(), v.clone()));
                i += 2;
            } else if switch_flags.contains(&a) {
                cli.switches.push(a.to_string());
                i += 1;
            } else {
                return Err(format!("unknown flag {a} (try --help)"));
            }
        } else {
            cli.positionals.push(a.to_string());
            i += 1;
        }
    }
    Ok(cli)
}

impl Cli {
    /// First occurrence of `--name value` (matches the old bins' `arg_value`).
    fn value(&self, name: &str) -> Option<&str> {
        self.values.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    fn values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.values.iter().filter(move |(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    fn has(&self, name: &str) -> bool {
        self.switches.iter().any(|s| s == name)
    }

    fn parsed<T: std::str::FromStr>(&self, name: &str, default: T) -> Result<T, String> {
        match self.value(name) {
            Some(v) => v.parse().map_err(|_| format!("bad {name} {v:?}")),
            None => Ok(default),
        }
    }

    fn no_positionals(&self) -> Result<(), String> {
        match self.positionals.first() {
            Some(p) => Err(format!("unexpected argument {p:?} (try --help)")),
            None => Ok(()),
        }
    }
}

/// Comma-separated frame list (`--shots`, `--vram-frames`).
fn parse_frame_list<T: std::str::FromStr>(spec: &str) -> Result<Vec<T>, String> {
    spec.split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().map_err(|_| format!("bad frame index {s:?}")))
        .collect()
}

// ---------------------------------------------------------------------------
// sramdump
// ---------------------------------------------------------------------------

fn cmd_sramdump(args: &[String]) -> Result<(), String> {
    let cli = parse_cli(args, &[], &[])?;
    let p = &cli.positionals;
    let (rom, out) = match (p.first(), p.get(1)) {
        (Some(rom), Some(out)) => (rom, out),
        _ => return Err(format!("usage: {USAGE_SRAMDUMP}")),
    };
    // Positional-tail semantics preserved from the standalone bin: a
    // non-numeric [frames] falls back to 800, and anything but dmg/cgb in the
    // [dmg|cgb] slot means auto.
    let frames: usize = p.get(2).and_then(|s| s.parse().ok()).unwrap_or(800);
    let bytes = std::fs::read(rom).expect("read ROM file");
    let cart = Cartridge::from_bytes(&bytes).expect("load ROM");
    let hardware = match p.get(3).map(|s| s.as_str()) {
        Some("dmg") => Hardware::DMG,
        Some("cgb") => Hardware::CGB,
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
    for _ in 0..frames {
        gb.run_until_frame(false);
    }
    let sram = gb.cartridge().expect("cartridge").save_ram().to_vec();
    std::fs::write(out, &sram).expect("write dump");
    eprintln!("dumped {} bytes", sram.len());
    Ok(())
}

// ---------------------------------------------------------------------------
// glitch
// ---------------------------------------------------------------------------

fn cmd_glitch(args: &[String]) -> Result<(), String> {
    let cli = parse_cli(
        args,
        &["--rom", "--state", "--frames", "--out", "--vram-frames"],
        &["--dump-all"],
    )?;
    cli.no_positionals()?;
    let rom_path = cli.value("--rom").ok_or("--rom <path> required")?;
    let state_path = cli.value("--state").ok_or("--state <path> required")?;
    let frame_count: u32 = cli.parsed("--frames", 600)?;
    let out = cli.value("--out").unwrap_or("glitch-out");
    let dump_all = cli.has("--dump-all");
    let vram_frames: Vec<u32> = parse_frame_list(cli.value("--vram-frames").unwrap_or(""))?;

    let rom_container = std::fs::read(rom_path).expect("read rom");
    let rom = Cartridge::extract_rom_bytes(&rom_container).expect("extract rom");
    let state = std::fs::read(state_path).expect("read state");
    let mut gb = GB::from_state_bytes(&state).expect("load state");
    if gb.cartridge_needs_rom() {
        assert!(gb.reattach_rom(&rom), "state carried no cartridge");
    }
    std::fs::create_dir_all(out).expect("mkdir out");

    let mut frames: Vec<(u64, Vec<u8>)> = Vec::with_capacity(frame_count as usize);
    for i in 0..frame_count {
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
            std::fs::write(format!("{out}/vram{i:04}.bin"), v).expect("write vram");
        }
    }

    for (i, (h, rgb)) in frames.iter().enumerate() {
        let prev = i.checked_sub(1).map(|p| frames[p].0);
        let next = frames.get(i + 1).map(|n| n.0);
        let spike = prev.is_some_and(|p| p != *h) && next.is_some_and(|n| n != *h);
        println!("frame {i:04} {h:016x}{}", if spike { " SPIKE" } else { "" });
        if dump_all || spike {
            let png = encode_rgb_png(160, 144, rgb);
            std::fs::write(format!("{out}/f{i:04}.png"), png).expect("write png");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// unlboot
// ---------------------------------------------------------------------------

fn cmd_unlboot(args: &[String]) -> Result<(), String> {
    let cli = parse_cli(args, &["--hw", "--out", "--press", "--shots"], &["--detect-only"])?;
    let mut positionals = cli.positionals.iter();
    let path = positionals.next().ok_or_else(|| format!("usage: {USAGE_UNLBOOT}"))?;
    // Any further positional is the frame count (last one wins, as before).
    let mut frames: usize = 600;
    for tok in positionals {
        frames = tok.parse().map_err(|_| format!("bad frame count {tok:?}"))?;
    }
    let hw = cli.value("--hw").unwrap_or("auto");
    let out_dir: Option<PathBuf> = cli.value("--out").map(PathBuf::from);
    let mut presses: Vec<(String, usize)> = Vec::new();
    for spec in cli.values("--press") {
        let (btn, frame) = spec
            .split_once('@')
            .ok_or_else(|| format!("--press wants BTN@FRAME, got {spec:?}"))?;
        let frame = frame.parse().map_err(|_| format!("bad --press frame {frame:?}"))?;
        presses.push((btn.to_string(), frame));
    }
    let detect_only = cli.has("--detect-only");
    let shots: Vec<usize> = parse_frame_list(cli.value("--shots").unwrap_or(""))?;

    let bytes = std::fs::read(path).expect("read ROM file");
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
        return Ok(());
    }

    let hardware = match hw {
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
        std::fs::create_dir_all(dir).expect("create out dir");
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
            let bytes = frame_rgb(&frame);
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
    Ok(())
}

// ---------------------------------------------------------------------------
// camera-drive
// ---------------------------------------------------------------------------

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

fn cmd_camera_drive(args: &[String]) -> Result<(), String> {
    let cli = parse_cli(
        args,
        &["--rom", "--frames", "--sav", "--image", "--input", "--out", "--screens", "--shots"],
        &[],
    )?;
    cli.no_positionals()?;
    let rom = cli.value("--rom").ok_or("--rom <path> required")?;
    let frames: usize = cli.parsed("--frames", 600)?;
    let out = PathBuf::from(cli.value("--out").unwrap_or("."));
    let screens: usize = cli.parsed("--screens", 0)?;
    let shots: Vec<usize> = parse_frame_list(cli.value("--shots").unwrap_or(""))?;
    let events = script::parse_script(cli.value("--input").unwrap_or(""));

    std::fs::create_dir_all(&out).expect("create out dir");

    // Read the (possibly zipped) ROM in-memory; from_bytes extracts zips
    // without touching disk and never auto-creates sidecar files.
    let bytes = std::fs::read(rom).expect("read ROM file");
    let mut cart = Cartridge::from_bytes(&bytes).expect("load ROM");
    println!(
        "camera cart: has_camera={} battery={} ram={}KB",
        cart.has_camera(),
        cart.has_battery(),
        cart.save_ram().len() / 1024,
    );
    if let Some(sav) = cli.value("--sav") {
        cart.attach_save_file(sav).expect("attach --sav");
        println!("sav attached: {sav}");
    }
    if let Some(image) = cli.value("--image") {
        cart.set_camera_image(&load_sensor_image(image));
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
    Ok(())
}

// ---------------------------------------------------------------------------
// printer-drive
// ---------------------------------------------------------------------------

fn cmd_printer_drive(args: &[String]) -> Result<(), String> {
    let cli = parse_cli(
        args,
        &["--rom", "--mode", "--frames", "--input", "--out", "--screens"],
        &[],
    )?;
    cli.no_positionals()?;
    let rom = cli.value("--rom").ok_or("--rom <path> required")?;
    let mode = cli.value("--mode").unwrap_or("cgb");
    let frames: usize = cli.parsed("--frames", 600)?;
    let out = PathBuf::from(cli.value("--out").unwrap_or("."));
    let screens: usize = cli.parsed("--screens", 0)?;
    let events = script::parse_script(cli.value("--input").unwrap_or(""));

    let hardware = match mode {
        "dmg" => Hardware::DMG,
        "cgb" => Hardware::CGB,
        other => return Err(format!("unknown mode {other:?}")),
    };
    std::fs::create_dir_all(&out).expect("create out dir");

    let mut gb = GB::new(hardware);
    gb.insert(Cartridge::load(rom).expect("load ROM"));
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
    Ok(())
}
