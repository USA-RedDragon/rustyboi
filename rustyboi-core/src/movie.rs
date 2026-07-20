//! Headless TAS / movie / regression core.
//!
//! rustyboi is fully deterministic (no wall-clock, deterministic RTC, per-dot
//! scheduling), so a movie is just the ordered list of per-frame joypad states
//! plus the starting condition (power-on or a savestate). Replaying the same
//! movie against a fresh `GB` reproduces every frame byte-for-byte. This one
//! input-timeline type backs TAS record/replay, the golden-checksum regression
//! harness, the compat-matrix / screenshot generator, scripted rendering tests,
//! and any future frontend movie UI.
//!
//! This module is WASM-clean by construction: no file I/O, no wall-clock, no
//! threads, no env knobs. A `Movie` serializes to/from a compact byte buffer
//! (`to_bytes`/`from_bytes`); the caller owns files. It builds only on the
//! public `GB` surface and never touches emulation internals.

use crate::gb::{Frame, Hardware, GB};
use crate::input::ButtonState;

use serde::{Deserialize, Serialize};

/// How a movie begins: cold power-on (then `skip_bios`), or resumed from a
/// serialized `GB` savestate (the exact bytes `GB::to_state_bytes` yields).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum MovieStart {
    /// Power on, insert the ROM, `skip_bios()`, then feed inputs from frame 0.
    PowerOn,
    /// Resume from a savestate blob (`GB::to_state_bytes` bincode); inputs
    /// continue from the savestate's current frame.
    SaveState(Vec<u8>),
}

/// Descriptive, non-load-bearing movie metadata. None of these affect replay.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct MovieMeta {
    pub author: String,
    pub rom_name: String,
    /// Number of recorded input frames (== `inputs.len()`); stored for readers
    /// that inspect the header without decoding the whole input stream.
    pub frame_count: u32,
    /// Free-form note (emulator version, TAS category, etc.).
    pub note: String,
}

/// A complete, self-describing input timeline. Serializes to a compact,
/// deterministic byte buffer via [`Movie::to_bytes`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Movie {
    /// SHA-256 of the raw ROM bytes the movie was recorded against. The replay
    /// harness compares this against the loaded ROM so a movie can only be
    /// applied to the cartridge it was authored for.
    pub rom_sha256: [u8; 32],
    pub hardware: Hardware,
    pub start: MovieStart,
    pub inputs: Vec<ButtonState>,
    pub meta: MovieMeta,
}

/// Magic + version for the on-disk container. Bumping the version is a breaking
/// change to the byte format.
const MOVIE_MAGIC: &[u8; 4] = b"RBMV";
const MOVIE_VERSION: u8 = 1;

impl Movie {
    /// Serialize to a compact, deterministic byte buffer.
    ///
    /// Layout: `RBMV` magic, version, ROM hash, hardware id, start kind (+ blob
    /// for `SaveState`), the input frames each packed to one byte, then the
    /// metadata as UTF-8 length-prefixed fields. Fully self-contained, no deps.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.inputs.len());
        out.extend_from_slice(MOVIE_MAGIC);
        out.push(MOVIE_VERSION);
        out.extend_from_slice(&self.rom_sha256);
        out.push(hardware_to_id(self.hardware));
        match &self.start {
            MovieStart::PowerOn => out.push(0),
            MovieStart::SaveState(blob) => {
                out.push(1);
                out.extend_from_slice(&(blob.len() as u32).to_le_bytes());
                out.extend_from_slice(blob);
            }
        }
        out.extend_from_slice(&(self.inputs.len() as u32).to_le_bytes());
        for input in &self.inputs {
            out.push(pack_buttons(input));
        }
        write_str(&mut out, &self.meta.author);
        write_str(&mut out, &self.meta.rom_name);
        out.extend_from_slice(&self.meta.frame_count.to_le_bytes());
        write_str(&mut out, &self.meta.note);
        out
    }

    /// Decode a buffer produced by [`Movie::to_bytes`]. Returns `Err` on a bad
    /// magic, unknown version, or truncation.
    pub fn from_bytes(bytes: &[u8]) -> Result<Movie, MovieError> {
        let mut r = Reader { bytes, pos: 0 };
        if r.take(4)? != MOVIE_MAGIC {
            return Err(MovieError::BadMagic);
        }
        let version = r.u8()?;
        if version != MOVIE_VERSION {
            return Err(MovieError::Version(version));
        }
        let mut rom_sha256 = [0u8; 32];
        rom_sha256.copy_from_slice(r.take(32)?);
        let hardware = hardware_from_id(r.u8()?)?;
        let start = match r.u8()? {
            0 => MovieStart::PowerOn,
            1 => {
                let len = r.u32()? as usize;
                MovieStart::SaveState(r.take(len)?.to_vec())
            }
            k => return Err(MovieError::StartKind(k)),
        };
        let n = r.u32()? as usize;
        let mut inputs = Vec::with_capacity(n);
        for _ in 0..n {
            inputs.push(unpack_buttons(r.u8()?));
        }
        let author = r.string()?;
        let rom_name = r.string()?;
        let frame_count = r.u32()?;
        let note = r.string()?;
        Ok(Movie {
            rom_sha256,
            hardware,
            start,
            inputs,
            meta: MovieMeta { author, rom_name, frame_count, note },
        })
    }
}

