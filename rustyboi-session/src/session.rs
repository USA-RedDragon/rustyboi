//! The `Session`: frontend-agnostic feature logic wrapping a `GB`.
//!
//! Owns the emulator, the persisted [`Config`], the input map, cheats, rewind
//! history, and TAS state, and exposes one drive method — [`Session::run_frame`]
//! — plus savestate/rewind/TAS/config/cheat operations. All host I/O goes
//! through the boxed service ports; video+audio come back as return values.
//! No wall clock, no filesystem, no threads: WASM-clean.

use crate::audio::{CaptureSink, SampleBuf};
use crate::cheats::{Cheat, CheatError, CheatSet};
use crate::config::Config;
use crate::input::AbstractInput;
use crate::ports::{Rumble, Storage, StorageError, Webcam, WEBCAM_PIXELS};
use crate::rewind::RewindBuffer;
use crate::tas::{Playback, Recording};

use rustyboi_core_lib::gb::{Frame, Hardware, GB};
use rustyboi_core_lib::input::ButtonState;
use rustyboi_core_lib::movie::{self, Movie};

use std::cell::RefCell;
use std::rc::Rc;

/// How the emulator advances each `run_frame` call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunMode {
    /// One GB frame per call.
    #[default]
    Normal,
    /// Run `factor` GB frames, return only the last (clamped ≥ 1). Audio from
    /// all sub-frames is concatenated so sound keeps up while fast-forwarding.
    FastForward(u32),
    /// Advance nothing; re-present the current frame.
    Paused,
    /// Run exactly one GB frame, then switch to `Paused`.
    FrameAdvance,
}

/// What a `run_frame` call produced: the frame to present, the audio samples
/// generated during it, and the frame index. `advanced` is false when the mode
/// ran no frames (paused) so the adapter can skip redundant work.
pub struct FrameOutput {
    pub frame: Frame,
    pub audio: Vec<(f32, f32)>,
    pub frame_count: u64,
    pub advanced: bool,
}

/// Metadata stored alongside a savestate slot. `timestamp` is supplied by the
/// caller (the session never reads a clock); 0 means "unknown".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotMeta {
    pub frame_count: u64,
    pub timestamp: u64,
}

/// Errors from session operations that reach a port or need machine state.
#[derive(Debug)]
pub enum SessionError {
    /// A storage read/write failed.
    Storage(StorageError),
    /// Serializing / deserializing the machine state failed.
    State(String),
    /// Requested slot has no saved state.
    NoState,
    /// The recorded/played movie was authored against a different ROM.
    RomMismatch,
    /// Operation needs a cartridge but none is inserted.
    NoCartridge,
}

impl From<StorageError> for SessionError {
    fn from(e: StorageError) -> Self {
        SessionError::Storage(e)
    }
}

impl core::fmt::Display for SessionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SessionError::Storage(e) => write!(f, "{e}"),
            SessionError::State(e) => write!(f, "state error: {e}"),
            SessionError::NoState => write!(f, "no saved state in slot"),
            SessionError::RomMismatch => write!(f, "movie ROM does not match loaded ROM"),
            SessionError::NoCartridge => write!(f, "no cartridge inserted"),
        }
    }
}

impl std::error::Error for SessionError {}

/// The service ports a session drives. Boxed trait objects (see the crate docs
/// for the generics-vs-objects rationale): the ports are cold-path, and one
/// concrete non-generic `Session` type is far friendlier to the C-facing
/// libretro/JNI wrappers than a monomorphized `Session<S,R,W>`.
pub struct Ports {
    pub storage: Box<dyn Storage>,
    pub rumble: Box<dyn Rumble>,
    pub webcam: Box<dyn Webcam>,
}

/// The frontend-agnostic emulator session.
pub struct Session {
    gb: GB,
    config: Config,
    ports: Ports,
    cheats: CheatSet,

    /// Stable ROM identity (SHA-256 of the raw ROM), for keying savestates and
    /// binding movies. Set at construction.
    rom_id: [u8; 32],

