//! The `Session`: frontend-agnostic feature logic wrapping a `GB`.
//!
//! Owns the emulator, the persisted [`Config`], the input map, cheats, rewind
//! history, and TAS state, and exposes one drive method — [`Session::run_frame`]
//! — plus savestate/rewind/TAS/config/cheat operations. All host I/O goes
//! through the boxed service ports; video+audio come back as return values.
//! No wall clock, no filesystem, no threads: WASM-clean.

use crate::action::{HardwareChoice, DmgPaletteChoice, ScalingMode};
use crate::apply::palette_shades;
use crate::audio::{CaptureSink, SampleBuf};
use crate::cheats::{Cheat, CheatError, CheatSet};
use crate::config::Config;
use crate::input::AbstractInput;
use crate::ports::{Rumble, Storage, StorageError, Webcam, WEBCAM_PIXELS};
use crate::rewind::RewindBuffer;
use crate::tas::{Playback, Recording};

use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Frame, Hardware, GB};
use rustyboi_core_lib::input::ButtonState;
use rustyboi_core_lib::movie::{self, Movie};
use rustyboi_core_lib::printer::PrintSheet;

use std::sync::{Arc, Mutex};

/// Plain Game Boy screen dimensions (pre-scale).
pub const GB_SIZE: (u32, u32) = (160, 144);
/// SGB composited (screen + border) dimensions (pre-scale).
pub const SGB_SIZE: (u32, u32) = (256, 224);

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

/// Scale drained stereo output samples by `gain` (0.0..=1.0). Applied ONLY to
/// the session's output copy — the core/APU are never touched — so hardware
/// suites stay byte-identical. A gain of exactly 1.0 is the identity path (no
/// multiply), keeping the default (volume 100) bit-for-bit unchanged.
fn scale_samples(
    samples: impl Iterator<Item = (f32, f32)>,
    gain: f32,
) -> Vec<(f32, f32)> {
    if gain == 1.0 {
        samples.collect()
    } else {
        samples.map(|(l, r)| (l * gain, r * gain)).collect()
    }
}