/// Errors from decoding a movie byte buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MovieError {
    BadMagic,
    Version(u8),
    StartKind(u8),
    Hardware(u8),
    Truncated,
}

impl core::fmt::Display for MovieError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            MovieError::BadMagic => write!(f, "not a rustyboi movie (bad magic)"),
            MovieError::Version(v) => write!(f, "unsupported movie version {v}"),
            MovieError::StartKind(k) => write!(f, "unknown movie start kind {k}"),
            MovieError::Hardware(h) => write!(f, "unknown hardware id {h}"),
            MovieError::Truncated => write!(f, "movie buffer truncated"),
        }
    }
}

impl std::error::Error for MovieError {}

/// Records a movie by wrapping a live `GB`: each `advance` stamps the current
/// frame's input and steps exactly one frame, so the recorded `inputs[i]` is
/// the button state that was live while frame `i` was produced.
pub struct Recorder<'a> {
    gb: &'a mut GB,
    inputs: Vec<ButtonState>,
    rom_sha256: [u8; 32],
    hardware: Hardware,
    start: MovieStart,
    meta: MovieMeta,
}

impl<'a> Recorder<'a> {
    /// Begin recording a power-on movie. `rom_sha256` is the SHA-256 of the raw
    /// ROM bytes (see [`sha256`]); the caller is responsible for having already
    /// inserted that ROM and called `skip_bios()`.
    pub fn new(gb: &'a mut GB, rom_sha256: [u8; 32], hardware: Hardware) -> Self {
        Recorder {
            gb,
            inputs: Vec::new(),
            rom_sha256,
            hardware,
            start: MovieStart::PowerOn,
            meta: MovieMeta::default(),
        }
    }

    /// Begin recording from a savestate blob (the exact bytes of a serialized
    /// `GB`). The blob is stored so `replay` can reconstruct the exact starting
    /// machine; the caller must pass a `gb` already at that state.
    pub fn from_savestate(
        gb: &'a mut GB,
        rom_sha256: [u8; 32],
        hardware: Hardware,
        savestate: Vec<u8>,
    ) -> Self {
        Recorder {
            gb,
            inputs: Vec::new(),
            rom_sha256,
            hardware,
            start: MovieStart::SaveState(savestate),
            meta: MovieMeta::default(),
        }
    }

    /// Attach metadata (author / note / rom_name). `frame_count` is filled in
    /// automatically by [`Recorder::finish`].
    pub fn with_meta(mut self, meta: MovieMeta) -> Self {
        self.meta = meta;
        self
    }

    /// Stamp `input` as this frame's joypad state and advance exactly one frame.
    /// Returns the produced frame's stable hash so a caller can build a
    /// per-frame golden trace while recording.
    pub fn set_input(&mut self, input: ButtonState) -> u64 {
        self.gb.set_input_state(input);
        self.inputs.push(input);
        let (frame, _breakpoint) = self.gb.run_until_frame(false);
        frame_hash(self.gb, &frame)
    }

