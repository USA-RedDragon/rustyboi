use crate::expectation::{Mode, Oracle, TestCase};
use crate::frame;
use rustyboi_core_lib::audio::AudioOutput;
use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::cpu::registers::{INTERRUPT_ENABLE, INTERRUPT_FLAG};
use rustyboi_core_lib::gb::{GB, Hardware};
use rustyboi_core_lib::ppu::{
    CgbColorConversion, FetchDebugEvent, LCD_CONTROL, LCD_STATUS, LY, PixelDebugEvent,
};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

type SharedSamples = Arc<Mutex<Vec<(f32, f32)>>>;
const CYCLES_PER_FRAME: u32 = 70224;
const MAX_CYCLES_UNTIL_LCD_FRAME: u32 = CYCLES_PER_FRAME * 64;
/// Minimum cycle budget (in frames) for dump oracles, large enough for the
/// biggest dumper (wram, 32 KiB) plus its post-copy LCD re-enable and LY wait.
const DUMP_MIN_FRAMES: usize = 64;

#[derive(Debug)]
pub struct CaseResult {
    pub case: TestCase,
    pub passed: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    pub frames: usize,
    pub scan_frames: usize,
    pub dump_dir: Option<PathBuf>,
    pub trace_rom: Option<String>,
    pub trace_limit: usize,
    pub trace_frame: Option<usize>,
    pub trace_ly: Option<u8>,
    /// When set, run the REAL boot ROM (from `bios/`) before each test instead
    /// of the synthetic `skip_bios()` seed (mirrors Gambatte's testrunner).
    /// Falls back to `skip_bios()` per case if the matching bios file is absent.
    pub real_bios: bool,
    /// Directory holding `dmg_boot.bin` / `cgb_boot.bin`. Defaults to `bios/`
    /// resolved against the candidate roots in `resolve_bios_path`.
    pub bios_dir: Option<PathBuf>,
}

/// Locate the boot ROM file for a hardware mode. Tries `bios_dir` first (if
/// given), then `bios/` relative to CWD and to the crate manifest dir, then the
/// worktree default. Returns the first existing path.
fn resolve_bios_path(mode: Mode, bios_dir: Option<&PathBuf>) -> Option<PathBuf> {
    let file = match mode {
        Mode::Dmg => "dmg_boot.bin",
        Mode::Cgb => "cgb_boot.bin",
        // AGB uses the GBA's CGB-compat boot ROM.
        Mode::Agb => "cgb_agb_boot.bin",
    };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = bios_dir {
        candidates.push(dir.join(file));
    }
    candidates.push(PathBuf::from("bios").join(file));
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("bios")
            .join(file),
    );
    candidates.into_iter().find(|p| p.exists())
}

/// Set initial state for a case: real boot ROM if requested and available,
/// otherwise the synthetic skip_bios seed. Returns true if the real boot ROM
/// ran (so callers can branch on which path produced the state).
fn seed_initial_state(gb: &mut GB, case: &TestCase, options: &RunOptions) -> bool {
    if options.real_bios
        && let Some(path) = resolve_bios_path(case.mode, options.bios_dir.as_ref())
        && let Ok(()) = gb.load_bios(&path.to_string_lossy())
    {
        gb.run_boot_rom();
        return true;
    }
    // Fallback: synthetic post-boot seed (per-oracle residue selection).
    if matches!(case.oracle, Oracle::SramDump { .. }) {
        gb.skip_bios_with_boot_residue();
    } else {
        gb.skip_bios();
    }
    false
}

/// Full observable post-boot state snapshot for the skip_bios-vs-real-boot diff.
struct BootSnapshot {
    // CPU registers.
    a: u8, f: u8, b: u8, c: u8, d: u8, e: u8, h: u8, l: u8,
    sp: u16, pc: u16, ime: bool,
    ie: u8, iff: u8,
    io: [u8; 0x80],        // FF00-FF7F
    oam: [u8; 0xA0],       // FE00-FE9F
    feax: [u8; 0x60],      // FEA0-FEFF (unusable tail)
    hram: [u8; 0x7F],      // FF80-FFFE
    vram0: [u8; 0x2000],   // 8000-9FFF bank 0
    vram1: [u8; 0x2000],   // 8000-9FFF bank 1 (CGB)
    bgpal: [u16; 32],      // 8 palettes x 4 colors
    objpal: [u16; 32],
    timer_counter: u16,
}

