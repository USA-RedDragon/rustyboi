//! Numbered savestate slots and the quicksave pair.
//!
//! Slot blobs are `[SlotMeta header][machine state]` in the session's storage
//! port, keyed by ROM id so states never collide across games.

use super::{Session, SessionError, SlotMeta, QUICK_SLOT};
use crate::audio::CaptureSink;
use rustyboi_core_lib::gb::GB;

impl Session {
    /// Storage key for a numbered slot, namespaced by ROM id so states never
    /// collide across games.
    pub(super) fn slot_key(&self, slot: u32) -> String {
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
    /// The reserved quick slot shares the key prefix but is not a numbered
    /// slot, so it is excluded.
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
            .filter(|&n| n != QUICK_SLOT)
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

    pub(super) fn split_slot_blob(blob: &[u8]) -> Result<(SlotMeta, &[u8]), SessionError> {
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
    pub(super) fn restore_state(&mut self, state: &[u8]) -> Result<(), SessionError> {
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
}