    /// Frames recorded so far.
    pub fn frame_count(&self) -> usize {
        self.inputs.len()
    }

    /// Finalize into a `Movie`, stamping `frame_count`.
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

/// Outcome of replaying a movie.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayResult {
    /// Stable hash of the final produced frame; the golden-regression key.
    pub(crate) final_frame_hash: u64,
    /// Per-frame hashes (`inputs.len()` entries) when requested, else empty.
    pub frame_hashes: Vec<u64>,
    /// True if the final frame is non-blank (rendered something).
    pub boot_ok: bool,
    /// Number of frames actually stepped (== movie input length).
    pub frames: usize,
}

/// Replay a movie's inputs frame-by-frame against `gb`, which the caller must
/// have already brought to the movie's start condition (ROM inserted; for a
/// `PowerOn` movie, `skip_bios()` called; for a `SaveState` movie, deserialized
/// from `movie.start`'s blob). Feeds `inputs[i]`, then steps one frame, for
/// every recorded frame.
///
/// `collect_frame_hashes` controls whether the per-frame trace is populated
/// (off keeps replay allocation-free beyond the single final frame).
pub fn replay(movie: &Movie, gb: &mut GB, collect_frame_hashes: bool) -> ReplayResult {
    let mut frame_hashes = if collect_frame_hashes {
        Vec::with_capacity(movie.inputs.len())
    } else {
        Vec::new()
    };
    let mut final_frame_hash = 0u64;
    let mut boot_ok = false;
    for input in &movie.inputs {
        gb.set_input_state(*input);
        let (frame, _breakpoint) = gb.run_until_frame(false);
        final_frame_hash = frame_hash(gb, &frame);
        boot_ok = frame_is_non_blank(gb, &frame);
        if collect_frame_hashes {
            frame_hashes.push(final_frame_hash);
        }
    }
    ReplayResult {
        final_frame_hash,
        frame_hashes,
        boot_ok,
        frames: movie.inputs.len(),
    }
}

/// Deterministic 64-bit hash of a frame's raw bytes (FNV-1a). Stable across
/// runs and machines; the golden-regression and per-frame trace key. Includes a
/// 1-byte tag so a monochrome and a color frame of coincidentally-equal bytes
/// never collide.
/// Hashes the *canonical* frame — colour models by their RGB (tag 1), monochrome
/// models by their shade indices (tag 0, via [`GB::dmg_shade_frame`]) — NOT the
/// presented RGB. So the hash is independent of the DMG palette / colour
/// correction, and recorded movies stay valid across presentation changes (a
/// green vs grey DMG frame is the same emulation).
pub fn frame_hash(gb: &GB, _frame: &Frame) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    let mut fold = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    };
    if gb.frame_renders_color() {
        fold(1);
        for &b in _frame.rgb().iter() {
            fold(b);
        }
    } else {
        fold(0);
        for &b in gb.dmg_shade_frame().iter() {
            fold(b);
        }
    }
    h
}

/// True if the frame rendered anything (any non-zero pixel), in the canonical
/// domain — colour RGB or mono shade indices — so it matches [`frame_hash`].
pub fn frame_is_non_blank(gb: &GB, frame: &Frame) -> bool {
    if gb.frame_renders_color() {
        frame.rgb().iter().any(|&b| b != 0)
    } else {
        gb.dmg_shade_frame().iter().any(|&b| b != 0)
    }
}

// ---------------------------------------------------------------------------
// Scripted rendering-test DSL
// ---------------------------------------------------------------------------

/// `cfg(test)`: no production consumer, but the CGB boot-palette-combo and
/// determinism tests below are built on it — deleting it would take that
/// coverage with it. Make it unconditional if a bin ever needs the DSL.
#[cfg(test)]
/// A fluent builder for scripted, deterministic rendering scenarios: pick
/// hardware, insert a ROM, optionally hold buttons across `skip_bios` (to drive
/// the CGB boot palette combo), then run N frames and inspect the final frame.
///
/// ```ignore
/// let frame = Scenario::new(Hardware::CGB)
///     .insert(cartridge)
///     .hold(ButtonState { up: true, a: true, ..Default::default() })
///     .skip_bios()
///     .run_frames(1)
///     .frame();
/// ```
pub(crate) struct Scenario {
    gb: GB,
    held: ButtonState,
}