fn snapshot_state(gb: &GB, cgb: bool) -> BootSnapshot {
    let r = gb.get_cpu_registers();
    let mut io = [0u8; 0x80];
    for (i, b) in io.iter_mut().enumerate() {
        *b = gb.read_memory(0xFF00 + i as u16);
    }
    let mut oam = [0u8; 0xA0];
    for (i, b) in oam.iter_mut().enumerate() {
        *b = gb.read_memory(0xFE00 + i as u16);
    }
    let mut feax = [0u8; 0x60];
    for (i, b) in feax.iter_mut().enumerate() {
        *b = gb.read_memory(0xFEA0 + i as u16);
    }
    let mut hram = [0u8; 0x7F];
    for (i, b) in hram.iter_mut().enumerate() {
        *b = gb.read_memory(0xFF80 + i as u16);
    }
    let mut vram0 = [0u8; 0x2000];
    let mut vram1 = [0u8; 0x2000];
    for i in 0..0x2000u16 {
        vram0[i as usize] = gb.read_vram_bank(0, 0x8000 + i);
        vram1[i as usize] = if cgb { gb.read_vram_bank(1, 0x8000 + i) } else { 0 };
    }
    let mut bgpal = [0u16; 32];
    let mut objpal = [0u16; 32];
    if cgb {
        for p in 0..8u8 {
            for c in 0..4u8 {
                bgpal[(p * 4 + c) as usize] = gb.bg_palette_pair(p, c);
                objpal[(p * 4 + c) as usize] = gb.obj_palette_pair(p, c);
            }
        }
    }
    BootSnapshot {
        a: r.a, f: r.f, b: r.b, c: r.c, d: r.d, e: r.e, h: r.h, l: r.l,
        sp: r.sp, pc: r.pc, ime: r.ime,
        ie: gb.read_memory(0xFFFF), iff: gb.read_memory(0xFF0F),
        io, oam, feax, hram, vram0, vram1, bgpal, objpal,
        timer_counter: gb.timer_internal_counter(),
    }
}

