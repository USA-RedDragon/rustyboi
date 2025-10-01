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
        }

        if result.passed {
            self.passed += 1;
        } else {
            self.failed += 1;
            match result.case.mode {
                Mode::Dmg => self.dmg_failed += 1,
                Mode::Cgb => self.cgb_failed += 1,
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

    if summary.skipped_roms > 0 {
        println!(
            "\nSkipped {} ROMs with no supported DMG/CGB Gambatte oracle.",
            summary.skipped_roms
        );
    }
}

pub fn write_json(summary: &Summary, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create JSON output directory: {error}"))?;
        }
    }

    let json = serde_json::to_string_pretty(summary)
        .map_err(|error| format!("failed to serialize JSON summary: {error}"))?;
    fs::write(path, json).map_err(|error| format!("failed to write JSON summary: {error}"))
}
