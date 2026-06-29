mod expectation;
mod frame;
mod report;
mod runner;

use crate::expectation::{Mode, cases_for_rom, is_rom_path};
use crate::report::Summary;
use clap::Parser;
use std::collections::HashSet;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use walkdir::WalkDir;

#[derive(Debug, Parser)]
#[command(about = "Run Gambatte-style Game Boy hardware tests against rustyboi")]
struct Args {
    /// Directory containing Gambatte hwtests. ROMs are discovered recursively.
    #[arg(long, value_name = "DIR")]
    suite: Option<PathBuf>,

    /// Hardware modes to run, comma-separated.
    #[arg(long, value_delimiter = ',', default_value = "dmg,cgb")]
    mode: Vec<Mode>,

    /// Number of post-BIOS LCD frames to run before evaluating each test.
    #[arg(long, default_value_t = 15)]
    frames: usize,

    /// Limit the number of ROMs considered after sorting.
    #[arg(long)]
    limit: Option<usize>,

    /// Stop after the first failing case.
    #[arg(long)]
    fail_fast: bool,

    /// After a PNG failure, scan this many additional frames and report if one matches.
    #[arg(long, default_value_t = 0)]
    scan_frames: usize,

    /// Write failing actual/expected frames as PPM files.
    #[arg(long, value_name = "DIR")]
    dump_dir: Option<PathBuf>,

    /// Trace CPU/PPU timing events for ROM paths containing this text.
    #[arg(long, value_name = "SUBSTRING")]
    trace_rom: Option<String>,

    /// Maximum timing trace events to print per traced case.
    #[arg(long, default_value_t = 160)]
    trace_limit: usize,

    /// Only emit timing trace events for this zero-based frame index.
    #[arg(long)]
    trace_frame: Option<usize>,

    /// Only emit timing trace events touching this LY value.
    #[arg(long, value_name = "LY")]
    trace_ly: Option<u8>,

    /// Write a machine-readable JSON summary.
    #[arg(long, value_name = "FILE")]
    json: Option<PathBuf>,

    /// Run the REAL boot ROM (bios/dmg_boot.bin, bios/cgb_boot.bin) before each
    /// test instead of the synthetic skip_bios seed (mirrors Gambatte). Falls
    /// back to skip_bios per case if the matching bios file is absent.
    #[arg(long)]
    real_bios: bool,

    /// Directory holding dmg_boot.bin / cgb_boot.bin (default: bios/).
    #[arg(long, value_name = "DIR")]
    bios_dir: Option<PathBuf>,

    /// Diagnostic: run the real boot ROM to handoff and diff the FULL post-boot
    /// state against skip_bios() for the requested mode(s); print every
    /// discrepancy and exit. Requires a ROM (uses the first discovered/given).
    #[arg(long)]
    validate_bios: bool,

    /// AGB bootstrap validation: compare rustyboi Hardware::AGB framebuffer
    /// hashes against a Gambatte-AGB hash file (produced by
    /// tools/gambatte_agb_ref). Each line: `<hex-hash>\t<rom-path>`. Runs each
    /// listed ROM in AGB mode for `--frames` frames and reports per-ROM match.
    #[arg(long, value_name = "HASHFILE")]
    agb_vs_gambatte: Option<PathBuf>,