/// Run the real boot ROM and skip_bios independently on the same ROM, then
/// print every byte/register where they differ. The diff exposes latent
/// skip_bios hardware-accuracy bugs. Returns the number of discrepancies.
pub fn validate_bios(
    rom_path: &PathBuf,
    mode: Mode,
    bios_dir: Option<&PathBuf>,
) -> Result<usize, String> {
    let cgb = matches!(mode, Mode::Cgb | Mode::Agb);
    let hw = match mode {
        Mode::Dmg => Hardware::DMG,
        Mode::Cgb => Hardware::CGB,
        Mode::Agb => Hardware::AGB,
    };

    let bios_path = resolve_bios_path(mode, bios_dir)
        .ok_or_else(|| format!("no boot ROM found for {:?}", mode))?;
    let rom_data = fs::read(rom_path).map_err(|e| format!("read ROM: {e}"))?;

    // Real boot ROM.
    let mut gb_real = GB::new(hw);
    gb_real.insert(
        Cartridge::from_bytes(&rom_data).map_err(|e| format!("load ROM: {e}"))?,
    );
    gb_real
        .load_bios(&bios_path.to_string_lossy())
        .map_err(|e| format!("load bios: {e}"))?;
    let steps = gb_real.run_boot_rom();
    let real = snapshot_state(&gb_real, cgb);

    // skip_bios.
    let mut gb_skip = GB::new(hw);
    gb_skip.insert(
        Cartridge::from_bytes(&rom_data).map_err(|e| format!("load ROM: {e}"))?,
    );
    gb_skip.skip_bios();
    let skip = snapshot_state(&gb_skip, cgb);

    let io_name = |addr: u16| -> &'static str {
        match addr {
            0xFF00 => "JOYP", 0xFF01 => "SB", 0xFF02 => "SC", 0xFF04 => "DIV",
            0xFF05 => "TIMA", 0xFF06 => "TMA", 0xFF07 => "TAC", 0xFF0F => "IF",
            0xFF40 => "LCDC", 0xFF41 => "STAT", 0xFF42 => "SCY", 0xFF43 => "SCX",
            0xFF44 => "LY", 0xFF45 => "LYC", 0xFF46 => "DMA", 0xFF47 => "BGP",
            0xFF48 => "OBP0", 0xFF49 => "OBP1", 0xFF4A => "WY", 0xFF4B => "WX",
            0xFF4C => "KEY0", 0xFF4D => "KEY1", 0xFF4F => "VBK", 0xFF50 => "BOOT",
            0xFF51 => "HDMA1", 0xFF52 => "HDMA2", 0xFF53 => "HDMA3",
            0xFF54 => "HDMA4", 0xFF55 => "HDMA5", 0xFF56 => "RP",
            0xFF68 => "BCPS", 0xFF69 => "BCPD", 0xFF6A => "OCPS", 0xFF6B => "OCPD",
            0xFF70 => "SVBK", _ => "",
        }
    };

    let mut diffs: Vec<String> = Vec::new();
    macro_rules! cmp {
        ($field:ident, $name:expr) => {
            if real.$field != skip.$field {
                diffs.push(format!(
                    "  {:<12} real=0x{:02X} skip=0x{:02X}",
                    $name, real.$field, skip.$field
                ));
            }
        };
    }
    cmp!(a, "A"); cmp!(f, "F"); cmp!(b, "B"); cmp!(c, "C");
    cmp!(d, "D"); cmp!(e, "E"); cmp!(h, "H"); cmp!(l, "L");
    if real.sp != skip.sp {
        diffs.push(format!("  {:<12} real=0x{:04X} skip=0x{:04X}", "SP", real.sp, skip.sp));
    }
    if real.pc != skip.pc {
        diffs.push(format!("  {:<12} real=0x{:04X} skip=0x{:04X}", "PC", real.pc, skip.pc));
    }
    if real.ime != skip.ime {
        diffs.push(format!("  {:<12} real={} skip={}", "IME", real.ime, skip.ime));
    }
    cmp!(ie, "IE(FFFF)"); cmp!(iff, "IF(FF0F)");
    if real.timer_counter != skip.timer_counter {
        diffs.push(format!(
            "  {:<12} real=0x{:04X} skip=0x{:04X}",
            "DIV_CTR", real.timer_counter, skip.timer_counter
        ));
    }

    let mut io_diffs = 0;
    for i in 0..0x80usize {
        if real.io[i] != skip.io[i] {
            let addr = 0xFF00 + i as u16;
            diffs.push(format!(
                "  IO 0x{:04X} {:<6} real=0x{:02X} skip=0x{:02X}",
                addr, io_name(addr), real.io[i], skip.io[i]
            ));
            io_diffs += 1;
        }
    }

    let count_diffs = |a: &[u8], b: &[u8]| a.iter().zip(b).filter(|(x, y)| x != y).count();
    let oam_d = count_diffs(&real.oam, &skip.oam);
    let feax_d = count_diffs(&real.feax, &skip.feax);
    let hram_d = count_diffs(&real.hram, &skip.hram);
    let vram0_d = count_diffs(&real.vram0, &skip.vram0);
    let vram1_d = count_diffs(&real.vram1, &skip.vram1);
    let bgpal_d = real.bgpal.iter().zip(&skip.bgpal).filter(|(x, y)| x != y).count();
    let objpal_d = real.objpal.iter().zip(&skip.objpal).filter(|(x, y)| x != y).count();

    println!("\n=== BIOS validation: {:?} {} ===", mode, rom_path.display());
    println!("boot ROM executed {steps} instructions; handoff PC=0x{:04X}", real.pc);
    println!("--- CPU / IO / timer discrepancies ({}) ---", diffs.len());
    if diffs.is_empty() {
        println!("  (none)");
    } else {
        for d in &diffs {
            println!("{d}");
        }
    }
    println!("--- region byte-diff counts ---");
    println!("  OAM (FE00-FE9F):     {oam_d} / 160 bytes differ");
    println!("  FEAX (FEA0-FEFF):    {feax_d} / 96 bytes differ");
    println!("  HRAM (FF80-FFFE):    {hram_d} / 127 bytes differ");
    println!("  VRAM bank0:          {vram0_d} / 8192 bytes differ");
    if cgb {
        println!("  VRAM bank1:          {vram1_d} / 8192 bytes differ");
        println!("  CGB BG palette:      {bgpal_d} / 32 slots differ");
        println!("  CGB OBJ palette:     {objpal_d} / 32 slots differ");
    }

    // Detailed first-bytes for any differing region (cap the spam).
    let dump_region = |label: &str, a: &[u8], b: &[u8], base: u16| {
        let mut shown = 0;
        for (i, (x, y)) in a.iter().zip(b).enumerate() {
            if x != y {
                if shown < 24 {
                    println!("    {label} 0x{:04X}: real=0x{:02X} skip=0x{:02X}", base + i as u16, x, y);
                }
                shown += 1;
            }
        }
        if shown > 24 {
            println!("    {label}: ... and {} more", shown - 24);
        }
    };
    if oam_d > 0 { dump_region("OAM ", &real.oam, &skip.oam, 0xFE00); }
    if feax_d > 0 { dump_region("FEAX", &real.feax, &skip.feax, 0xFEA0); }
    if hram_d > 0 { dump_region("HRAM", &real.hram, &skip.hram, 0xFF80); }
    if vram0_d > 0 { dump_region("VRM0", &real.vram0, &skip.vram0, 0x8000); }
    if cgb && vram1_d > 0 { dump_region("VRM1", &real.vram1, &skip.vram1, 0x8000); }
    if cgb && (bgpal_d > 0 || objpal_d > 0) {
        for s in 0..32usize {
            if real.bgpal[s] != skip.bgpal[s] {
                println!("    BGPAL pal{} col{}: real=0x{:04X} skip=0x{:04X}", s/4, s%4, real.bgpal[s], skip.bgpal[s]);
            }
        }
        for s in 0..32usize {
            if real.objpal[s] != skip.objpal[s] {
                println!("    OBJPAL pal{} col{}: real=0x{:04X} skip=0x{:04X}", s/4, s%4, real.objpal[s], skip.objpal[s]);
            }
        }
    }

    let total = diffs.len() + oam_d + feax_d + hram_d + vram0_d
        + if cgb { vram1_d + bgpal_d + objpal_d } else { 0 };
    let _ = io_diffs;
    println!("TOTAL discrepancies: {total}");
    Ok(total)
}

