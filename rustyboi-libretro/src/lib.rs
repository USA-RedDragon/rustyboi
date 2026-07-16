//! libretro core frontend for the rustyboi Game Boy / Color emulator.
//!
//! Builds a `cdylib` that RetroArch (and other libretro frontends) can load.
//! Video is emitted as XRGB8888, audio as interleaved stereo i16 at 44.1 kHz,
//! input from the libretro joypad. Save states use the core's own
//! `GB::to_state_bytes` / `from_state_bytes` (length-prefixed) so RetroArch's
//! rewind, netplay and manual states all round-trip the full machine.
//!
//! This is a thin, `unsafe`-free adapter: all of the libretro C ABI lives in
//! [`rustyboi_libretro_sys`] (our own hand-written bindings + a safe `Core`
//! trait), so here we just `impl Core`. Like the desktop / web / Android
//! frontends it drives the shared [`rustyboi_session::Session`] — the run loop,
//! audio capture, cheat application, and savestate-restore logic live there.
//! What stays libretro-specific: `RETRO_MEMORY_*` exposure, memory maps for
//! RetroAchievements, geometry negotiation, the savestate length framing, and
//! the system-directory boot-ROM convention.

mod core_options;

use std::cell::Cell;
use std::ffi::CStr;
use std::rc::Rc;

use rustyboi_libretro_sys::{
    libretro_core, AvInfo, Core, Environment, Frame, Game, Geometry, MemoryDescriptor, MemoryKind,
    SystemInfo, RETRO_DEVICE_ID_JOYPAD_A, RETRO_DEVICE_ID_JOYPAD_B, RETRO_DEVICE_ID_JOYPAD_DOWN,
    RETRO_DEVICE_ID_JOYPAD_LEFT, RETRO_DEVICE_ID_JOYPAD_RIGHT, RETRO_DEVICE_ID_JOYPAD_SELECT,
    RETRO_DEVICE_ID_JOYPAD_START, RETRO_DEVICE_ID_JOYPAD_UP, RETRO_MEMORY_RTC,
    RETRO_MEMORY_SAVE_RAM, RETRO_MEMORY_SYSTEM_RAM, RETRO_MEMORY_VIDEO_RAM,
};

use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Hardware, GB};
use rustyboi_core_lib::ppu::{
    CgbColorConversion, SGB_FRAME_HEIGHT, SGB_FRAME_SIZE, SGB_FRAME_WIDTH,
};
use rustyboi_session::action::{GbcDmgPalette, HardwareChoice, PaletteChoice};
use rustyboi_session::ports::{MemStorage, MemWebcam};
use rustyboi_session::{
    frame_to_pixels, rgb_to_pixels, AbstractInput, Config, GbButton, PixelOrder, Ports, Rumble,
    Session,
};

const WIDTH: u32 = 160;
const HEIGHT: u32 = 144;
// SGB composited output (GB screen + 256x224 border).
const SGB_WIDTH: u32 = SGB_FRAME_WIDTH as u32;
const SGB_HEIGHT: u32 = SGB_FRAME_HEIGHT as u32;
// 4194304 Hz CPU clock / 70224 dots per frame => 59.7275 fps.
const FPS: f64 = 4194304.0 / 70224.0;
// rustyboi resamples APU output to a fixed host rate (see audio::controller).
const SAMPLE_RATE: f64 = 44100.0;
// Bytes of little-endian payload-length header prefixed to each savestate.
const SERIALIZE_HEADER_LEN: usize = 8;

