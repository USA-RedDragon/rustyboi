use crate::expectation::Mode;
use crate::runner::CaseResult;
use serde::Serialize;
use std::fs;
use std::path::Path;

#[derive(Debug, Default, Serialize)]
pub struct Summary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped_roms: usize,
    pub dmg_total: usize,
    pub dmg_failed: usize,
    pub cgb_total: usize,
    pub cgb_failed: usize,
    #[serde(default)]
    pub agb_total: usize,
    #[serde(default)]
    pub agb_failed: usize,
    pub failures: Vec<FailureRecord>,
}

#[derive(Debug, Serialize)]
pub struct FailureRecord {
    pub rom: String,
    pub mode: Mode,
    pub oracle: String,
    pub detail: String,
}

impl Summary {
    pub fn record(&mut self, result: &CaseResult) {
        self.total += 1;

        match result.case.mode {
            Mode::Dmg => self.dmg_total += 1,
            Mode::Cgb => self.cgb_total += 1,
            Mode::Agb => self.agb_total += 1,
        }

        if result.passed {
            self.passed += 1;
        } else {
            self.failed += 1;
            match result.case.mode {
                Mode::Dmg => self.dmg_failed += 1,
                Mode::Cgb => self.cgb_failed += 1,
                Mode::Agb => self.agb_failed += 1,
            }

            self.failures.push(FailureRecord {
                rom: result.case.rom_path.display().to_string(),
                mode: result.case.mode,
                oracle: result.case.oracle.label(),
                detail: result.detail.clone(),
            });
        }
    }

    pub fn exit_code(&self) -> u8 {
        if self.failed == 0 { 0 } else { 1 }
    }
}

pub fn print_failure(result: &CaseResult) {
    println!(
        "\nFAILED: {} {} {}: {}",
        result.case.rom_path.display(),
        result.case.mode.label(),
        result.case.oracle.label(),
        result.detail
    );
}

pub fn print_summary(summary: &Summary) {
    println!("\nRan {} total tests.", summary.total);
    println!("{} total failures.", summary.failed);

    println!("\nRan {} CGB tests.", summary.cgb_total);
    println!("{} CGB failures.", summary.cgb_failed);

    println!("\nRan {} DMG tests.", summary.dmg_total);
    println!("{} DMG failures.", summary.dmg_failed);

    if summary.agb_total > 0 {
        println!("\nRan {} AGB tests.", summary.agb_total);
        println!("{} AGB failures.", summary.agb_failed);
    }

    if summary.skipped_roms > 0 {
        println!(
            "\nSkipped {} ROMs with no supported DMG/CGB Gambatte oracle.",
            summary.skipped_roms
        );
    }
}

pub fn write_json(summary: &Summary, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create JSON output directory: {error}"))?;
        }

    let json = serde_json::to_string_pretty(summary)
        .map_err(|error| format!("failed to serialize JSON summary: {error}"))?;
    fs::write(path, json).map_err(|error| format!("failed to write JSON summary: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expectation::{Oracle, TestCase};

    fn result(mode: Mode, passed: bool) -> CaseResult {
        CaseResult {
            case: TestCase {
                rom_path: std::path::PathBuf::from("dummy.gb"),
                mode,
                oracle: Oracle::Serial,
                revision: None,
                input: Vec::new(),
                frames: None,
                cart_lazy_sram_cs: false,
            },
            passed,
            detail: "detail".to_string(),
        }
    }

    #[test]
    fn record_tallies_per_mode_totals_and_failures() {
        let mut s = Summary::default();
        s.record(&result(Mode::Dmg, true));
        s.record(&result(Mode::Dmg, false));
        s.record(&result(Mode::Cgb, false));
        s.record(&result(Mode::Agb, true));

        assert_eq!(s.total, 4);
        assert_eq!(s.passed, 2);
        assert_eq!(s.failed, 2);
        assert_eq!((s.dmg_total, s.dmg_failed), (2, 1));
        assert_eq!((s.cgb_total, s.cgb_failed), (1, 1));
        assert_eq!((s.agb_total, s.agb_failed), (1, 0));
        // Only the failing cases produce a FailureRecord.
        assert_eq!(s.failures.len(), 2);
    }

    #[test]
    fn exit_code_is_zero_iff_nothing_failed() {
        assert_eq!(Summary::default().exit_code(), 0);

        let mut all_pass = Summary::default();
        all_pass.record(&result(Mode::Cgb, true));
        assert_eq!(all_pass.exit_code(), 0);

        let mut one_fail = Summary::default();
        one_fail.record(&result(Mode::Dmg, false));
        assert_eq!(one_fail.exit_code(), 1);
    }

    // A failing case carrying distinctive field values, to inspect the record.
    fn fail_case(mode: Mode, oracle: Oracle, detail: &str) -> CaseResult {
        CaseResult {
            case: TestCase {
                rom_path: std::path::PathBuf::from("/roms/cool.gb"),
                mode,
                oracle,
                revision: None,
                input: Vec::new(),
                frames: None,
                cart_lazy_sram_cs: false,
            },
            passed: false,
            detail: detail.to_string(),
        }
    }

    #[test]
    fn recorded_failure_captures_case_fields() {
        let mut s = Summary::default();
        s.record(&fail_case(
            Mode::Cgb,
            Oracle::Hex { marker: "out", expected: "1A".to_string() },
            "boom",
        ));
        let f = &s.failures[0];
        assert_eq!(f.rom, "/roms/cool.gb");
        assert_eq!(f.mode, Mode::Cgb);
        assert_eq!(f.oracle, "out1A"); // Oracle::label()
        assert_eq!(f.detail, "boom");
    }

    #[test]
    fn json_shape_names_fields_and_serializes_agb_defaults() {
        let mut s = Summary {
            skipped_roms: 3,
            ..Default::default()
        };
        s.record(&fail_case(Mode::Dmg, Oracle::Serial, "why"));
        let v = serde_json::to_value(&s).unwrap();

        assert_eq!(v["total"], 1);
        assert_eq!(v["failed"], 1);
        assert_eq!(v["skipped_roms"], 3);
        // The #[serde(default)] agb_* fields still serialize under their names.
        assert_eq!(v["agb_total"], 0);
        assert_eq!(v["agb_failed"], 0);

        let f = &v["failures"][0];
        assert_eq!(f["rom"], "/roms/cool.gb");
        assert_eq!(f["mode"], "dmg"); // Mode serde rename_all = lowercase
        assert_eq!(f["oracle"], "serial");
        assert_eq!(f["detail"], "why");
    }
}