pub fn run_case(case: TestCase, options: &RunOptions) -> CaseResult {
    match run_case_inner(&case, options) {
        Ok(()) => CaseResult {
            case,
            passed: true,
            detail: "ok".to_string(),
        },
        Err(detail) => CaseResult {
            case,
            passed: false,
            detail,
        },
    }
}

fn run_case_inner(case: &TestCase, options: &RunOptions) -> Result<(), String> {
    if options.frames == 0 {
        return Err("frame count must be greater than zero".to_string());
    }

    let rom_data = fs::read(&case.rom_path).map_err(|error| format!("failed to read ROM: {error}"))?;
    let cartridge = Cartridge::from_bytes(&rom_data)
        .map_err(|error| format!("failed to load ROM bytes: {error}"))?;

    let mut gb = GB::new(match case.mode {
        Mode::Dmg => Hardware::DMG,
        Mode::Cgb => Hardware::CGB,
        Mode::Agb => Hardware::AGB,
    });
    gb.insert(cartridge);
    // Initial state: real boot ROM (Gambatte-faithful) when --real-bios is set
    // and the bios file is present, else the synthetic skip_bios seed. The
    // synthetic path selects per-oracle residue (SRAM `.bin` dumper oracles were
    // captured WITH the boot ROM having run, so they read the boot-ROM-final
    // residue; `.dump` region oracles need the no-boot zeroed state).
    seed_initial_state(&mut gb, case, options);

    if matches!(case.mode, Mode::Cgb | Mode::Agb) {
        gb.set_cgb_color_conversion(CgbColorConversion::Gambatte);
    }

    let collect_audio = matches!(case.oracle, Oracle::Audio { .. });
    let captured_audio = if collect_audio {
        let samples = Arc::new(Mutex::new(Vec::new()));
        gb.enable_audio(Box::new(CapturedAudio {
            samples: Arc::clone(&samples),
        }))
        .map_err(|error| format!("failed to enable audio capture: {error}"))?;
        Some(samples)
    } else {
        None
    };

    let mut last_frame = None;
    let mut last_audio_samples = Vec::new();

    let trace_case = options
        .trace_rom
        .as_deref()
        .map(|needle| case.rom_path.to_string_lossy().contains(needle))
        .unwrap_or(false);
    let mut trace = trace_case.then(|| TimingTrace::new(options.trace_limit, options.trace_ly));

    // Dump oracles read memory back directly and do not need a final LCD frame.
    // The dumpers turn the LCD off while copying (so no frame completes) and then
    // spin in a tight idle loop, so the frame-driven path would time out. Drive a
    // generous fixed cycle budget, large enough for the biggest dumper (wram,
    // 32 KiB across banks) plus its post-copy LCD re-enable and LY wait, then read
    // the region back.
    let dump_oracle = matches!(
        case.oracle,
        Oracle::SramDump { .. } | Oracle::RegionDump { .. }
    );
    if dump_oracle {
        let cycle_budget = (options.frames.max(DUMP_MIN_FRAMES) as u64) * (CYCLES_PER_FRAME as u64);
        let mut cycles_run: u64 = 0;
        while cycles_run < cycle_budget {
            let (_breakpoint_hit, cycles) = gb.step_instruction(false);
            cycles_run += cycles as u64;
        }
        return evaluate_dump_oracle(&gb, &case.oracle);
    }

    for frame_index in 0..options.frames {
        let trace_this_frame = trace.is_some()
            && options
                .trace_frame
                .map(|trace_frame| trace_frame == frame_index)
                .unwrap_or(true);
        gb.set_fetch_debug_events_enabled(trace_this_frame);

        let (frame, _breakpoint_hit) = if trace_this_frame {
            let trace = trace.as_mut().expect("trace is enabled for this frame");
            run_until_frame_traced(&mut gb, collect_audio, frame_index, trace)?
        } else {
            gb.run_until_lcd_frame(collect_audio, MAX_CYCLES_UNTIL_LCD_FRAME)
                .map_err(|error| format!("{error} while running frame {frame_index}"))?
        };
        last_frame = Some(frame::normalize_frame(frame));

        if let Some(samples) = &captured_audio {
            let mut samples = samples
                .lock()
                .map_err(|_| "audio capture lock was poisoned".to_string())?;
            last_audio_samples = std::mem::take(&mut *samples);
        }
    }

    match &case.oracle {
        Oracle::Hex { expected, .. } => {
            let frame = last_frame.ok_or_else(|| "no frame was produced".to_string())?;
            if let Some(mismatch) = frame::hex_output_mismatch(&frame, expected) {
                let artifact_detail = dump_failure_frame(case, options, &frame, None)?;
                Err(format!(
                    "screen did not match hex output {expected}: {mismatch}{artifact_detail}"
                ))
            } else {
                Ok(())
            }
        }
        Oracle::Audio { audible, .. } => {
            if frame::audio_matches(&last_audio_samples, *audible) {
                Ok(())
            } else if *audible {
                Err("audio output was expected but final samples were identical".to_string())
            } else {
                Err("silence was expected but final samples changed".to_string())
            }
        }
        Oracle::Png { path } => {
            let actual = last_frame.ok_or_else(|| "no frame was produced".to_string())?;
            let expected = frame::read_png_rgb(path)?;

            if let Some(mismatch) = frame::frame_buffer_mismatch(&actual, &expected) {
                let scan_detail = scan_for_later_png_match(&mut gb, options, &expected)?;
                let artifact_detail = dump_failure_frame(case, options, &actual, Some(&expected))?;
                Err(format!(
                    "screen did not match PNG {}: {}{scan_detail}{artifact_detail}",
                    path.display(),
                    mismatch.describe()
                ))
            } else {
                Ok(())
            }
        }
        Oracle::SramDump { .. } | Oracle::RegionDump { .. } => {
            // Handled before the frame loop via the cycle-driven dump path.
            evaluate_dump_oracle(&gb, &case.oracle)
        }
    }
}

