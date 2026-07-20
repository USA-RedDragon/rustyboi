//! TAS: recording the live input stream and replaying a `.rbmovie`
//! deterministically.

use super::{Session, SessionError};
use crate::audio::CaptureSink;
use rustyboi_core_lib::gb::GB;
use crate::tas::{Playback, Recording};
use rustyboi_core_lib::movie::{self, Movie};

impl Session {
    /// Begin recording a power-on movie from the current input timeline. (For a
    /// re-record-from-here recording, use [`Session::start_recording_from_state`].)
    pub fn start_recording(&mut self) {
        self.recording = Some(Recording::power_on(self.rom_id, self.config.hardware));
    }

    /// Begin recording from the current machine state (re-record entry point):
    /// snapshots the live `GB` into the movie's start so replay reconstructs
    /// exactly here.
    pub(crate) fn start_recording_from_state(&mut self) -> Result<(), SessionError> {
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
    pub(crate) fn stop_playback(&mut self) {
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
}