#[cfg(test)]
impl Scenario {
    #[allow(dead_code)] // no in-tree caller; `pub` was masking dead_code. Unwired-peripheral and
    // unfinished-feature code lives here — check the feature roadmap before deleting.
    /// Start a scenario on the given hardware (no ROM inserted yet).
    pub fn new(hardware: Hardware) -> Self {
        Scenario { gb: GB::new(hardware), held: ButtonState::default() }
    }

    #[allow(dead_code)] // no in-tree caller; `pub` was masking dead_code. Unwired-peripheral and
    // unfinished-feature code lives here — check the feature roadmap before deleting.
    /// Insert a cartridge.
    pub fn insert(mut self, cartridge: crate::cartridge::Cartridge) -> Self {
        self.gb.insert(cartridge);
        self
    }

    #[allow(dead_code)] // no in-tree caller; `pub` was masking dead_code. Unwired-peripheral and
    // unfinished-feature code lives here — check the feature roadmap before deleting.
    /// Hold a button combo. Applied to the machine immediately, so if called
    /// before [`Scenario::skip_bios`] the buttons are "held during boot" and
    /// drive the CGB compatibility-palette combo override.
    pub fn hold(mut self, state: ButtonState) -> Self {
        self.held = state;
        self.gb.set_input_state(state);
        self
    }

    #[allow(dead_code)] // no in-tree caller; `pub` was masking dead_code. Unwired-peripheral and
    // unfinished-feature code lives here — check the feature roadmap before deleting.
    /// Run the synthetic post-boot seed. Buttons set via [`Scenario::hold`]
    /// beforehand act as a boot-time combo (CGB palette override).
    pub fn skip_bios(mut self) -> Self {
        self.gb.skip_bios();
        // Re-assert the held buttons so they remain pressed into the run.
        self.gb.set_input_state(self.held);
        self
    }

    #[allow(dead_code)] // no in-tree caller; `pub` was masking dead_code. Unwired-peripheral and
    // unfinished-feature code lives here — check the feature roadmap before deleting.
    /// Advance exactly `n` frames with the currently-held input.
    pub(crate) fn run_frames(mut self, n: usize) -> Self {
        for _ in 0..n {
            self.gb.set_input_state(self.held);
            self.gb.run_until_frame(false);
        }
        self
    }

    #[allow(dead_code)] // no in-tree caller; `pub` was masking dead_code. Unwired-peripheral and
    // unfinished-feature code lives here — check the feature roadmap before deleting.
    /// The current frame (for assertions).
    pub fn frame(&mut self) -> Frame {
        self.gb.get_current_frame()
    }

    #[allow(dead_code)] // no in-tree caller; `pub` was masking dead_code. Unwired-peripheral and
    // unfinished-feature code lives here — check the feature roadmap before deleting.
    /// The current frame's canonical hash (colour by RGB, mono by shade index).
    pub fn frame_hash(&mut self) -> u64 {
        let f = self.gb.get_current_frame();
        frame_hash(&self.gb, &f)
    }

    #[allow(dead_code)] // no in-tree caller; `pub` was masking dead_code. Unwired-peripheral and
    // unfinished-feature code lives here — check the feature roadmap before deleting.
    /// Borrow the underlying `GB` (palette/memory accessors for assertions).
    pub fn gb(&self) -> &GB {
        &self.gb
    }

    #[allow(dead_code)] // no in-tree caller; `pub` was masking dead_code. Unwired-peripheral and
    // unfinished-feature code lives here — check the feature roadmap before deleting.
    /// Mutably borrow the underlying `GB`.
    pub fn gb_mut(&mut self) -> &mut GB {
        &mut self.gb
    }
}