    mode: RunMode,
    frame_count: u64,

    rewind: RewindBuffer,
    recording: Option<Recording>,
    playback: Option<Playback>,

    /// Shared audio capture buffer; the installed `CaptureSink` writes here and
    /// `run_frame` drains it.
    audio_buf: SampleBuf,
}

impl Session {
    /// Build a session around a freshly-constructed `GB` for `hardware`, an
    /// inserted-and-booted machine is the caller's job *before* this if they
    /// want a specific cartridge — but the common path is [`Session::with_gb`].
    ///
    /// `rom_id` is the SHA-256 of the raw ROM bytes (see
    /// [`rustyboi_core_lib::movie::sha256`]); it keys savestate slots and binds
    /// movies. Pass all-zero for a no-cartridge session.
    pub fn new(config: Config, ports: Ports, rom_id: [u8; 32]) -> Session {
        let gb = GB::new(config.hardware);
        Self::with_gb(gb, config, ports, rom_id)
    }

    /// Build a session around an already-prepared `GB` (ROM inserted, BIOS
    /// skipped, at whatever state the caller wants frame 0 to be). Installs the
    /// audio capture sink and applies any Game Genie ROM patches from cheats
    /// (none yet at construction, but keeps the invariant).
    pub fn with_gb(mut gb: GB, config: Config, ports: Ports, rom_id: [u8; 32]) -> Session {
        let audio_buf: SampleBuf = Rc::new(RefCell::new(Vec::new()));
        // enable_audio only errors if a sink was already installed or start()
        // fails; our CaptureSink::start is infallible and gb is fresh here.
        let _ = gb.enable_audio(Box::new(CaptureSink::new(audio_buf.clone())));
        let rewind = RewindBuffer::new(config.rewind.depth, config.rewind.interval_frames);
        Session {
            gb,
            config,
            ports,
            cheats: CheatSet::new(),
            rom_id,
            mode: RunMode::Normal,
            frame_count: 0,
            rewind,
            recording: None,
            playback: None,
            audio_buf,
        }
    }

    // --- run loop -----------------------------------------------------------

    /// Advance the machine per the current [`RunMode`] and return the frame +
    /// audio. `raw` is the host's abstract input for this frame; it is resolved
    /// through the config's remap. During movie playback the recorded input
    /// overrides `raw`.
    pub fn run_frame(&mut self, raw: AbstractInput) -> FrameOutput {
        let live_state = self.config.input_map.resolve(raw);
        self.audio_buf.borrow_mut().clear();

        let (frame, advanced) = match self.mode {
            RunMode::Paused => (self.gb.get_current_frame(), false),
            RunMode::Normal | RunMode::FrameAdvance => {
                let f = self.step_one(live_state);
                if self.mode == RunMode::FrameAdvance {
                    self.mode = RunMode::Paused;
                }
                (f, true)
            }
            RunMode::FastForward(factor) => {
                let n = factor.max(1);
                let mut last = None;
                for _ in 0..n {
                    last = Some(self.step_one(live_state));
                }
                (last.unwrap_or_else(|| self.gb.get_current_frame()), true)
            }
        };

        let audio = self.audio_buf.borrow_mut().drain(..).collect();
        FrameOutput { frame, audio, frame_count: self.frame_count, advanced }
    }