/// Down-sample fast-forward output audio by `factor` so it plays at real time
/// (and thus at a raised pitch) instead of piling `factor`× the samples into
/// the output device every presented frame — the backlog that otherwise
/// manifested as choppy (desktop) or doubled/overlapping (web) sound. Each
/// output sample is the mean of a group of `factor` inputs (a cheap box filter
/// that also anti-aliases a little), scaled by `gain`. Applied ONLY to the
/// output copy — the core/APU are untouched, so hardware suites (always Normal
/// mode) stay byte-identical.
fn decimate_samples(samples: &[(f32, f32)], factor: u32, gain: f32) -> Vec<(f32, f32)> {
    let n = factor.max(1) as usize;
    if n == 1 {
        return scale_samples(samples.iter().copied(), gain);
    }
    let inv = gain / n as f32;
    samples
        .chunks_exact(n)
        .map(|chunk| {
            let (mut l, mut r) = (0.0f32, 0.0f32);
            for &(cl, cr) in chunk {
                l += cl;
                r += cr;
            }
            (l * inv, r * inv)
        })
        .collect()
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
    /// A TAS movie file failed to decode.
    Movie(String),
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
            SessionError::Movie(e) => write!(f, "movie decode error: {e}"),
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
    // Boxed so the ~207 KB machine (four inline framebuffers) stays heap-
    // resident: only an 8-byte pointer is ever moved on construction/ownership
    // transfer. Moving it by value overflowed the small Android `android_main`
    // thread stack (SIGSEGV). Hot emulation paths are unaffected — `gb()`/
    // `gb_mut()` still hand out `&GB`/`&mut GB` via deref coercion.
    gb: Box<GB>,
    config: Config,
    ports: Ports,
    cheats: CheatSet,

    /// Stable ROM identity (SHA-256 of the raw ROM), for keying savestates and
    /// binding movies. Set at construction.
    rom_id: [u8; 32],

    /// The unpatched ROM bytes last loaded through [`finish_load_rom`], retained
    /// so [`apply_rom_patch`](Self::apply_rom_patch) always patches the original
    /// (an IPS/UPS/BPS patch is applied to the pristine ROM, never a re-patched
    /// one). `None` until a ROM is loaded from bytes.
    original_rom: Option<Vec<u8>>,

    /// Human-readable name of the loaded game: the canonical No-Intro name if the
    /// ROM is indexed, else its cartridge header title. Drives the window title
    /// and the ROM library. `None` until a ROM is loaded / when unidentifiable.
    game_name: Option<String>,

    /// Cheats downloaded from the libretro cheat DB awaiting the user's pick,
    /// populated by [`finish_fetched_cheats`](Self::finish_fetched_cheats) and
    /// surfaced in [`SessionUiState`](crate::action::SessionUiState). Empty until
    /// a fetch completes; cleared on dismiss or a fresh fetch.
    fetched_cheats: Vec<crate::cheat_db::FetchedCheat>,

    mode: RunMode,
    frame_count: u64,

    rewind: RewindBuffer,
    recording: Option<Recording>,
    playback: Option<Playback>,

    /// When set, `step_one` does NOT serialize the rewind snapshot inline.
    /// Instead a due capture is exposed via [`Session::take_pending_snapshot`]
    /// (a cheap `GB::clone`) so an external owner (the native platform) can run
    /// the expensive `to_state_bytes` on a worker thread and feed the finished
    /// blob back with [`Session::push_rewind_bytes`]. Off by default so the
    /// WASM / library path keeps its self-contained inline capture.
    rewind_offloaded: bool,
    /// A cheap clone snapshot captured this frame in offloaded mode, waiting to
    /// be picked up by the platform worker. `(frame_index, cloned_gb)`. Boxed so
    /// this `Option` does not embed a full ~207 KB `GB` inline in every
    /// `Session` (which bloated `App` by value and overflowed Android's stack).
    pending_snapshot: Option<(u64, Box<GB>)>,

    /// Shared audio capture buffer; the installed `CaptureSink` writes here and
    /// `run_frame` drains it.
    audio_buf: SampleBuf,

    // --- presentation state the shared `apply` owns -------------------------
    /// Whether to present the SGB border composite when one is available.
    sgb_border: bool,
    /// Whether the on-screen touch overlay is shown.
    touch_controls: bool,
    /// The DMG presentation palette choice (the concrete shades live in
    /// `config.dmg_palette`; this is the menu selection they mirror).
    palette: DmgPaletteChoice,
    /// Real boot ROM bytes supplied by the adapter (DMG or CGB), run in place of
    /// `skip_bios` when `config.use_real_boot_rom` is set and this is `Some`.
    /// `None` = no boot ROM available; the session always falls back to
    /// `skip_bios`. The session is WASM-clean, so it never reads a file itself —
    /// the adapter resolves the bytes and installs them via `set_boot_rom`.
    boot_rom: Option<Vec<u8>>,

    // --- debug-step requests set by `apply`, drained by the frontend --------
    pending_step_cycles: Option<u32>,
    pending_step_frames: Option<u32>,

    /// Game Boy Printer strips accumulated for the in-progress photo. The printer
    /// emits one sheet per PRINT command; a photo is usually several contiguous
    /// strips fed out together, so [`take_prints`](Self::take_prints) stitches
    /// them vertically into one long sheet, breaking on the paper-feed margins.
    printer_strips: Vec<PrintSheet>,
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
        let gb = Box::new(GB::new(config.hardware));
        Self::with_gb(gb, config, ports, rom_id)
    }

    /// Build a session around an already-prepared `GB` (ROM inserted, BIOS
    /// skipped, at whatever state the caller wants frame 0 to be). Installs the
    /// audio capture sink and applies any Game Genie ROM patches from cheats
    /// (none yet at construction, but keeps the invariant).
    pub fn with_gb(mut gb: Box<GB>, config: Config, ports: Ports, rom_id: [u8; 32]) -> Session {
        let audio_buf: SampleBuf = Arc::new(Mutex::new(Vec::new()));
        // enable_audio only errors if a sink was already installed or start()
        // fails; our CaptureSink::start is infallible and gb is fresh here.
        let _ = gb.enable_audio(Box::new(CaptureSink::new(audio_buf.clone())));
        // Presentation-only machine settings (CGB colour correction) apply to the
        // caller's already-prepared machine here; every later (re)build funnels
        // through `apply_presentation`.
        gb.set_cgb_color_conversion(config.color_correction);
        gb.set_dmg_palette(config.dmg_palette_choice);
        let rewind = RewindBuffer::new(config.rewind.depth, config.rewind.interval_frames);
        let palette = config.dmg_palette_choice;
        Session {
            gb,
            config,
            ports,
            cheats: CheatSet::new(),
            rom_id,
            original_rom: None,
            game_name: None,
            fetched_cheats: Vec::new(),
            mode: RunMode::Normal,
            frame_count: 0,
            rewind,
            recording: None,
            playback: None,
            rewind_offloaded: false,
            pending_snapshot: None,
            audio_buf,
            sgb_border: true,
            touch_controls: cfg!(mobile),
            palette,
            boot_rom: None,
            pending_step_cycles: None,
            pending_step_frames: None,
            printer_strips: Vec::new(),
        }
    }

    /// Re-apply presentation-only machine settings (currently CGB colour
    /// correction) after a machine (re)build. Called from every sink that
    /// installs a fresh `GB` so the setting survives ROM restarts and state
    /// loads. Presentation-only: it never affects emulation determinism.
    fn apply_presentation(&mut self) {
        self.gb.set_cgb_color_conversion(self.config.color_correction);
        self.gb.set_dmg_palette(self.config.dmg_palette_choice);
    }

    /// Boot a freshly-built machine: run the supplied real boot ROM when the
    /// feature is enabled and bytes are available (and valid for the model),
    /// else seed the synthetic post-boot state. A wrong/mismatched boot ROM
    /// fails validation inside `load_bios_bytes` and falls back to `skip_bios`,
    /// so content always loads.
    fn boot_or_skip(&self, gb: &mut GB) {
        // Force the chosen CGB DMG-compat palette (Auto = None) before booting so
        // the skip_bios colorization path picks it up when a DMG game runs on CGB
        // hardware. No effect on DMG hardware or CGB titles.
        gb.set_forced_compat_palette(self.config.gbc_dmg_palette.forced_id());
        if self.config.use_real_boot_rom
            && let Some(bytes) = self.boot_rom.as_deref()
            && gb.load_bios_bytes(bytes).is_ok()
            && gb.has_bios()
        {
            gb.run_boot_rom();
        } else {
            gb.skip_bios();
        }
    }

    // --- run loop -----------------------------------------------------------

    /// Advance the machine per the current [`RunMode`] and return the frame +
    /// audio. `raw` is the host's abstract input for this frame; it is resolved
    /// through the config's remap. During movie playback the recorded input
    /// overrides `raw`.
    pub fn run_frame(&mut self, raw: AbstractInput) -> FrameOutput {
        let live_state = self.config.input_map.resolve(raw);
        self.audio_buf.lock().unwrap().clear();

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

        // Scale the drained OUTPUT copy by master volume. The core/APU are never
        // touched, so hardware suites (APU register/SRAM checks) stay byte-
        // identical; at volume 100 the gain is exactly 1.0 and we skip the mul.
        // Fast-forward produced `factor`× the samples this frame; resample the
        // output copy back to one real-time frame's worth so it plays cleanly on
        // every platform instead of backing up (uncapped has no fixed ratio, so
        // it's muted).
        let gain = self.config.volume_gain();
        let audio = match self.mode {
            RunMode::FastForward(_) if self.config.ff_uncapped() => Vec::new(),
            RunMode::FastForward(n) => {
                let drained: Vec<(f32, f32)> =
                    self.audio_buf.lock().unwrap().drain(..).collect();
                decimate_samples(&drained, n.max(1), gain)
            }
            _ => scale_samples(self.audio_buf.lock().unwrap().drain(..), gain),
        };
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

        // Rewind snapshot on the configured cadence. In offloaded mode we only
        // take the cheap `GB::clone` here and stash it for the platform worker
        // to serialize; otherwise serialize inline as before.
        if self.config.rewind.enabled && self.rewind.should_capture(self.frame_count) {
            if self.rewind_offloaded {
                // A previous pending snapshot the platform never drained would
                // be overwritten; the platform drains every frame so this is a
                // worst-case single-frame skip, never a leak.
                self.pending_snapshot = Some((self.frame_count, self.gb.clone()));
            } else if let Ok(state) = self.gb.to_state_bytes() {
                self.rewind.push(self.frame_count, crate::rewind::compress_snapshot(state));
            }
        }

        frame
    }

    /// Replace the underlying machine and re-bind the session to a new ROM
    /// identity, keeping the same ports, config, and cheat set. Use this when
    /// the frontend loads a different cartridge (or a raw state whose ROM id it
    /// knows): the audio capture sink is re-installed, Game Genie ROM patches
    /// re-applied, the frame counter reset, rewind history cleared, and any TAS
    /// recording/playback dropped (they were bound to the old ROM).
    ///
    /// `rom_id` should be the SHA-256 of the new ROM (all-zero for none) so
    /// savestate slots re-key to the new game.
    pub fn replace_machine(&mut self, mut gb: GB, rom_id: [u8; 32]) {
        let _ = gb.enable_audio(Box::new(CaptureSink::new(self.audio_buf.clone())));
        self.cheats.apply_rom_patches(&mut gb);
        *self.gb = gb;
        self.rom_id = rom_id;
        self.frame_count = 0;
        self.rewind.clear();
        self.recording = None;
        self.playback = None;
        self.mode = RunMode::Normal;
        self.printer_strips.clear();
        self.apply_presentation();
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
    /// audio sink and re-applying Game Genie ROM patches. The savestate holds the
    /// cartridge's RUNTIME state (RAM/bank regs/RTC) but NOT its ROM image, so the
    /// ROM is re-attached from the currently-live machine (rewind/quickload/movie
    /// always resume the same ROM); without it the restored machine open-buses the
    /// wrong bank and bricks.
    fn restore_state(&mut self, state: &[u8]) -> Result<(), SessionError> {
        let mut gb = GB::from_state_bytes(state).map_err(|e| SessionError::State(e.to_string()))?;
        if gb.cartridge_needs_rom()
            && let Some(rom) = self.gb.detach_rom_bytes()
        {
            gb.reattach_rom(&rom);
        }
        let _ = gb.enable_audio(Box::new(CaptureSink::new(self.audio_buf.clone())));
        self.cheats.apply_rom_patches(&mut gb);
        *self.gb = gb;
        self.apply_presentation();
        Ok(())
    }

    // --- rewind -------------------------------------------------------------

    /// Step back to the most recent rewind snapshot, restoring the machine.
    /// Returns the frame index restored to, or `None` if history is empty.
    pub fn rewind(&mut self) -> Option<u64> {
        let snap = self.rewind.rewind()?;
        // Ring blobs are deflate-framed (see `rewind::compress_snapshot`);
        // unframed raw blobs pass through for hosts that push uncompressed.
        let state = crate::rewind::decompress_snapshot(&snap.state)?;
        if self.restore_state(&state).is_ok() {
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
        self.pending_snapshot = None;
    }

    // --- offloaded rewind capture (native platform worker) ------------------
    //
    // These let a host run the expensive savestate serialization off the
    // emulation thread. The session stays thread-free: it only produces a cheap
    // `GB::clone` snapshot and accepts the finished blob back. The host owns the
    // worker thread and the snapshot->serialize->push handoff.

    /// Switch rewind capture into offloaded mode. When enabled, `run_frame`
    /// stops serializing snapshots inline; instead each due capture is a cheap
    /// `GB::clone` retrievable via [`Session::take_pending_snapshot`]. Disabling
    /// restores the self-contained inline serialize path.
    pub fn set_rewind_offloaded(&mut self, offloaded: bool) {
        self.rewind_offloaded = offloaded;
        if !offloaded {
            self.pending_snapshot = None;
        }
    }

    /// Whether offloaded rewind capture is active.
    pub fn rewind_offloaded(&self) -> bool {
        self.rewind_offloaded
    }

    /// Take the cheap snapshot captured this frame (offloaded mode only), if a
    /// capture was due. The caller serializes the returned `GB` on a worker
    /// (via [`rustyboi_core_lib::gb::GB::to_state_bytes`]) and feeds the result
    /// back with [`Session::push_rewind_bytes`]. `None` when no capture was due
    /// or not in offloaded mode.
    pub fn take_pending_snapshot(&mut self) -> Option<(u64, Box<GB>)> {
        self.pending_snapshot.take()
    }

    /// Feed a serialized rewind blob (produced off-thread from a
    /// [`Session::take_pending_snapshot`] clone) into the rewind ring, applying
    /// the same drop-oldest policy as inline capture. Frames may arrive slightly
    /// out of order relative to live play, but each blob is self-describing
    /// (carries its own frame index) so restore is unaffected.
    ///
    /// Hosts should run the blob through [`crate::rewind::compress_snapshot`]
    /// on their worker (as the inline path does) so the ring stores the compact
    /// framed form; raw uncompressed blobs still restore correctly but forgo
    /// the memory saving.
    pub fn push_rewind_bytes(&mut self, frame: u64, bytes: Vec<u8>) {
        self.rewind.push(frame, bytes);
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
        // A power-on movie was recorded against the synthetic post-boot state;
        // replay it that way regardless of the real-boot-ROM setting so the
        // recorded input stays bit-identical.
        gb.skip_bios();
        let _ = gb.enable_audio(Box::new(CaptureSink::new(self.audio_buf.clone())));
        self.cheats.apply_rom_patches(&mut gb);
        *self.gb = gb;
        self.apply_presentation();
        Ok(())
    }

    // --- config -------------------------------------------------------------

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The loaded game's display name (No-Intro name, else header title), if any.
    pub fn game_name(&self) -> Option<&str> {
        self.game_name.as_deref()
    }

    /// Resolve the display name from raw ROM bytes. For construction paths that
    /// receive a pre-built machine plus the ROM bytes (desktop CLI `--rom`),
    /// where [`load_rom_bytes`](Self::load_rom_bytes) isn't on the path.
    ///
    /// Retains the extracted ROM as `original_rom` too, so a later
    /// [`finish_no_intro_dats`](Self::finish_no_intro_dats) (the DAT index is
    /// still empty at construction time) can re-resolve the No-Intro name, and so
    /// [`apply_rom_patch`](Self::apply_rom_patch) has a pristine image to patch.
    pub fn set_rom_identity(&mut self, rom: &[u8]) {
        let rom = crate::rom_zip::extract_rom(rom);
        self.game_name = crate::no_intro::resolve_game_name(&rom);
        self.original_rom = Some(rom);
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

    // --- presentation state (shared `apply` owns these) ---------------------

    /// Whether the SGB border composite is presented when available.
    pub fn sgb_border(&self) -> bool {
        self.sgb_border
    }

    /// Set whether the SGB border composite is presented.
    pub fn set_sgb_border(&mut self, on: bool) {
        self.sgb_border = on;
    }

    /// Whether the on-screen touch overlay is shown.
    pub fn touch_controls(&self) -> bool {
        self.touch_controls
    }

    /// Set whether the on-screen touch overlay is shown.
    pub fn set_touch_controls(&mut self, on: bool) {
        self.touch_controls = on;
    }

    /// The current DMG presentation palette choice.
    pub fn palette(&self) -> DmgPaletteChoice {
        self.palette
    }

    /// Whether the SGB border is actually being presented this frame (toggle on
    /// AND the machine offers a composite).
    pub fn showing_sgb_border(&self) -> bool {
        self.sgb_border && self.gb.sgb_composited_frame().is_some()
    }

    /// The content size (pre-scale) that should drive the window: the SGB
    /// composite size only when the border is actually shown, else the plain GB
    /// screen.
    pub fn content_size(&self) -> (u32, u32) {
        if self.showing_sgb_border() {
            SGB_SIZE
        } else {
            GB_SIZE
        }
    }

    /// Whether fast-forward is currently engaged.
    pub fn is_fast_forward(&self) -> bool {
        matches!(self.mode, RunMode::FastForward(_))
    }

    /// Whether emulation is paused (the run mode re-presents the current frame).
    /// The web worker drives pause through this; desktop pause is owned by the
    /// frontend `App`, so its session mode stays `Normal`.
    pub fn is_paused(&self) -> bool {
        matches!(self.mode, RunMode::Paused)
    }

    /// Toggle fast-forward on/off (fast-forward ↔ normal).
    pub fn toggle_fast_forward(&mut self) {
        match self.mode {
            RunMode::FastForward(_) => self.mode = RunMode::Normal,
            _ => self.fast_forward(),
        }
    }

    // --- config-mutating actions (persist through storage) ------------------

    /// A menu-choice view of the configured hardware model.
    pub fn hardware_choice(&self) -> HardwareChoice {
        HardwareChoice::from_hardware(self.config.hardware)
    }

    /// Change the emulated hardware model and rebuild the machine for it,
    /// carrying the current cartridge. Persists the config.
    pub fn set_hardware_choice(&mut self, choice: HardwareChoice) {
        self.config.hardware = choice.to_hardware();
        let gb = self.rebuild_current_gb();
        self.replace_machine(*gb, self.rom_id);
        self.persist_config();
    }

    /// Change the DMG presentation palette; persists the config.
    pub fn set_palette_choice(&mut self, choice: DmgPaletteChoice) {
        self.init_palette_choice(choice);
        self.persist_config();
    }

    /// Seed the presentation palette without persisting (startup, from the
    /// CLI/config-derived choice).
    pub fn init_palette_choice(&mut self, choice: DmgPaletteChoice) {
        self.palette = choice;
        self.config.dmg_palette_choice = choice;
        self.config.dmg_palette.shades = palette_shades(choice, self.config.color_correction);
        // The core applies the palette to mono frames now (unified RGB output).
        self.gb.set_dmg_palette(choice);
    }

    /// The CGB colorization scheme for DMG games (Auto / a boot-ROM scheme).
    pub fn gbc_dmg_palette(&self) -> crate::action::GbcDmgPalette {
        self.config.gbc_dmg_palette
    }

    /// Change the CGB colorization scheme for DMG games and rebuild the machine
    /// (the palette is latched at boot); persists the config.
    pub fn set_gbc_dmg_palette(&mut self, choice: crate::action::GbcDmgPalette) {
        self.config.gbc_dmg_palette = choice;
        let gb = self.rebuild_current_gb();
        self.replace_machine(*gb, self.rom_id);
        self.persist_config();
    }

    /// Whether the DMG palette settings apply to the loaded game: false for a
    /// CGB title (it supplies its own colours), so the frontend greys the menu.
    /// True when no cart is loaded (the setting is harmless then).
    pub fn dmg_palette_active(&self) -> bool {
        self.gb.cartridge().is_none_or(|c| !c.supports_cgb())
    }

    /// The current CGB colour-correction curve.
    pub fn color_correction(&self) -> rustyboi_core_lib::ppu::ColorCorrection {
        self.config.color_correction
    }

    /// Set the CGB colour-correction curve (Linear/LCD) live and persist it.
    /// Presentation-only: it changes CGB output bytes but not emulation.
    pub fn set_color_correction(
        &mut self,
        conversion: rustyboi_core_lib::ppu::ColorCorrection,
    ) {
        self.config.color_correction = conversion;
        self.gb.set_cgb_color_conversion(conversion);
        // Correction composes with the DMG base palette, so refresh the cached
        // mono shades (Green/Pocket have distinct raw vs LCD variants).
        self.config.dmg_palette.shades = palette_shades(self.palette, conversion);
        self.persist_config();
    }

    /// Whether the real-boot-ROM feature is enabled.
    pub fn use_real_boot_rom(&self) -> bool {
        self.config.use_real_boot_rom
    }

    /// Enable/disable running a real boot ROM. Rebuilds the machine (a boot mode
    /// change only takes effect from power-on) and persists the config.
    pub fn set_real_boot_rom(&mut self, enabled: bool) {
        self.config.use_real_boot_rom = enabled;
        let gb = self.rebuild_current_gb();
        self.replace_machine(*gb, self.rom_id);
        self.persist_config();
    }

    /// Install real boot ROM bytes (DMG or CGB) the adapter resolved from a file.
    /// Does not itself validate the image — validation happens at boot time in
    /// [`GB::load_bios_bytes`](rustyboi_core_lib::gb::GB::load_bios_bytes), which
    /// falls back to `skip_bios` on a bad/mismatched ROM. Not persisted (the raw
    /// ROM is a resource the adapter re-supplies), unlike the enable flag.
    pub fn set_boot_rom(&mut self, bytes: Option<Vec<u8>>) {
        self.boot_rom = bytes;
    }

    /// Finish a boot-ROM import: store the resolved bytes and, if the feature is
    /// already enabled, rebuild the machine so it boots with the new ROM.
    pub fn finish_load_boot_rom(&mut self, bytes: &[u8]) {
        self.boot_rom = Some(bytes.to_vec());
        if self.config.use_real_boot_rom {
            let gb = self.rebuild_current_gb();
            self.replace_machine(*gb, self.rom_id);
        }
    }

    /// The current upscale texture filter (presentation-only).
    pub fn texture_filter(&self) -> crate::action::TextureFilter {
        self.config.texture_filter
    }

    /// Set the upscale texture filter and persist it (presentation-only; the
    /// renderer reads it each frame).
    pub fn set_texture_filter(&mut self, filter: crate::action::TextureFilter) {
        self.config.texture_filter = filter;
        self.persist_config();
    }

    /// The current LCD post-process effect (presentation-only).
    pub fn lcd_effect(&self) -> crate::action::LcdEffect {
        self.config.lcd_effect
    }

    /// Set the LCD post-process effect and persist it (presentation-only).
    pub fn set_lcd_effect(&mut self, effect: crate::action::LcdEffect) {
        self.config.lcd_effect = effect;
        self.persist_config();
    }

    /// The integer upscale factor applied to saved Game Boy Printer output.
    pub fn printer_scale(&self) -> u8 {
        self.config.printer_scale.max(1)
    }

    /// Set the printer output upscale factor (clamped ≥ 1) and persist it.
    pub fn set_printer_scale(&mut self, scale: u8) {
        self.config.printer_scale = scale.max(1);
        self.persist_config();
    }

    /// The on-screen touch control opacity (0..=100 percent).
    pub fn touch_opacity(&self) -> u8 {
        self.config.touch_opacity.min(100)
    }

    /// Set the on-screen touch control opacity (clamped 0..=100) and persist it.
    pub fn set_touch_opacity(&mut self, opacity: u8) {
        self.config.touch_opacity = opacity.min(100);
        self.persist_config();
    }

    /// Whether the on-screen FPS overlay is shown.
    pub fn show_fps(&self) -> bool {
        self.config.show_fps
    }

    /// Enable/disable the on-screen FPS overlay; persists the config.
    pub fn set_show_fps(&mut self, on: bool) {
        self.config.show_fps = on;
        self.persist_config();
    }

    /// Enable/disable rewind capture; persists the config.
    pub fn set_rewind_enabled(&mut self, enabled: bool) {
        self.config.rewind.enabled = enabled;
        self.rewind
            .reconfigure(self.config.rewind.depth, self.config.rewind.interval_frames);
        self.persist_config();
    }

    /// Set the rewind snapshot interval (frames between captures, ≥ 1);
    /// persists the config.
    pub fn set_rewind_interval(&mut self, interval_frames: u32) {
        self.config.rewind.interval_frames = interval_frames.max(1);
        self.rewind
            .reconfigure(self.config.rewind.depth, self.config.rewind.interval_frames);
        self.persist_config();
    }

    /// Set how many rewind snapshots are retained (≥ 1); persists the config.
    pub fn set_rewind_depth(&mut self, depth: usize) {
        self.config.rewind.depth = depth.max(1);
        self.rewind
            .reconfigure(self.config.rewind.depth, self.config.rewind.interval_frames);
        self.persist_config();
    }

    /// Set the master output volume (clamped 0..=100); persists the config. Only
    /// scales the session's drained audio copy in [`run_frame`](Self::run_frame);
    /// the core/APU are never touched (keeps hardware suites byte-identical).
    pub fn set_volume(&mut self, volume: u8) {
        self.config.volume = volume.min(100);
        self.persist_config();
    }

    /// Current master volume (0..=100).
    pub fn volume(&self) -> u8 {
        self.config.volume.min(100)
    }

    /// Set the fast-forward speed (GB frames per presented frame; `0` = uncapped)
    /// and persist it. If fast-forward is already engaged, re-derive the run mode
    /// so the new speed takes effect immediately.
    pub fn set_fast_forward_factor(&mut self, factor: u32) {
        self.config.fast_forward_factor = factor;
        if matches!(self.mode, RunMode::FastForward(_)) {
            self.mode = RunMode::FastForward(self.config.ff_factor());
        }
        self.persist_config();
    }

    /// Current fast-forward speed setting (`0` = uncapped).
    pub fn fast_forward_factor(&self) -> u32 {
        self.config.fast_forward_factor
    }

    /// Set the frame letterboxing policy; persists the config.
    pub fn set_scaling_mode(&mut self, scaling: ScalingMode) {
        self.config.scaling = scaling;
        self.persist_config();
    }

    /// Current frame letterboxing policy.
    pub fn scaling_mode(&self) -> ScalingMode {
        self.config.scaling
    }

    /// Choose the rendering backend; persists the config. The running window
    /// keeps its current surface/device — the choice applies at the next
    /// launch (see [`crate::action::GraphicsBackend`]).
    pub fn set_graphics_backend(&mut self, backend: crate::action::GraphicsBackend) {
        self.config.graphics_backend = backend;
        self.persist_config();
    }

    /// Currently requested rendering backend.
    pub fn graphics_backend(&self) -> crate::action::GraphicsBackend {
        self.config.graphics_backend
    }

    /// Replace the rebindable input map (GB-button bindings + chord hotkeys);
    /// persists the config. The adapter's next `resolve` call sees the new map.
    pub fn set_input_config(&mut self, input: crate::input_config::InputConfig) {
        self.config.input = input;
        self.persist_config();
    }

    /// The current rebindable input map.
    pub fn input_config(&self) -> &crate::input_config::InputConfig {
        &self.config.input
    }

    fn persist_config(&mut self) {
        if let Err(e) = self.save_config() {
            log_config_error(&e);
        }
    }

    /// Power-cycle the current console: rebuild the machine from the session's
    /// hardware model + current cartridge (so every model-derived flag is
    /// re-applied — `GB::new`, not in-place reset), clear rewind, run normally.
    pub fn restart(&mut self) {
        let gb = self.rebuild_current_gb();
        self.replace_machine(*gb, self.rom_id);
        self.clear_rewind();
        self.mode = RunMode::Normal;
    }

    /// Build a fresh, booted machine for the current hardware carrying a clone
    /// of the inserted cartridge (if any). Boxed to keep the ~207 KB machine off
    /// the stack.
    fn rebuild_current_gb(&self) -> Box<GB> {
        let mut gb = GB::new(self.config.hardware);
        if let Some(cart) = self.gb.cartridge() {
            let mut cart = cart.clone();
            // Power-cycle semantics: the clone carries the running cart's
            // volatile MBC latches (bank registers, banking mode); re-home
            // them so e.g. an MBC1M multicart restarts into its game-select
            // menu. Battery RAM/RTC state survives inside the clone.
            cart.reset();
            gb.insert(cart);
            self.boot_or_skip(&mut gb);
        }
        Box::new(gb)
    }

    // --- debug-step requests (set by `apply`, drained by the run loop) ------

    /// Queue a multi-instruction debug step (consumed by the frontend's run
    /// loop via [`Session::take_step_cycles`]).
    pub fn request_step_cycles(&mut self, count: u32) {
        self.pending_step_cycles = Some(count);
    }

    /// Queue a multi-frame debug step (consumed via
    /// [`Session::take_step_frames`]).
    pub fn request_step_frames(&mut self, count: u32) {
        self.pending_step_frames = Some(count);
    }

    /// Take a pending multi-instruction step request, if any.
    pub fn take_step_cycles(&mut self) -> Option<u32> {
        self.pending_step_cycles.take()
    }

    /// Take a pending multi-frame step request, if any.
    pub fn take_step_frames(&mut self) -> Option<u32> {
        self.pending_step_frames.take()
    }

    /// Load a ROM from raw bytes: build a fresh booted machine for the current
    /// hardware, insert the cartridge, and re-bind the session to it. Returns
    /// the new ROM id on success.
    pub fn finish_load_rom(&mut self, bytes: &[u8]) -> Result<[u8; 32], SessionError> {
        // Unzip a `.zip` container so identification/patching/rom-id see the ROM,
        // not the archive (the core also unzips when building the cartridge).
        let rom = crate::rom_zip::extract_rom(bytes);
        let rom_id = self.load_rom_bytes(&rom)?;
        // Retain the pristine ROM so a later `apply_rom_patch` always patches the
        // original, not an already-patched image.
        self.original_rom = Some(rom);
        Ok(rom_id)
    }

    /// Shared cartridge (re)build used by both [`finish_load_rom`] and
    /// [`apply_rom_patch`]: insert `bytes`, re-bind the session, and hydrate the
    /// battery image. Does NOT touch `original_rom` (the caller decides whether
    /// these bytes are the pristine ROM or a patched derivative).
    fn load_rom_bytes(&mut self, bytes: &[u8]) -> Result<[u8; 32], SessionError> {
        let cart = Cartridge::from_bytes(bytes).map_err(|e| SessionError::State(e.to_string()))?;
        let mut gb = GB::new(self.config.hardware);
        gb.insert(cart);
        self.boot_or_skip(&mut gb);
        let rom_id = rustyboi_core_lib::movie::sha256(bytes);
        self.game_name = crate::no_intro::resolve_game_name(bytes);
        self.replace_machine(gb, rom_id);
        // Restore a battery image persisted through the storage port (web
        // IndexedDB / desktop GUI loads that have no sidecar `.sav`).
        self.hydrate_battery();
        Ok(rom_id)
    }

    /// Apply an IPS/UPS/BPS `patch` to the pristine ROM and re-load the patched
    /// cartridge (a romhack / translation applied in-app). The original ROM must
    /// have been loaded through [`finish_load_rom`] first. Returns the patched
    /// ROM's id on success; on a bad/mismatched patch the current machine is left
    /// untouched. The retained pristine ROM is unchanged, so re-applying a
    /// different patch still starts from the original.
    pub fn apply_rom_patch(&mut self, patch: &[u8]) -> Result<[u8; 32], SessionError> {
        let original = self
            .original_rom
            .as_ref()
            .ok_or_else(|| SessionError::State("no ROM loaded to patch".into()))?;
        let patched = crate::patch::apply_patch(original, patch).map_err(SessionError::State)?;
        self.load_rom_bytes(&patched)
    }

    /// Load a savestate from raw bytes, re-binding to `rom_id` (derived by the
    /// caller from the reload ROM, or the existing id when `None`). The current
    /// cartridge is re-attached by [`Session::restore_state`] as needed; a
    /// caller-supplied `reload_rom` is inserted first when the state carried no
    /// cartridge.
    pub fn finish_load_state(
        &mut self,
        state: &[u8],
        reload_rom: Option<&[u8]>,
        rom_id: [u8; 32],
    ) -> Result<(), SessionError> {
        let mut gb = GB::from_state_bytes(state).map_err(|e| SessionError::State(e.to_string()))?;
        if gb.cartridge_needs_rom() {
            if let Some(rom) = reload_rom {
                gb.reattach_rom(rom);
            } else if let Some(rom) = self.gb.detach_rom_bytes() {
                gb.reattach_rom(&rom);
            }
        } else if let Some(rom) = reload_rom {
            match Cartridge::from_bytes(rom) {
                Ok(cart) => {
                    gb.insert(cart);
                }
                Err(e) => return Err(SessionError::State(format!("failed to reattach ROM: {e}"))),
            }
        }
        self.replace_machine(gb, rom_id);
        // `replace_machine` already re-applies presentation settings.
        Ok(())
    }

    /// Finish loading a TAS movie: decode the `.rbmovie` bytes produced by
    /// [`stop_recording`](Self::stop_recording) → [`Movie::to_bytes`] and begin
    /// deterministic playback (see [`play_movie`](Self::play_movie)). The parallel
    /// to the other `finish_*` finishers for the `LoadPurpose::Movie` file-resolve
    /// path. Fails if the bytes are not a movie or were recorded against a
    /// different ROM than the one loaded.
    pub fn finish_load_movie(&mut self, bytes: &[u8]) -> Result<(), SessionError> {
        let movie = Movie::from_bytes(bytes).map_err(|e| SessionError::Movie(e.to_string()))?;
        self.play_movie(&movie)
    }

    // --- save-data import/export (battery + RTC) ----------------------------

    /// Finish a battery-save import: resolved `bytes` from a picked `.sav` are
    /// copied into the live cartridge (and flushed through any attached sidecar).
    /// The parallel to [`finish_load_rom`](Self::finish_load_rom) for the
    /// `LoadPurpose::Battery` file-resolve path.
    pub fn finish_import_battery(&mut self, bytes: &[u8]) -> Result<(), SessionError> {
        self.import_battery(bytes).map_err(SessionError::State)
    }

    /// Finish an RTC import: resolved `bytes` from a picked `.rtc` restore the
    /// cartridge's clock (with wall-clock catch-up). The parallel to
    /// [`finish_load_rom`](Self::finish_load_rom) for the `LoadPurpose::Rtc`
    /// file-resolve path.
    pub fn finish_import_rtc(&mut self, bytes: &[u8]) -> Result<(), SessionError> {
        self.import_rtc(bytes).map_err(SessionError::State)
    }

    /// The cartridge's battery-backed SRAM image (File → Export Battery Save),
    /// or `None` when the inserted cart has no battery / no cart is loaded.
    pub fn export_battery(&self) -> Option<Vec<u8>> {
        let cart = self.gb.cartridge()?;
        if !cart.has_battery() {
            return None;
        }
        Some(cart.save_ram().to_vec())
    }

    /// Import a battery save image into the current cartridge (File → Import
    /// Battery Save). Copies into the cart's SRAM (bounds-checked) and, on
    /// desktop, flushes through the attached `.sav`; then mirrors the image to
    /// the storage port keyed by ROM id so platforms without a sidecar (web
    /// IndexedDB) survive a reload. Errors when no cart is loaded, the cart has
    /// no battery, or the file is the wrong size.
    pub fn import_battery(&mut self, bytes: &[u8]) -> Result<(), String> {
        let cart = self
            .gb
            .cartridge_mut()
            .ok_or_else(|| "no cartridge loaded".to_string())?;
        cart.import_save_ram(bytes)?;
        self.persist_battery();
        Ok(())
    }

    /// Storage key for the cartridge battery image, namespaced by ROM id (mirror
    /// of [`slot_key`](Self::slot_key)).
    fn battery_key(&self) -> String {
        let mut hex = String::with_capacity(64);
        for b in self.rom_id {
            hex.push_str(&format!("{b:02x}"));
        }
        format!("battery/{hex}")
    }

    /// Mirror the current cartridge SRAM to the storage port (the persist path
    /// platforms without a sidecar `.sav` rely on — web IndexedDB). No-op for
    /// non-battery carts. Sidecar-backed platforms (desktop) also persist here
    /// harmlessly in addition to their `.sav`.
    pub fn persist_battery(&mut self) {
        let Some(cart) = self.gb.cartridge() else { return };
        if !cart.has_battery() {
            return;
        }
        let bytes = cart.save_ram().to_vec();
        let key = self.battery_key();
        if let Err(e) = self.ports.storage.write(&key, &bytes) {
            log_config_error(&SessionError::from(e));
        }
    }

    /// Restore a previously [`persist_battery`](Self::persist_battery)ed SRAM
    /// image into the current cartridge (called after a ROM load so a battery
    /// imported in a prior session survives a reload on storage-only platforms).
    /// No-op when nothing is stored, or for non-battery carts.
    pub fn hydrate_battery(&mut self) {
        let key = self.battery_key();
        let Some(bytes) = self.ports.storage.read(&key) else { return };
        if let Some(cart) = self.gb.cartridge_mut()
            && cart.has_battery()
        {
            let _ = cart.import_save_ram(&bytes);
        }
    }

    /// The cartridge's RTC state serialized to the `.rtc` sidecar format (File →
    /// Export RTC), or `None` when the cart has no real-time clock.
    pub fn export_rtc(&self) -> Option<Vec<u8>> {
        self.gb.cartridge().and_then(|c| c.export_rtc())
    }

    /// Import a `.rtc` blob into the current cartridge (File → Import RTC),
    /// restoring the clock with wall-clock catch-up. Errors when no cart is
    /// loaded, the cart has no RTC, or the blob doesn't match the cart.
    pub fn import_rtc(&mut self, bytes: &[u8]) -> Result<(), String> {
        let cart = self
            .gb
            .cartridge_mut()
            .ok_or_else(|| "no cartridge loaded".to_string())?;
        cart.import_rtc(bytes)
    }

    /// Whether the inserted cartridge has battery-backed save RAM (drives the
    /// Import/Export Battery menu gating).
    pub fn has_battery(&self) -> bool {
        self.gb.cartridge().is_some_and(|c| c.has_battery())
    }

    /// Whether the inserted cartridge has a real-time clock (drives the
    /// Import/Export RTC menu gating).
    pub fn has_rtc(&self) -> bool {
        self.gb.cartridge().is_some_and(|c| c.has_rtc())
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

    /// Remove all cheats (e.g. libretro's `retro_cheat_reset`). Like
    /// [`remove_cheat`](Self::remove_cheat), already-applied Game Genie ROM
    /// patches are only undone on the next ROM (re)load.
    pub fn clear_cheats(&mut self) {
        self.cheats.clear();
    }

    /// The active cheat codes.
    pub fn cheats(&self) -> impl Iterator<Item = &str> {
        self.cheats.codes()
    }

    /// The raw bytes of the currently-loaded ROM (unpatched), or `None` if no
    /// ROM has been loaded from bytes. Used to identify the game for the cheat-DB
    /// fetch.
    pub fn original_rom_bytes(&self) -> Option<&[u8]> {
        self.original_rom.as_deref()
    }

    /// The candidate libretro-cheat-DB URLs for the loaded game, or `None` if no
    /// ROM is loaded or the ROM isn't in the No-Intro index. The frontend fetches
    /// these (in order) and feeds the body to [`finish_fetched_cheats`].
    pub fn cheat_fetch_urls(&self) -> Option<Vec<String>> {
        let rom = self.original_rom.as_deref()?;
        let name = crate::no_intro::identify(rom)?;
        Some(crate::cheat_db::candidate_urls(&name, crate::cheat_db::is_cgb(rom)))
    }

    /// The two libretro No-Intro DAT URLs to download for offline game
    /// identification. Static (ROM-independent): the frontend fetches these once
    /// (caching the bodies) and feeds them to [`finish_no_intro_dats`]. The
    /// session performs no HTTP itself. The downloaded data is CC-BY-SA-4.0
    /// libretro-database material — not embedded in any binary — so callers log
    /// the attribution at download time.
    pub fn no_intro_fetch_urls(&self) -> Vec<String> {
        log_no_intro_attribution();
        crate::no_intro::dat_urls()
    }

    /// Feed downloaded No-Intro DAT bodies into the runtime identification index
    /// (merging with any already loaded), then re-resolve the current ROM's
    /// display name now that identification may succeed. Bodies may be supplied
    /// incrementally (one per DAT) or together.
    pub fn finish_no_intro_dats(&mut self, bodies: &[String]) {
        crate::no_intro::load_dats(bodies);
        if let Some(rom) = self.original_rom.as_deref() {
            self.game_name = crate::no_intro::resolve_game_name(rom);
        }
    }

    /// Parse a downloaded libretro `.cht` body into the pending fetched-cheat
    /// list (replacing any previous fetch). Returns the number of cheats parsed.
    /// The frontend then shows them for the user to pick; selected codes are
    /// added through the normal [`add_cheat`](Self::add_cheat) path.
    pub fn finish_fetched_cheats(&mut self, body: &str) -> usize {
        self.fetched_cheats = crate::cheat_db::parse_cht(body);
        self.fetched_cheats.len()
    }

    /// The cheats fetched from the libretro DB awaiting the user's selection.
    pub fn fetched_cheats(&self) -> &[crate::cheat_db::FetchedCheat] {
        &self.fetched_cheats
    }

    /// Discard the pending fetched-cheat list (user closed the picker).
    pub fn clear_fetched_cheats(&mut self) {
        self.fetched_cheats.clear();
    }

    // --- access -------------------------------------------------------------

    /// Immutable access to the underlying machine (for adapters that need
    /// palette/memory accessors for presentation).
    pub fn gb(&self) -> &GB {
        &self.gb
    }

    /// Mutable access to the underlying machine, for host-side debug tooling
    /// that operates directly on the core (breakpoints, single-cycle stepping,
    /// direct memory pokes) and has no feature-level session equivalent.
    /// Feature operations (run, savestate, rewind, TAS) must still go through
    /// the session so its bookkeeping (frame count, rewind cadence, capture
    /// sink) stays consistent.
    pub fn gb_mut(&mut self) -> &mut GB {
        &mut self.gb
    }

    /// The ROM identity hash this session is bound to.
    pub fn rom_id(&self) -> [u8; 32] {
        self.rom_id
    }

    /// Drain any Game Boy Printer photos finished since the last call, each as
    /// one tall [`PrintSheet`] with its strips stitched vertically (one long
    /// sheet of paper). A photo is emitted when a strip carries a paper-feed
    /// margin: a non-zero *before*-feed on a fresh strip closes the previous
    /// photo, and a non-zero *after*-feed ends the current one. Strips with no
    /// feed keep accumulating (a multi-band print is not ejected mid-image).
    ///
    /// Replaces draining `GB::take_printer_sheets` directly, so every frontend
    /// (which polls this each frame) saves one image per photo instead of one
    /// file/download per band.
    pub fn take_prints(&mut self) -> Vec<PrintSheet> {
        let strips = self.gb.take_printer_sheets();
        self.accumulate_prints(strips)
    }

    /// Group freshly-drained printer strips into finished photos by the
    /// paper-feed margins (see [`take_prints`](Self::take_prints)). Split out for
    /// unit testing without driving a print ROM.
    fn accumulate_prints(&mut self, strips: Vec<PrintSheet>) -> Vec<PrintSheet> {
        let scale = self.config.printer_scale;
        let mut finished = Vec::new();
        let mut eject = |strips: Vec<PrintSheet>| {
            finished.push(scale_sheet(stitch_prints(strips), scale));
        };
        for strip in strips {
            // margins byte: high nibble = feed before, low nibble = feed after.
            let feed_before = strip.margins >> 4 != 0;
            let feed_after = strip.margins & 0x0F != 0;
            if feed_before && !self.printer_strips.is_empty() {
                eject(std::mem::take(&mut self.printer_strips));
            }
            self.printer_strips.push(strip);
            if feed_after {
                eject(std::mem::take(&mut self.printer_strips));
            }
        }
        finished
    }

    /// The emulated hardware model.
    pub fn hardware(&self) -> Hardware {
        self.config.hardware
    }
}

/// Reserved slot number for quicksave/quickload.
pub const QUICK_SLOT: u32 = u32::MAX;

/// Stitch a photo's strips (all `PRINT_WIDTH` wide) into one tall sheet by
/// vertically concatenating their shade rows. Metadata comes from the last strip
/// (the one that carried the ejecting feed). `strips` is always non-empty.
fn stitch_prints(strips: Vec<PrintSheet>) -> PrintSheet {
    if strips.len() == 1 {
        return strips.into_iter().next().unwrap();
    }
    let width = strips[0].width;
    let height: u32 = strips.iter().map(|s| s.height).sum();
    let mut shades = Vec::with_capacity(strips.iter().map(|s| s.shades.len()).sum());
    for s in &strips {
        shades.extend_from_slice(&s.shades);
    }
    let last = strips.last().unwrap();
    PrintSheet {
        width,
        height,
        shades,
        sheets: last.sheets,
        margins: last.margins,
        palette: last.palette,
        exposure: last.exposure,
    }
}

/// Nearest-neighbour integer upscale of a print sheet by `scale` (≥1). The
/// native printer image is 160px wide — tiny on a modern screen — so frontends
/// save/download this enlarged copy. `scale == 1` returns the sheet unchanged.
fn scale_sheet(sheet: PrintSheet, scale: u8) -> PrintSheet {
    let scale = scale.max(1) as u32;
    if scale == 1 {
        return sheet;
    }
    let (w, h) = (sheet.width, sheet.height);
    let (nw, nh) = (w * scale, h * scale);
    let mut shades = Vec::with_capacity((nw * nh) as usize);
    for ny in 0..nh {
        let row_start = (ny / scale * w) as usize;
        let row = &sheet.shades[row_start..row_start + w as usize];
        for nx in 0..nw {
            shades.push(row[(nx / scale) as usize]);
        }
    }
    PrintSheet {
        width: nw,
        height: nh,
        shades,
        sheets: sheet.sheets,
        margins: sheet.margins,
        palette: sheet.palette,
        exposure: sheet.exposure,
    }
}

/// The No-Intro game-name data is not embedded in any rustyboi binary; each
/// frontend downloads it at runtime from the CC-BY-SA-4.0 libretro-database. Log
/// the attribution whenever a fetch is initiated. `eprintln` on native, dropped
/// on wasm to stay clean (the web frontend logs it to the console separately).
fn log_no_intro_attribution() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        eprintln!(
            "No-Intro database (game names) is licensed CC-BY-SA-4.0 — https://creativecommons.org/licenses/by-sa/4.0/"
        );
    }
}

/// Log a config-save failure. Non-fatal (a failed persist never bricks a
/// running session); `eprintln` on native, dropped on wasm to stay clean.
fn log_config_error(e: &SessionError) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        eprintln!("Failed to save config: {e}");
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = e;
    }
}

