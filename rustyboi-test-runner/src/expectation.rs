use clap::ValueEnum;
use rustyboi_core_lib::gb::Hardware;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Dmg,
    Cgb,
    /// GBA-in-GBC-compat mode. Runs a CGB ROM on Hardware::AGB. Opt-in only
    /// (never in the default mode set); selected via `--mode agb`.
    Agb,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dmg => "DMG",
            Self::Cgb => "CGB",
            Self::Agb => "AGB",
        }
    }

    pub fn progress_char(self) -> char {
        match self {
            Self::Dmg => 'd',
            Self::Cgb => 'c',
            Self::Agb => 'a',
        }
    }
}

/// Memory region a `.dump` oracle is captured from. The base address is fixed
/// per region; the length comes from the reference file size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DumpRegion {
    /// OAM region starting at 0xFE00 (a `.dump` may cover the full 256 bytes
    /// 0xFE00..=0xFEFF, not only the 160-byte sprite table).
    Oam,
    /// VRAM region starting at 0x8000.
    Vram,
}

impl DumpRegion {
    pub fn base_address(self) -> u16 {
        match self {
            Self::Oam => 0xFE00,
            Self::Vram => 0x8000,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Oracle {
    Hex { marker: &'static str, expected: String },
    Audio { marker: &'static str, audible: bool },
    Png { path: PathBuf },
    /// Cart SRAM contents dumped by the ROM, compared against a `.bin` reference.
    /// `skip` lists byte offsets in the reference that are NOT graded: the
    /// `fexx_*_dumper` references include the FE00-FE9F OAM sprite table, whose
    /// power-on contents are per-unit nondeterministic garbage (Pan Docs) and are
    /// not a portable hardware assertion -- Gambatte's own two DMG fexx references
    /// disagree on 105/160 of these bytes for the identical power-on. The tests'
    /// named subject is FEXX (FEA0-FEFF unusable region) + FFXX (I/O/HRAM), which
    /// stays fully graded. Gambatte's official testrunner does not grade the fexx
    /// dumpers at all (no `.png`/`_out`); the `.bin` is a rustyboi-added oracle.
    SramDump {
        path: PathBuf,
        skip: Vec<std::ops::Range<usize>>,
    },
    /// A memory region (OAM/VRAM) read back after the test, compared against a
    /// `.dump` reference. Length is the reference file size.
    RegionDump { path: PathBuf, region: DumpRegion },

    // --- c-sp public-suite oracles (manifest-driven; see `--manifest`) ---
    /// Framebuffer compared to a c-sp reference PNG (any color type/bit depth).
    /// Unlike `Png` (Gambatte suite), this runs instruction-by-instruction with
    /// an early stop on the `LD B,B` (0x40) done-marker and uses the Linear CGB
    /// color conversion (bucket-identical to the c-sp shift formula under the
    /// 0xF8 mask). Used by acid2 and mealybug-tearoom.
    CspPng { path: PathBuf },
    /// Like `CspPng` but runs a flat cycle budget (no `LD B,B` / frame-ready
    /// stop) and grades the final held framebuffer. For ROMs that turn the LCD
    /// off after rendering their result screen (e.g. blargg oam_bug). Manifest
    /// grading `png_fixed`.
    CspPngFixed { path: PathBuf },
    /// Like `CspPng` but grades the LAYOUT up to a consistent 1:1
    /// recoloring (`frame_buffer_mismatch_recolor`) instead of exact RGB. For
    /// tests whose reference was captured with a different palette than
    /// rustyboi's hardware-correct one, where the pixel layout — not the exact
    /// colors — is what the test measures: scxly-cgb (a DMG-compat cart the
    /// capture emulator rendered in DMG-green) and mbc3-tester-cgb (the ref's
    /// compat shade #7BFF4A vs rustyboi's boot-ROM-correct #7BFF31). Still fails
    /// any genuine layout error (a color that must map two ways). Grading
    /// `png_layout`.
    CspPngLayout { path: PathBuf },
    /// blargg serial-port text protocol: scan the bytes written to SB (FF01) on
    /// each SC (FF02) start edge for "Passed" / "Failed".
    Serial,
    /// blargg cart-RAM memory protocol: the result code is written to 0xA000
    /// (0x00 == pass) once the signature 0xDE 0xB0 0x61 appears at 0xA001-3.
    BlarggMem,
    /// gbmicrotest / generic memory check: after a fixed cycle budget, read
    /// `addr` and require it to equal `expected`. gbmicrotest uses FF82==0x01.
    MemValue { addr: u16, expected: u8 },
    /// mooneye: run to `LD B,B` and require the Fibonacci magic registers
    /// B,C,D,E,H,L = 3,5,8,13,21,34.
    MooneyeFib,
    /// mooneye (wilbertpol / age-test-roms era): identical Fibonacci-register
    /// check, but the done-marker is the illegal opcode `0xED` (the 2016 Mooneye
    /// convention) rather than `LD B,B` (0x40). Manifest grading `mooneye_ed`.
    MooneyeFibEd,

    /// GBEmulatorShootout-exact screenshot grading (`png_shootout`). Runs a flat
    /// per-test frame budget (from the manifest `frames=` token, itself derived
    /// from the shootout's `runtime` seconds) and grades the final held
    /// framebuffer against one-or-more reference PNGs using the shootout's
    /// lenient grayscale rule: convert both to PIL "L", per-pixel abs diff, pass
    /// iff every pixel's diff is <= 50 (see `frame::shootout_mismatch`). Multiple
    /// refs are an OR-match (the shootout's `pass_result` list). This mirrors the
    /// shootout's own `Emulator.runTest` + `Test.checkResult`, so the numbers are
    /// apples-to-apples with docboy et al. Uses the Gambatte CGB color conversion
    /// (the shootout compares full-RGB screenshots, not 5-bit-masked buckets).
    PngShootout { refs: Vec<PathBuf>, frames: usize },
}

impl Oracle {
    pub fn label(&self) -> String {
        match self {
            Self::Hex { marker, expected } => format!("{marker}{expected}"),
            Self::Audio { marker, audible } => {
                let suffix = if *audible { "audio1" } else { "audio0" };
                format!("{marker}{suffix}")
            }
            Self::Png { path }
            | Self::CspPng { path }
            | Self::CspPngFixed { path }
            | Self::CspPngLayout { path } => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "png".to_string()),
            Self::SramDump { path, .. } | Self::RegionDump { path, .. } => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "dump".to_string()),
            Self::Serial => "serial".to_string(),
            Self::BlarggMem => "blargg_mem".to_string(),
            Self::MemValue { addr, expected } => format!("mem[{addr:04X}]={expected:02X}"),
            Self::MooneyeFib => "mooneye".to_string(),
            Self::MooneyeFibEd => "mooneye_ed".to_string(),
            Self::PngShootout { refs, .. } => refs
                .first()
                .and_then(|p| p.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "png_shootout".to_string()),
        }
    }
}

/// One scripted joypad event: at `frame` (70224-cycle windows since case
/// start), optionally waiting for LY to equal `ly`, replace the held-button
/// state with `buttons` (a `BTN_*` bitmask; 0 releases everything). Parsed
/// from a manifest `input=` token; see `parse_input_script`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputEvent {
    pub frame: u32,
    pub ly: Option<u8>,
    pub buttons: u8,
}

pub const BTN_A: u8 = 0x01;
pub const BTN_B: u8 = 0x02;
pub const BTN_SELECT: u8 = 0x04;
pub const BTN_START: u8 = 0x08;
pub const BTN_RIGHT: u8 = 0x10;
pub const BTN_LEFT: u8 = 0x20;
pub const BTN_UP: u8 = 0x40;
pub const BTN_DOWN: u8 = 0x80;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TestCase {
    pub rom_path: PathBuf,
    pub mode: Mode,
    pub oracle: Oracle,
    /// Hardware sub-revision override. `None` uses the default model for the
    /// case's `mode` (DMG-ABC / CGB-04 / AGB). `Some` selects a specific boot
    /// revision (DMG0/MGB/SGB/CGB0/...) for the mooneye boot_regs/boot_div/
    /// boot_hwio tests that check a particular silicon revision's post-boot
    /// state. Set from a `rev=<model>` manifest token; see `parse_manifest`.
    pub revision: Option<Hardware>,
    /// Scripted joypad input (frame-keyed, deterministic), from an `input=`
    /// manifest token. Empty for the (overwhelmingly common) input-less case.
    /// Only PNG-graded cases (`png`/`png_fixed`/`png_shootout`) accept it.
    pub input: Vec<InputEvent>,
    /// Per-case frame-budget override for `png`/`png_fixed` gradings, from a
    /// `frames=<N>` manifest token; `None` uses the run-wide `--frames`.
    /// (`png_shootout` carries its budget inside the oracle as before.)
    pub frames: Option<usize>,
}

pub fn cases_for_rom(rom_path: &Path, requested_modes: &HashSet<Mode>) -> Vec<TestCase> {
    let base = extension_stripped_string(rom_path);
    let mut cases = Vec::new();

    // AGB cases are derived from the CGB oracle reference files, so when AGB is
    // requested we generate the CGB case templates (even if CGB itself was not
    // requested) and twin them to AGB below. The CGB templates are dropped
    // afterward unless CGB was actually requested.
    let cgb_requested = requested_modes.contains(&Mode::Cgb);
    let agb_requested = requested_modes.contains(&Mode::Agb);
    let mut enabled = requested_modes.clone();
    if agb_requested {
        enabled.insert(Mode::Cgb);
    }
    let enabled_modes = &enabled;

    if base.contains("dmg08_cgb04c_out") {
        push_string_case(
            &mut cases,
            rom_path,
            Mode::Cgb,
            "dmg08_cgb04c_out",
            enabled_modes,
            &base,
        );
        push_string_case(
            &mut cases,
            rom_path,
            Mode::Dmg,
            "dmg08_cgb04c_out",
            enabled_modes,
            &base,
        );
    } else if base.contains("dmg08_out") {
        push_string_case(
            &mut cases,
            rom_path,
            Mode::Dmg,
            "dmg08_out",
            enabled_modes,
            &base,
        );

        if base.contains("cgb04c_out") {
            push_string_case(
                &mut cases,
                rom_path,
                Mode::Cgb,
                "cgb04c_out",
                enabled_modes,
                &base,
            );
        }
    } else if base.contains("_out") {
        push_string_case(&mut cases, rom_path, Mode::Cgb, "_out", enabled_modes, &base);
    }

    let shared_png = PathBuf::from(format!("{base}_dmg08_cgb04c.png"));
    if shared_png.exists() {
        push_png_case(&mut cases, rom_path, Mode::Cgb, enabled_modes, &shared_png);
        push_png_case(&mut cases, rom_path, Mode::Dmg, enabled_modes, &shared_png);
    } else {
        let cgb_png = PathBuf::from(format!("{base}_cgb04c.png"));
        if cgb_png.exists() {
            push_png_case(&mut cases, rom_path, Mode::Cgb, enabled_modes, &cgb_png);
        }

        let dmg_png = PathBuf::from(format!("{base}_dmg08.png"));
        if dmg_png.exists() {
            push_png_case(&mut cases, rom_path, Mode::Dmg, enabled_modes, &dmg_png);
        }
    }

    push_dump_cases(&mut cases, rom_path, enabled_modes, &base);

    // AGB cases reuse the CGB oracle reference files: AGB is CGB-compatible, so
    // the same CGB (`cgb04c`) references are the baseline. This measures how many
    // CGB references rustyboi-AGB still matches. (Where AGB's isAgb() diffs make
    // the output legitimately differ from CGB hardware, the divergence is
    // expected and is cross-checked against Gambatte-AGB by the bootstrap
    // oracle tool, not these CGB references.) Opt-in: only when `agb` is enabled.
    if agb_requested {
        let cgb_twins: Vec<TestCase> = cases
            .iter()
            .filter(|c| c.mode == Mode::Cgb)
            .map(|c| TestCase {
                rom_path: c.rom_path.clone(),
                mode: Mode::Agb,
                oracle: c.oracle.clone(),
                revision: c.revision,
                input: c.input.clone(),
                frames: c.frames,
            })
            .collect();
        cases.extend(cgb_twins);
    }
    // Drop the CGB templates if CGB was not actually requested (AGB-only run).
    if !cgb_requested {
        cases.retain(|c| c.mode != Mode::Cgb);
    }

    cases
}

/// Parse a c-sp suite manifest into `TestCase`s, keeping only the requested
/// modes. Each non-blank, non-`#` line is `|`-separated:
///   `<id>|<mode>|<grading>|<rom_path>[|<arg>]`
/// where `<mode>` is `dmg`/`cgb`/`agb`, `<grading>` is one of `png`, `serial`,
/// `blargg_mem`, `memauto`, `mem`, `mooneye`, `mooneye_ed`, and `<arg>` is the reference-PNG
/// path (png), `ADDR=VAL` hex (mem), or empty. The `<id>` is descriptive only.
pub fn parse_manifest(
    text: &str,
    requested_modes: &HashSet<Mode>,
) -> Result<Vec<TestCase>, String> {
    let mut cases = Vec::new();
    for (line_no, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('|').collect();
        if fields.len() < 4 {
            return Err(format!("manifest line {}: too few fields: {raw}", line_no + 1));
        }
        // Gambatte hwtests: the oracle (register/audio/dumper) and the modes
        // are encoded in the ROM FILENAME (`_dmg08_cgb04c_outNN`, dumper
        // sibling files, ...). `auto` defers both to `cases_for_rom` — one
        // manifest line may expand to several cases, exactly like the old
        // --suite walker.
        if fields[2] == "gambatte" {
            if fields[1] != "auto" {
                return Err(format!(
                    "manifest line {}: gambatte grading uses mode `auto`",
                    line_no + 1
                ));
            }
            cases.extend(cases_for_rom(Path::new(fields[3]), requested_modes));
            continue;
        }
        let mode = match fields[1] {
            "dmg" => Mode::Dmg,
            "cgb" => Mode::Cgb,
            "agb" => Mode::Agb,
            other => return Err(format!("manifest line {}: bad mode {other}", line_no + 1)),
        };
        if !requested_modes.contains(&mode) {
            continue;
        }
        let grading = fields[2];
        let rom_path = PathBuf::from(fields[3]);
        let arg = fields.get(4).copied().unwrap_or("").trim();
        // Field 5 (index 5) is an optional extra token. For `png_shootout` it
        // carries `frames=<N>` (the per-test frame budget derived from the
        // shootout `runtime` seconds); other gradings ignore it.
        let arg2 = fields.get(5).copied().unwrap_or("").trim();
        let oracle = match grading {
            // GBEmulatorShootout-exact grading: field 4 is a `;`-separated list
            // of reference PNGs (OR-match), field 5 is `frames=<N>`.
            "png_shootout" => {
                let refs: Vec<PathBuf> = arg
                    .split(';')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from)
                    .collect();
                if refs.is_empty() {
                    return Err(format!(
                        "manifest line {}: png_shootout needs at least one ref PNG",
                        line_no + 1
                    ));
                }
                let frames = arg2
                    .strip_prefix("frames=")
                    .map(|n| {
                        n.parse::<usize>().map_err(|_| {
                            format!("manifest line {}: bad frames {arg2}", line_no + 1)
                        })
                    })
                    .transpose()?
                    .unwrap_or(0);
                Oracle::PngShootout { refs, frames }
            }
            "png" => Oracle::CspPng {
                path: PathBuf::from(arg),
            },
            "png_fixed" => Oracle::CspPngFixed {
                path: PathBuf::from(arg),
            },
            "png_layout" => Oracle::CspPngLayout {
                path: PathBuf::from(arg),
            },
            "serial" => Oracle::Serial,
            "blargg_mem" => Oracle::BlarggMem,
            "mooneye" => Oracle::MooneyeFib,
            "mooneye_ed" => Oracle::MooneyeFibEd,
            "memauto" => Oracle::MemValue {
                addr: 0xFF82,
                expected: 0x01,
            },
            "mem" => {
                let (a, v) = arg
                    .split_once('=')
                    .ok_or_else(|| format!("manifest line {}: mem arg needs ADDR=VAL", line_no + 1))?;
                let addr = u16::from_str_radix(a.trim().trim_start_matches("0x"), 16)
                    .map_err(|_| format!("manifest line {}: bad addr {a}", line_no + 1))?;
                let expected = u8::from_str_radix(v.trim().trim_start_matches("0x"), 16)
                    .map_err(|_| format!("manifest line {}: bad value {v}", line_no + 1))?;
                Oracle::MemValue { addr, expected }
            }
            other => return Err(format!("manifest line {}: bad grading {other}", line_no + 1)),
        };
        // Optional hardware sub-revision override, carried as a `rev=<model>`
        // token in ANY trailing field. Used by the mooneye boot_regs/boot_div/
        // boot_hwio tests that check a specific silicon revision's post-boot
        // state, and by SGB tests (`rev=sgb`) which need Hardware::SGB while
        // still grading via png_shootout (whose field 4/5 carry the ref/frames).
        // Absent, the case uses the default model for `mode`.
        let rev_tok = fields
            .iter()
            .skip(4)
            .map(|f| f.trim())
            .find(|f| f.starts_with("rev="));
        let revision = if let Some(tok) = rev_tok {
            Some(parse_revision(&tok["rev=".len()..]).ok_or_else(|| {
                format!("manifest line {}: unknown revision {tok}", line_no + 1)
            })?)
        } else {
            None
        };
        // Optional scripted joypad input, carried as an `input=` token in ANY
        // trailing field. Only meaningful for screenshot gradings (the input
        // schedule is keyed to the frame clock those paths drive); reject it
        // elsewhere so a wiring mistake fails loudly instead of silently
        // running input-less.
        let input_tok = fields
            .iter()
            .skip(4)
            .map(|f| f.trim())
            .find(|f| f.starts_with("input="));
        let input = if let Some(tok) = input_tok {
            if !matches!(
                oracle,
                Oracle::CspPng { .. } | Oracle::CspPngFixed { .. } | Oracle::CspPngLayout { .. } | Oracle::PngShootout { .. }
            ) {
                return Err(format!(
                    "manifest line {}: input= is only supported for png/png_fixed/png_shootout gradings",
                    line_no + 1
                ));
            }
            parse_input_script(&tok["input=".len()..])
                .map_err(|e| format!("manifest line {}: {e}", line_no + 1))?
        } else {
            Vec::new()
        };
        // Optional per-case frame budget for `png`/`png_fixed`/`memauto`/`mem`,
        // carried as a `frames=<N>` token in ANY trailing field (for
        // `png_shootout` the positional field-5 token already feeds the oracle;
        // it is not duplicated into the case override). Some gbmicrotest cases
        // (e.g. is_if_set_during_ime0) settle their FF82 verdict later than the
        // 60-frame default.
        let frames = if matches!(
            oracle,
            Oracle::CspPng { .. } | Oracle::CspPngFixed { .. } | Oracle::CspPngLayout { .. } | Oracle::MemValue { .. }
        ) {
            fields
                .iter()
                .skip(4)
                .map(|f| f.trim())
                .find_map(|f| f.strip_prefix("frames="))
                .map(|n| {
                    n.parse::<usize>()
                        .map_err(|_| format!("manifest line {}: bad frames {n}", line_no + 1))
                })
                .transpose()?
        } else {
            None
        };
        cases.push(TestCase {
            rom_path,
            mode,
            oracle,
            revision,
            input,
            frames,
        });
    }
    Ok(cases)
}

/// Parse an `input=` script: comma-separated events `<frame>[@<ly>]:<buttons>`
/// where `<buttons>` is a `+`-separated list of `a`/`b`/`start`/`select`/`up`/
/// `down`/`left`/`right` (case-insensitive), or `-` for "release everything".
/// The event replaces the whole held-button state at `<frame>` (70224-cycle
/// windows since case start); with `@<ly>` it waits (up to two extra frames)
/// for LY to equal `<ly>` first, so presses can land mid-frame at a chosen
/// scanline. Events must be in non-decreasing frame order.
pub fn parse_input_script(script: &str) -> Result<Vec<InputEvent>, String> {
    let mut events = Vec::new();
    let mut last_frame = 0u32;
    for tok in script.split(',') {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        let (when, buttons_str) = tok
            .split_once(':')
            .ok_or_else(|| format!("bad input event {tok}: missing ':'"))?;
        let (frame_str, ly) = match when.split_once('@') {
            Some((f, ly)) => (
                f,
                Some(
                    ly.parse::<u8>()
                        .ok()
                        .filter(|ly| *ly <= 153)
                        .ok_or_else(|| format!("bad input event {tok}: LY must be 0-153"))?,
                ),
            ),
            None => (when, None),
        };
        let frame = frame_str
            .parse::<u32>()
            .map_err(|_| format!("bad input event {tok}: bad frame number"))?;
        if frame < last_frame {
            return Err(format!(
                "input events must be in non-decreasing frame order (event {tok})"
            ));
        }
        last_frame = frame;
        let mut buttons = 0u8;
        if buttons_str != "-" {
            for name in buttons_str.split('+') {
                buttons |= match name.trim().to_ascii_lowercase().as_str() {
                    "a" => BTN_A,
                    "b" => BTN_B,
                    "select" => BTN_SELECT,
                    "start" => BTN_START,
                    "right" => BTN_RIGHT,
                    "left" => BTN_LEFT,
                    "up" => BTN_UP,
                    "down" => BTN_DOWN,
                    other => return Err(format!("unknown button {other}")),
                };
            }
        }
        events.push(InputEvent { frame, ly, buttons });
    }
    Ok(events)
}

/// Map a `rev=` manifest token to a `Hardware` sub-revision. The token names
/// the mooneye boot-test target model (`dmg0`/`mgb`/`sgb`/`sgb2`/`cgb0`/`cgb`/
/// `agb`); `dmg`/`cgb` here name the default ABC/04 silicon used elsewhere.
fn parse_revision(token: &str) -> Option<Hardware> {
    match token {
        "dmg" => Some(Hardware::DMG),
        "dmg0" => Some(Hardware::DMG0),
        "mgb" => Some(Hardware::MGB),
        "sgb" => Some(Hardware::SGB),
        "sgb2" => Some(Hardware::SGB2),
        "cgb0" => Some(Hardware::CGB0),
        // CPU-CGB-A/B APU revision (SameSuite's *_extra_length_clocking-cgbB /
        // freq_change_timing-cgb0BC silicon): boot state == CGB, differs only
        // in the CGB-B-or-earlier APU length-glitch + <=C step-back/PCM gates.
        "cgbb" => Some(Hardware::CGBB),
        "cgb" => Some(Hardware::CGB),
        // CPU-CGB-D/E APU revision (SameSuite's validation silicon); boot
        // state == CGB, differs only in the C-vs-D/E APU gates.
        "cgbe" => Some(Hardware::CGBE),
        "agb" => Some(Hardware::AGB),
        _ => None,
    }
}

/// Detect SRAM `.bin` and region `.dump` oracles that accompany a dumper ROM.
fn push_dump_cases(
    cases: &mut Vec<TestCase>,
    rom_path: &Path,
    enabled_modes: &HashSet<Mode>,
    base: &str,
) {
    // Per-model SRAM dumps: `<base>_dmg08.bin` / `<base>_cgb.bin`.
    let dmg_bin = PathBuf::from(format!("{base}_dmg08.bin"));
    if dmg_bin.exists() && enabled_modes.contains(&Mode::Dmg) {
        cases.push(TestCase {
            rom_path: rom_path.to_path_buf(),
            mode: Mode::Dmg,
            oracle: Oracle::SramDump {
                path: dmg_bin,
                skip: dmg_dump_skip(base),
            },
            revision: None,
            input: Vec::new(),
            frames: None,
        });
    }
    let cgb_bin = PathBuf::from(format!("{base}_cgb.bin"));
    if cgb_bin.exists() && enabled_modes.contains(&Mode::Cgb) {
        cases.push(TestCase {
            rom_path: rom_path.to_path_buf(),
            mode: Mode::Cgb,
            oracle: Oracle::SramDump {
                path: cgb_bin,
                skip: Vec::new(),
            },
            revision: None,
            input: Vec::new(),
            frames: None,
        });
    }

    // Region `.dump` oracles. CGB-only single file `<base>.dump`, plus an
    // optional DMG variant `<base>_dmg08.dump` (e.g. the oambusy dumpers).
    if let Some(region) = dump_region_for(base) {
        let cgb_dump = PathBuf::from(format!("{base}.dump"));
        if cgb_dump.exists() && enabled_modes.contains(&Mode::Cgb) {
            cases.push(TestCase {
                rom_path: rom_path.to_path_buf(),
                mode: Mode::Cgb,
                oracle: Oracle::RegionDump {
                    path: cgb_dump,
                    region,
                },
                revision: None,
                input: Vec::new(),
                frames: None,
            });
        }
        let dmg_dump = PathBuf::from(format!("{base}_dmg08.dump"));
        if dmg_dump.exists() && enabled_modes.contains(&Mode::Dmg) {
            cases.push(TestCase {
                rom_path: rom_path.to_path_buf(),
                mode: Mode::Dmg,
                oracle: Oracle::RegionDump {
                    path: dmg_dump,
                    region,
                },
                revision: None,
                input: Vec::new(),
                frames: None,
            });
        }
    }
}

/// Byte offsets in a DMG SRAM dump reference that are NOT graded because they
/// hold nondeterministic power-on OAM garbage rather than a portable hardware
/// assertion. Only the `fexx_*_dumper` references dump FE00-FFFF (or FE00-FEFF)
/// with the OAM sprite table (FE00-FE9F) untouched at power-on; that 0xA0-byte
/// window is per-unit garbage (Pan Docs) and Gambatte's own two DMG references
/// disagree on 105/160 of these bytes. Everything else -- FEA0-FEFF (the FEXX
/// unusable region) and FF00-FFFF (FFXX I/O/HRAM), the tests' named subject --
/// stays graded. `fexx_read_reset_set_dumper` writes 3 back-to-back 256-byte
/// dumps of FE00-FEFF; only the FIRST dump's OAM is power-on garbage (dumps 2/3
/// are the deterministic clear->0x00 / set->0xFF that verify OAM writability),
/// so only offsets 0x00-0x9F are skipped.
fn dmg_dump_skip(base: &str) -> Vec<std::ops::Range<usize>> {
    if base.contains("fexx_ffxx_dumper") || base.contains("fexx_read_reset_set_dumper") {
        vec![0x00..0xA0]
    } else {
        Vec::new()
    }
}

fn dump_region_for(base: &str) -> Option<DumpRegion> {
    if base.contains("oamdumper") || base.contains("oambusy_dumper") {
        Some(DumpRegion::Oam)
    } else if base.contains("vramdumper") {
        Some(DumpRegion::Vram)
    } else {
        None
    }
}

fn push_string_case(
    cases: &mut Vec<TestCase>,
    rom_path: &Path,
    mode: Mode,
    marker: &'static str,
    enabled_modes: &HashSet<Mode>,
    base: &str,
) {
    if !enabled_modes.contains(&mode) {
        return;
    }

    if let Some(oracle) = string_oracle(base, marker) {
        cases.push(TestCase {
            rom_path: rom_path.to_path_buf(),
            mode,
            oracle,
            revision: None,
            input: Vec::new(),
            frames: None,
        });
    }
}

fn push_png_case(
    cases: &mut Vec<TestCase>,
    rom_path: &Path,
    mode: Mode,
    enabled_modes: &HashSet<Mode>,
    png_path: &Path,
) {
    if enabled_modes.contains(&mode) {
        cases.push(TestCase {
            rom_path: rom_path.to_path_buf(),
            mode,
            oracle: Oracle::Png {
                path: png_path.to_path_buf(),
            },
            revision: None,
            input: Vec::new(),
            frames: None,
        });
    }
}

fn string_oracle(base: &str, marker: &'static str) -> Option<Oracle> {
    let output_pos = base.find(marker)?;
    let output = &base[output_pos + marker.len()..];

    if output.starts_with("audio0") {
        Some(Oracle::Audio {
            marker,
            audible: false,
        })
    } else if output.starts_with("audio1") {
        Some(Oracle::Audio {
            marker,
            audible: true,
        })
    } else {
        let expected = output
            .chars()
            .take_while(|character| character.is_ascii_hexdigit())
            .collect::<String>();

        if expected.is_empty() {
            None
        } else {
            Some(Oracle::Hex { marker, expected })
        }
    }
}

fn extension_stripped_string(path: &Path) -> String {
    let path = path.to_string_lossy();
    path.rfind('.')
        .map(|extension_pos| path[..extension_pos].to_string())
        .unwrap_or_else(|| path.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_modes() -> HashSet<Mode> {
        [Mode::Dmg, Mode::Cgb].into_iter().collect()
    }

    #[test]
    fn parses_manifest_oracles_and_filters_modes() {
        let manifest = "\
# comment line
acid2|dmg|png|/roms/dmg-acid2.gb|/refs/acid2.png
blargg/cpu|cgb|serial|/roms/cpu_instrs.gb
sound/01|dmg|blargg_mem|/roms/01.gb
000-oam|dmg|memauto|/roms/000-oam.gb
custom|dmg|mem|/roms/custom.gb|0xFF80=0x01
mn/add|cgb|mooneye|/roms/add.gb
";
        let dmg_only: HashSet<Mode> = [Mode::Dmg].into_iter().collect();
        let cases = parse_manifest(manifest, &dmg_only).unwrap();
        // Only the dmg lines survive the mode filter (4 of 6 entries).
        assert_eq!(cases.len(), 4);
        assert!(matches!(&cases[0].oracle, Oracle::CspPng { path } if path == Path::new("/refs/acid2.png")));
        assert!(matches!(cases[1].oracle, Oracle::BlarggMem));
        assert!(matches!(
            cases[2].oracle,
            Oracle::MemValue { addr: 0xFF82, expected: 0x01 }
        ));
        assert!(matches!(
            cases[3].oracle,
            Oracle::MemValue { addr: 0xFF80, expected: 0x01 }
        ));

        let all = parse_manifest(manifest, &all_modes()).unwrap();
        assert_eq!(all.len(), 6);
        assert!(matches!(all[1].oracle, Oracle::Serial));
        assert!(matches!(all[5].oracle, Oracle::MooneyeFib));
    }

    #[test]
    fn rejects_malformed_manifest_lines() {
        assert!(parse_manifest("too|few|fields", &all_modes()).is_err());
        assert!(parse_manifest("id|xbox|png|/r.gb|p", &all_modes()).is_err());
        assert!(parse_manifest("id|dmg|bogus|/r.gb", &all_modes()).is_err());
        assert!(parse_manifest("id|dmg|mem|/r.gb|noeq", &all_modes()).is_err());
    }

    #[test]
    fn parses_input_scripts() {
        let events = parse_input_script("20:A,30:-,40@21:Down+b,55:start").unwrap();
        assert_eq!(
            events,
            vec![
                InputEvent { frame: 20, ly: None, buttons: BTN_A },
                InputEvent { frame: 30, ly: None, buttons: 0 },
                InputEvent { frame: 40, ly: Some(21), buttons: BTN_DOWN | BTN_B },
                InputEvent { frame: 55, ly: None, buttons: BTN_START },
            ]
        );
        // Out-of-order frames, bad LY, unknown buttons, missing ':' all fail.
        assert!(parse_input_script("30:A,20:-").is_err());
        assert!(parse_input_script("20@200:A").is_err());
        assert!(parse_input_script("20:X").is_err());
        assert!(parse_input_script("20A").is_err());
    }

    #[test]
    fn parses_manifest_input_and_frames_tokens() {
        let manifest = "\
rtc/basic|dmg|png|/r.gb|/ref.png|input=20:A,30:-|frames=850
plain|dmg|png|/r.gb|/ref.png
";
        let cases = parse_manifest(manifest, &all_modes()).unwrap();
        assert_eq!(cases[0].input.len(), 2);
        assert_eq!(cases[0].input[0].buttons, BTN_A);
        assert_eq!(cases[0].frames, Some(850));
        assert!(cases[1].input.is_empty());
        assert_eq!(cases[1].frames, None);
        // input= on a non-screenshot grading is rejected.
        assert!(parse_manifest("x|dmg|mooneye|/r.gb|input=20:A", &all_modes()).is_err());
    }

    #[test]
    fn expands_shared_dmg_cgb_hex_output() {
        let cases = cases_for_rom(Path::new("foo_dmg08_cgb04c_outA.gb"), &all_modes());

        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].mode, Mode::Cgb);
        assert_eq!(cases[1].mode, Mode::Dmg);
        assert!(matches!(
            &cases[0].oracle,
            Oracle::Hex {
                marker: "dmg08_cgb04c_out",
                expected
            } if expected == "A"
        ));
    }

