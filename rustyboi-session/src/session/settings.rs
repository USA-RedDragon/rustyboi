//! Config-backed settings: the presentation state the shared `apply` owns and
//! the getter/setter pairs that persist through the storage port.

use super::{log_config_error, RunMode, Session, SessionError, GB_SIZE, SGB_SIZE};
use crate::action::{HardwareChoice, DmgPaletteChoice, ScalingMode, SgbPaletteChoice};
use crate::apply::palette_shades;
use crate::config::Config;

impl Session {
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
    pub(crate) fn set_sgb_border(&mut self, on: bool) {
        self.sgb_border = on;
    }

    /// Whether the on-screen touch overlay is shown.
    pub fn touch_controls(&self) -> bool {
        self.touch_controls
    }

    /// Set whether the on-screen touch overlay is shown.
    pub(crate) fn set_touch_controls(&mut self, on: bool) {
        self.touch_controls = on;
    }

    /// The current DMG presentation palette choice.
    pub fn palette(&self) -> DmgPaletteChoice {
        self.palette
    }

    /// Whether the SGB border is actually being presented this frame (toggle on
    /// AND the machine offers a composite).
    pub(crate) fn showing_sgb_border(&self) -> bool {
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
    pub(crate) fn set_hardware_choice(&mut self, choice: HardwareChoice) {
        self.config.hardware = choice.to_hardware();
        let gb = self.rebuild_current_gb();
        self.replace_machine(*gb, self.rom_id);
        self.persist_config();
    }

    /// Change the DMG presentation palette; persists the config.
    pub(crate) fn set_palette_choice(&mut self, choice: DmgPaletteChoice) {
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

    /// The SGB colorization choice for DMG games (Auto / a system palette /
    /// Grayscale).
    pub fn sgb_palette(&self) -> SgbPaletteChoice {
        self.config.sgb_palette
    }

    /// Change the SGB colorization for DMG games; persists the config.
    /// Presentation-only — applied live, no machine rebuild.
    pub(crate) fn set_sgb_palette(&mut self, choice: SgbPaletteChoice) {
        self.init_sgb_palette(choice);
        self.persist_config();
    }

    /// Seed the SGB colorization without persisting (startup, from the
    /// CLI/config-derived choice).
    pub fn init_sgb_palette(&mut self, choice: SgbPaletteChoice) {
        self.config.sgb_palette = choice;
        self.gb.set_sgb_palette(choice);
    }

    /// Whether the SGB palette setting applies to the loaded machine: only on
    /// SGB/SGB2 hardware, where the SNES-side firmware colourizes mono output.
    /// Reads the machine (not the config) so it cannot drift from what is
    /// actually running.
    pub fn sgb_palette_active(&self) -> bool {
        self.gb.sgb().is_some()
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
    pub(crate) fn set_real_boot_rom(&mut self, enabled: bool) {
        self.config.use_real_boot_rom = enabled;
        let gb = self.rebuild_current_gb();
        self.replace_machine(*gb, self.rom_id);
        self.persist_config();
    }

    /// Install (or clear) the SNES-side SGB firmware the adapter resolved from
    /// a file — `bios/sgb1.sfc` for SGB hardware, `bios/sgb2.sfc` for SGB2.
    /// Applies to the running machine immediately and to every later rebuild.
    ///
    /// The border is presentation state, not boot state, so this deliberately
    /// does NOT rebuild the machine: a running game keeps playing and simply
    /// gains its default border. A dump that fails validation is retained but
    /// has no effect (see [`GB::load_sgb_firmware_bytes`]).
    pub fn set_sgb_firmware(&mut self, bytes: Option<Vec<u8>>) {
        self.sgb_firmware = bytes;
        if let Some(fw) = self.sgb_firmware.as_deref() {
            let _ = self.gb.load_sgb_firmware_bytes(fw);
        }
    }

    /// Finish an SGB-firmware import: store the resolved bytes and install them
    /// into the running machine.
    pub fn finish_load_sgb_firmware(&mut self, bytes: &[u8]) {
        self.set_sgb_firmware(Some(bytes.to_vec()));
    }

    /// Whether the running machine has an SGB system border from firmware.
    pub fn has_sgb_firmware(&self) -> bool {
        self.gb.has_sgb_firmware()
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
    pub(crate) fn set_printer_scale(&mut self, scale: u8) {
        self.config.printer_scale = scale.max(1);
        self.persist_config();
    }

    /// The on-screen touch control opacity (0..=100 percent).
    pub fn touch_opacity(&self) -> u8 {
        self.config.touch_opacity.min(100)
    }

    /// Set the on-screen touch control opacity (clamped 0..=100) and persist it.
    pub(crate) fn set_touch_opacity(&mut self, opacity: u8) {
        self.config.touch_opacity = opacity.min(100);
        self.persist_config();
    }

    /// Whether the on-screen FPS overlay is shown.
    pub fn show_fps(&self) -> bool {
        self.config.show_fps
    }

    /// Enable/disable the on-screen FPS overlay; persists the config.
    pub(crate) fn set_show_fps(&mut self, on: bool) {
        self.config.show_fps = on;
        self.persist_config();
    }

    /// Enable/disable rewind capture; persists the config.
    pub(crate) fn set_rewind_enabled(&mut self, enabled: bool) {
        self.config.rewind.enabled = enabled;
        self.rewind
            .reconfigure(self.config.rewind.depth, self.config.rewind.interval_frames);
        self.persist_config();
    }

    /// Set the rewind snapshot interval (frames between captures, ≥ 1);
    /// persists the config.
    pub(crate) fn set_rewind_interval(&mut self, interval_frames: u32) {
        self.config.rewind.interval_frames = interval_frames.max(1);
        self.rewind
            .reconfigure(self.config.rewind.depth, self.config.rewind.interval_frames);
        self.persist_config();
    }

    /// Set how many rewind snapshots are retained (≥ 1); persists the config.
    pub(crate) fn set_rewind_depth(&mut self, depth: usize) {
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
    pub(crate) fn set_graphics_backend(&mut self, backend: crate::action::GraphicsBackend) {
        self.config.graphics_backend = backend;
        self.persist_config();
    }

    /// Currently requested rendering backend.
    pub fn graphics_backend(&self) -> crate::action::GraphicsBackend {
        self.config.graphics_backend
    }

    /// Replace the rebindable input map (GB-button bindings + chord hotkeys);
    /// persists the config. The adapter's next `resolve` call sees the new map.
    pub(crate) fn set_input_config(&mut self, input: crate::input_config::InputConfig) {
        self.config.input = input;
        self.persist_config();
    }

    /// The current rebindable input map.
    pub fn input_config(&self) -> &crate::input_config::InputConfig {
        &self.config.input
    }

    /// The full UI read-model for the menus, assembled from the session's own
    /// accessors.
    ///
    /// This is the *only* producer of [`SessionUiState`]: frontends call it and
    /// override only fields the host owns (the web worker tracks `has_rom`
    /// itself, since its ROM bytes arrive over the worker boundary). Both
    /// windowed frontends used to hand-copy all 35 fields, which had already
    /// drifted — desktop read `config.volume` raw where web read the clamped
    /// `volume()`.
    pub fn ui_state(&self) -> crate::action::SessionUiState {
        let cfg = &self.config;
        crate::action::SessionUiState {
            hardware: self.hardware_choice(),
            palette: self.palette(),
            gbc_dmg_palette: self.gbc_dmg_palette(),
            dmg_palette_active: self.dmg_palette_active(),
            sgb_palette: self.sgb_palette(),
            sgb_palette_active: self.sgb_palette_active(),
            color_correction: self.color_correction(),
            use_real_boot_rom: self.use_real_boot_rom(),
            texture_filter: self.texture_filter(),
            lcd_effect: self.lcd_effect(),
            printer_scale: self.printer_scale(),
            touch_opacity: self.touch_opacity(),
            rewind_enabled: cfg.rewind.enabled,
            rewind_interval_frames: cfg.rewind.interval_frames,
            rewind_depth: cfg.rewind.depth,
            volume: self.volume(),
            scaling: self.scaling_mode(),
            graphics_backend: self.graphics_backend(),
            sgb_border: self.sgb_border(),
            paused: self.is_paused(),
            fast_forward: self.is_fast_forward(),
            fast_forward_factor: self.fast_forward_factor(),
            touch_controls: self.touch_controls(),
            show_fps: self.show_fps(),
            printer_attached: self.gb().printer_attached(),
            recording: self.is_recording(),
            replaying: self.is_playing(),
            slots: self.list_slots(),
            cheats: self.cheats().map(str::to_owned).collect(),
            fetched_cheats: self.fetched_cheats().to_vec(),
            has_battery: self.has_battery(),
            has_rtc: self.has_rtc(),
            has_rom: self.gb().has_rom(),
            game_name: self.game_name().map(str::to_owned),
            input: self.input_config().clone(),
        }
    }

    fn persist_config(&mut self) {
        if let Err(e) = self.save_config() {
            log_config_error(&e);
        }
    }
}