#[cfg(test)]
mod offload_tests {
    use super::*;
    use crate::ports::{MemRumble, MemStorage, MemWebcam};

    fn test_ports() -> Ports {
        Ports {
            storage: Box::new(MemStorage::new()),
            rumble: Box::new(MemRumble::default()),
            webcam: Box::new(MemWebcam::default()),
        }
    }

    fn cfg() -> Config {
        let mut c = Config::default();
        c.rewind.enabled = true;
        c.rewind.depth = 8;
        c.rewind.interval_frames = 2;
        c
    }

    // The offloaded capture path must produce byte-identical rewind blobs to the
    // inline path: same WHAT (serialized state) captured at the same frames,
    // only serialized elsewhere. Two ROM-less machines run identically, so we
    // can compare the ring contents directly.
    #[test]
    fn offloaded_capture_matches_inline_bytes() {
        let mut inline = Session::new(cfg(), test_ports(), [0u8; 32]);

        let mut offl = Session::new(cfg(), test_ports(), [0u8; 32]);
        offl.set_rewind_offloaded(true);

        for _ in 0..12 {
            inline.run_frame(AbstractInput::none());

            offl.run_frame(AbstractInput::none());
            // Synchronously stand in for the worker: serialize + compress the
            // clone and feed it back, exactly as the platform worker would.
            if let Some((frame, mut gb)) = offl.take_pending_snapshot() {
                let bytes = gb.to_state_bytes().expect("serialize clone");
                offl.push_rewind_bytes(frame, crate::rewind::compress_snapshot(bytes));
            }
        }

        // Same number of retained snapshots and identical footprint.
        assert_eq!(inline.rewind_stats(), offl.rewind_stats());
        assert!(inline.rewind_stats().0 > 0, "expected some snapshots captured");

        // Drain both rings newest-first; blobs must be byte-identical.
        loop {
            let a = inline.rewind.rewind();
            let b = offl.rewind.rewind();
            match (a, b) {
                (Some(x), Some(y)) => {
                    assert_eq!(x.frame, y.frame, "frame index mismatch");
                    assert_eq!(x.state, y.state, "serialized state mismatch");
                }
                (None, None) => break,
                _ => panic!("ring length mismatch"),
            }
        }
    }

