mod expectation;
mod frame;
mod report;
mod runner;

use crate::expectation::{Mode, TestCase, cases_for_rom, parse_manifest};
use crate::report::Summary;
use clap::Parser;
use std::collections::HashSet;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

#[derive(Debug, Parser)]
#[command(about = "Run Gambatte-style Game Boy hardware tests against rustyboi")]
struct Args {
    /// Run a suite manifest (acid2/mealybug/blargg/gambatte/...). Each line:
    /// `<id>|<mode>|<grading>|<rom>[|<arg>...]` where grading is one of
    /// png/png_fixed/png_shootout/serial/blargg_mem/memauto/mem/mooneye/
    /// mooneye_ed/sram/gambatte (gambatte rows use mode `auto`: oracle + modes
    /// are filename-encoded). Regenerate manifests with `tools/gen_manifests.py`.
    #[arg(long, value_name = "FILE")]
    manifest: Option<PathBuf>,

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

    /// Parallel case workers. Cases are independent (one GB instance each);
    /// results are printed/recorded in case order, so the text and JSON output
    /// are byte-identical to a sequential run. Default: cores-1. Forced to 1
    /// when --trace-rom, --fail-fast, or --ss-dump is active (their streamed
    /// diagnostics / early-stop semantics require sequential execution).
    #[arg(long, value_name = "N")]
    jobs: Option<usize>,

    /// Diagnostic: after each mooneye-graded case, dump N 8-byte rows of the
    /// SameSuite results buffer to stderr (forces --jobs 1).
    #[arg(long, value_name = "N")]
    ss_dump: Option<u16>,

    /// Base address for --ss-dump (hex, default C000; some tests store their
    /// results in VRAM).
    #[arg(long, value_name = "ADDR", value_parser = parse_hex_u16)]
    ss_dump_base: Option<u16>,

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

    // Manifest mode: a c-sp public suite. Cases come from the manifest directly
    // (it carries the model + grading per ROM), not the name-based discovery.
    if let Some(manifest_path) = &args.manifest {
        return run_manifest(manifest_path, &enabled_modes, &args);
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
        ss_dump: args.ss_dump,
        ss_dump_base: args.ss_dump_base,
    };

    run_cases(cases, &run_options, resolve_jobs(&args), args.fail_fast, &mut summary)?;

    println!();
    report::print_summary(&summary);

    if let Some(path) = args.json {
        report::write_json(&summary, &path)?;
    }

    Ok(summary.exit_code())
}

fn parse_hex_u16(v: &str) -> Result<u16, String> {
    u16::from_str_radix(v.trim_start_matches("0x"), 16).map_err(|e| e.to_string())
}

/// Resolve the worker count: --jobs, else cores-1.
/// Trace/diagnostic modes and --fail-fast force 1 (streamed stderr output
/// would interleave; fail-fast must stop at the first failure in case order).
fn resolve_jobs(args: &Args) -> usize {
    if args.trace_rom.is_some() || args.fail_fast || args.ss_dump.is_some() {
        if args.jobs.is_some_and(|jobs| jobs > 1) {
            eprintln!("note: --jobs forced to 1 (--trace-rom / --fail-fast / --ss-dump)");
        }
        return 1;
    }
    if let Some(jobs) = args.jobs {
        return jobs.max(1);
    }
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);
    cores.saturating_sub(1).max(1)
}