/// The cartridge rumble motor as a [`Rumble`] port. `Session::run_frame` calls
/// `set` deep inside the frame step, where no `Frame` context is in scope, so
/// the adapter only records the state into a shared cell; `run` reads it
/// afterwards and drives the motors.
struct LibretroRumble {
    state: Rc<Cell<bool>>,
}
impl Rumble for LibretroRumble {
    fn set(&mut self, on: bool) {
        self.state.set(on);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum HardwarePref {
    Auto,
    Model(HardwareChoice),
}

struct RustyboiCore {
    session: Option<Session>,
    hardware_pref: HardwarePref,
    palette: PaletteChoice,
    gbc_dmg_palette: GbcDmgPalette,
    color_correction: CgbColorConversion,
    framebuffer: Vec<u8>,
    /// Shared with the [`LibretroRumble`] port; `run` reads and forwards it.
    rumble_state: Rc<Cell<bool>>,
    rumble_enabled: bool,
    use_real_boot_rom: bool,
    sgb_border_enabled: bool,
    // Tracks the geometry currently advertised so `run` only requests a new
    // geometry when the SGB border toggles the output dimensions.
    sgb_border_active: bool,
}

impl RustyboiCore {
    /// Resolve the configured model preference to a concrete core [`Hardware`],
    /// using the ROM header for `Auto`.
    fn pick_hardware(&self, rom: &[u8]) -> Hardware {
        match self.hardware_pref {
            HardwarePref::Model(choice) => choice.to_hardware(),
            // Header byte 0x143 bit 7 set => CGB-aware cartridge.
            HardwarePref::Auto => {
                if rom.get(0x143).is_some_and(|b| b & 0x80 != 0) {
                    Hardware::CGB
                } else {
                    Hardware::DMG
                }
            }
        }
    }

    /// Conventional RetroArch system-directory filename for the boot ROM of a
    /// given model (matches the de-facto libretro core naming convention).
    fn boot_rom_filename(hardware: Hardware) -> &'static str {
        match hardware {
            Hardware::DMG0 | Hardware::DMG => "dmg_boot.bin",
            Hardware::MGB => "mgb_boot.bin",
            Hardware::SGB => "sgb_boot.bin",
            Hardware::SGB2 => "sgb2_boot.bin",
            Hardware::AGB => "agb_boot.bin",
            Hardware::CGB0 | Hardware::CGBB | Hardware::CGB | Hardware::CGBE => "cgb_boot.bin",
        }
    }

    /// Read every core option from the frontend and apply the effects that can
    /// change live (colour correction, DMG palette). Hardware model,
    /// real-boot-ROM and SGB border only take effect on the next content load /
    /// geometry check, which is why this is safe to call from both load and
    /// options-changed.
    fn read_options(&mut self, env: &Environment) {
        if let Some(value) = env.get_variable(core_options::KEY_HARDWARE) {
            self.hardware_pref = HardwareChoice::from_option_id(&value)
                .map(HardwarePref::Model)
                .unwrap_or(HardwarePref::Auto);
        }
        if let Some(value) = env.get_variable(core_options::KEY_REAL_BOOT_ROM) {
            self.use_real_boot_rom = value == core_options::ON;
        }
        if let Some(value) = env.get_variable(core_options::KEY_SGB_BORDER) {
            self.sgb_border_enabled = value == core_options::ON;
        }
        if let Some(value) = env.get_variable(core_options::KEY_DMG_PALETTE) {
            self.palette = PaletteChoice::from_option_id(&value).unwrap_or(PaletteChoice::Grayscale);
            if let Some(session) = self.session.as_mut() {
                session.init_palette_choice(self.palette);
            }
        }
        if let Some(value) = env.get_variable(core_options::KEY_GBC_DMG_PALETTE) {
            self.gbc_dmg_palette =
                GbcDmgPalette::from_option_id(&value).unwrap_or(GbcDmgPalette::Auto);
            if let Some(session) = self.session.as_mut() {
                session.set_gbc_dmg_palette(self.gbc_dmg_palette);
            }
        }
        if let Some(value) = env.get_variable(core_options::KEY_GBC_COLOR_CORRECTION) {
            self.color_correction =
                core_options::parse_color_correction(&value).unwrap_or(CgbColorConversion::Lcd);
            if let Some(session) = self.session.as_mut() {
                session.set_color_correction(self.color_correction);
            }
        }
    }

    /// Publish the GB's live RAM regions to the frontend so RetroAchievements
    /// and RAM tools (and SAVE_RAM persistence) can see them. The regions are
    /// inside the heap-owned `GB`, which does not move for the content lifetime.
    fn publish_memory_maps(&mut self, env: &Environment) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let gb = session.gb_mut();

        let mut descriptors: Vec<MemoryDescriptor> = Vec::new();
        // Cartridge save RAM (0xA000-0xBFFF window).
        if let Some(cart) = gb.cartridge_mut() {
            descriptors.push(MemoryDescriptor::new(cart.save_ram_mut(), 0xA000, MemoryKind::SaveRam));
        }
        // System RAM: WRAM bank 0 (0xC000) then the switchable bank (0xD000).
        descriptors.push(MemoryDescriptor::new(gb.wram_bank0_mut(), 0xC000, MemoryKind::SystemRam));
        descriptors.push(MemoryDescriptor::new(gb.wram_bank1_mut(), 0xD000, MemoryKind::SystemRam));
        // High RAM (0xFF80-0xFFFE) and Video RAM (0x8000-0x9FFF), bank 0.
        descriptors.push(MemoryDescriptor::new(gb.hram_mut(), 0xFF80, MemoryKind::SystemRam));
        descriptors.push(MemoryDescriptor::new(gb.vram_mut(), 0x8000, MemoryKind::VideoRam));

        env.set_memory_maps(&descriptors);
    }

