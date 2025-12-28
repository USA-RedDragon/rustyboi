use clap::ValueEnum;
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
    SramDump { path: PathBuf },
    /// A memory region (OAM/VRAM) read back after the test, compared against a
    /// `.dump` reference. Length is the reference file size.
    RegionDump { path: PathBuf, region: DumpRegion },
}

impl Oracle {
    pub fn label(&self) -> String {
        match self {
            Self::Hex { marker, expected } => format!("{marker}{expected}"),
            Self::Audio { marker, audible } => {
                let suffix = if *audible { "audio1" } else { "audio0" };
                format!("{marker}{suffix}")
            }
            Self::Png { path } => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "png".to_string()),
            Self::SramDump { path } | Self::RegionDump { path, .. } => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "dump".to_string()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TestCase {
    pub rom_path: PathBuf,
    pub mode: Mode,
    pub oracle: Oracle,
}

pub fn is_rom_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            let extension = extension.to_ascii_lowercase();
            extension.starts_with("gb") || extension == "sgb"
        })
        .unwrap_or(false)
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
            oracle: Oracle::SramDump { path: dmg_bin },
        });
    }
    let cgb_bin = PathBuf::from(format!("{base}_cgb.bin"));
    if cgb_bin.exists() && enabled_modes.contains(&Mode::Cgb) {
        cases.push(TestCase {
            rom_path: rom_path.to_path_buf(),
            mode: Mode::Cgb,
            oracle: Oracle::SramDump { path: cgb_bin },
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
            });
        }
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
    fn identifies_game_boy_rom_extensions() {
        assert!(is_rom_path(Path::new("a.gb")));
        assert!(is_rom_path(Path::new("a.gbc")));
        assert!(is_rom_path(Path::new("a.sgb")));
        assert!(!is_rom_path(Path::new("a.png")));
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