    /// Emulate exactly one frame: pick the input (movie playback overrides
    /// live), pump the webcam/cheats, step the GB, service rumble, record, and
    /// snapshot for rewind.
    fn step_one(&mut self, live_state: ButtonState) -> Frame {
        // Movie playback overrides live input; when it runs out, live resumes.
        let input = match self.playback.as_mut().and_then(|p| p.next_input()) {
            Some(recorded) => recorded,
            None => {
                if self.playback.as_ref().is_some_and(|p| p.finished()) {
                    self.playback = None;
                }
                live_state
            }
        };

        // Feed the Game Boy Camera sensor if the cart wants it and a frame is
        // available (128x112 grayscale).
        if self.gb.cartridge().is_some_and(|c| c.has_camera())
            && let Some(pixels) = self.ports.webcam.grab()
            && pixels.len() == WEBCAM_PIXELS
        {
            let mut buf = [0u8; WEBCAM_PIXELS];
            buf.copy_from_slice(&pixels);
            if let Some(cart) = self.gb.cartridge_mut() {
                cart.set_camera_image(&buf);
            }
        }

        self.gb.set_input_state(input);
        let (frame, _breakpoint) = self.gb.run_until_frame(true);

        // Re-apply GameShark RAM pokes every frame (Game Genie ROM patches are
        // one-shot, applied on insert / cheat change).
        if self.cheats.has_ram_pokes() {
            self.cheats.apply_ram_pokes(&mut self.gb);
        }

        // Drive the rumble motor from the cart's emulated state.
        let rumble_on = self
            .gb
            .cartridge()
            .is_some_and(|c| c.has_rumble() && c.rumble_active());
        self.ports.rumble.set(rumble_on);

        // TAS record: log the input that was live for this frame.
        if let Some(rec) = self.recording.as_mut() {
            rec.push_input(input);
        }

        self.frame_count += 1;

        // Rewind snapshot on the configured cadence.
        if self.config.rewind.enabled && self.rewind.should_capture(self.frame_count)
            && let Ok(state) = self.gb.to_state_bytes()
        {
            self.rewind.push(self.frame_count, state);
        }

        frame
    }

    // --- run mode -----------------------------------------------------------