/// Run all cases and record them into `summary` in case order. `jobs <= 1`
/// preserves the original sequential loop (including --fail-fast). Otherwise
/// cases are executed by a worker pool (dynamic dispatch via an atomic index)
/// and results flow back over a channel into an index-keyed reorder buffer, so
/// progress chars, failure details, and summary/JSON records are emitted in
/// exactly the sequential order — output is byte-identical for equal results
/// regardless of scheduling.
fn run_cases(
    cases: Vec<TestCase>,
    run_options: &runner::RunOptions,
    jobs: usize,
    fail_fast: bool,
    summary: &mut Summary,
) -> Result<(), String> {
    let emit = |result: &runner::CaseResult, summary: &mut Summary| -> Result<(), String> {
        print!("{}", result.case.mode.progress_char());
        io::stdout()
            .flush()
            .map_err(|error| format!("failed to flush stdout: {error}"))?;
        if !result.passed {
            report::print_failure(result);
        }
        summary.record(result);
        Ok(())
    };

    if jobs <= 1 || cases.len() <= 1 {
        for case in cases {
            let result = runner::run_case(case, run_options);
            let failed = !result.passed;
            emit(&result, summary)?;
            if failed && fail_fast {
                break;
            }
        }
        return Ok(());
    }

    let cases = &cases[..];
    let next_case = AtomicUsize::new(0);
    let (sender, receiver) = mpsc::channel::<(usize, runner::CaseResult)>();
    let mut pending: Vec<Option<runner::CaseResult>> = Vec::new();
    pending.resize_with(cases.len(), || None);
    let mut next_emit = 0usize;

    std::thread::scope(|scope| -> Result<(), String> {
        for _ in 0..jobs.min(cases.len()) {
            let sender = sender.clone();
            let next_case = &next_case;
            scope.spawn(move || {
                loop {
                    let index = next_case.fetch_add(1, Ordering::Relaxed);
                    if index >= cases.len() {
                        break;
                    }
                    let result = runner::run_case(cases[index].clone(), run_options);
                    if sender.send((index, result)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);

        while let Ok((index, result)) = receiver.recv() {
            pending[index] = Some(result);
            while next_emit < pending.len() {
                let Some(result) = pending[next_emit].take() else {
                    break;
                };
                emit(&result, summary)?;
                next_emit += 1;
            }
        }
        Ok(())
    })
}

/// Run a c-sp public-suite manifest. Parses the `|`-separated manifest into
/// cases (keeping only the requested modes), runs each, prints per-failure
/// detail, and emits the same summary + optional JSON as the Gambatte path.
fn run_manifest(
    manifest_path: &PathBuf,
    enabled_modes: &HashSet<Mode>,
    args: &Args,
) -> Result<u8, String> {
    let text = std::fs::read_to_string(manifest_path)
        .map_err(|e| format!("read manifest {}: {e}", manifest_path.display()))?;
    let mut cases = parse_manifest(&text, enabled_modes)?;
    if let Some(limit) = args.limit {
        cases.truncate(limit);
    }
    if cases.is_empty() {
        return Err(format!(
            "manifest {} produced no cases for the requested modes",
            manifest_path.display()
        ));
    }

    println!(
        "Manifest {}: {} runnable cases.",
        manifest_path.display(),
        cases.len()
    );

    let mut summary = Summary::default();
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
        ss_dump: args.ss_dump,
        ss_dump_base: args.ss_dump_base,
    };

    run_cases(cases, &run_options, resolve_jobs(args), args.fail_fast, &mut summary)?;

    println!();
    report::print_summary(&summary);

    if let Some(path) = &args.json {
        report::write_json(&summary, path)?;
    }

    Ok(summary.exit_code())
}

fn collect_roms(args: &Args) -> Result<Vec<PathBuf>, String> {
    Ok(args.roms.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_u16_accepts_prefixed_and_bare() {
        assert_eq!(parse_hex_u16("C000"), Ok(0xC000));
        assert_eq!(parse_hex_u16("0xC000"), Ok(0xC000));
        assert_eq!(parse_hex_u16("0x00"), Ok(0));
        assert!(parse_hex_u16("gg").is_err());
        assert!(parse_hex_u16("10000").is_err(), "overflows u16");
    }

    // Clap defaults, then override the fields under test.
    fn args() -> Args {
        Args::parse_from(["test-runner"])
    }

    #[test]
    fn resolve_jobs_honors_explicit_count() {
        let mut a = args();
        a.jobs = Some(4);
        assert_eq!(resolve_jobs(&a), 4);
        a.jobs = Some(0);
        assert_eq!(resolve_jobs(&a), 1, "0 clamps up to 1");
    }

    #[test]
    fn resolve_jobs_defaults_to_at_least_one() {
        assert!(resolve_jobs(&args()) >= 1);
    }

    #[test]
    fn resolve_jobs_forces_serial_for_streamed_modes() {
        for set in [
            |a: &mut Args| a.fail_fast = true,
            |a: &mut Args| a.trace_rom = Some("foo".to_string()),
            |a: &mut Args| a.ss_dump = Some(4),
        ] {
            let mut a = args();
            a.jobs = Some(8);
            set(&mut a);
            assert_eq!(resolve_jobs(&a), 1);
        }
    }
}