    // A due capture in offloaded mode must NOT serialize inline (the emu-thread
    // cost is only a clone) and must surface exactly one pending snapshot.
    #[test]
    fn offloaded_defers_serialization() {
        let mut s = Session::new(cfg(), test_ports(), [0u8; 32]);
        s.set_rewind_offloaded(true);

        // interval_frames == 2 -> frame 2 is the first due capture.
        s.run_frame(AbstractInput::none()); // frame 1, not due
        assert!(s.take_pending_snapshot().is_none());
        assert_eq!(s.rewind_stats().0, 0, "nothing pushed inline in offloaded mode");

        s.run_frame(AbstractInput::none()); // frame 2, due
        let snap = s.take_pending_snapshot();
        assert!(snap.is_some(), "expected a pending clone at the due frame");
        assert_eq!(s.rewind_stats().0, 0, "still nothing in the ring until pushed back");
    }
}

#[cfg(test)]
mod volume_tests {
    use super::*;
    use crate::ports::{MemRumble, MemStorage, MemWebcam};

    fn test_ports() -> Ports {
        Ports {
            storage: Box::new(MemStorage::new()),
            rumble: Box::new(MemRumble::default()),
            webcam: Box::new(MemWebcam::default()),
        }
    }

    // A fixed synthetic stereo stream standing in for drained APU output, so the
    // scaling is verified on real signal regardless of what a ROM-less machine
    // happens to emit. `run_frame` scales exactly this way (it calls
    // `scale_samples` on the drained buffer).
    fn stream() -> Vec<(f32, f32)> {
        vec![(0.8, -0.4), (1.0, -1.0), (0.25, 0.5), (-0.6, 0.0)]
    }