    pub fn mode(&self) -> RunMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: RunMode) {
        self.mode = mode;
    }

    /// Toggle pause on/off (pause↔normal).
    pub fn toggle_pause(&mut self) {
        self.mode = match self.mode {
            RunMode::Paused => RunMode::Normal,
            _ => RunMode::Paused,
        };
    }

    /// Queue a single-frame advance (runs one frame next `run_frame`, then
    /// pauses).
    pub fn frame_advance(&mut self) {
        self.mode = RunMode::FrameAdvance;
    }

    /// Enter fast-forward at the config's factor.
    pub fn fast_forward(&mut self) {
        self.mode = RunMode::FastForward(self.config.ff_factor());
    }

    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    // --- savestates + slots -------------------------------------------------

    /// Storage key for a numbered slot, namespaced by ROM id so states never
    /// collide across games.
    fn slot_key(&self, slot: u32) -> String {
        let mut hex = String::with_capacity(64);
        for b in self.rom_id {
            hex.push_str(&format!("{b:02x}"));
        }
        format!("state/{hex}/slot{slot}")
    }

    /// Save the current machine state into `slot` via storage. `timestamp` is
    /// caller-supplied wall-clock (the session never reads a clock); it and the
    /// frame count are prepended as an 8+8 byte little-endian header so a load
    /// can surface [`SlotMeta`] without deserializing the whole machine.
    pub fn save_slot(&mut self, slot: u32, timestamp: u64) -> Result<(), SessionError> {
        let state = self.gb.to_state_bytes().map_err(|e| SessionError::State(e.to_string()))?;
        let mut blob = Vec::with_capacity(16 + state.len());
        blob.extend_from_slice(&self.frame_count.to_le_bytes());
        blob.extend_from_slice(&timestamp.to_le_bytes());
        blob.extend_from_slice(&state);
        let key = self.slot_key(slot);
        self.ports.storage.write(&key, &blob)?;
        Ok(())
    }

    /// Load `slot`, replacing the current machine. The audio sink is
    /// re-installed (deserialization produces a fresh `GB` with no sink).
    pub fn load_slot(&mut self, slot: u32) -> Result<SlotMeta, SessionError> {
        let key = self.slot_key(slot);
        let blob = self.ports.storage.read(&key).ok_or(SessionError::NoState)?;
        let (meta, state) = Self::split_slot_blob(&blob)?;
        self.restore_state(state)?;
        self.frame_count = meta.frame_count;
        Ok(meta)
    }

    /// Read a slot's metadata (frame count + timestamp) without loading it.
    pub fn slot_meta(&self, slot: u32) -> Option<SlotMeta> {
        let blob = self.ports.storage.read(&self.slot_key(slot))?;
        Self::split_slot_blob(&blob).ok().map(|(m, _)| m)
    }

    /// All slot numbers with a saved state for the current ROM, ascending.
    pub fn list_slots(&self) -> Vec<u32> {
        let mut hex = String::with_capacity(64);
        for b in self.rom_id {
            hex.push_str(&format!("{b:02x}"));
        }
        let prefix = format!("state/{hex}/slot");
        let mut slots: Vec<u32> = self
            .ports
            .storage
            .list(&prefix)
            .into_iter()
            .filter_map(|k| k.rsplit("slot").next().and_then(|n| n.parse().ok()))
            .collect();
        slots.sort_unstable();
        slots
    }

    /// Quicksave to the reserved quick slot (`u32::MAX`).
    pub fn quicksave(&mut self, timestamp: u64) -> Result<(), SessionError> {
        self.save_slot(QUICK_SLOT, timestamp)
    }

    /// Quickload from the reserved quick slot.
    pub fn quickload(&mut self) -> Result<SlotMeta, SessionError> {
        self.load_slot(QUICK_SLOT)
    }

    fn split_slot_blob(blob: &[u8]) -> Result<(SlotMeta, &[u8]), SessionError> {
        if blob.len() < 16 {
            return Err(SessionError::State("slot blob truncated".into()));
        }
        let frame_count = u64::from_le_bytes(blob[0..8].try_into().unwrap());
        let timestamp = u64::from_le_bytes(blob[8..16].try_into().unwrap());
        Ok((SlotMeta { frame_count, timestamp }, &blob[16..]))
    }

    /// Replace the current `GB` from a raw savestate blob, re-installing the
    /// audio sink and re-applying Game Genie ROM patches.
    fn restore_state(&mut self, state: &[u8]) -> Result<(), SessionError> {
        let mut gb = GB::from_state_bytes(state).map_err(|e| SessionError::State(e.to_string()))?;
        let _ = gb.enable_audio(Box::new(CaptureSink::new(self.audio_buf.clone())));
        self.cheats.apply_rom_patches(&mut gb);
        self.gb = gb;
        Ok(())
    }

    // --- rewind -------------------------------------------------------------

    /// Step back to the most recent rewind snapshot, restoring the machine.
    /// Returns the frame index restored to, or `None` if history is empty.
    pub fn rewind(&mut self) -> Option<u64> {
        let snap = self.rewind.rewind()?;
        if self.restore_state(&snap.state).is_ok() {
            self.frame_count = snap.frame;
            Some(snap.frame)
        } else {
            None
        }
    }

    /// Retained rewind snapshots and their total byte footprint.
    pub fn rewind_stats(&self) -> (usize, usize) {
        (self.rewind.len(), self.rewind.memory_bytes())
    }

    /// Drop rewind history (e.g. on ROM change).
    pub fn clear_rewind(&mut self) {
        self.rewind.clear();
    }

    // --- TAS ----------------------------------------------------------------

    /// Begin recording a power-on movie from the current input timeline. (For a
    /// re-record-from-here recording, use [`Session::start_recording_from_state`].)
    pub fn start_recording(&mut self) {
        self.recording = Some(Recording::power_on(self.rom_id, self.config.hardware));
    }

    /// Begin recording from the current machine state (re-record entry point):
    /// snapshots the live `GB` into the movie's start so replay reconstructs
    /// exactly here.
    pub fn start_recording_from_state(&mut self) -> Result<(), SessionError> {
        let state = self.gb.to_state_bytes().map_err(|e| SessionError::State(e.to_string()))?;
        self.recording =
            Some(Recording::from_savestate(self.rom_id, self.config.hardware, state));
        Ok(())
    }

    /// True while a TAS recording is in progress.
    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// Stop recording and return the finished movie (or `None` if not
    /// recording).
    pub fn stop_recording(&mut self) -> Option<Movie> {
        self.recording.take().map(|r| r.finish())
    }

    /// Begin read-only playback of `movie`. Rewinds the machine to the movie's
    /// start (power-on: fresh boot; savestate: restore the blob) so the
    /// replay is bit-identical to the recording. Fails on a ROM mismatch.
    pub fn play_movie(&mut self, movie: &Movie) -> Result<(), SessionError> {
        if movie.rom_sha256 != self.rom_id {
            return Err(SessionError::RomMismatch);
        }
        match &movie.start {
            movie::MovieStart::PowerOn => self.reboot_for_playback()?,
            movie::MovieStart::SaveState(blob) => self.restore_state(blob)?,
        }
        self.frame_count = 0;
        self.playback = Some(Playback::new(movie));
        Ok(())
    }

    /// True while a movie is playing back.
    pub fn is_playing(&self) -> bool {
        self.playback.is_some()
    }

    /// Stop playback, resuming live input.
    pub fn stop_playback(&mut self) {
        self.playback = None;
    }

    /// Rebuild a fresh, booted machine carrying the same cartridge for a
    /// power-on movie replay. The cartridge is moved out of the current `GB`
    /// into the fresh one (a movie replay always starts from scratch).
    fn reboot_for_playback(&mut self) -> Result<(), SessionError> {
        // A power-on replay needs a genuinely fresh machine, not a state
        // round-trip. Clone the inserted cartridge (`Cartridge: Clone`) into a
        // brand-new booted `GB`; cheat ROM patches are re-applied afterwards.
        let cart = self
            .gb
            .cartridge()
            .ok_or(SessionError::NoCartridge)?
            .clone();
        let mut gb = GB::new(self.config.hardware);
        gb.insert(cart);
        gb.skip_bios();
        let _ = gb.enable_audio(Box::new(CaptureSink::new(self.audio_buf.clone())));
        self.cheats.apply_rom_patches(&mut gb);
        self.gb = gb;
        Ok(())
    }

    // --- config -------------------------------------------------------------

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Apply an updated config: reconfigures the rewind buffer to match (other
    /// fields — hardware, palette, remap, ff factor — take effect on their next
    /// use). Persist separately via [`Session::save_config`].
    pub fn set_config(&mut self, config: Config) {
        self.rewind
            .reconfigure(config.rewind.depth, config.rewind.interval_frames);
        self.config = config;
    }

    /// Persist the current config through storage.
    pub fn save_config(&mut self) -> Result<(), SessionError> {
        self.config.save(self.ports.storage.as_mut())?;
        Ok(())
    }

    // --- cheats -------------------------------------------------------------

    /// Add a Game Genie / GameShark code. Game Genie codes patch the ROM
    /// immediately; GameShark codes take effect on the next frame.
    pub fn add_cheat(&mut self, code: &str) -> Result<Cheat, CheatError> {
        let cheat = self.cheats.add(code)?;
        if matches!(cheat, Cheat::GameGenie { .. }) {
            self.cheats.apply_rom_patches(&mut self.gb);
        }
        Ok(cheat)
    }

    /// Remove a cheat by its raw code string. Game Genie removal takes effect on
    /// the next ROM (re)load (an applied ROM patch cannot be reverted in place).
    pub fn remove_cheat(&mut self, code: &str) -> bool {
        self.cheats.remove(code)
    }

    /// The active cheat codes.
    pub fn cheats(&self) -> impl Iterator<Item = &str> {
        self.cheats.codes()
    }

    // --- access -------------------------------------------------------------

    /// Immutable access to the underlying machine (for adapters that need
    /// palette/memory accessors for presentation).
    pub fn gb(&self) -> &GB {
        &self.gb
    }

    /// The ROM identity hash this session is bound to.
    pub fn rom_id(&self) -> [u8; 32] {
        self.rom_id
    }

    /// The emulated hardware model.
    pub fn hardware(&self) -> Hardware {
        self.config.hardware
    }
}

/// Reserved slot number for quicksave/quickload.
pub const QUICK_SLOT: u32 = u32::MAX;
