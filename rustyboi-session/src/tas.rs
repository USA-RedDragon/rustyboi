//! TAS record / replay, built on `rustyboi_core_lib::movie`.
//!
//! The session drives the `GB` frame-by-frame itself, so instead of holding a
//! `movie::Recorder` (which borrows `&mut GB` for its whole lifetime and would
//! fight the session for the machine) we keep a light [`Recording`]: the input
//! log plus the start condition. `stop()` assembles it into a
//! [`rustyboi_core_lib::movie::Movie`] using the exact same `Movie` type the core
//! replay/determinism harness consumes — so a recorded movie replays
//! bit-identically via `movie::replay`.

use rustyboi_core_lib::gb::Hardware;
use rustyboi_core_lib::input::ButtonState;
use rustyboi_core_lib::movie::{Movie, MovieMeta, MovieStart};

/// An in-progress TAS recording: the ordered per-frame inputs plus how the
/// timeline began (power-on, or resumed from a savestate blob).
#[derive(Clone, Debug)]
pub struct Recording {
    rom_sha256: [u8; 32],
    hardware: Hardware,
    start: MovieStart,
    inputs: Vec<ButtonState>,
    meta: MovieMeta,
}

impl Recording {
    /// Begin a power-on recording (the caller has inserted the ROM and skipped
    /// BIOS; inputs are logged from frame 0).
    pub fn power_on(rom_sha256: [u8; 32], hardware: Hardware) -> Self {
        Recording {
            rom_sha256,
            hardware,
            start: MovieStart::PowerOn,
            inputs: Vec::new(),
            meta: MovieMeta::default(),
        }
    }

    /// Begin a recording that resumes from a savestate blob (re-record from a
    /// point mid-run). The blob is stored so the movie can reconstruct the
    /// exact starting machine.
    pub fn from_savestate(rom_sha256: [u8; 32], hardware: Hardware, savestate: Vec<u8>) -> Self {
        Recording {
            rom_sha256,
            hardware,
            start: MovieStart::SaveState(savestate),
            inputs: Vec::new(),
            meta: MovieMeta::default(),
        }
    }

    /// Attach descriptive metadata (author/note/rom_name). `frame_count` is set
    /// automatically by [`Recording::finish`].
    pub fn with_meta(mut self, meta: MovieMeta) -> Self {
        self.meta = meta;
        self
    }

    /// Log the input that was live for the frame just produced. Called by the
    /// session once per emulated frame while recording.
    pub fn push_input(&mut self, input: ButtonState) {
        self.inputs.push(input);
    }

    /// Frames recorded so far.
    pub fn frame_count(&self) -> usize {
        self.inputs.len()
    }

    /// Finalize into a `Movie` (stamping `frame_count`), ready for
    /// `movie::to_bytes` or `movie::replay`.
    pub fn finish(mut self) -> Movie {
        self.meta.frame_count = self.inputs.len() as u32;
        Movie {
            rom_sha256: self.rom_sha256,
            hardware: self.hardware,
            start: self.start,
            inputs: self.inputs,
            meta: self.meta,
        }
    }
}

/// Read-only movie playback state: feeds recorded inputs back frame-by-frame.
/// The session advances it in lock-step with the emulator; when it runs out,
/// playback is done and live input resumes.
#[derive(Clone, Debug)]
pub struct Playback {
    inputs: Vec<ButtonState>,
    cursor: usize,
}

impl Playback {
    /// Start playing a movie's input timeline. The caller is responsible for
    /// having brought the `GB` to the movie's start condition.
    pub fn new(movie: &Movie) -> Self {
        Playback { inputs: movie.inputs.clone(), cursor: 0 }
    }

    /// The next input to feed, advancing the cursor. `None` once the movie is
    /// exhausted (playback finished).
    pub fn next_input(&mut self) -> Option<ButtonState> {
        let input = self.inputs.get(self.cursor).copied();
        if input.is_some() {
            self.cursor += 1;
        }
        input
    }

    /// True once every recorded frame has been played back.
    pub fn finished(&self) -> bool {
        self.cursor >= self.inputs.len()
    }

    /// Frames played back so far.
    pub fn position(&self) -> usize {
        self.cursor
    }

    /// Total frames in the movie.
    pub fn len(&self) -> usize {
        self.inputs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty()
    }
}