/// Read back the dumped memory region and compare it to the reference file.
fn evaluate_dump_oracle(gb: &GB, oracle: &Oracle) -> Result<(), String> {
    match oracle {
        Oracle::SramDump { path } => {
            let expected = fs::read(path)
                .map_err(|error| format!("failed to read SRAM dump {}: {error}", path.display()))?;
            let cartridge = gb
                .cartridge()
                .ok_or_else(|| "no cartridge present when reading SRAM dump".to_string())?;
            let actual = cartridge.save_ram();
            compare_dump(&format!("SRAM dump {}", path.display()), &expected, actual)
        }
        Oracle::RegionDump { path, region } => {
            let expected = fs::read(path).map_err(|error| {
                format!("failed to read region dump {}: {error}", path.display())
            })?;
            let base = region.base_address();
            let actual: Vec<u8> = (0..expected.len())
                .map(|offset| gb.read_memory(base.wrapping_add(offset as u16)))
                .collect();
            compare_dump(
                &format!(
                    "region dump {} ({:?} @ {:#06X})",
                    path.display(),
                    region,
                    base
                ),
                &expected,
                &actual,
            )
        }
        _ => Err("evaluate_dump_oracle called with non-dump oracle".to_string()),
    }
}

/// Compare a captured byte region against a reference dump. Reports the first
/// differing offset with expected/actual bytes, or a length mismatch.
fn compare_dump(label: &str, expected: &[u8], actual: &[u8]) -> Result<(), String> {
    if actual.len() < expected.len() {
        return Err(format!(
            "{label}: captured region too small ({} bytes available, {} expected)",
            actual.len(),
            expected.len()
        ));
    }

    for (offset, (&want, &got)) in expected.iter().zip(actual.iter()).enumerate() {
        if want != got {
            return Err(format!(
                "{label}: first mismatch at offset {offset:#06X}: expected {want:#04X}, got {got:#04X}",
            ));
        }
    }

    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct TraceSnapshot {
    pc: u16,
    abs_cc: u64,
    ime: bool,
    ime_delay: u8,
    ppu_state: rustyboi_core_lib::ppu::State,
    ppu_ticks: u128,
    ppu_x: u8,
    fetch_fifo: usize,
    fetch_tile: u8,
    sprite_stall: u8,
    ly: u8,
    stat: u8,
    lcdc: u8,
    interrupt_flag: u8,
    interrupt_enable: u8,
}

struct TimingTrace {
    limit: usize,
    ly_filter: Option<u8>,
    emitted: usize,
}

impl TimingTrace {
    fn new(limit: usize, ly_filter: Option<u8>) -> Self {
        Self {
            limit,
            ly_filter,
            emitted: 0,
        }
    }

    fn emit(&mut self, frame_index: usize, before: TraceSnapshot, after: TraceSnapshot, cycles: u32) {
        if self.emitted >= self.limit {
            return;
        }

        if let Some(ly) = self.ly_filter
            && before.ly != ly
            && after.ly != ly
        {
            return;
        }

        let interesting_pc = before.pc == 0x0048 || (0x1000..=0x1078).contains(&before.pc);
        let lcd_interrupt_enabled = ((before.interrupt_enable | after.interrupt_enable) & 0x02) != 0;
        let changed = before.lcdc != after.lcdc
            || before.interrupt_enable != after.interrupt_enable
            || before.ime != after.ime
            || before.ime_delay != after.ime_delay
            || (lcd_interrupt_enabled
                && (before.stat != after.stat || before.interrupt_flag != after.interrupt_flag));

        if !interesting_pc && !changed {
            return;
        }

        eprintln!(
            "TRACE frame={frame_index} cc={} pc={:#06X}->{:#06X} cyc={} ime={}->{} delay={}->{} ppu={:?}@{} x={} ly={} fifo={} tile={} stall={} -> {:?}@{} x={} ly={} fifo={} tile={} stall={} stat={:#04X}->{:#04X} lcdc={:#04X}->{:#04X} if={:#04X}->{:#04X} ie={:#04X}->{:#04X}",
            before.abs_cc,
            before.pc,
            after.pc,
            cycles,
            before.ime,
            after.ime,
            before.ime_delay,
            after.ime_delay,
            before.ppu_state,
            before.ppu_ticks,
            before.ppu_x,
            before.ly,
            before.fetch_fifo,
            before.fetch_tile,
            before.sprite_stall,
            after.ppu_state,
            after.ppu_ticks,
            after.ppu_x,
            after.ly,
            after.fetch_fifo,
            after.fetch_tile,
            after.sprite_stall,
            before.stat,
            after.stat,
            before.lcdc,
            after.lcdc,
            before.interrupt_flag,
            after.interrupt_flag,
            before.interrupt_enable,
            after.interrupt_enable,
        );
        self.emitted += 1;
    }

    fn emit_fetch_events(&mut self, frame_index: usize, pc: u16, events: &[FetchDebugEvent]) {
        if self.emitted >= self.limit {
            return;
        }

        for event in events {
            if self.emitted >= self.limit {
                return;
            }

            if let Some(ly) = self.ly_filter
                && event.ly != ly
            {
                continue;
            }

            let interesting_pc = pc == 0x0048 || (0x1000..=0x1078).contains(&pc);
            let interesting_x = event.x < 40 || (128..=160).contains(&event.x);
            if !interesting_pc && !interesting_x {
                continue;
            }

            let addr = event
                .addr
                .map(|addr| format!("{addr:#06X}"))
                .unwrap_or_else(|| "-".to_string());
            let value = event
                .value
                .map(|value| format!("{value:#04X}"))
                .unwrap_or_else(|| "-".to_string());

            eprintln!(
                "FETCH frame={frame_index} pc={pc:#06X} {:?} ppu@{} x={} ly={} fifo={} tile={} num={:#04X} attr={:#04X} line={} addr={} val={} lcdc={:#04X} tdidx={} window={}",
                event.kind,
                event.ppu_ticks,
                event.x,
                event.ly,
                event.fifo_size,
                event.tile_index,
                event.tile_num,
                event.tile_attributes,
                event.tile_line,
                addr,
                value,
                event.lcdc,
                event.tile_index_is_tile_data,
                event.fetching_window,
            );
            self.emitted += 1;
        }
    }

    fn emit_pixel_events(&mut self, frame_index: usize, pc: u16, events: &[PixelDebugEvent]) {
        if self.emitted >= self.limit || !(pc == 0x0048 || (0x1000..=0x1078).contains(&pc)) {
            return;
        }

        for event in events {
            if self.emitted >= self.limit {
                return;
            }

            if let Some(ly) = self.ly_filter
                && event.ly != ly
            {
                continue;
            }

            if event.x >= 32 && !(128..=152).contains(&event.x) {
                continue;
            }

            eprintln!(
                "PIXEL frame={frame_index} pc={pc:#06X} ppu@{} x={} ly={} bg={} rgb=#{:02X}{:02X}{:02X} lcdc={:#04X}",
                event.ppu_ticks,
                event.x,
                event.ly,
                event.bg_pixel_idx,
                event.rgb[0],
                event.rgb[1],
                event.rgb[2],
                event.lcdc,
            );
            self.emitted += 1;
        }
    }
}

fn run_until_frame_traced(
    gb: &mut GB,
    collect_audio: bool,
    frame_index: usize,
    trace: &mut TimingTrace,
) -> Result<(rustyboi_core_lib::gb::Frame, bool), String> {
    let mut cpu_cycles_this_frame = 0u32;

    loop {
        let before = trace_snapshot(gb);
        let (breakpoint_hit, cycles) = gb.step_instruction(collect_audio);
        let fetch_events = gb.take_fetch_debug_events();
        let pixel_events = gb.take_pixel_debug_events();
        let after = trace_snapshot(gb);
        trace.emit(frame_index, before, after, cycles);
        trace.emit_fetch_events(frame_index, before.pc, &fetch_events);
        trace.emit_pixel_events(frame_index, before.pc, &pixel_events);

        cpu_cycles_this_frame += cycles;
        if breakpoint_hit {
            return Ok((gb.get_current_frame(), true));
        }

        if gb.get_ppu_debug_info().0.frame_ready() {
            return Ok((gb.get_current_frame(), false));
        }

        if cpu_cycles_this_frame >= MAX_CYCLES_UNTIL_LCD_FRAME {
            return Err(format!("timed out waiting for LCD frame {frame_index}"));
        }
    }
}

fn trace_snapshot(gb: &GB) -> TraceSnapshot {
    let registers = gb.get_cpu_registers();
    let (ppu, _fetcher_buffer) = gb.get_ppu_debug_info();

    TraceSnapshot {
        pc: registers.pc,
        abs_cc: gb.master_cc(),
        ime: registers.ime,
        ime_delay: gb.get_ime_enable_delay(),
        ppu_state: *ppu.get_state(),
        ppu_ticks: ppu.get_ticks(),
        ppu_x: ppu.get_x(),
        fetch_fifo: ppu.get_fetcher_fifo_size(),
        fetch_tile: ppu.get_fetcher_tile_index(),
        sprite_stall: ppu.get_sprite_fetch_stall(),
        ly: gb.read_memory(LY),
        stat: gb.read_memory(LCD_STATUS),
        lcdc: gb.read_memory(LCD_CONTROL),
        interrupt_flag: gb.read_memory(INTERRUPT_FLAG),
        interrupt_enable: gb.read_memory(INTERRUPT_ENABLE),
    }
}

fn scan_for_later_png_match(
    gb: &mut GB,
    options: &RunOptions,
    expected: &[u32],
) -> Result<String, String> {
    if options.scan_frames == 0 {
        return Ok(String::new());
    }

    for extra_frame in 1..=options.scan_frames {
        let (frame, _breakpoint_hit) = gb
            .run_until_lcd_frame(false, MAX_CYCLES_UNTIL_LCD_FRAME)
            .map_err(|error| format!("{error} while scanning extra frame {extra_frame}"))?;
        let actual = frame::normalize_frame(frame);
        if frame::frame_buffer_mismatch(&actual, expected).is_none() {
            return Ok(format!(
                "; matched after +{extra_frame} frame(s), at --frames {}",
                options.frames + extra_frame
            ));
        }
    }

    Ok(format!("; no match in next {} frame(s)", options.scan_frames))
}

fn dump_failure_frame(
    case: &TestCase,
    options: &RunOptions,
    actual: &[u32],
    expected: Option<&[u32]>,
) -> Result<String, String> {
    let Some(dump_dir) = &options.dump_dir else {
        return Ok(String::new());
    };

    let stem = artifact_stem(case);
    let actual_path = dump_dir.join(format!("{stem}.actual.ppm"));
    frame::write_ppm(&actual_path, actual)?;

    let mut paths = vec![actual_path];
    if let Some(expected) = expected {
        let expected_path = dump_dir.join(format!("{stem}.expected.ppm"));
        frame::write_ppm(&expected_path, expected)?;
        paths.push(expected_path);
    }

    Ok(format!(
        "; artifacts: {}",
        paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn artifact_stem(case: &TestCase) -> String {
    let rom_stem = case
        .rom_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("rom");
    let oracle = case.oracle.label();
    sanitize_artifact_component(&format!(
        "{}_{}_{}",
        rom_stem,
        case.mode.label().to_ascii_lowercase(),
        oracle
    ))
}

fn sanitize_artifact_component(component: &str) -> String {
    component
        .chars()
        .map(|character| match character {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' => character,
            _ => '_',
        })
        .collect()
}

struct CapturedAudio {
    samples: SharedSamples,
}

impl AudioOutput for CapturedAudio {
    fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    fn add_samples(&mut self, samples: &[(f32, f32)]) {
        if let Ok(mut captured) = self.samples.lock() {
            captured.extend_from_slice(samples);
        }
    }
}
