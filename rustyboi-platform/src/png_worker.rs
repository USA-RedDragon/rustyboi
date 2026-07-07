//! Off-thread PNG encode + write for Game Boy Printer output (native desktop).
//!
//! A finished print sheet is a `PrintSheet` (plain shade data). Encoding it to
//! PNG and writing the file used to run inline on the emulation/update thread,
//! stalling the core loop for the duration of the encode + blocking disk I/O.
//! This worker moves both off-thread: the update thread only hands over the
//! (cheap, `Clone`) sheet + target path.
//!
//! `PrintSheet` is `Send` (plain owned data — no audio sink), so no unsafe is
//! needed here.

use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;

use rustyboi_core_lib::printer::PrintSheet;

struct Job {
    path: PathBuf,
    sheet: PrintSheet,
}

/// Owns the background PNG encoder/writer thread.
pub struct PngWorker {
    tx: Option<Sender<Job>>,
    handle: Option<JoinHandle<()>>,
}

impl PngWorker {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        let handle = std::thread::Builder::new()
            .name("printer-png".to_string())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let png = job.sheet.to_png();
                    match std::fs::write(&job.path, &png) {
                        Ok(()) => println!(
                            "Printed {}x{} sheet to: {}",
                            job.sheet.width,
                            job.sheet.height,
                            job.path.display()
                        ),
                        Err(e) => {
                            println!("Failed to write print to {}: {}", job.path.display(), e)
                        }
                    }
                }
            })
            .expect("spawn printer PNG worker thread");
        PngWorker { tx: Some(tx), handle: Some(handle) }
    }

    /// Queue a sheet to be encoded and written to `path`. Cheap on the caller —
    /// moves the sheet into the channel; the encode + disk write happen on the
    /// worker.
    pub fn write_sheet(&mut self, path: PathBuf, sheet: PrintSheet) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Job { path, sheet });
        }
    }
}

impl Drop for PngWorker {
    fn drop(&mut self) {
        // Close the channel so the worker drains its queue then exits; join so
        // pending writes complete before the process moves on.
        self.tx = None;
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