    // `run_frame`'s output scaler: gain 1.0 is identity (default volume 100),
    // gain 0.5 halves every sample (volume 50), gain 0.0 silences (volume 0).
    #[test]
    fn scale_samples_applies_gain() {
        let src = stream();

        // Volume 100 -> gain 1.0 -> byte-identical (the default path is untouched).
        let full = scale_samples(src.iter().copied(), 1.0);
        assert_eq!(full, src, "gain 1.0 is the identity path");

        // Volume 50 -> gain 0.5 -> each channel exactly halved (x*0.5 is exact f32).
        let half = scale_samples(src.iter().copied(), 0.5);
        for ((sl, sr), (hl, hr)) in src.iter().zip(half.iter()) {
            assert_eq!(*hl, sl * 0.5, "left halved");
            assert_eq!(*hr, sr * 0.5, "right halved");
        }

        // Volume 0 -> gain 0.0 -> every sample silenced.
        let mute = scale_samples(src.iter().copied(), 0.0);
        assert!(mute.iter().all(|(l, r)| *l == 0.0 && *r == 0.0), "gain 0 silences");
    }

    // Fast-forward's output resampler collapses each group of `factor` input
    // samples to their mean, so `factor`× the samples become one real-time
    // frame's worth (played back at a raised pitch instead of backing up).
    #[test]
    fn decimate_samples_averages_groups() {
        let src = stream(); // 4 samples

        // factor 1 is the identity (delegates to `scale_samples`).
        assert_eq!(decimate_samples(&src, 1, 1.0), src);

        // factor 2 -> two outputs, each the mean of a consecutive pair.
        let by2 = decimate_samples(&src, 2, 1.0);
        assert_eq!(by2.len(), 2, "output length is len/factor");
        assert_eq!(by2[0], ((0.8 + 1.0) / 2.0, (-0.4 + -1.0) / 2.0));
        assert_eq!(by2[1], ((0.25 + -0.6) / 2.0, (0.5 + 0.0) / 2.0));

        // Gain composes with the averaging (volume 50 halves the means).
        let by2_half = decimate_samples(&src, 2, 0.5);
        assert_eq!(by2_half[0], (by2[0].0 * 0.5, by2[0].1 * 0.5));

        // A ragged tail (len not a multiple of factor) is dropped by chunks_exact.
        assert_eq!(decimate_samples(&src, 3, 1.0).len(), 1);
    }