    fn gb_mut(&mut self) -> Option<&mut GB> {
        self.session.as_mut().map(|s| s.gb_mut())
    }
}

impl Core for RustyboiCore {
    fn info() -> SystemInfo {
        // Version as a 'static NUL-terminated CStr from the crate version.
        const VERSION: &CStr =
            match CStr::from_bytes_with_nul(concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes()) {
                Ok(c) => c,
                Err(_) => c"0",
            };
        SystemInfo {
            library_name: c"rustyboi",
            library_version: VERSION,
            valid_extensions: c"gb|gbc|zip",
            need_fullpath: false,
            block_extract: false,
        }
    }

    fn new() -> Self {
        RustyboiCore {
            session: None,
            hardware_pref: HardwarePref::Auto,
            palette: PaletteChoice::Grayscale,
            gbc_dmg_palette: GbcDmgPalette::Auto,
            color_correction: CgbColorConversion::Linear,
            // Sized for the largest possible frame (SGB 256x224) so the same
            // buffer serves both the plain 160x144 and the composited SGB paths.
            framebuffer: vec![0u8; SGB_FRAME_SIZE * 4],
            rumble_state: Rc::new(Cell::new(false)),
            rumble_enabled: false,
            use_real_boot_rom: false,
            sgb_border_enabled: false,
            sgb_border_active: false,
        }
    }

    fn set_environment(&mut self, env: &Environment) {
        // Game Boy always needs content; advertise no-content as unsupported.
        env.set_support_no_game(false);
        // Register the core-option table generated from the shared enums (single
        // source of truth — no hand-maintained option list).
        env.set_core_options(&core_options::build());
    }

    fn init(&mut self, env: &Environment) {
        env.set_joypad_descriptors(&[
            (RETRO_DEVICE_ID_JOYPAD_UP, c"Up"),
            (RETRO_DEVICE_ID_JOYPAD_DOWN, c"Down"),
            (RETRO_DEVICE_ID_JOYPAD_LEFT, c"Left"),
            (RETRO_DEVICE_ID_JOYPAD_RIGHT, c"Right"),
            (RETRO_DEVICE_ID_JOYPAD_A, c"A"),
            (RETRO_DEVICE_ID_JOYPAD_B, c"B"),
            (RETRO_DEVICE_ID_JOYPAD_START, c"Start"),
            (RETRO_DEVICE_ID_JOYPAD_SELECT, c"Select"),
        ]);
    }

    fn av_info(&self) -> AvInfo {
        // Base size is whatever is currently active (plain GB, or the SGB border
        // after a state that had it on). Max must always cover the SGB frame so a
        // later geometry change can grow the output to 256x224 without realloc.
        let (base_w, base_h) = if self.sgb_border_active {
            (SGB_WIDTH, SGB_HEIGHT)
        } else {
            (WIDTH, HEIGHT)
        };
        AvInfo {
            geometry: Geometry {
                base_width: base_w,
                base_height: base_h,
                max_width: SGB_WIDTH,
                max_height: SGB_HEIGHT,
                aspect_ratio: base_w as f32 / base_h as f32,
            },
            fps: FPS,
            sample_rate: SAMPLE_RATE,
        }
    }