    #[test]
    fn expands_split_dmg_and_cgb_hex_outputs() {
        let cases = cases_for_rom(Path::new("foo_dmg08_out0_cgb04c_outF.gbc"), &all_modes());

        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].mode, Mode::Dmg);
        assert_eq!(cases[1].mode, Mode::Cgb);
        assert!(matches!(
            &cases[0].oracle,
            Oracle::Hex { expected, .. } if expected == "0"
        ));
        assert!(matches!(
            &cases[1].oracle,
            Oracle::Hex { expected, .. } if expected == "F"
        ));
    }

    #[test]
    fn parses_audio_expectations() {
        let cases = cases_for_rom(Path::new("silence_dmg08_outaudio0.gb"), &all_modes());

        assert_eq!(cases.len(), 1);
        assert!(matches!(
            cases[0].oracle,
            Oracle::Audio {
                marker: "dmg08_out",
                audible: false
            }
        ));
    }

    #[test]
    fn infers_dump_region_from_filename() {
        assert_eq!(dump_region_for("foo_oamdumper_1"), Some(DumpRegion::Oam));
        assert_eq!(
            dump_region_for("oamdma_src80_oambusy_dumper_1"),
            Some(DumpRegion::Oam)
        );
        assert_eq!(dump_region_for("foo_vramdumper_1"), Some(DumpRegion::Vram));
        assert_eq!(dump_region_for("foo_outA"), None);
    }

    #[test]
    fn detects_sram_and_region_dump_oracles() {
        let dir = std::env::temp_dir().join(format!("rtk_dump_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Per-model SRAM dumps.
        std::fs::write(dir.join("vram_dumper_dmg08.bin"), [0u8; 4]).unwrap();
        std::fs::write(dir.join("vram_dumper_cgb.bin"), [0u8; 4]).unwrap();
        let cases = cases_for_rom(&dir.join("vram_dumper.gbc"), &all_modes());
        assert_eq!(cases.len(), 2);
        let dmg = cases.iter().find(|c| c.mode == Mode::Dmg).unwrap();
        assert!(matches!(dmg.oracle, Oracle::SramDump { .. }));

        // CGB-only region dump plus a DMG variant.
        std::fs::write(dir.join("oambusy_dumper_1.dump"), [0u8; 4]).unwrap();
        std::fs::write(dir.join("oambusy_dumper_1_dmg08.dump"), [0u8; 4]).unwrap();
        let cases = cases_for_rom(&dir.join("oambusy_dumper_1.gbc"), &all_modes());
        assert_eq!(cases.len(), 2);
        assert!(cases.iter().all(|c| matches!(
            c.oracle,
            Oracle::RegionDump {
                region: DumpRegion::Oam,
                ..
            }
        )));

        // CGB-only when only the base .dump exists.
        std::fs::write(dir.join("vramdumper_1.dump"), [0u8; 4]).unwrap();
        let cases = cases_for_rom(&dir.join("vramdumper_1.gbc"), &all_modes());
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].mode, Mode::Cgb);
        assert!(matches!(
            cases[0].oracle,
            Oracle::RegionDump {
                region: DumpRegion::Vram,
                ..
            }
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
