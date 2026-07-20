//! Headless movie recorder for rustyboi.
//!
//!   movie record  --rom R [--movie M] [--mode dmg|cgb|auto] [--frames N]
//!                 [--input "0:START;5:;20:A"] [--author A] [--golden G]
//!       Play a ROM under a scripted `frame:BUTTONS` timeline, write the movie
//!       to `--movie` (default <rom>.rbmv) and print (or write to `--golden`)
//!       the final-frame golden hash. Regenerates goldens intentionally.
//!
//! Record/replay live in the deterministic `rustyboi_core_lib::movie` core;
//! this bin only owns files. Determinism (no wall clock / RTC / threads in
//! core) makes every hash reproducible: the same ROM + script always yields
//! the same bytes.

use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Hardware, GB};
use rustyboi_core_lib::movie::{sha256, MovieMeta, Recorder};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rustyboi_test_runner_lib::cli::reject_unknown_flags;
use rustyboi_test_runner_lib::script::expand_timeline;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(String::as_str);
    let rest = &args[args.len().min(2)..];
    const USAGE_RECORD: &str = "movie record  --rom R [--movie M] [--mode dmg|cgb|auto] \
                                [--frames N] [--input SCRIPT] [--author A] [--golden FILE]";
    if sub != Some("record") {
        eprintln!("usage:\n  {USAGE_RECORD}");
        return ExitCode::from(2);
    }
    // Handled before the strict parse, which would reject `--help` as undeclared.
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: {USAGE_RECORD}");
        return ExitCode::SUCCESS;
    }
    let result = cmd_record(rest);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_record(args: &[String]) -> Result<(), String> {
    reject_unknown_flags(
        args,
        &["--rom", "--movie", "--mode", "--frames", "--input", "--author", "--golden"],
        &[],
    )?;
    let rom_path = arg(args, "--rom").ok_or("record: --rom <path> required")?;
    let mode = arg(args, "--mode").unwrap_or_else(|| "auto".into());
    let frames: usize = parse_num(args, "--frames", 900)?;
    let script = arg(args, "--input").unwrap_or_default();
    let author = arg(args, "--author").unwrap_or_default();
    let movie_path = arg(args, "--movie")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_movie_path(&rom_path));

    let rom_bytes = std::fs::read(&rom_path).map_err(|e| format!("read {rom_path}: {e}"))?;
    let cart = Cartridge::from_bytes(&rom_bytes).map_err(|e| format!("load ROM: {e}"))?;
    let hardware = resolve_hardware(&mode, &cart);
    let rom_hash = sha256(&rom_bytes);

    let mut gb = GB::new(hardware);
    let rom_name = Path::new(&rom_path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    gb.insert(cart);
    gb.skip_bios();

    let timeline = expand_timeline(&script, frames);
    let mut recorder = Recorder::new(&mut gb, rom_hash, hardware).with_meta(MovieMeta {
        author,
        rom_name: rom_name.clone(),
        frame_count: 0,
        note: format!("recorded via movie record; mode={mode}"),
    });
    let mut final_hash = 0u64;
    for input in &timeline {
        final_hash = recorder.set_input(*input);
    }
    let movie = recorder.finish();
    let bytes = movie.to_bytes();
    std::fs::write(&movie_path, &bytes).map_err(|e| format!("write {}: {e}", movie_path.display()))?;

    println!(
        "recorded {} frames of {rom_name} ({hardware:?}) -> {} ({} bytes)",
        movie.inputs.len(),
        movie_path.display(),
        bytes.len()
    );
    println!("golden_final_hash={final_hash:016x}");
    if let Some(golden_path) = arg(args, "--golden") {
        std::fs::write(&golden_path, format!("{final_hash:016x}\n"))
            .map_err(|e| format!("write golden {golden_path}: {e}"))?;
    }
    Ok(())
}

/// Value of `--flag value`, if present.
fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn parse_num(args: &[String], name: &str, default: usize) -> Result<usize, String> {
    match arg(args, name) {
        Some(v) => v.parse().map_err(|_| format!("bad {name} {v:?}")),
        None => Ok(default),
    }
}

fn default_movie_path(rom: &str) -> PathBuf {
    let mut p = PathBuf::from(rom);
    p.set_extension("rbmv");
    p
}

fn resolve_hardware(mode: &str, cart: &Cartridge) -> Hardware {
    match mode {
        "dmg" => Hardware::DMG,
        "cgb" => Hardware::CGB,
        "auto" | "" => {
            if cart.supports_cgb() {
                Hardware::CGB
            } else {
                Hardware::DMG
            }
        }
        other => {
            eprintln!("warning: unknown mode {other:?}, using auto");
            if cart.supports_cgb() { Hardware::CGB } else { Hardware::DMG }
        }
    }
}