    fn load_game(&mut self, game: &Game, env: &Environment) -> bool {
        if game.data.is_empty() {
            return false;
        }
        if !env.set_pixel_format_xrgb8888() {
            return false;
        }

        // Latch option values before building the machine so hardware model,
        // real-boot-ROM and border are up to date (the frontend may never fire
        // options-changed before first load).
        self.read_options(env);

        let rom = game.data.to_vec();
        let Ok(mut cartridge) = Cartridge::from_bytes(&rom) else {
            return false;
        };
        // RetroArch owns .srm/.rtc persistence via the memory-data hooks; make
        // sure the cart never opens or writes its own sidecar save file.
        cartridge.set_host_managed_saves(true);

        let hardware = self.pick_hardware(&rom);
        let mut gb = GB::new(hardware);
        gb.insert(cartridge);
        // Force the chosen CGB DMG-compat palette before booting (Auto = None).
        gb.set_forced_compat_palette(self.gbc_dmg_palette.forced_id());

        // Boot: run the real boot ROM from the system directory if asked and a
        // matching file exists; otherwise the synthetic post-boot state.
        let mut booted = false;
        if self.use_real_boot_rom
            && let Some(dir) = env.system_directory()
        {
            let path = dir.join(Self::boot_rom_filename(hardware));
            if let Some(path_str) = path.to_str()
                && gb.load_bios(path_str).is_ok()
                && gb.has_bios()
            {
                gb.run_boot_rom();
                booted = true;
            }
        }
        if !booted {
            gb.skip_bios();
        }

        // Determinism invariants: identity input map, rewind disabled (RetroArch
        // does its own), volume 100 (identity audio gain), presentation from opts.
        let config = Config {
            hardware,
            volume: 100,
            color_correction: self.color_correction,
            gbc_dmg_palette: self.gbc_dmg_palette,
            dmg_palette: rustyboi_session::config::DmgPalette {
                shades: self.palette.rgba_shades(),
            },
            rewind: rustyboi_session::config::RewindConfig {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        };
        self.rumble_state.set(false);
        let ports = Ports {
            storage: Box::new(MemStorage::new()),
            rumble: Box::new(LibretroRumble { state: self.rumble_state.clone() }),
            webcam: Box::new(MemWebcam::default()),
        };
        let rom_id = rustyboi_session::sha256(&rom);
        self.session = Some(Session::with_gb(Box::new(gb), config, ports, rom_id));

        // Geometry starts at the plain GB size; `run` switches to 256x224 the
        // first frame the SGB border becomes available.
        self.sgb_border_active = false;

        // Try to enable the rumble interface for MBC5 rumble carts.
        self.rumble_enabled = env.enable_rumble();

        self.publish_memory_maps(env);
        true
    }

    fn unload_game(&mut self, env: &Environment) {
        self.session = None;
        // The rumble state sent to the frontend persists until changed and `run`
        // stops driving it once the game is gone: a rumble cart unloaded with the
        // motor latched on would buzz forever. Stop it.
        if self.rumble_enabled {
            env.set_rumble(0, 0);
        }
    }

    fn reset(&mut self, env: &Environment) {
        if let Some(gb) = self.gb_mut() {
            gb.reset();
        }
        self.publish_memory_maps(env);
        self.rumble_state.set(false);
        if self.rumble_enabled {
            env.set_rumble(0, 0);
        }
    }

    fn options_changed(&mut self, env: &Environment) {
        self.read_options(env);
    }

    fn cheat_reset(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.clear_cheats();
        }
    }