    /// Explicit ROM paths, Gambatte testrunner style.
    #[arg(value_name = "ROM")]
    roms: Vec<PathBuf>,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<u8, String> {
    let args = Args::parse();
    let enabled_modes = args.mode.iter().copied().collect::<HashSet<_>>();
    if enabled_modes.is_empty() {
        return Err("at least one mode must be selected".to_string());
    }

    let mut roms = collect_roms(&args)?;
    if roms.is_empty() {
        return Err("no ROMs were provided or discovered".to_string());
    }

    roms.sort();
    roms.dedup();

    if let Some(limit) = args.limit {
        roms.truncate(limit);
    }

    if let Some(hashfile) = &args.agb_vs_gambatte {
        return run_agb_vs_gambatte(hashfile, &roms, args.frames);
    }

    if args.validate_bios {
        let rom = roms
            .first()
            .ok_or_else(|| "no ROM available for --validate-bios".to_string())?;
        let mut total = 0usize;
        for mode in [Mode::Dmg, Mode::Cgb] {
            if enabled_modes.contains(&mode) {
                match runner::validate_bios(rom, mode, args.bios_dir.as_ref()) {
                    Ok(n) => total += n,
                    Err(e) => eprintln!("validate-bios {:?}: {e}", mode),
                }
            }
        }
        println!("\nGRAND TOTAL discrepancies across modes: {total}");
        return Ok(0);
    }

    let discovered_roms = roms.len();
    let mut cases = Vec::new();
    let mut skipped_roms = 0;

    for rom in roms {
        let mut rom_cases = cases_for_rom(&rom, &enabled_modes);
        if rom_cases.is_empty() {
            skipped_roms += 1;
        }
        cases.append(&mut rom_cases);
    }

    if cases.is_empty() {
        return Err(format!(
            "found {discovered_roms} ROMs, but none had supported DMG/CGB Gambatte oracles"
        ));
    }

    println!(
        "Discovered {discovered_roms} ROMs and {} runnable cases.",
        cases.len()
    );

    let mut summary = Summary {
        skipped_roms,
        ..Summary::default()
    };
    let run_options = runner::RunOptions {
        frames: args.frames,
        scan_frames: args.scan_frames,
        dump_dir: args.dump_dir.clone(),
        trace_rom: args.trace_rom.clone(),
        trace_limit: args.trace_limit,
        trace_frame: args.trace_frame,
        trace_ly: args.trace_ly,
        real_bios: args.real_bios,
        bios_dir: args.bios_dir.clone(),
    };

    for case in cases {
        print!("{}", case.mode.progress_char());
        io::stdout()
            .flush()
            .map_err(|error| format!("failed to flush stdout: {error}"))?;

        let result = runner::run_case(case, &run_options);
        let failed = !result.passed;
        if failed {
            report::print_failure(&result);
        }
        summary.record(&result);

        if failed && args.fail_fast {
            break;
        }
    }

    println!();
    report::print_summary(&summary);

    if let Some(path) = args.json {
        report::write_json(&summary, &path)?;
    }

    Ok(summary.exit_code())
}

/// AGB bootstrap validation. Parse the Gambatte-AGB hash file, run each ROM
/// through rustyboi's Hardware::AGB, and compare the framebuffer hashes. Reports
/// the match rate and lists every divergence. A divergence is either a
/// rustyboi-AGB bug or a known Gambatte AGB-FIXME ("Actual AGB results"); they
/// are reported together for downstream classification against real hardware.
fn run_agb_vs_gambatte(
    hashfile: &PathBuf,
    roms: &[PathBuf],
    frames: usize,
) -> Result<u8, String> {
    let text = std::fs::read_to_string(hashfile)
        .map_err(|e| format!("read hash file {}: {e}", hashfile.display()))?;
    let mut gambatte: std::collections::HashMap<String, Option<u64>> =
        std::collections::HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut it = line.splitn(2, '\t');
        let hash = it.next().unwrap_or("");
        let Some(path) = it.next() else { continue };
        let val = if hash == "LOADFAIL" {
            None
        } else {
            u64::from_str_radix(hash, 16).ok()
        };
        gambatte.insert(path.to_string(), val);
    }

    let mut compared = 0usize;
    let mut matched = 0usize;
    let mut diverged: Vec<String> = Vec::new();
    let mut errored: Vec<String> = Vec::new();

    for rom in roms {
        let key = rom.to_string_lossy().to_string();
        let Some(ghash) = gambatte.get(&key) else {
            continue; // ROM not in the Gambatte-AGB reference set
        };
        let Some(ghash) = ghash else {
            continue; // Gambatte failed to load this ROM; skip
        };
        compared += 1;
        match runner::agb_frame_hash(rom, frames) {
            Ok(rhash) if rhash == *ghash => matched += 1,
            Ok(rhash) => diverged.push(format!(
                "  DIVERGE  rusty={rhash:016x} gambatte={ghash:016x}  {key}"
            )),
            Err(e) => errored.push(format!("  ERROR    {e}  {key}")),
        }
    }

    println!("\n=== AGB bootstrap validation (rustyboi-AGB vs Gambatte-AGB) ===");
    println!("compared {compared} ROMs; {matched} matched, {} diverged, {} errored",
        diverged.len(), errored.len());
    if compared > 0 {
        println!("match rate: {:.1}%", 100.0 * matched as f64 / compared as f64);
    }
    if !diverged.is_empty() {
        println!("\n--- divergences (rustyboi-AGB bug OR Gambatte AGB-FIXME) ---");
        for d in &diverged {
            println!("{d}");
        }
    }
    if !errored.is_empty() {
        println!("\n--- errors ---");
        for e in &errored {
            println!("{e}");
        }
    }
    Ok(0)
}

fn collect_roms(args: &Args) -> Result<Vec<PathBuf>, String> {
    let mut roms = args.roms.clone();

    if let Some(suite) = &args.suite {
        for entry in WalkDir::new(suite) {
            let entry = entry.map_err(|error| format!("failed to walk test suite: {error}"))?;
            if entry.file_type().is_file() && is_rom_path(entry.path()) {
                roms.push(entry.path().to_path_buf());
            }
        }
    }

    Ok(roms)
}