    // Setting the fast-forward speed persists it and, when already engaged,
    // re-derives the live run mode. Speed 0 = uncapped (a batch factor).
    #[test]
    fn set_fast_forward_factor_updates_live_mode() {
        let mut s = Session::new(Config::default(), test_ports(), [0u8; 32]);
        s.fast_forward();
        assert!(matches!(s.mode(), RunMode::FastForward(4)), "default speed is 4×");

        s.set_fast_forward_factor(10);
        assert!(matches!(s.mode(), RunMode::FastForward(10)), "live mode follows the new speed");
        assert_eq!(s.fast_forward_factor(), 10);
        assert!(!s.config().ff_uncapped());

        s.set_fast_forward_factor(0);
        assert!(s.config().ff_uncapped(), "speed 0 is uncapped");
        assert!(matches!(s.mode(), RunMode::FastForward(_)), "still fast-forwarding while uncapped");
    }

    // The gain `run_frame` uses tracks the config volume, so setting volume 0/50/
    // 100 drives the scaler to 0.0/0.5/1.0 respectively.
    #[test]
    fn config_volume_drives_run_frame_gain() {
        let src = stream();
        for (vol, want_gain) in [(0u8, 0.0f32), (50, 0.5), (100, 1.0)] {
            let cfg = Config { volume: vol, ..Default::default() };
            let s = Session::new(cfg, test_ports(), [0u8; 32]);
            let gain = s.config().volume_gain();
            assert_eq!(gain, want_gain, "volume {vol} -> gain {want_gain}");
            let scaled = scale_samples(src.iter().copied(), gain);
            for ((sl, sr), (dl, dr)) in src.iter().zip(scaled.iter()) {
                assert_eq!(*dl, sl * want_gain);
                assert_eq!(*dr, sr * want_gain);
            }
        }
    }