    fn cheat_set(&mut self, _index: u32, enabled: bool, code: &str) {
        if !enabled {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        // A single cheat entry may contain several separator-joined codes; the
        // session auto-detects Game Genie vs GameShark per code.
        for part in code.split(['+', '\n', ' ']).map(str::trim).filter(|s| !s.is_empty()) {
            let _ = session.add_cheat(part);
        }
    }

    fn run(&mut self, frame: &mut Frame) {
        let Some(session) = self.session.as_mut() else {
            return;
        };

        let mut input = AbstractInput::none();
        input.set(GbButton::A, frame.pressed(RETRO_DEVICE_ID_JOYPAD_A));
        input.set(GbButton::B, frame.pressed(RETRO_DEVICE_ID_JOYPAD_B));
        input.set(GbButton::Start, frame.pressed(RETRO_DEVICE_ID_JOYPAD_START));
        input.set(GbButton::Select, frame.pressed(RETRO_DEVICE_ID_JOYPAD_SELECT));
        input.set(GbButton::Up, frame.pressed(RETRO_DEVICE_ID_JOYPAD_UP));
        input.set(GbButton::Down, frame.pressed(RETRO_DEVICE_ID_JOYPAD_DOWN));
        input.set(GbButton::Left, frame.pressed(RETRO_DEVICE_ID_JOYPAD_LEFT));
        input.set(GbButton::Right, frame.pressed(RETRO_DEVICE_ID_JOYPAD_RIGHT));

        // RTC persistence handshake: adopt `.rtc` data the frontend memcpy'd into
        // the RETRO_MEMORY_RTC region (with wall-clock catch-up), and refresh the
        // region so frontend (auto)saves read the current clock.
        if let Some(cart) = session.gb_mut().cartridge_mut() {
            cart.rtc_memory_frame_sync();
        }

        let out = session.run_frame(input);

        // SGB border compositing: only when the option is on and the game has
        // uploaded a border (else None and we render the plain 160x144 frame).
        let sgb_border = if self.sgb_border_enabled {
            session.gb().sgb_composited_frame()
        } else {
            None
        };

        let (draw_w, draw_h) = if let Some(border) = sgb_border {
            rgb_to_pixels(&border[..], PixelOrder::Bgra, &mut self.framebuffer);
            (SGB_WIDTH, SGB_HEIGHT)
        } else {
            let shades = self.palette.rgba_shades();
            frame_to_pixels(&out.frame, &shades, PixelOrder::Bgra, &mut self.framebuffer);
            (WIDTH, HEIGHT)
        };

        // Tell the frontend when the output size changed (SGB border toggling).
        // A geometry change keeps max_width/max_height, which av_info already
        // sized for the SGB frame, so no reallocation occurs.
        let border_now_active = draw_w == SGB_WIDTH;
        if border_now_active != self.sgb_border_active {
            self.sgb_border_active = border_now_active;
            frame.set_geometry(Geometry {
                base_width: draw_w,
                base_height: draw_h,
                max_width: SGB_WIDTH,
                max_height: SGB_HEIGHT,
                aspect_ratio: draw_w as f32 / draw_h as f32,
            });
        }

        frame.draw_xrgb8888(&self.framebuffer, draw_w, draw_h);

        // Drive the rumble motor for MBC5 rumble carts (state recorded by the
        // Rumble port during the frame step).
        if self.rumble_enabled {
            let strength = if self.rumble_state.get() { u16::MAX } else { 0 };
            frame.set_rumble(strength, strength);
        }

        // Emit interleaved stereo audio.
        let mut interleaved = Vec::with_capacity(out.audio.len() * 2);
        for (l, r) in out.audio {
            interleaved.push((l.clamp(-1.0, 1.0) * 32767.0) as i16);
            interleaved.push((r.clamp(-1.0, 1.0) * 32767.0) as i16);
        }
        frame.audio(&interleaved);
    }

    fn memory(&mut self, id: u32) -> Option<&mut [u8]> {
        let gb = self.gb_mut()?;
        match id {
            RETRO_MEMORY_SAVE_RAM => match gb.cartridge_mut() {
                Some(cart) if cart.has_battery() => Some(cart.save_ram_mut()),
                _ => None,
            },
            RETRO_MEMORY_RTC => match gb.cartridge_mut() {
                Some(cart) if cart.has_rtc() => Some(cart.rtc_memory_mut()),
                _ => None,
            },
            RETRO_MEMORY_SYSTEM_RAM => Some(gb.wram_bank0_mut()),
            RETRO_MEMORY_VIDEO_RAM => Some(gb.vram_mut()),
            _ => None,
        }
    }

    fn serialize_size(&mut self) -> usize {
        // Queried ONCE; the frontend pre-allocates savestate/rewind/netplay
        // buffers of this size, so it must be a stable upper bound. The state is
        // bincode (ROM held out); only the RLE-coded framebuffer portion drifts
        // with content. A 1/64 + 64 KiB pad plus the 8-byte header covers that;
        // serialize also guards the write.
        match self.gb_mut() {
            Some(gb) => match gb.to_state_bytes() {
                Ok(bytes) => SERIALIZE_HEADER_LEN + bytes.len() + bytes.len() / 64 + 64 * 1024,
                Err(_) => 0,
            },
            None => 0,
        }
    }

    fn serialize(&mut self, into: &mut [u8]) -> bool {
        let Some(gb) = self.gb_mut() else {
            return false;
        };
        let Ok(bytes) = gb.to_state_bytes() else {
            return false;
        };
        if SERIALIZE_HEADER_LEN + bytes.len() > into.len() {
            return false;
        }
        into[..SERIALIZE_HEADER_LEN].copy_from_slice(&(bytes.len() as u64).to_le_bytes());
        into[SERIALIZE_HEADER_LEN..SERIALIZE_HEADER_LEN + bytes.len()].copy_from_slice(&bytes);
        true
    }

    fn unserialize(&mut self, data: &[u8], env: &Environment) -> bool {
        if data.len() < SERIALIZE_HEADER_LEN {
            return false;
        }
        let len = u64::from_le_bytes(data[..SERIALIZE_HEADER_LEN].try_into().unwrap()) as usize;
        let Some(payload) = data.get(SERIALIZE_HEADER_LEN..SERIALIZE_HEADER_LEN + len) else {
            return false;
        };
        let payload = payload.to_vec();
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        let rom_id = session.rom_id();
        if session.finish_load_state(&payload, None, rom_id).is_err() {
            return false;
        }
        // Re-assert the libretro-only cart flag (RetroArch owns SRAM/RTC I/O).
        if let Some(cart) = session.gb_mut().cartridge_mut() {
            cart.set_host_managed_saves(true);
        }
        // The machine was replaced (new allocation), so re-publish the memory
        // descriptors that point into it for RetroAchievements / RAM tools.
        self.publish_memory_maps(env);
        true
    }
}

libretro_core!(RustyboiCore);

#[cfg(test)]
mod tests {
    use super::*;