// ---------------------------------------------------------------------------
// Compact button packing
// ---------------------------------------------------------------------------

fn pack_buttons(s: &ButtonState) -> u8 {
    (s.a as u8)
        | (s.b as u8) << 1
        | (s.select as u8) << 2
        | (s.start as u8) << 3
        | (s.right as u8) << 4
        | (s.left as u8) << 5
        | (s.up as u8) << 6
        | (s.down as u8) << 7
}

fn unpack_buttons(b: u8) -> ButtonState {
    ButtonState {
        a: b & 1 != 0,
        b: b & 2 != 0,
        select: b & 4 != 0,
        start: b & 8 != 0,
        right: b & 0x10 != 0,
        left: b & 0x20 != 0,
        up: b & 0x40 != 0,
        down: b & 0x80 != 0,
    }
}

// ---------------------------------------------------------------------------
// Hardware <-> stable byte id (independent of enum ordering)
// ---------------------------------------------------------------------------

fn hardware_to_id(h: Hardware) -> u8 {
    match h {
        Hardware::DMG => 0,
        Hardware::DMG0 => 1,
        Hardware::MGB => 2,
        Hardware::SGB => 3,
        Hardware::SGB2 => 4,
        Hardware::CGB0 => 5,
        Hardware::CGBB => 6,
        Hardware::CGB => 7,
        Hardware::CGBE => 8,
        Hardware::AGB => 9,
    }
}

fn hardware_from_id(id: u8) -> Result<Hardware, MovieError> {
    Ok(match id {
        0 => Hardware::DMG,
        1 => Hardware::DMG0,
        2 => Hardware::MGB,
        3 => Hardware::SGB,
        4 => Hardware::SGB2,
        5 => Hardware::CGB0,
        6 => Hardware::CGBB,
        7 => Hardware::CGB,
        8 => Hardware::CGBE,
        9 => Hardware::AGB,
        _ => return Err(MovieError::Hardware(id)),
    })
}

// ---------------------------------------------------------------------------
// Byte-buffer read/write helpers
// ---------------------------------------------------------------------------

fn write_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], MovieError> {
        let end = self.pos.checked_add(n).ok_or(MovieError::Truncated)?;
        let slice = self.bytes.get(self.pos..end).ok_or(MovieError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }
    fn u8(&mut self) -> Result<u8, MovieError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, MovieError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn string(&mut self) -> Result<String, MovieError> {
        let len = self.u32()? as usize;
        let bytes = self.take(len)?;
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }
}

// ---------------------------------------------------------------------------
// SHA-256 (self-contained, WASM-clean, no deps) — for the ROM identity hash
// ---------------------------------------------------------------------------