    // The wiring itself: `run_frame` never changes the sample COUNT, only the
    // amplitude, so a ROM-less machine yields the same-length stream at any volume.
    #[test]
    fn run_frame_output_length_is_volume_independent() {
        let len_at = |vol: u8| {
            let cfg = Config { volume: vol, ..Default::default() };
            let mut s = Session::new(cfg, test_ports(), [0u8; 32]);
            s.run_frame(AbstractInput::none()).audio.len()
        };
        assert_eq!(len_at(100), len_at(50));
        assert_eq!(len_at(100), len_at(0));
    }

    // The setter clamps to 0..=100 and persists, and the gain multiplier tracks it.
    #[test]
    fn set_volume_clamps_and_reports() {
        let mut s = Session::new(Config::default(), test_ports(), [0u8; 32]);
        assert_eq!(s.volume(), 100);
        s.set_volume(200);
        assert_eq!(s.volume(), 100, "over-100 clamps to 100");
        s.set_volume(50);
        assert_eq!(s.volume(), 50);
        assert_eq!(s.config().volume_gain(), 0.5);
        s.set_volume(0);
        assert_eq!(s.config().volume_gain(), 0.0);
    }

    // The scaling-mode setter round-trips through the persisted config.
    #[test]
    fn set_scaling_mode_persists() {
        let mut s = Session::new(Config::default(), test_ports(), [0u8; 32]);
        assert_eq!(s.scaling_mode(), ScalingMode::FitAspect);
        s.set_scaling_mode(ScalingMode::IntegerAspect);
        assert_eq!(s.scaling_mode(), ScalingMode::IntegerAspect);
        assert_eq!(s.config().scaling, ScalingMode::IntegerAspect);
    }
}

#[cfg(test)]
mod printer_tests {
    //! The Game Boy Printer stitches a photo's strips into one long sheet,
    //! breaking on the paper-feed margins (high nibble = feed before, low nibble
    //! = feed after). Driven directly through `accumulate_prints` (no print ROM).
    use super::*;
    use crate::ports::{MemRumble, MemStorage, MemWebcam};

    fn test_ports() -> Ports {
        Ports {
            storage: Box::new(MemStorage::new()),
            rumble: Box::new(MemRumble::default()),
            webcam: Box::new(MemWebcam::default()),
        }
    }

    fn session_scaled(scale: u8) -> Session {
        let cfg = Config { printer_scale: scale, ..Default::default() };
        Session::new(cfg, test_ports(), [0u8; 32])
    }

    /// The stitching tests use 1× so they assert raw geometry.
    fn session() -> Session {
        session_scaled(1)
    }

    /// An 8-row strip filled with `fill`, printed with `margins`.
    fn strip(fill: u8, margins: u8) -> PrintSheet {
        PrintSheet {
            width: 160,
            height: 8,
            shades: vec![fill; 160 * 8],
            sheets: 1,
            margins,
            palette: 0xE4,
            exposure: 0x40,
        }
    }

    #[test]
    fn strips_stitch_into_one_photo_on_after_feed() {
        let mut s = session();
        // Two mid-image strips (no feed) eject nothing yet...
        assert!(s.accumulate_prints(vec![strip(1, 0x00), strip(2, 0x00)]).is_empty());
        // ...the strip that feeds paper out ejects one stitched photo.
        let out = s.accumulate_prints(vec![strip(3, 0x03)]);
        assert_eq!(out.len(), 1);
        let photo = &out[0];
        assert_eq!((photo.width, photo.height), (160, 24), "three 8-row strips");
        // Rows are concatenated in arrival order.
        assert_eq!(photo.shades[0], 1);
        assert_eq!(photo.shades[160 * 8], 2);
        assert_eq!(photo.shades[160 * 16], 3);
    }

    #[test]
    fn before_feed_closes_the_previous_photo() {
        let mut s = session();
        assert!(s.accumulate_prints(vec![strip(1, 0x00)]).is_empty());
        // A fresh strip with a *before* feed ejects the pending photo and starts
        // a new one.
        let out = s.accumulate_prints(vec![strip(2, 0x10)]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].height, 8, "just the first strip");
        // The second strip is now pending; an after-feed ejects strip2+strip3.
        let out = s.accumulate_prints(vec![strip(3, 0x01)]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].height, 16);
    }

    #[test]
    fn a_lone_feed_strip_is_one_photo() {
        let mut s = session();
        let out = s.accumulate_prints(vec![strip(1, 0x03)]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].height, 8);
    }

    #[test]
    fn printer_scale_upscales_the_photo() {
        let mut s = session_scaled(3);
        let out = s.accumulate_prints(vec![strip(2, 0x03)]);
        assert_eq!(out.len(), 1);
        // 160x8 nearest-neighbour to 3x.
        assert_eq!((out[0].width, out[0].height), (480, 24));
        assert_eq!(out[0].shades.len(), 480 * 24);
        assert!(out[0].shades.iter().all(|&sh| sh == 2), "shade preserved by NN upscale");
    }
}

#[cfg(test)]
mod tas_tests {
    //! The frontend-facing TAS surface: record from the live state, stop into a
    //! serialized `.rbmovie`, then reload it for deterministic playback. This
    //! covers the session-level plumbing the four frontends drive through
    //! `apply(ToggleRecording)` / `finish_load_movie`; the bit-exact replay
    //! guarantee itself is proven in `rustyboi_core_lib::movie`.
    use super::*;
    use crate::action::UiAction;
    use crate::apply::PlatformRequest;
    use crate::ports::{MemRumble, MemStorage, MemWebcam};
    use rustyboi_core_lib::movie::Movie;