    // Every emulated model must map to a boot-ROM filename the frontend looks up
    // in its system directory, and the DMG/CGB families share the standard names.
    #[test]
    fn boot_rom_filename_covers_every_hardware() {
        use rustyboi_core_lib::gb::Hardware::*;
        for hw in [DMG0, DMG, MGB, SGB, SGB2, AGB, CGB0, CGBB, CGB, CGBE] {
            let name = RustyboiCore::boot_rom_filename(hw);
            assert!(name.ends_with("_boot.bin"), "{hw:?} -> unexpected {name:?}");
        }
        assert_eq!(RustyboiCore::boot_rom_filename(DMG), "dmg_boot.bin");
        assert_eq!(RustyboiCore::boot_rom_filename(CGB), "cgb_boot.bin");
        assert_eq!(RustyboiCore::boot_rom_filename(CGBE), "cgb_boot.bin");
        assert_eq!(RustyboiCore::boot_rom_filename(SGB2), "sgb2_boot.bin");
    }

    // The SET_MEMORY_MAPS descriptors RetroArch holds are raw pointers into the
    // machine's (heap-boxed) RAM. Every event that re-allocates that RAM —
    // retro_reset (Mmio::reset builds a fresh Mmio) and retro_unserialize
    // (replace_machine swaps in a whole new GB) — must re-publish the maps, or
    // RetroAchievements/RAM tools read freed memory. This drives the real
    // dispatch entry points against a mock retro_environment callback and
    // asserts the freshly published pointers equal the live machine's buffers.
    mod memory_map_freshness {
        use super::*;
        use rustyboi_libretro_sys::dispatch;
        use rustyboi_libretro_sys::ffi;
        use std::ffi::{c_uint, c_void};
        use std::sync::Mutex;

