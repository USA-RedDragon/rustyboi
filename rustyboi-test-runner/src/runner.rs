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
    });
    gb.insert(cartridge);
    gb.skip_bios();

    if case.mode == Mode::Cgb {
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
            "TRACE frame={frame_index} pc={:#06X}->{:#06X} cyc={} ime={}->{} delay={}->{} ppu={:?}@{} x={} ly={} fifo={} tile={} stall={} -> {:?}@{} x={} ly={} fifo={} tile={} stall={} stat={:#04X}->{:#04X} lcdc={:#04X}->{:#04X} if={:#04X}->{:#04X} ie={:#04X}->{:#04X}",
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