    fn test_ports() -> Ports {
        Ports {
            storage: Box::new(MemStorage::new()),
            rumble: Box::new(MemRumble::default()),
            webcam: Box::new(MemWebcam::default()),
        }
    }

    fn session() -> Session {
        Session::new(Config::default(), test_ports(), [0u8; 32])
    }

    // One ToggleRecording arms recording; a second stops it and hands back a
    // decodable `.rbmovie` whose frame count matches the frames stepped while
    // armed. Loading those bytes begins playback; StopReplay ends it.
    #[test]
    fn toggle_recording_round_trips_and_replays() {
        let mut s = session();

        assert!(!s.is_recording());
        s.apply(UiAction::ToggleRecording, 0);
        assert!(s.is_recording(), "first toggle starts recording");

        // Step three frames so the recording logs three inputs.
        for _ in 0..3 {
            s.run_frame(AbstractInput::none());
        }

        let out = s.apply(UiAction::ToggleRecording, 0);
        assert!(!s.is_recording(), "second toggle stops recording");
        let bytes = out
            .requests
            .iter()
            .find_map(|r| match r {
                PlatformRequest::SaveBytes { suggested_name, bytes } => {
                    assert!(suggested_name.ends_with(".rbmovie"));
                    Some(bytes.clone())
                }
                _ => None,
            })
            .expect("stop-recording emits a SaveBytes export");

        let movie = Movie::from_bytes(&bytes).expect("exported bytes decode as a movie");
        assert_eq!(movie.inputs.len(), 3, "one input logged per stepped frame");

        // Reload the exported movie: playback begins and StopReplay ends it.
        s.finish_load_movie(&bytes).expect("reload the just-exported movie");
        assert!(s.is_playing(), "loading a movie begins playback");
        s.apply(UiAction::StopReplay, 0);
        assert!(!s.is_playing(), "StopReplay resumes live input");
    }

    // A movie recorded against a different ROM id is rejected rather than
    // silently played against the wrong game.
    #[test]
    fn load_movie_rejects_rom_mismatch() {
        let mut recorder = Session::new(Config::default(), test_ports(), [1u8; 32]);
        recorder.start_recording_from_state().unwrap();
        recorder.run_frame(AbstractInput::none());
        let movie = recorder.stop_recording().unwrap();

        // A session for a different ROM id must refuse it.
        let mut other = session();
        assert!(matches!(
            other.finish_load_movie(&movie.to_bytes()),
            Err(SessionError::RomMismatch)
        ));
    }

    // Garbage bytes surface as a decode error, never a panic.
    #[test]
    fn load_movie_rejects_garbage() {
        let mut s = session();
        assert!(matches!(s.finish_load_movie(b"not a movie"), Err(SessionError::Movie(_))));
    }
}

#[cfg(test)]
mod slot_and_import_tests {
    //! Slot storage round-trips against in-memory storage, blob-header
    //! validation, and the import/patch error branches. Driven ROM-less through
    //! `Session::new`, which serializes a fresh `GB` just like the real path.
    use super::*;
    use crate::ports::{MemRumble, MemStorage, MemWebcam};

    fn test_ports() -> Ports {
        Ports {
            storage: Box::new(MemStorage::new()),
            rumble: Box::new(MemRumble::default()),
            webcam: Box::new(MemWebcam::default()),
        }
    }

    fn session() -> Session {
        Session::new(Config::default(), test_ports(), [0xEEu8; 32])
    }

    #[test]
    fn save_slot_round_trips_through_storage() {
        let mut s = session();
        for _ in 0..4 {
            s.run_frame(AbstractInput::none());
        }
        s.save_slot(3, 777).unwrap();

        // Metadata is readable without a full load.
        let meta = s.slot_meta(3).expect("slot 3 exists");
        assert_eq!(meta.frame_count, 4);
        assert_eq!(meta.timestamp, 777);
        assert!(s.slot_meta(1).is_none(), "an unsaved slot has no meta");

        // Diverge, then load restores the saved frame count.
        for _ in 0..5 {
            s.run_frame(AbstractInput::none());
        }
        assert_eq!(s.frame_count(), 9);
        let loaded = s.load_slot(3).unwrap();
        assert_eq!(loaded, SlotMeta { frame_count: 4, timestamp: 777 });
        assert_eq!(s.frame_count(), 4);
    }

    #[test]
    fn list_slots_is_sorted_and_scoped_to_saved_slots() {
        let mut s = session();
        s.run_frame(AbstractInput::none());
        s.save_slot(5, 1).unwrap();
        s.save_slot(1, 1).unwrap();
        s.save_slot(9, 1).unwrap();
        assert_eq!(s.list_slots(), vec![1, 5, 9]);
    }

    #[test]
    fn load_missing_slot_is_no_state() {
        let mut s = session();
        assert!(matches!(s.load_slot(2), Err(SessionError::NoState)));
    }

    #[test]
    fn split_slot_blob_rejects_short_and_accepts_full_header() {
        // Fewer than the 16 header bytes → truncated error.
        assert!(matches!(
            Session::split_slot_blob(&[0u8; 15]),
            Err(SessionError::State(_))
        ));
        // Exactly the header (frame_count then timestamp, both LE u64) with an
        // empty state tail parses.
        let mut blob = Vec::new();
        blob.extend_from_slice(&7u64.to_le_bytes());
        blob.extend_from_slice(&42u64.to_le_bytes());
        let (meta, state) = Session::split_slot_blob(&blob).unwrap();
        assert_eq!(meta, SlotMeta { frame_count: 7, timestamp: 42 });
        assert!(state.is_empty());
    }

    #[test]
    fn corrupt_slot_blob_is_rejected_on_load() {
        // A stored blob shorter than the header is surfaced as a state error,
        // never a panic; `slot_meta` treats it as absent.
        let mut s = session();
        let key = s.slot_key(4);
        s.ports.storage.write(&key, &[1u8, 2, 3]).unwrap();
        assert!(matches!(s.load_slot(4), Err(SessionError::State(_))));
        assert!(s.slot_meta(4).is_none());
    }

    #[test]
    fn finish_load_state_rejects_garbage_bytes() {
        let mut s = session();
        assert!(matches!(
            s.finish_load_state(b"not a savestate", None, [0u8; 32]),
            Err(SessionError::State(_))
        ));
    }

    #[test]
    fn apply_rom_patch_without_a_loaded_rom_errors() {
        let mut s = session(); // original_rom is None ROM-less
        assert!(matches!(s.apply_rom_patch(&[0u8; 8]), Err(SessionError::State(_))));
    }

    #[test]
    fn import_battery_and_rtc_error_without_a_cartridge() {
        let mut s = session(); // no cartridge inserted
        assert!(matches!(s.finish_import_battery(&[0u8; 16]), Err(SessionError::State(_))));
        assert!(matches!(s.finish_import_rtc(&[0u8; 48]), Err(SessionError::State(_))));
    }
}

#[cfg(test)]
mod cheat_tests {
    //! The session-level cheat surface: add/remove/clear over the stored set and
    //! parsing a fetched `.cht` body. Applying Game Genie patches to a cartridge
    //! is covered elsewhere; here we drive the ROM-less bookkeeping.
    use super::*;
    use crate::ports::{MemRumble, MemStorage, MemWebcam};

    fn test_ports() -> Ports {
        Ports {
            storage: Box::new(MemStorage::new()),
            rumble: Box::new(MemRumble::default()),
            webcam: Box::new(MemWebcam::default()),
        }
    }

    fn session() -> Session {
        Session::new(Config::default(), test_ports(), [0u8; 32])
    }

    #[test]
    fn add_remove_and_clear_track_the_active_set() {
        let mut s = session();
        // A GameShark RAM poke and a Game Genie ROM patch.
        s.add_cheat("01FFDEC0").expect("gameshark parses");
        s.add_cheat("00A-B7F-C61").expect("game genie parses");
        assert_eq!(s.cheats().count(), 2);

        // Adding the same raw code again is idempotent.
        s.add_cheat("01FFDEC0").unwrap();
        assert_eq!(s.cheats().count(), 2);

        assert!(s.remove_cheat("01FFDEC0"), "an active code is removed");
        assert!(!s.remove_cheat("01FFDEC0"), "a second remove reports nothing removed");
        assert_eq!(s.cheats().count(), 1);

        s.clear_cheats();
        assert_eq!(s.cheats().count(), 0);
    }

    #[test]
    fn add_cheat_rejects_a_malformed_code() {
        let mut s = session();
        assert!(s.add_cheat("nope").is_err());
        assert_eq!(s.cheats().count(), 0);
    }

    #[test]
    fn finish_fetched_cheats_counts_and_stores_them() {
        let mut s = session();
        let body = "cheats = 2\n\
                    cheat0_desc = \"Infinite Health\"\n\
                    cheat0_code = \"010AF4C6\"\n\
                    cheat1_desc = \"Max Lives\"\n\
                    cheat1_code = \"010999DA\"\n";
        assert_eq!(s.finish_fetched_cheats(body), 2);
        assert_eq!(s.fetched_cheats().len(), 2);

        // A later fetch replaces the previous pending list.
        assert_eq!(s.finish_fetched_cheats("cheats = 0\n"), 0);
        assert!(s.fetched_cheats().is_empty());
    }
}
