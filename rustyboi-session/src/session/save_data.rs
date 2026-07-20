//! Battery SRAM and RTC import/export, plus the storage-port persistence the
//! run loop drives.

use super::{log_config_error, Session, SessionError};

impl Session {
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
    pub(crate) fn hydrate_battery(&mut self) {
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
}