        // (start, ptr, len) triples per SET_MEMORY_MAPS call, copied out
        // immediately (the descriptor array itself is transient).
        static CAPTURED: Mutex<Vec<Vec<(usize, usize, usize)>>> = Mutex::new(Vec::new());

        unsafe extern "C" fn mock_env(cmd: c_uint, data: *mut c_void) -> bool {
            match cmd {
                ffi::RETRO_ENVIRONMENT_SET_PIXEL_FORMAT => true,
                ffi::RETRO_ENVIRONMENT_SET_MEMORY_MAPS => {
                    let map = unsafe { &*(data as *const ffi::retro_memory_map) };
                    let descs = unsafe {
                        std::slice::from_raw_parts(map.descriptors, map.num_descriptors as usize)
                    };
                    CAPTURED
                        .lock()
                        .unwrap()
                        .push(descs.iter().map(|d| (d.start, d.ptr as usize, d.len)).collect());
                    true
                }
                _ => false,
            }
        }

        // Smallest ROM the loader accepts: 32 KiB no-MBC with a valid header
        // checksum (0x134..=0x14C).
        fn minimal_rom() -> Vec<u8> {
            let mut rom = vec![0u8; 0x8000];
            let mut chk: u8 = 0;
            for &b in &rom[0x134..=0x14C] {
                chk = chk.wrapping_sub(b).wrapping_sub(1);
            }
            rom[0x14D] = chk;
            rom
        }

        fn assert_last_publish_matches_live(core: &mut RustyboiCore) {
            let captured = CAPTURED.lock().unwrap();
            let last = captured.last().expect("no SET_MEMORY_MAPS captured").clone();
            drop(captured);
            let gb = core.gb_mut().expect("machine loaded");
            let expect = [
                (0xC000usize, gb.wram_bank0_mut().as_ptr() as usize),
                (0xD000, gb.wram_bank1_mut().as_ptr() as usize),
                (0xFF80, gb.hram_mut().as_ptr() as usize),
                (0x8000, gb.vram_mut().as_ptr() as usize),
            ];
            for (start, live_ptr) in expect {
                let published = last
                    .iter()
                    .find(|(s, _, _)| *s == start)
                    .unwrap_or_else(|| panic!("no descriptor published for {start:#06x}"));
                assert_eq!(
                    published.1, live_ptr,
                    "descriptor for {start:#06x} points at stale memory"
                );
            }
        }

        #[test]
        fn maps_republished_fresh_after_reset_and_unserialize() {
            let mut core = RustyboiCore::new();
            dispatch::set_environment(&mut core, Some(mock_env));
            dispatch::init(&mut core);

            let rom = minimal_rom();
            let info = ffi::retro_game_info {
                path: std::ptr::null(),
                data: rom.as_ptr() as *const c_void,
                size: rom.len(),
                meta: std::ptr::null(),
            };
            assert!(dispatch::load_game(&mut core, &info), "load_game failed");
            let after_load = CAPTURED.lock().unwrap().len();
            assert!(after_load >= 1, "load_game must publish memory maps");
            assert_last_publish_matches_live(&mut core);

            dispatch::reset(&mut core);
            assert!(
                CAPTURED.lock().unwrap().len() > after_load,
                "reset must re-publish memory maps"
            );
            assert_last_publish_matches_live(&mut core);

            let size = dispatch::serialize_size(&mut core);
            let mut state = vec![0u8; size];
            assert!(dispatch::serialize(&mut core, state.as_mut_ptr() as *mut c_void, size));
            let before_load_state = CAPTURED.lock().unwrap().len();
            assert!(dispatch::unserialize(&mut core, state.as_ptr() as *const c_void, size));
            assert!(
                CAPTURED.lock().unwrap().len() > before_load_state,
                "unserialize must re-publish memory maps"
            );
            assert_last_publish_matches_live(&mut core);
        }
    }
}
