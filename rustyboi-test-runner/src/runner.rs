use crate::expectation::{
    BTN_A, BTN_B, BTN_DOWN, BTN_LEFT, BTN_RIGHT, BTN_SELECT, BTN_START, BTN_UP, InputEvent, Mode,
    Oracle, TestCase,
};
use crate::frame;
use rustyboi_core_lib::audio::AudioOutput;
use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::input::ButtonState;
use rustyboi_core_lib::cpu::registers::{INTERRUPT_ENABLE, INTERRUPT_FLAG};
use rustyboi_core_lib::gb::{GB, Hardware};
use rustyboi_core_lib::ppu::{
    ColorCorrection, FetchDebugEvent, LCD_CONTROL, LCD_STATUS, LY, PixelDebugEvent,
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
pub(crate) struct CaseResult {
    pub case: TestCase,
    pub passed: bool,
    pub detail: String,
}

/// Scripted joypad input for one case (from a manifest `input=` token).
/// Deterministic and purely per-case (parallel-executor safe): events fire off
/// the case's own cycle count. An event is due once `total_cycles` reaches its
/// frame window (`frame * 70224`); with an `@ly` condition it additionally
/// waits for LY to equal the target so a press lands mid-frame at a chosen
/// scanline (LY equality is observed at instruction granularity, well within a
/// 456-cycle scanline). If LY never matches — e.g. the LCD is off — the event
/// force-fires two frames past due so a script cannot hang a case.
struct InputScript {
    events: Vec<InputEvent>,
    next: usize,
}

impl InputScript {
    fn new(events: &[InputEvent]) -> Self {
        Self {
            events: events.to_vec(),
            next: 0,
        }
    }

    /// Apply every due event. Called once per stepped instruction (cheap: a
    /// single bounds check when no event is pending).
    fn poll(&mut self, gb: &mut GB, total_cycles: u64) {
        while let Some(event) = self.events.get(self.next) {
            let due = event.frame as u64 * CYCLES_PER_FRAME as u64;
            if total_cycles < due {
                break;
            }
            if let Some(ly) = event.ly
                && gb.read_memory(LY) != ly
                && total_cycles < due + 2 * CYCLES_PER_FRAME as u64
            {
                break;
            }
            gb.set_input_state(buttons_to_state(event.buttons));
            self.next += 1;
        }
    }

    /// Frame-boundary variant for loops that run whole LCD frames at a time
    /// (`png_shootout`): applies every event whose frame has been reached.
    /// `@ly` conditions are ignored at this granularity.
    fn poll_frame(&mut self, gb: &mut GB, frame_index: usize) {
        while let Some(event) = self.events.get(self.next) {
            if (event.frame as usize) > frame_index {
                break;
            }
            gb.set_input_state(buttons_to_state(event.buttons));
            self.next += 1;
        }
    }
}

fn buttons_to_state(buttons: u8) -> ButtonState {
    ButtonState {
        a: buttons & BTN_A != 0,
        b: buttons & BTN_B != 0,
        start: buttons & BTN_START != 0,
        select: buttons & BTN_SELECT != 0,
        up: buttons & BTN_UP != 0,
        down: buttons & BTN_DOWN != 0,
        left: buttons & BTN_LEFT != 0,
        right: buttons & BTN_RIGHT != 0,
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RunOptions {
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
    /// Diagnostic: after a mooneye-graded case, dump this many 8-byte rows of
    /// the SameSuite results buffer (forces --jobs 1).
    pub ss_dump: Option<u16>,
    /// Base address for `--ss-dump` (default 0xC000; some tests store results
    /// in VRAM).
    pub ss_dump_base: Option<u16>,
}

/// Boot-ROM filename for a hardware model, or None when rustyboi has no distinct
/// boot ROM provisioned for it. DMG/CGB/AGB/SGB plus the revision variants
/// DMG0/MGB/SGB2/CGB0/CGBE have dumps in `bios/`. CGBB (CPU-CGB-A/B) shares the
/// standard CGB boot ROM — no distinct dump exists — so it maps to cgb_boot.bin.
pub fn bios_filename(hw: Hardware) -> Option<&'static str> {
    match hw {
        Hardware::DMG => Some("dmg_boot.bin"),
        Hardware::CGB => Some("cgb_boot.bin"),
        // AGB uses the GBA's CGB-compat boot ROM (SameBoy naming: agb_boot.bin).
        Hardware::AGB => Some("agb_boot.bin"),
        Hardware::SGB => Some("sgb_boot.bin"),
        Hardware::DMG0 => Some("dmg0_boot.bin"),
        Hardware::MGB => Some("mgb_boot.bin"),
        Hardware::SGB2 => Some("sgb2_boot.bin"),
        Hardware::CGB0 => Some("cgb0_boot.bin"),
        Hardware::CGBE => Some("cgbE_boot.bin"),
        // CGB-A/B CPU revision shares the standard CGB boot ROM (no distinct dump).
        Hardware::CGBB => Some("cgb_boot.bin"),
    }
}

/// Locate the boot ROM file for a specific silicon revision. Tries `bios_dir`
/// first (if given), then `bios/` relative to CWD and to the crate manifest
/// dir, then the worktree default. Returns the first existing path.
///
/// Keyed on `Hardware`, not `Mode`: a `rev=`-pinned case runs on the pinned
/// silicon, so it must boot that revision's image (cgb0_boot.bin for rev=cgb0,
/// cgbE_boot.bin for rev=cgbe, ...). Keying this on the mode booted the mode
/// default and silently graded pinned rows against the wrong boot ROM.
fn resolve_bios_path(hw: Hardware, bios_dir: Option<&PathBuf>) -> Option<PathBuf> {
    let file = bios_filename(hw)?;
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
        && let Some(path) = resolve_bios_path(case.hardware(), options.bios_dir.as_ref())
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
pub(crate) fn validate_bios(
    rom_path: &PathBuf,
    mode: Mode,
    bios_dir: Option<&PathBuf>,
) -> Result<usize, String> {
    let cgb = matches!(mode, Mode::Cgb | Mode::Agb);
    let hw = mode.default_hardware();

    let bios_path = resolve_bios_path(hw, bios_dir)
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

    for i in 0..0x80usize {
        if real.io[i] != skip.io[i] {
            let addr = 0xFF00 + i as u16;
            diffs.push(format!(
                "  IO 0x{:04X} {:<6} real=0x{:02X} skip=0x{:02X}",
                addr, io_name(addr), real.io[i], skip.io[i]
            ));
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
    println!("TOTAL discrepancies: {total}");
    Ok(total)
}

pub(crate) fn run_case(case: TestCase, options: &RunOptions) -> CaseResult {
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

    // A `rev=` manifest token pins a specific boot sub-revision (the mooneye
    // boot_regs/boot_div/boot_hwio tests each target one silicon revision);
    // otherwise the case runs on the default model for its mode.
    let hardware = case.hardware();
    let mut gb = GB::new(hardware);
    gb.insert(cartridge);
    if case.cart_lazy_sram_cs {
        // Fixture pin: the capture cart's SRAM CS is lazy-decoded (see the
        // `cart=lazy_sram_cs` manifest token / Cartridge::dma_sram_bus_read).
        gb.set_cart_sram_cs_lazy(true);
    }
    // Initial state: real boot ROM (Gambatte-faithful) when --real-bios is set
    // and the bios file is present, else the synthetic skip_bios seed. The
    // synthetic path selects per-oracle residue (SRAM `.bin` dumper oracles were
    // captured WITH the boot ROM having run, so they read the boot-ROM-final
    // residue; `.dump` region oracles need the no-boot zeroed state).
    seed_initial_state(&mut gb, case, options);

    if matches!(case.mode, Mode::Cgb | Mode::Agb) {
        // c-sp PNG references use the `(X<<3)|(X>>2)` shift formula; Linear is
        // bucket-identical to it under the 0xF8 comparison mask (RESULTS.md).
        // The gambatte suite keeps the LCD conversion its references were rendered with.
        let conversion = if matches!(
            case.oracle,
            Oracle::CspPng { .. } | Oracle::CspPngFixed { .. } | Oracle::CspPngLayout { .. }
        ) {
            ColorCorrection::Linear
        } else {
            ColorCorrection::Lcd
        };
        gb.set_cgb_color_conversion(conversion);
    }

    // c-sp public-suite oracles drive the CPU instruction-by-instruction rather
    // than the Gambatte frame loop (they need the `LD B,B` done-marker and the
    // serial / cart-RAM / FF82 / register protocols). Handle them up front.
    match &case.oracle {
        Oracle::Serial => return evaluate_serial(&mut gb, options.frames),
        Oracle::SerialText { pass, fail } => {
            let frames = case.frames.unwrap_or(options.frames);
            return evaluate_serial_text(&mut gb, frames, pass, fail.as_deref());
        }
        Oracle::BlarggMem => return evaluate_blargg_mem(&mut gb, options.frames),
        Oracle::MemValue { addr, expected } => {
            let frames = case.frames.unwrap_or(options.frames);
            return evaluate_mem_value(&mut gb, frames, *addr, *expected);
        }
        Oracle::MooneyeFib => return evaluate_mooneye(&mut gb, 0x40, options),
        Oracle::MooneyeFibEd => return evaluate_mooneye(&mut gb, 0xED, options),
        Oracle::CspPng { path } => {
            let trace_case = options
                .trace_rom
                .as_deref()
                .map(|needle| case.rom_path.to_string_lossy().contains(needle))
                .unwrap_or(false);
            if trace_case {
                let mut trace = TimingTrace::new(options.trace_limit, options.trace_ly);
                return evaluate_csp_png_traced(&mut gb, options, case, path, &mut trace);
            }
            return evaluate_csp_png(&mut gb, options, case, path, false, false);
        }
        Oracle::CspPngFixed { path } => {
            return evaluate_csp_png(&mut gb, options, case, path, true, false);
        }
        Oracle::CspPngLayout { path } => {
            return evaluate_csp_png(&mut gb, options, case, path, false, true);
        }
        Oracle::PngShootout { refs, frames } => {
            return evaluate_png_shootout(&mut gb, options, case, refs, *frames);
        }
        _ => {}
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
        // Manifest-driven `sram` cases may carry a per-case `frames=` budget
        // (still clamped up to the dump minimum); filename-discovered gambatte
        // dump cases never do (case.frames is None) and keep the run-wide value.
        let frames = case.frames.unwrap_or(options.frames);
        let cycle_budget = (frames.max(DUMP_MIN_FRAMES) as u64) * (CYCLES_PER_FRAME as u64);
        let mut cycles_run: u64 = 0;
        // Scripted input (manifest `input=`): SRAM-graded hardware tests can
        // require held buttons (gbc-hw-tests joy_interrupt_manual_delay's
        // results.txt: "Keep any button pressed when initing the ROM").
        let mut input = InputScript::new(&case.input);
        while cycles_run < cycle_budget {
            input.poll(&mut gb, cycles_run);
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
        last_frame = Some(frame::normalize_frame(&gb, &frame));

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
        // c-sp suite oracles are dispatched (and returned) before the frame loop.
        Oracle::CspPng { .. }
        | Oracle::CspPngFixed { .. }
        | Oracle::CspPngLayout { .. }
        | Oracle::PngShootout { .. }
        | Oracle::Serial
        | Oracle::SerialText { .. }
        | Oracle::BlarggMem
        | Oracle::MemValue { .. }
        | Oracle::MooneyeFib
        | Oracle::MooneyeFibEd => {
            unreachable!("c-sp oracle handled before the Gambatte frame loop")
        }
    }
}

/// Read back the dumped memory region and compare it to the reference file.
fn evaluate_dump_oracle(gb: &GB, oracle: &Oracle) -> Result<(), String> {
    match oracle {
        Oracle::SramDump { path, skip } => {
            let expected = fs::read(path)
                .map_err(|error| format!("failed to read SRAM dump {}: {error}", path.display()))?;
            let cartridge = gb
                .cartridge()
                .ok_or_else(|| "no cartridge present when reading SRAM dump".to_string())?;
            let actual = cartridge.save_ram();
            compare_dump(&format!("SRAM dump {}", path.display()), &expected, actual, skip)
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
                &[],
            )
        }
        _ => Err("evaluate_dump_oracle called with non-dump oracle".to_string()),
    }
}

/// Compare a captured byte region against a reference dump. Reports the first
/// differing offset with expected/actual bytes, or a length mismatch. Offsets
/// within any `skip` range are not graded (nondeterministic power-on regions;
/// see `Oracle::SramDump`).
fn compare_dump(
    label: &str,
    expected: &[u8],
    actual: &[u8],
    skip: &[std::ops::Range<usize>],
) -> Result<(), String> {
    if actual.len() < expected.len() {
        return Err(format!(
            "{label}: captured region too small ({} bytes available, {} expected)",
            actual.len(),
            expected.len()
        ));
    }

    // Diagnostic (RB_SRAM_VERBOSE=1): report EVERY mismatching cell instead of
    // bailing at the first. The gbc-hw-tests captures are per-probe timing
    // tables; the full cell list is what a timing derivation needs (same
    // spirit as --ss-dump).
    if std::env::var_os("RB_SRAM_VERBOSE").is_some() {
        let mut diffs = Vec::new();
        for (offset, (&want, &got)) in expected.iter().zip(actual.iter()).enumerate() {
            if skip.iter().any(|range| range.contains(&offset)) {
                continue;
            }
            if want != got {
                diffs.push(format!("{offset:#06X}:want={want:#04X},got={got:#04X}"));
            }
        }
        if !diffs.is_empty() {
            return Err(format!("{label}: {} diffs: {}", diffs.len(), diffs.join(" ")));
        }
        return Ok(());
    }
    for (offset, (&want, &got)) in expected.iter().zip(actual.iter()).enumerate() {
        if skip.iter().any(|range| range.contains(&offset)) {
            continue;
        }
        if want != got {
            return Err(format!(
                "{label}: first mismatch at offset {offset:#06X}: expected {want:#04X}, got {got:#04X}",
            ));
        }
    }

    Ok(())
}

/// Run instruction-by-instruction until the `marker` done-marker opcode is the
/// next opcode, capped at `max_cycles`. Returns true if the marker was reached.
/// `marker` is `0x40` (`LD B,B`) for the modern Mooneye/age convention or `0xED`
/// (illegal opcode) for the 2016 Mooneye/wilbertpol convention.
fn run_until_ldbb(gb: &mut GB, max_cycles: u64, marker: u8) -> bool {
    let mut cycles = 0u64;
    while cycles < max_cycles {
        let pc = gb.get_cpu_registers().pc;
        if gb.read_memory(pc) == marker {
            return true;
        }
        let (_breakpoint, c) = gb.step_instruction(false);
        cycles += c as u64;
    }
    false
}

/// mooneye: run to the done-marker opcode and check the Fibonacci magic
/// registers. `marker` is `0x40` (`LD B,B`) or `0xED` (2016-era illegal-opcode).
fn evaluate_mooneye(gb: &mut GB, marker: u8, options: &RunOptions) -> Result<(), String> {
    // mooneye tests complete quickly; 250M cycles is ~60s of GB time, ample.
    if !run_until_ldbb(gb, 250_000_000, marker) {
        return Err("no done-marker (timeout)".to_string());
    }
    // Diagnostic: SameSuite tests store per-subtest results at $C000 before
    // comparing against their embedded CorrectResults table. --ss-dump N
    // dumps N rows of 8 bytes so a failure pinpoints WHICH subtest diverged.
    if let Some(rows) = options.ss_dump {
        let dump_base = options.ss_dump_base.unwrap_or(0xC000);
        for row in 0..rows {
            let base = dump_base + row * 8;
            let bytes: Vec<String> = (0..8)
                .map(|i| format!("{:02X}", gb.read_memory(base + i)))
                .collect();
            eprintln!("SS_DUMP {base:04X}: {}", bytes.join(" "));
        }
    }
    let r = gb.get_cpu_registers();
    if r.b == 3 && r.c == 5 && r.d == 8 && r.e == 13 && r.h == 21 && r.l == 34 {
        Ok(())
    } else {
        Err(format!(
            "regs B={:02X} C={:02X} D={:02X} E={:02X} H={:02X} L={:02X} (want 03 05 08 0D 15 22)",
            r.b, r.c, r.d, r.e, r.h, r.l
        ))
    }
}

/// gbmicrotest / generic memory check: run a flat cycle budget, then compare a
/// memory byte. gbmicrotest's protocol is FF82==0x01 (FF80=actual, FF81=expected).
fn evaluate_mem_value(gb: &mut GB, frames: usize, addr: u16, expected: u8) -> Result<(), String> {
    let budget = frames as u64 * CYCLES_PER_FRAME as u64;
    let mut cycles = 0u64;
    while cycles < budget {
        let (_breakpoint, c) = gb.step_instruction(false);
        cycles += c as u64;
    }
    // The APU advances lazily; sync it so a check targeting an APU register
    // (e.g. audio_testbench's NR52) observes live state like hardware would.
    gb.sync_lazy_peripherals();
    let got = gb.read_memory(addr);
    if got == expected {
        Ok(())
    } else if addr == 0xFF82 {
        let actual = gb.read_memory(0xFF80);
        let want = gb.read_memory(0xFF81);
        Err(format!(
            "FF82={got:02X} (want {expected:02X}); FF80(actual)={actual:02X} FF81(expected)={want:02X}"
        ))
    } else {
        Err(format!("[{addr:04X}]={got:02X} want {expected:02X}"))
    }
}

/// blargg serial grading. blargg ROMs write each result byte to SB (FF01) then
/// start a transfer via SC (FF02) bit7+bit0. Capture SB on each rising edge of
/// the start bit, reconstruct the text, and scan for "Passed"/"Failed"/"Error".
fn evaluate_serial(gb: &mut GB, frames: usize) -> Result<(), String> {
    evaluate_serial_markers(gb, frames, "Passed", &["Failed", "Error"], true)
}

/// Parameterized serial-text grading (`serial_text`). The pass string (and
/// optional early-fail marker) come from the manifest. Unlike blargg's
/// SC-handshake protocol, the ROMs this serves (sketchtests) print by writing
/// each character RAW to SB (FF01) and never touch SC (verified by
/// disassembly: three `ldh [FF01],a` sites, zero FF02 writes), so the capture
/// samples FF01 value CHANGES per instruction step instead of SC start edges.
/// Limitation: back-to-back identical characters collapse into one, which is
/// harmless for the markers in use ("Test OK!" / model names have no
/// consecutive repeats; the "Expected..." fail prefix survives regardless).
fn evaluate_serial_text(
    gb: &mut GB,
    frames: usize,
    pass: &str,
    fail: Option<&str>,
) -> Result<(), String> {
    let fails: Vec<&str> = fail.into_iter().collect();
    evaluate_serial_markers(gb, frames, pass, &fails, false)
}

/// Shared serial-console capture: reconstruct the serial byte stream (SC
/// start-edge handshake when `sc_edge`, raw FF01 value changes otherwise) and
/// grade it against a pass marker and zero-or-more early-fail markers.
fn evaluate_serial_markers(
    gb: &mut GB,
    frames: usize,
    pass: &str,
    fails: &[&str],
    sc_edge: bool,
) -> Result<(), String> {
    let budget = frames as u64 * CYCLES_PER_FRAME as u64;
    // Don't scan before the stream could possibly hold the shortest marker.
    let min_len = fails
        .iter()
        .map(|f| f.len())
        .chain(std::iter::once(pass.len()))
        .min()
        .unwrap_or(1);
    let mut cycles = 0u64;
    let mut prev_start = false;
    let mut prev_sb = gb.read_memory(0xFF01);
    let mut out: Vec<u8> = Vec::new();
    while cycles < budget {
        let byte = if sc_edge {
            let sc = gb.read_memory(0xFF02);
            let start = (sc & 0x80) != 0 && (sc & 0x01) != 0;
            let edge = start && !prev_start;
            prev_start = start;
            edge.then(|| gb.read_memory(0xFF01))
        } else {
            let sb = gb.read_memory(0xFF01);
            let changed = sb != prev_sb;
            prev_sb = sb;
            changed.then_some(sb)
        };
        if let Some(byte) = byte {
            out.push(byte);
            if out.len() >= min_len {
                let s = String::from_utf8_lossy(&out);
                if s.contains(pass) {
                    return Ok(());
                }
                if fails.iter().any(|f| s.contains(f)) {
                    return Err(format!("serial verdict: {}", verdict_tail(&s)));
                }
            }
        }
        let (_breakpoint, c) = gb.step_instruction(false);
        cycles += c as u64;
    }
    let s = String::from_utf8_lossy(&out);
    if s.contains(pass) {
        Ok(())
    } else if s.is_empty() {
        Err("no serial output (timeout)".to_string())
    } else {
        Err(format!("no {pass:?}; tail: {}", verdict_tail(&s)))
    }
}

fn verdict_tail(s: &str) -> String {
    let tail: String = s.chars().rev().take(60).collect::<String>().chars().rev().collect();
    tail.replace('\n', " ").trim().to_string()
}

/// blargg cart-RAM memory protocol. The result code is written to 0xA000 (0x80
/// while running, final code on completion: 0x00 == pass) once the signature
/// 0xDE 0xB0 0x61 appears at 0xA001-3, with ASCII detail at 0xA004... We read
/// from the cart RAM backing store so the result survives blargg disabling RAM.
fn evaluate_blargg_mem(gb: &mut GB, frames: usize) -> Result<(), String> {
    let budget = frames as u64 * CYCLES_PER_FRAME as u64;
    let mut cycles = 0u64;
    let read_ram = |gb: &GB, off: usize| -> u8 {
        gb.cartridge()
            .map(|c| c.save_ram().get(off).copied().unwrap_or(0xFF))
            .unwrap_or(0xFF)
    };
    // Only trust a completion after observing the 0x80 running marker, so the
    // uninitialized 0xFF window is not mistaken for a verdict.
    let mut saw_running = false;
    loop {
        let sig = [read_ram(gb, 1), read_ram(gb, 2), read_ram(gb, 3)];
        let status = read_ram(gb, 0);
        if sig == [0xDE, 0xB0, 0x61] && status == 0x80 {
            saw_running = true;
        }
        if saw_running && sig == [0xDE, 0xB0, 0x61] && status != 0x80 {
            let mut txt = Vec::new();
            for off in 4usize..0x200 {
                let b = read_ram(gb, off);
                if b == 0 {
                    break;
                }
                txt.push(b);
            }
            let s = String::from_utf8_lossy(&txt);
            let oneline = s.replace('\n', " ");
            if status == 0x00 {
                return Ok(());
            }
            return Err(format!("code={status:02X}: {}", oneline.trim()));
        }
        if cycles >= budget {
            return Err("no result signature (timeout)".to_string());
        }
        let (_breakpoint, c) = gb.step_instruction(false);
        cycles += c as u64;
    }
}

/// GBEmulatorShootout-exact screenshot grading. Mirrors the shootout's
/// `Emulator.runTest`: run the normal Gambatte LCD frame loop and, after EACH
/// completed frame, screenshot-grade it against every reference PNG; PASS the
/// instant any frame matches any ref (the shootout early-exits on a match and
/// its `pass_result` list is an OR-match). The frame budget (`case_frames` when
/// nonzero, else `--frames`) is the shootout's own poll deadline
/// (`runtime + startup + 5 s`), so we never grade past the window the shootout
/// would have allowed. Polling every frame (rather than only the final one)
/// avoids both under-running slow-rendering tests and over-running tests whose
/// screen changes after the match — exactly the shootout's behaviour.
fn evaluate_png_shootout(
    gb: &mut GB,
    options: &RunOptions,
    case: &TestCase,
    refs: &[std::path::PathBuf],
    case_frames: usize,
) -> Result<(), String> {
    let frames = if case_frames > 0 {
        case_frames
    } else {
        options.frames
    };

    let mut expected_refs: Vec<Vec<u32>> = Vec::with_capacity(refs.len());
    for r in refs {
        expected_refs.push(
            frame::read_png_rgb(r).map_err(|e| format!("reference PNG {}: {e}", r.display()))?,
        );
    }

    let mut input = InputScript::new(&case.input);
    let mut worst: Option<frame::FrameMismatch> = None;
    // Budget in frame units. Several SameSuite APU ROMs keep the LCD OFF for
    // multi-million-cycle measurement stretches before drawing their result
    // screen; a timed-out frame wait burned MAX_CYCLES_UNTIL_LCD_FRAME
    // (64 frames) of the budget without producing a frame — keep running
    // instead of failing (the shootout's runtime-seconds budget does).
    let total_budget = frames.max(1) as i64;
    let mut budget = total_budget;
    while budget > 0 {
        // Scripted input applies at frame-boundary granularity here (the loop
        // runs whole LCD frames); the frame index is budget-derived.
        input.poll_frame(gb, (total_budget - budget) as usize);
        match gb.run_until_lcd_frame(false, MAX_CYCLES_UNTIL_LCD_FRAME) {
            Ok((frame, _bp)) => {
                budget -= 1;
                let actual = frame::normalize_frame(gb, &frame);
                // OR-match across pass refs; the shootout passes on the first
                // match.
                for expected in &expected_refs {
                    match frame::shootout_mismatch(&actual, expected) {
                        None => return Ok(()),
                        Some(m) => worst = Some(m),
                    }
                }
            }
            Err(_) => budget -= (MAX_CYCLES_UNTIL_LCD_FRAME / CYCLES_PER_FRAME) as i64,
        }
    }
    let Some(m) = worst else {
        return Err("no LCD frame within the case budget".to_string());
    };
    if let Some(dir) = &options.dump_dir {
        let stem = refs[0].file_stem().and_then(|s| s.to_str()).unwrap_or("case");
        let last = gb.get_current_frame();
        let actual = frame::normalize_frame(gb, &last);
        let _ = frame::write_ppm(&dir.join(format!("{stem}.actual.ppm")), &actual);
        let _ = frame::write_ppm(&dir.join(format!("{stem}.expected.ppm")), &expected_refs[0]);
    }
    Err(format!(
        "screen did not match shootout PNG {}: {}",
        refs[0].display(),
        m.describe()
    ))
}

/// c-sp framebuffer-PNG grading. Runs LCD frames, early-stopping once the
/// `LD B,B` (0x40) done-marker is the next opcode (mealybug/acid2 convention),
/// then compares the final frame to the reference. When `fixed` is set (the
/// `png_fixed` grading), instead runs a flat frame-cycle budget and grades
/// the final held framebuffer — for ROMs that turn the LCD off after rendering
/// their result screen and never complete another frame. The budget is the
/// case's own `frames=` override when present, else `--frames`; scripted
/// `input=` events (if any) fire off the case cycle clock as the run steps.
fn evaluate_csp_png(
    gb: &mut GB,
    options: &RunOptions,
    case: &TestCase,
    refpng: &std::path::Path,
    fixed: bool,
    recolor: bool,
) -> Result<(), String> {
    let frames = case.frames.unwrap_or(options.frames);
    let mut input = InputScript::new(&case.input);
    let actual = if fixed {
        let budget = frames as u64 * CYCLES_PER_FRAME as u64;
        let mut cycles = 0u64;
        while cycles < budget {
            input.poll(gb, cycles);
            let (_breakpoint, c) = gb.step_instruction(false);
            cycles += c as u64;
        }
        {
            let f = gb.get_current_frame();
            frame::normalize_frame(gb, &f)
        }
    } else {
        run_csp_frames_until_ldbb(gb, frames, &mut input)?
    };

    let expected = frame::read_png_rgb(refpng)
        .map_err(|e| format!("reference PNG {}: {e}", refpng.display()))?;

    let mismatch = if recolor {
        frame::frame_buffer_mismatch_recolor(&actual, &expected)
    } else {
        frame::frame_buffer_mismatch(&actual, &expected)
    };
    if let Some(mismatch) = mismatch {
        if let Some(dir) = &options.dump_dir {
            let stem = refpng.file_stem().and_then(|s| s.to_str()).unwrap_or("case");
            let _ = frame::write_ppm(&dir.join(format!("{stem}.actual.ppm")), &actual);
            let _ = frame::write_ppm(&dir.join(format!("{stem}.expected.ppm")), &expected);
        }
        let kind = if recolor { "layout (recolor-invariant)" } else { "PNG" };
        Err(format!(
            "screen did not match {kind} {}: {}",
            refpng.display(),
            mismatch.describe()
        ))
    } else {
        Ok(())
    }
}

/// Trace-enabled variant of the csp PNG path: runs the same ldbb frame loop but
/// emits TRACE/FETCH/PIXEL events for the selected frame (diagnostic only).
fn evaluate_csp_png_traced(
    gb: &mut GB,
    options: &RunOptions,
    case: &TestCase,
    refpng: &std::path::Path,
    trace: &mut TimingTrace,
) -> Result<(), String> {
    let frames = case.frames.unwrap_or(options.frames);
    let mut input = InputScript::new(&case.input);
    let mut total_cycles = 0u64;
    let mut last: Option<Vec<u32>> = None;
    let mut done = false;
    for frame_index in 0..frames {
        let trace_this_frame = options
            .trace_frame
            .map(|trace_frame| trace_frame == frame_index)
            .unwrap_or(true);
        gb.set_fetch_debug_events_enabled(trace_this_frame);
        let mut cycles = 0u32;
        loop {
            input.poll(gb, total_cycles);
            let pc = gb.get_cpu_registers().pc;
            if gb.read_memory(pc) == 0x40 {
                done = true;
                break;
            }
            if trace_this_frame {
                let before = trace_snapshot(gb);
                let (_breakpoint, c) = gb.step_instruction(false);
                let fetch_events = gb.take_fetch_debug_events();
                let pixel_events = gb.take_pixel_debug_events();
                let after = trace_snapshot(gb);
                trace.emit(frame_index, before, after, c);
                trace.emit_fetch_events(frame_index, before.pc, &fetch_events);
                trace.emit_pixel_events(frame_index, before.pc, &pixel_events);
                cycles += c;
                total_cycles += c as u64;
            } else {
                let (_breakpoint, c) = gb.step_instruction(false);
                cycles += c;
                total_cycles += c as u64;
            }
            if gb.get_ppu_debug_info().0.frame_ready() {
                break;
            }
            if cycles >= MAX_CYCLES_UNTIL_LCD_FRAME {
                return Err(format!("frame {frame_index} timeout"));
            }
        }
        let f = gb.get_current_frame();
        last = Some(frame::normalize_frame(gb, &f));
        if done {
            break;
        }
    }
    let actual = last.ok_or_else(|| "no frame produced".to_string())?;
    let expected = frame::read_png_rgb(refpng)
        .map_err(|e| format!("reference PNG {}: {e}", refpng.display()))?;
    if let Some(mismatch) = frame::frame_buffer_mismatch(&actual, &expected) {
        Err(format!(
            "screen did not match PNG {}: {}",
            refpng.display(),
            mismatch.describe()
        ))
    } else {
        Ok(())
    }
}

/// Run up to `frames` LCD frames, stepping instruction-by-instruction within a
/// frame so the `LD B,B` (0x40) done-marker can stop the run early. Returns the
/// last completed (or marker-time) frame. Scripted input events fire off the
/// cumulative case cycle count as the frames run.
fn run_csp_frames_until_ldbb(
    gb: &mut GB,
    frames: usize,
    input: &mut InputScript,
) -> Result<Vec<u32>, String> {
    let mut last: Option<Vec<u32>> = None;
    let mut done = false;
    let mut total_cycles = 0u64;
    for frame_index in 0..frames {
        let mut cycles = 0u32;
        loop {
            input.poll(gb, total_cycles);
            let pc = gb.get_cpu_registers().pc;
            if gb.read_memory(pc) == 0x40 {
                done = true;
                break;
            }
            let (_breakpoint, c) = gb.step_instruction(false);
            cycles += c;
            total_cycles += c as u64;
            if gb.get_ppu_debug_info().0.frame_ready() {
                break;
            }
            if cycles >= MAX_CYCLES_UNTIL_LCD_FRAME {
                return Err(format!("frame {frame_index} timeout"));
            }
        }
        let f = gb.get_current_frame();
        last = Some(frame::normalize_frame(gb, &f));
        if done {
            break;
        }
    }
    last.ok_or_else(|| "no frame produced".to_string())
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
        let actual = frame::normalize_frame(gb, &frame);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_to_state_empty_and_full() {
        assert_eq!(buttons_to_state(0x00), ButtonState::default());
        let all = buttons_to_state(0xFF);
        assert!(all.a && all.b && all.start && all.select && all.up && all.down && all.left && all.right);
    }

    #[test]
    fn buttons_to_state_maps_each_bit_without_swaps() {
        // Each BTN_* bit lights exactly its own field (a bit-swap regression
        // would light the wrong direction/button).
        assert_eq!(buttons_to_state(BTN_A), ButtonState { a: true, ..Default::default() });
        assert_eq!(buttons_to_state(BTN_B), ButtonState { b: true, ..Default::default() });
        assert_eq!(buttons_to_state(BTN_START), ButtonState { start: true, ..Default::default() });
        assert_eq!(buttons_to_state(BTN_SELECT), ButtonState { select: true, ..Default::default() });
        assert_eq!(buttons_to_state(BTN_UP), ButtonState { up: true, ..Default::default() });
        assert_eq!(buttons_to_state(BTN_DOWN), ButtonState { down: true, ..Default::default() });
        assert_eq!(buttons_to_state(BTN_LEFT), ButtonState { left: true, ..Default::default() });
        assert_eq!(buttons_to_state(BTN_RIGHT), ButtonState { right: true, ..Default::default() });
    }

    #[test]
    fn compare_dump_accepts_exact_and_longer_actual() {
        assert!(compare_dump("t", &[1, 2, 3], &[1, 2, 3], &[]).is_ok());
        // Extra trailing actual bytes past the expected length are ignored.
        assert!(compare_dump("t", &[1, 2, 3], &[1, 2, 3, 9, 9], &[]).is_ok());
    }

    #[test]
    fn compare_dump_rejects_too_small_actual() {
        let err = compare_dump("region", &[1, 2, 3, 4], &[1, 2], &[]).unwrap_err();
        assert!(err.contains("too small"), "{err}");
    }

    #[test]
    fn compare_dump_reports_first_mismatch_offset() {
        let err = compare_dump("region", &[0, 1, 2, 3], &[0, 9, 2, 3], &[]).unwrap_err();
        assert!(err.contains("offset 0x0001"), "{err}");
        assert!(err.contains("expected 0x01") && err.contains("got 0x09"), "{err}");
    }

    #[test]
    fn compare_dump_skip_ranges_exclude_offsets() {
        let skip = std::slice::from_ref(&(1usize..2));
        // The only mismatch is at offset 1, which the skip range covers.
        assert!(compare_dump("region", &[0, 1, 2, 3], &[0, 9, 2, 3], skip).is_ok());
        // A mismatch outside the skip range still fails.
        assert!(compare_dump("region", &[0, 1, 2, 3], &[0, 9, 8, 3], skip).is_err());
    }

    // --- pure string helpers --------------------------------------------

    #[test]
    fn verdict_tail_trims_newlines_and_caps_length() {
        assert_eq!(verdict_tail("Passed\n"), "Passed");
        assert_eq!(verdict_tail("a\nb\n"), "a b");
        // Only the last 60 characters survive.
        let long: String = std::iter::repeat_n('x', 100).collect();
        assert_eq!(verdict_tail(&long).len(), 60);
    }

    #[test]
    fn sanitize_artifact_component_keeps_safe_chars() {
        assert_eq!(sanitize_artifact_component("abc-XYZ_1.9"), "abc-XYZ_1.9");
        // Path separators, spaces and colons become underscores.
        assert_eq!(sanitize_artifact_component("a/b c:d"), "a_b_c_d");
    }

    #[test]
    fn artifact_stem_composes_rom_mode_oracle() {
        let case = TestCase {
            rom_path: PathBuf::from("/roms/my rom.gb"),
            mode: Mode::Cgb,
            oracle: Oracle::Serial,
            revision: None,
            input: Vec::new(),
            frames: None,
            cart_lazy_sram_cs: false,
        };
        assert_eq!(artifact_stem(&case), "my_rom_cgb_serial");
    }

    // Regression: `--real-bios` used to pick the boot image from the manifest
    // MODE, so every `rev=`-pinned row booted the mode default -- a rev=cgb0
    // row booted cgb_boot.bin, a rev=agb row (mode cgb) booted cgb_boot.bin
    // instead of agb_boot.bin. The boot image must follow the same effective
    // Hardware the case actually runs on.
    #[test]
    fn bios_follows_the_rev_pin_not_the_mode() {
        let pinned = |mode: Mode, revision: Option<Hardware>| {
            bios_filename(
                TestCase {
                    rom_path: PathBuf::from("x.gb"),
                    mode,
                    oracle: Oracle::Serial,
                    revision,
                    input: Vec::new(),
                    frames: None,
                    cart_lazy_sram_cs: false,
                }
                .hardware(),
            )
            .unwrap()
        };

        // Unpinned rows keep the mode default.
        assert_eq!(pinned(Mode::Dmg, None), "dmg_boot.bin");
        assert_eq!(pinned(Mode::Cgb, None), "cgb_boot.bin");
        assert_eq!(pinned(Mode::Agb, None), "agb_boot.bin");

        // A rev= pin overrides the mode default. These are the pins actually
        // used by gbc_hw_tests (cgb+agb / cgb+cgbe) and licensee_boot_div.
        assert_eq!(pinned(Mode::Cgb, Some(Hardware::AGB)), "agb_boot.bin");
        assert_eq!(pinned(Mode::Cgb, Some(Hardware::CGBE)), "cgbE_boot.bin");
        assert_eq!(pinned(Mode::Cgb, Some(Hardware::CGB0)), "cgb0_boot.bin");
        assert_eq!(pinned(Mode::Dmg, Some(Hardware::MGB)), "mgb_boot.bin");
    }

    #[test]
    fn run_case_inner_rejects_zero_frames() {
        let case = TestCase {
            rom_path: PathBuf::from("does-not-exist.gb"),
            mode: Mode::Dmg,
            oracle: Oracle::Serial,
            revision: None,
            input: Vec::new(),
            frames: None,
            cart_lazy_sram_cs: false,
        };
        // frames == 0 short-circuits before the ROM is ever read.
        let err = run_case_inner(&case, &RunOptions::default()).unwrap_err();
        assert!(err.contains("frame count"), "{err}");
    }

    // --- InputScript firing logic against a real GB ---------------------

    // A minimal 32 KiB ROM-only cart that jumps to a tight spin loop.
    fn spin_gb() -> GB {
        let mut rom = vec![0u8; 0x8000];
        rom[0x100] = 0xC3; // jp 0x0150
        rom[0x101] = 0x50;
        rom[0x102] = 0x01;
        rom[0x150] = 0x18; // jr -2 (spin)
        rom[0x151] = 0xFE;
        let cart = Cartridge::from_bytes(&rom).unwrap();
        let mut gb = GB::new(Hardware::DMG);
        gb.insert(cart);
        gb.skip_bios();
        gb
    }

    #[test]
    fn poll_fires_events_when_their_frame_window_is_reached() {
        let mut gb = spin_gb();
        let events = vec![
            InputEvent { frame: 0, ly: None, buttons: BTN_A },
            InputEvent { frame: 2, ly: None, buttons: BTN_START },
        ];
        let mut s = InputScript::new(&events);
        s.poll(&mut gb, 0);
        assert_eq!(s.next, 1); // frame-0 event is due at cycle 0
        s.poll(&mut gb, CYCLES_PER_FRAME as u64);
        assert_eq!(s.next, 1); // frame-2 event not due at frame 1
        s.poll(&mut gb, 2 * CYCLES_PER_FRAME as u64);
        assert_eq!(s.next, 2);
    }

    #[test]
    fn poll_same_frame_events_all_fire_at_once() {
        let mut gb = spin_gb();
        let events = vec![
            InputEvent { frame: 0, ly: None, buttons: BTN_A },
            InputEvent { frame: 0, ly: None, buttons: BTN_B },
        ];
        let mut s = InputScript::new(&events);
        s.poll(&mut gb, 0);
        assert_eq!(s.next, 2);
    }

    #[test]
    fn poll_at_ly_fires_on_match() {
        let mut gb = spin_gb();
        // Not stepping keeps LY fixed, so an event targeting the live LY matches.
        let ly = gb.read_memory(LY);
        let events = vec![InputEvent { frame: 0, ly: Some(ly), buttons: BTN_A }];
        let mut s = InputScript::new(&events);
        s.poll(&mut gb, 0);
        assert_eq!(s.next, 1);
    }

    #[test]
    fn poll_at_ly_force_fires_two_frames_past_due() {
        let mut gb = spin_gb();
        // Target an LY the (un-stepped) machine never shows, so only the
        // two-frames-past-due safety valve can fire the event.
        let never = gb.read_memory(LY).wrapping_add(1);
        let events = vec![InputEvent { frame: 0, ly: Some(never), buttons: BTN_A }];
        let mut s = InputScript::new(&events);
        s.poll(&mut gb, 0);
        assert_eq!(s.next, 0); // due, but LY mismatch holds it
        s.poll(&mut gb, 2 * CYCLES_PER_FRAME as u64 - 1);
        assert_eq!(s.next, 0);
        s.poll(&mut gb, 2 * CYCLES_PER_FRAME as u64);
        assert_eq!(s.next, 1); // force-fired at due + 2 frames
    }

    #[test]
    fn poll_frame_ignores_ly_and_gates_on_frame_index() {
        let mut gb = spin_gb();
        let events = vec![
            InputEvent { frame: 1, ly: Some(99), buttons: BTN_A }, // @ly ignored here
            InputEvent { frame: 3, ly: None, buttons: BTN_B },
        ];
        let mut s = InputScript::new(&events);
        s.poll_frame(&mut gb, 0);
        assert_eq!(s.next, 0); // frame 1 not yet reached
        s.poll_frame(&mut gb, 1);
        assert_eq!(s.next, 1); // reached, @ly disregarded
        s.poll_frame(&mut gb, 5);
        assert_eq!(s.next, 2);
    }
}
