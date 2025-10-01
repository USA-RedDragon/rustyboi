use clap::ValueEnum;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Dmg,
    Cgb,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dmg => "DMG",
            Self::Cgb => "CGB",
        }
    }

    pub fn progress_char(self) -> char {
        match self {
            Self::Dmg => 'd',
            Self::Cgb => 'c',
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Oracle {
    Hex { marker: &'static str, expected: String },
    Audio { marker: &'static str, audible: bool },
    Png { path: PathBuf },
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

pub fn cases_for_rom(rom_path: &Path, enabled_modes: &HashSet<Mode>) -> Vec<TestCase> {
    let base = extension_stripped_string(rom_path);
    let mut cases = Vec::new();

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

    cases
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
}