/// SHA-256 of an arbitrary byte slice. Used to bind a movie to the exact ROM it
/// was recorded against. Pure, deterministic, dependency-free.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1,
        0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
        0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
        0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c,
        0x1f83d9ab, 0x5be0cd19,
    ];

    // Pre-processing: pad to a multiple of 64 bytes.
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    let mut w = [0u32; 64];
    for chunk in msg.chunks_exact(64) {
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ ((!v[4]) & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v[7] = v[6];
            v[6] = v[5];
            v[5] = v[4];
            v[4] = v[3].wrapping_add(t1);
            v[3] = v[2];
            v[2] = v[1];
            v[1] = v[0];
            v[0] = t1.wrapping_add(t2);
        }
        for (hi, vi) in h.iter_mut().zip(v.iter()) {
            *hi = hi.wrapping_add(*vi);
        }
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cartridge::Cartridge;

    /// A minimal synthetic DMG ROM that endlessly toggles BGP so the frame
    /// output changes every few frames (proves determinism actually exercises
    /// rendering, not a static blank frame). Header is filled so the cartridge
    /// loader accepts it.
    fn test_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        // Entry at 0x100: jp 0x0150
        rom[0x100] = 0xC3;
        rom[0x101] = 0x50;
        rom[0x102] = 0x01;
        // A tiny loop at 0x150 that increments A and writes it to BGP (FF47),
        // then loops forever. This mutates the palette so successive frames
        // differ, giving the frame hash something to bite on.
        let prog: &[u8] = &[
            0x3E, 0x00, //        LD A, 0x00        (0x150)
            0xE0, 0x47, //        LDH (0x47), A     ; BGP = A
            0x3C, //              INC A
            0xC3, 0x54, 0x01, //  JP 0x0154         ; loop back to INC A
        ];
        rom[0x150..0x150 + prog.len()].copy_from_slice(prog);
        // Cartridge-type ROM-only, sane sizes, valid header checksum.
        rom[0x147] = 0x00; // ROM ONLY
        rom[0x148] = 0x00; // 32 KiB
        rom[0x149] = 0x00; // no RAM
        let mut checksum: u8 = 0;
        for &b in &rom[0x134..0x14D] {
            checksum = checksum.wrapping_sub(b).wrapping_sub(1);
        }
        rom[0x14D] = checksum;
        rom
    }

    fn fresh_gb(rom: &[u8]) -> GB {
        let cart = Cartridge::from_bytes(rom).expect("load test ROM");
        let mut gb = GB::new(Hardware::DMG);
        gb.insert(cart);
        gb.skip_bios();
        gb
    }

    #[test]
    fn sha256_known_answer() {
        // NIST vectors.
        assert_eq!(
            sha256(b""),
            hex("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        assert_eq!(
            sha256(b"abc"),
            hex("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        assert_eq!(
            sha256(b"The quick brown fox jumps over the lazy dog"),
            hex("d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592")
        );
    }

    fn hex(s: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }

    #[test]
    fn movie_round_trip_is_stable() {
        let movie = Movie {
            rom_sha256: sha256(b"rom-bytes"),
            hardware: Hardware::CGB,
            start: MovieStart::SaveState(vec![1, 2, 3, 4, 5]),
            inputs: vec![
                ButtonState { a: true, ..Default::default() },
                ButtonState { start: true, down: true, ..Default::default() },
                ButtonState::default(),
            ],
            meta: MovieMeta {
                author: "rustyboi".into(),
                rom_name: "test".into(),
                frame_count: 3,
                note: "round-trip".into(),
            },
        };
        let bytes = movie.to_bytes();
        let decoded = Movie::from_bytes(&bytes).expect("decode");
        assert_eq!(movie, decoded);
        // Re-encoding is byte-identical (deterministic format).
        assert_eq!(bytes, decoded.to_bytes());
    }

    #[test]
    fn from_bytes_rejects_garbage() {
        assert_eq!(Movie::from_bytes(b"nope").unwrap_err(), MovieError::BadMagic);
        assert_eq!(Movie::from_bytes(&[]).unwrap_err(), MovieError::Truncated);
    }

    #[test]
    fn button_packing_round_trips_all_states() {
        for bits in 0u16..=255 {
            let b = bits as u8;
            assert_eq!(pack_buttons(&unpack_buttons(b)), b);
        }
    }

    /// The core determinism guarantee: record a short movie, then replay it
    /// TWICE from a fresh GB. Both replays must yield the identical final hash
    /// and per-frame trace, and must match the hashes observed while recording.
    #[test]
    fn record_then_replay_twice_is_bit_identical() {
        let rom = test_rom();
        let hash = sha256(&rom);

        // Record a short scripted movie, capturing per-frame hashes live.
        let mut rec_gb = fresh_gb(&rom);
        let mut recorder = Recorder::new(&mut rec_gb, hash, Hardware::DMG);
        let script = [
            ButtonState::default(),
            ButtonState { a: true, ..Default::default() },
            ButtonState { a: true, right: true, ..Default::default() },
            ButtonState { start: true, ..Default::default() },
            ButtonState::default(),
            ButtonState { b: true, up: true, ..Default::default() },
        ];
        let mut recorded_hashes = Vec::new();
        for input in script {
            recorded_hashes.push(recorder.set_input(input));
        }
        let movie = recorder.finish();
        assert_eq!(movie.inputs.len(), script.len());
        assert_eq!(movie.meta.frame_count, script.len() as u32);

        // Replay #1 from a fresh machine.
        let mut gb1 = fresh_gb(&rom);
        let r1 = replay(&movie, &mut gb1, true);
        // Replay #2 from another fresh machine.
        let mut gb2 = fresh_gb(&rom);
        let r2 = replay(&movie, &mut gb2, true);

        assert_eq!(r1, r2, "two replays must be bit-identical");
        assert_eq!(r1.frame_hashes, recorded_hashes, "replay must match recording");
        assert_eq!(r1.final_frame_hash, *recorded_hashes.last().unwrap());
        assert_eq!(r1.frames, script.len());

        // Round-trip the movie through bytes and confirm the replay is unchanged.
        let movie2 = Movie::from_bytes(&movie.to_bytes()).unwrap();
        let mut gb3 = fresh_gb(&rom);
        let r3 = replay(&movie2, &mut gb3, true);
        assert_eq!(r1, r3, "serialized movie must replay identically");
    }

    /// Concrete proof of the user's example: a DMG game (no CGB flag) booted on
    /// CGB hardware is colorized by the CGB boot ROM's compatibility palette,
    /// and a d-pad direction held at `skip_bios()` time overrides the automatic
    /// choice with a boot-combo palette. Holding Up (boot-combo byte 0x40)
    /// selects palette ID 0x12, whose BG palette differs from the default
    /// (0x7C) an unrecognized title otherwise gets. We read the installed BG
    /// palette via the same accessor `--validate-bios` uses (`bg_palette_pair`,
    /// which ignores the DMG-cart CGB-features bus gate).
    #[test]
    fn cgb_boot_combo_selects_compat_palette() {
        use crate::cgb_compat_palette::{key_combo_palette_id, palettes_for_id, select_palette_id};

        let rom = test_rom(); // unrecognized title "" -> default scheme.
        let cart = || Cartridge::from_bytes(&rom).unwrap();

        // Reference palettes straight from the boot-ROM tables.
        let default_id = select_palette_id(&[0u8; 16], 0x00, *b"\0\0");
        assert_eq!(default_id, 0x7C, "unrecognized title must fall back to default");
        let up_id = key_combo_palette_id(0x40).expect("Up is a recognized boot combo");
        assert_eq!(up_id, 0x12);
        let default_bg = palettes_for_id(default_id).bg;
        let up_bg = palettes_for_id(up_id).bg;
        assert_ne!(default_bg, up_bg, "the combo must change the BG palette");

        // Reads a full BG palette 0 (4 RGB555 pairs) as 8 little-endian bytes.
        let read_bg0 = |sc: &Scenario| -> [u8; 8] {
            let mut out = [0u8; 8];
            for c in 0..4u8 {
                let pair = sc.gb().bg_palette_pair(0, c);
                out[c as usize * 2] = (pair & 0xFF) as u8;
                out[c as usize * 2 + 1] = (pair >> 8) as u8;
            }
            out
        };

        // No buttons held: the DMG cart gets the automatic default scheme.
        let auto = Scenario::new(Hardware::CGB).insert(cart()).skip_bios();
        assert_eq!(read_bg0(&auto), default_bg, "auto path installs the default palette");

        // Up held across skip_bios: the boot-combo override installs ID 0x12.
        let combo = Scenario::new(Hardware::CGB)
            .insert(cart())
            .hold(ButtonState { up: true, ..Default::default() })
            .skip_bios();
        let combo_bg = read_bg0(&combo);
        assert_eq!(combo_bg, up_bg, "combo path installs the Up-combo palette");
        assert_ne!(combo_bg, default_bg, "combo palette differs from the default");
    }

    /// The Scenario DSL must be deterministic and drive real frames.
    #[test]
    fn scenario_runs_frames_deterministically() {
        let rom = test_rom();
        let cart = || Cartridge::from_bytes(&rom).unwrap();
        let h1 = Scenario::new(Hardware::DMG).insert(cart()).skip_bios().run_frames(10).frame_hash();
        let h2 = Scenario::new(Hardware::DMG).insert(cart()).skip_bios().run_frames(10).frame_hash();
        assert_eq!(h1, h2);
    }
}
