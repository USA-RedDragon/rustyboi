//! libretro core frontend for the rustyboi Game Boy / Color emulator.
//!
//! Builds a `cdylib` that RetroArch (and other libretro frontends) can load.
//! Video is emitted as XRGB8888, audio as interleaved stereo i16 at 44.1 kHz,
//! input from the libretro joypad. Save states use the core's own
//! `GB::to_state_bytes` / `from_state_bytes` (length-prefixed) so RetroArch's
//! rewind, netplay and manual states all round-trip the full machine.
//!
//! Like the desktop / web / Android frontends, this core is a thin adapter over
//! the shared [`rustyboi_session::Session`]: it implements the `Ports` seam
//! (a rumble adapter; storage/saves stay RetroArch-native) and drives
//! `Session::run_frame`, so the run loop, audio capture, cheat application, and
//! savestate-restore logic live in one place instead of being duplicated here.
//! What stays libretro-specific: the C ABI surface, `RETRO_MEMORY_*` exposure,
//! memory maps for RetroAchievements, geometry negotiation, the savestate
//! length framing, and the system-directory boot-ROM convention.

use rust_libretro::{
    contexts::*,
    core::{Core, CoreOptions},
    env_version, environment, input_descriptors, retro_core, sys::*, types::*,
};
use std::cell::Cell;
use std::ffi::{c_void, CStr, CString};
use std::rc::Rc;

mod core_options;

use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Hardware, GB};
use rustyboi_core_lib::ppu::{
    CgbColorConversion, SGB_FRAME_HEIGHT, SGB_FRAME_SIZE, SGB_FRAME_WIDTH,
};
use rustyboi_session::action::{HardwareChoice, PaletteChoice};
use rustyboi_session::ports::{MemStorage, MemWebcam};
use rustyboi_session::{
    frame_to_pixels, rgb_to_pixels, AbstractInput, Config, GbButton, PixelOrder, Ports, Rumble,
    Session,
};

/// C layout of `retro_game_info`. The published bindings expose these fields,
/// but bindgen can emit an opaque struct depending on the host libclang (the
/// header forward-declares the type), so we mirror the layout and reinterpret
/// the value the wrapper hands us as a raw pointer. This stays correct as long
/// as the frontend passes a real `retro_game_info`; see the README note about
/// libclang versions if content fails to load.
#[repr(C)]
struct GameInfo {
    path: *const std::os::raw::c_char,
    data: *const std::os::raw::c_void,
    size: usize,
    meta: *const std::os::raw::c_char,
}

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
/// `set` deep inside the frame step, where no libretro `RunContext` /
/// environment callback is in scope, so the adapter only records the state into
/// a shared cell; `on_run` reads it afterwards and issues `set_rumble_state`.
struct LibretroRumble {
    state: Rc<Cell<bool>>,
}

impl Rumble for LibretroRumble {
    fn set(&mut self, on: bool) {
        self.state.set(on);
    }
}

struct RustyboiCore {
    session: Option<Session>,
    hardware_pref: HardwarePref,
    palette: PaletteChoice,
    color_correction: CgbColorConversion,
    framebuffer: Vec<u8>,
    /// Shared with the [`LibretroRumble`] port; `on_run` reads and forwards it.
    rumble_state: Rc<Cell<bool>>,
    rumble_enabled: bool,
    use_real_boot_rom: bool,
    sgb_border_enabled: bool,
    // Tracks the geometry currently advertised to the frontend so `on_run` only
    // issues a SET_GEOMETRY when the SGB border toggles the output dimensions.
    sgb_border_active: bool,
    // Backing storage for the runtime-generated core-option table; kept alive so
    // the C string pointers handed to the frontend stay valid.
    option_storage: Option<core_options::OwnedOptions>,
}

// The option table is registered at runtime from the shared enums (see
// `on_set_environment` / `core_options`), so the derive-macro default is unused.
impl CoreOptions for RustyboiCore {}

#[derive(Clone, Copy, PartialEq)]
enum HardwarePref {
    Auto,
    Model(HardwareChoice),
}

retro_core!(RustyboiCore {
    session: None,
    hardware_pref: HardwarePref::Auto,
    palette: PaletteChoice::Grayscale,
    color_correction: CgbColorConversion::Linear,
    // Sized for the largest possible frame (SGB 256x224) so the same buffer
    // serves both the plain 160x144 and the composited SGB paths.
    framebuffer: vec![0u8; SGB_FRAME_SIZE * 4],
    rumble_state: Rc::new(Cell::new(false)),
    rumble_enabled: false,
    use_real_boot_rom: false,
    sgb_border_enabled: false,
    sgb_border_active: false,
    option_storage: None,
});

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

    /// Stop the rumble motor (both effects). The frontend keeps the last
    /// rumble state a core sent until it is overwritten, so this must be
    /// called whenever the emulated cart stops driving the motor line
    /// (content unload, core reset). No-op without a rumble interface.
    fn stop_rumble(&self, ctx: &GenericContext) {
        if self.rumble_enabled {
            ctx.set_rumble_state(0, retro_rumble_effect::RETRO_RUMBLE_STRONG, 0);
            ctx.set_rumble_state(0, retro_rumble_effect::RETRO_RUMBLE_WEAK, 0);
        }
    }

    /// Read every core option from the frontend and apply the effects that can
    /// change live (colour correction, DMG palette). Hardware model,
    /// real-boot-ROM and SGB border only take effect on the next content load /
    /// geometry check, which is why this is safe to call from both
    /// `on_load_game` and `on_options_changed`. Works off the raw environment
    /// callback so any context can drive it.
    ///
    /// # Safety
    /// `callback` must be a valid environment callback for the current session.
    unsafe fn read_options(&mut self, callback: retro_environment_t) {
        let get = |key: &'static str| unsafe { environment::get_variable(callback, key) };
        // Every key + value id comes from `core_options`, the same module that
        // generated the option table, so the two can never disagree (a mistyped
        // key would fail to resolve the constant at compile time).
        if let Some(value) = get(core_options::KEY_HARDWARE) {
            self.hardware_pref = HardwareChoice::from_option_id(value)
                .map(HardwarePref::Model)
                .unwrap_or(HardwarePref::Auto);
        }
        if let Some(value) = get(core_options::KEY_REAL_BOOT_ROM) {
            self.use_real_boot_rom = value == core_options::ON;
        }
        if let Some(value) = get(core_options::KEY_SGB_BORDER) {
            self.sgb_border_enabled = value == core_options::ON;
        }
        if let Some(value) = get(core_options::KEY_DMG_PALETTE) {
            self.palette =
                PaletteChoice::from_option_id(value).unwrap_or(PaletteChoice::Grayscale);
            if let Some(session) = self.session.as_mut() {
                session.init_palette_choice(self.palette);
            }
        }
        if let Some(value) = get(core_options::KEY_GBC_COLOR_CORRECTION) {
            self.color_correction =
                core_options::parse_color_correction(value).unwrap_or(CgbColorConversion::Linear);
            if let Some(session) = self.session.as_mut() {
                session.set_color_correction(self.color_correction);
            }
        }
    }

    /// Publish the GB's live RAM regions to the frontend so RetroAchievements
    /// and RAM tools (and SAVE_RAM persistence) can see them. Pointers are into
    /// the heap-owned `GB`, which does not move for the lifetime of the content.
    fn publish_memory_maps(&mut self, ctx: &GenericContext) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let gb = session.gb_mut();

        let mut descriptors: Vec<retro_memory_descriptor> = Vec::new();
        // `c_char` is i8 on x86 but u8 on ARM/Android; use the alias so the
        // `addrspace` field type matches on every target.
        let mut push = |ptr: *mut u8, len: usize, start: usize, flags: u64, name: *const std::os::raw::c_char| {
            if len == 0 {
                return;
            }
            descriptors.push(retro_memory_descriptor {
                flags,
                ptr: ptr as *mut c_void,
                offset: 0,
                start,
                select: 0,
                disconnect: 0,
                len,
                addrspace: name,
            });
        };

        // Cartridge save RAM (0xA000-0xBFFF window).
        if let Some(cart) = gb.cartridge_mut() {
            let sram = cart.save_ram_mut();
            push(
                sram.as_mut_ptr(),
                sram.len(),
                0xA000,
                RETRO_MEMDESC_SAVE_RAM as u64,
                std::ptr::null(),
            );
        }

        // System RAM: WRAM bank 0 (0xC000) then the switchable bank (0xD000).
        let wram0 = gb.wram_bank0_mut();
        push(
            wram0.as_mut_ptr(),
            wram0.len(),
            0xC000,
            RETRO_MEMDESC_SYSTEM_RAM as u64,
            std::ptr::null(),
        );
        let wram1 = gb.wram_bank1_mut();
        push(
            wram1.as_mut_ptr(),
            wram1.len(),
            0xD000,
            RETRO_MEMDESC_SYSTEM_RAM as u64,
            std::ptr::null(),
        );
        // High RAM (0xFF80-0xFFFE).
        let hram = gb.hram_mut();
        push(
            hram.as_mut_ptr(),
            hram.len(),
            0xFF80,
            RETRO_MEMDESC_SYSTEM_RAM as u64,
            std::ptr::null(),
        );
        // Video RAM (0x8000-0x9FFF), bank 0.
        let vram = gb.vram_mut();
        push(
            vram.as_mut_ptr(),
            vram.len(),
            0x8000,
            RETRO_MEMDESC_VIDEO_RAM as u64,
            std::ptr::null(),
        );

        let map = retro_memory_map {
            descriptors: descriptors.as_ptr(),
            num_descriptors: descriptors.len() as u32,
        };
        // SAFETY: `descriptors` outlives the call; the frontend copies the
        // descriptor array, and the pointers it stores stay valid for the
        // lifetime of `self.session`'s (heap-boxed) `GB`.
        unsafe {
            environment::set_memory_maps(*ctx.environment_callback(), map);
        }
    }

    /// The live machine, or `None` when no content is loaded.
    fn gb(&self) -> Option<&GB> {
        self.session.as_ref().map(|s| s.gb())
    }

    /// The live machine (mutable), or `None` when no content is loaded.
    fn gb_mut(&mut self) -> Option<&mut GB> {
        self.session.as_mut().map(|s| s.gb_mut())
    }
}

impl Core for RustyboiCore {
    fn get_info(&self) -> SystemInfo {
        SystemInfo {
            library_name: CString::new("rustyboi").unwrap(),
            library_version: CString::new(env_version!("CARGO_PKG_VERSION").to_string()).unwrap(),
            valid_extensions: CString::new("gb|gbc|zip").unwrap(),
            need_fullpath: false,
            block_extract: false,
        }
    }

    fn on_init(&mut self, ctx: &mut InitContext) {
        const INPUT_DESCRIPTORS: &[retro_input_descriptor] = &input_descriptors!(
            { 0, RETRO_DEVICE_JOYPAD, 0, RETRO_DEVICE_ID_JOYPAD_UP, "Up" },
            { 0, RETRO_DEVICE_JOYPAD, 0, RETRO_DEVICE_ID_JOYPAD_DOWN, "Down" },
            { 0, RETRO_DEVICE_JOYPAD, 0, RETRO_DEVICE_ID_JOYPAD_LEFT, "Left" },
            { 0, RETRO_DEVICE_JOYPAD, 0, RETRO_DEVICE_ID_JOYPAD_RIGHT, "Right" },
            { 0, RETRO_DEVICE_JOYPAD, 0, RETRO_DEVICE_ID_JOYPAD_A, "A" },
            { 0, RETRO_DEVICE_JOYPAD, 0, RETRO_DEVICE_ID_JOYPAD_B, "B" },
            { 0, RETRO_DEVICE_JOYPAD, 0, RETRO_DEVICE_ID_JOYPAD_START, "Start" },
            { 0, RETRO_DEVICE_JOYPAD, 0, RETRO_DEVICE_ID_JOYPAD_SELECT, "Select" },
        );

        let gctx: GenericContext = ctx.into();
        gctx.set_input_descriptors(INPUT_DESCRIPTORS);
    }

    fn on_set_environment(&mut self, initial: bool, ctx: &mut SetEnvironmentContext) {
        let cb = {
            let gctx: GenericContext = ctx.into();
            // SAFETY: the context's environment callback is valid for this call.
            unsafe { *gctx.environment_callback() }
        };
        unsafe {
            // Game Boy always needs content; advertise no-content as unsupported.
            environment::set_support_no_game(cb, false);
            // Register the core-option table generated from the shared enums
            // (single source of truth — no hand-maintained option list).
            if initial {
                let opts = core_options::build();
                environment::set_core_options_v2(cb, &opts.as_v2());
                self.option_storage = Some(opts);
            }
        }
    }

    fn on_get_av_info(&mut self, _ctx: &mut GetAvInfoContext) -> retro_system_av_info {
        // Base size is whatever is currently active (plain GB, or the SGB
        // border after a state that had it on). Max must always cover the SGB
        // frame so a later SET_GEOMETRY can grow the output to 256x224 without
        // reallocating (SET_GEOMETRY may not raise max_width/max_height).
        let (base_w, base_h) = if self.sgb_border_active {
            (SGB_WIDTH, SGB_HEIGHT)
        } else {
            (WIDTH, HEIGHT)
        };
        retro_system_av_info {
            geometry: retro_game_geometry {
                base_width: base_w,
                base_height: base_h,
                max_width: SGB_WIDTH,
                max_height: SGB_HEIGHT,
                aspect_ratio: base_w as f32 / base_h as f32,
            },
            timing: retro_system_timing {
                fps: FPS,
                sample_rate: SAMPLE_RATE,
            },
        }
    }

    fn on_get_region(&mut self, _ctx: &mut GetRegionContext) -> std::os::raw::c_uint {
        // Game Boy has no PAL/NTSC distinction; report NTSC.
        RETRO_REGION_NTSC
    }

    fn on_load_game(
        &mut self,
        info: Option<retro_game_info>,
        ctx: &mut LoadGameContext,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let info = info.ok_or("No content provided")?;
        // SAFETY: `retro_game_info` has the C layout mirrored by `GameInfo`.
        let info: GameInfo = unsafe { std::mem::transmute(info) };
        if info.data.is_null() || info.size == 0 {
            return Err("Empty content buffer".into());
        }

        if !ctx.set_pixel_format(PixelFormat::XRGB8888) {
            return Err("XRGB8888 is not supported by the frontend".into());
        }

        // Latch the current option values before building the machine so the
        // hardware model, real-boot-ROM and border choices are all up to date
        // (the frontend may never fire on_options_changed before first load).
        // SAFETY: `environment_callback` returns the live callback for this
        // session; we only read core options and the system directory with it.
        let callback = unsafe {
            let gctx: GenericContext = (&mut *ctx).into();
            *gctx.environment_callback()
        };
        unsafe {
            self.read_options(callback);
        }

        let rom = unsafe { std::slice::from_raw_parts(info.data as *const u8, info.size) }.to_vec();
        let mut cartridge = Cartridge::from_bytes(&rom)?;
        // RetroArch owns .srm/.rtc persistence via the memory-data hooks; make
        // sure the cart never opens or writes its own sidecar save file.
        cartridge.set_host_managed_saves(true);

        let hardware = self.pick_hardware(&rom);
        let mut gb = GB::new(hardware);
        gb.insert(cartridge);

        // Boot: run the real boot ROM from the system directory if the user
        // asked for it and a matching file exists; otherwise fall back to the
        // synthetic post-boot state so content always loads. (Boot ROM sourcing
        // stays libretro-specific — the system-dir filename convention — so this
        // does not go through the session's boot-ROM path.)
        let mut booted = false;
        if self.use_real_boot_rom {
            // SAFETY: valid environment callback.
            let sysdir = unsafe { environment::get_system_directory(callback) };
            if let Some(dir) = sysdir {
                let path = dir.join(Self::boot_rom_filename(hardware));
                if let Some(path_str) = path.to_str()
                    && gb.load_bios(path_str).is_ok()
                    && gb.has_bios()
                {
                    gb.run_boot_rom();
                    booted = true;
                }
            }
        }
        if !booted {
            gb.skip_bios();
        }

        // Build the session around the prepared machine. Determinism invariants
        // (see the module docs): identity input map (default), rewind disabled
        // (RetroArch does its own), volume 100 (identity audio gain), and the
        // colour-correction / palette / hardware pulled from the core options.
        let config = Config {
            hardware,
            volume: 100,
            color_correction: self.color_correction,
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
        // `with_gb` recovers the palette choice from `config.dmg_palette`, and
        // the colour correction was already applied to the machine — so no extra
        // presentation seeding is needed here.
        self.session = Some(Session::with_gb(Box::new(gb), config, ports, rom_id));

        // Geometry starts at the plain GB size; on_run switches to 256x224 the
        // first frame the SGB border becomes available.
        self.sgb_border_active = false;

        // Try to enable the rumble interface for MBC5 rumble carts.
        self.rumble_enabled = ctx.enable_rumble_interface().is_ok();

        let gctx: GenericContext = ctx.into();
        self.publish_memory_maps(&gctx);

        Ok(())
    }

    fn on_unload_game(&mut self, ctx: &mut UnloadGameContext) {
        self.session = None;
        // The rumble state sent to the frontend persists until changed and
        // `on_run` stops driving it once the game is gone: a rumble cart
        // unloaded with the motor latched on would buzz forever. Stop it.
        self.stop_rumble(ctx);
    }

    fn on_reset(&mut self, ctx: &mut ResetContext) {
        // Reset the core in place (matches the historical behavior and preserves
        // a real-boot-ROM machine); the session's own bookkeeping — which
        // libretro doesn't drive (rewind off, RetroArch owns savestates) — is
        // irrelevant here.
        if let Some(gb) = self.gb_mut() {
            gb.reset();
        }
        self.rumble_state.set(false);
        self.stop_rumble(ctx);
    }

    fn on_options_changed(&mut self, ctx: &mut OptionsChangedContext) {
        let gctx: GenericContext = ctx.into();
        // SAFETY: the context's environment callback is valid for this call.
        unsafe {
            self.read_options(*gctx.environment_callback());
        }
    }

    fn on_cheat_reset(&mut self, _ctx: &mut CheatResetContext) {
        if let Some(session) = self.session.as_mut() {
            session.clear_cheats();
        }
    }

    fn on_cheat_set(
        &mut self,
        _index: std::os::raw::c_uint,
        enabled: bool,
        code: &CStr,
        _ctx: &mut CheatSetContext,
    ) {
        if !enabled {
            return;
        }
        let Ok(text) = code.to_str() else {
            return;
        };
        let Some(session) = self.session.as_mut() else {
            return;
        };
        // A single cheat entry may contain several separator-joined codes; the
        // session auto-detects Game Genie vs GameShark per code.
        for part in text.split(['+', '\n', ' ']).map(str::trim).filter(|s| !s.is_empty()) {
            let _ = session.add_cheat(part);
        }
    }

    #[inline]
    fn on_run(&mut self, ctx: &mut RunContext, _delta_us: Option<i64>) {
        let Some(session) = self.session.as_mut() else {
            return;
        };

        let pressed = |id| ctx.get_input_state(0, RETRO_DEVICE_JOYPAD, 0, id) != 0;
        let mut input = AbstractInput::none();
        input.set(GbButton::A, pressed(RETRO_DEVICE_ID_JOYPAD_A));
        input.set(GbButton::B, pressed(RETRO_DEVICE_ID_JOYPAD_B));
        input.set(GbButton::Start, pressed(RETRO_DEVICE_ID_JOYPAD_START));
        input.set(GbButton::Select, pressed(RETRO_DEVICE_ID_JOYPAD_SELECT));
        input.set(GbButton::Up, pressed(RETRO_DEVICE_ID_JOYPAD_UP));
        input.set(GbButton::Down, pressed(RETRO_DEVICE_ID_JOYPAD_DOWN));
        input.set(GbButton::Left, pressed(RETRO_DEVICE_ID_JOYPAD_LEFT));
        input.set(GbButton::Right, pressed(RETRO_DEVICE_ID_JOYPAD_RIGHT));

        // RTC persistence handshake: adopt `.rtc` data the frontend memcpy'd
        // into the RETRO_MEMORY_RTC region (with wall-clock catch-up), and
        // refresh the region so frontend (auto)saves read the current clock.
        if let Some(cart) = session.gb_mut().cartridge_mut() {
            cart.rtc_memory_frame_sync();
        }

        // Drive one frame through the shared session (input remap, cheats,
        // rumble port, audio capture all happen inside).
        let out = session.run_frame(input);

        // SGB border compositing: only when the option is on and the game has
        // uploaded a border (else `None` and we render the plain 160x144 GB
        // frame). The composited buffer is RGB888, 256x224.
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

        // Tell the frontend when the output size changed (SGB border toggling
        // on/off). SET_GEOMETRY keeps max_width/max_height, which on_get_av_info
        // already sized for the SGB frame, so no reallocation occurs.
        let border_now_active = draw_w == SGB_WIDTH;
        if border_now_active != self.sgb_border_active {
            self.sgb_border_active = border_now_active;
            let geometry = retro_game_geometry {
                base_width: draw_w,
                base_height: draw_h,
                max_width: SGB_WIDTH,
                max_height: SGB_HEIGHT,
                aspect_ratio: draw_w as f32 / draw_h as f32,
            };
            let gctx: GenericContext = (&mut *ctx).into();
            // SAFETY: valid environment callback; SET_GEOMETRY is constant-time.
            unsafe {
                environment::set_game_geometry(*gctx.environment_callback(), geometry);
            }
        }

        ctx.draw_frame(&self.framebuffer, draw_w, draw_h, draw_w as usize * 4);

        // Drive the rumble motor for MBC5 rumble carts (state recorded by the
        // Rumble port during the frame step).
        if self.rumble_enabled {
            let strength = if self.rumble_state.get() { u16::MAX } else { 0 };
            let gctx: GenericContext = (&mut *ctx).into();
            gctx.set_rumble_state(0, retro_rumble_effect::RETRO_RUMBLE_STRONG, strength);
            gctx.set_rumble_state(0, retro_rumble_effect::RETRO_RUMBLE_WEAK, strength);
        }

        let mut interleaved = Vec::with_capacity(out.audio.len() * 2);
        for (l, r) in out.audio {
            interleaved.push((l.clamp(-1.0, 1.0) * 32767.0) as i16);
            interleaved.push((r.clamp(-1.0, 1.0) * 32767.0) as i16);
        }
        let audio_ctx: AudioContext = ctx.into();
        audio_ctx.batch_audio_samples(&interleaved);
    }

    fn get_memory_data(
        &mut self,
        id: std::os::raw::c_uint,
        _ctx: &mut GetMemoryDataContext,
    ) -> *mut c_void {
        let Some(gb) = self.gb_mut() else {
            return std::ptr::null_mut();
        };
        match id {
            RETRO_MEMORY_SAVE_RAM => match gb.cartridge_mut() {
                Some(cart) if cart.has_battery() => {
                    let sram = cart.save_ram_mut();
                    if sram.is_empty() {
                        std::ptr::null_mut()
                    } else {
                        sram.as_mut_ptr() as *mut c_void
                    }
                }
                _ => std::ptr::null_mut(),
            },
            RETRO_MEMORY_RTC => match gb.cartridge_mut() {
                Some(cart) if cart.has_rtc() => {
                    cart.rtc_memory_mut().as_mut_ptr() as *mut c_void
                }
                _ => std::ptr::null_mut(),
            },
            RETRO_MEMORY_SYSTEM_RAM => gb.wram_bank0_mut().as_mut_ptr() as *mut c_void,
            RETRO_MEMORY_VIDEO_RAM => gb.vram_mut().as_mut_ptr() as *mut c_void,
            _ => std::ptr::null_mut(),
        }
    }

    fn get_memory_size(
        &mut self,
        id: std::os::raw::c_uint,
        _ctx: &mut GetMemorySizeContext,
    ) -> usize {
        let Some(gb) = self.gb_mut() else {
            return 0;
        };
        match id {
            RETRO_MEMORY_SAVE_RAM => match gb.cartridge_mut() {
                Some(cart) if cart.has_battery() => cart.save_ram().len(),
                _ => 0,
            },
            RETRO_MEMORY_RTC => match gb.cartridge_mut() {
                Some(cart) if cart.has_rtc() => cart.rtc_memory_mut().len(),
                _ => 0,
            },
            // System RAM exposes fixed WRAM bank 0 here (the full 0xC000 bank).
            RETRO_MEMORY_SYSTEM_RAM => gb.wram_bank0_mut().len(),
            RETRO_MEMORY_VIDEO_RAM => gb.vram_mut().len(),
            _ => 0,
        }
    }

    fn get_serialize_size(&mut self, _ctx: &mut GetSerializeSizeContext) -> usize {
        // The frontend queries this ONCE and pre-allocates buffers of this size
        // for savestates, rewind and netplay, so it must be a stable upper bound
        // that never shrinks below any later `to_state_bytes` result. The state
        // is JSON: the bulk is the ROM bytes (constant for a given cart), and
        // only the small RAM/register portion drifts as digit widths change
        // (a 2-digit value growing to 3). A 1/64 + 64 KiB pad on top of the
        // current length plus the 8-byte header covers that drift with margin;
        // `on_serialize` also guards the write, so an under-estimate only fails
        // a state, never overflows.
        match self.gb() {
            Some(gb) => match gb.to_state_bytes() {
                Ok(bytes) => SERIALIZE_HEADER_LEN + bytes.len() + bytes.len() / 64 + 64 * 1024,
                Err(_) => 0,
            },
            None => 0,
        }
    }

    fn on_serialize(&mut self, slice: &mut [u8], _ctx: &mut SerializeContext) -> bool {
        let Some(gb) = self.gb() else {
            return false;
        };
        let Ok(bytes) = gb.to_state_bytes() else {
            return false;
        };
        // Length-prefixed: the buffer is fixed-size and zero-padded, and the
        // JSON deserializer rejects trailing NUL bytes, so store the exact
        // payload length up front and slice to it on load.
        if SERIALIZE_HEADER_LEN + bytes.len() > slice.len() {
            return false;
        }
        slice[..SERIALIZE_HEADER_LEN].copy_from_slice(&(bytes.len() as u64).to_le_bytes());
        slice[SERIALIZE_HEADER_LEN..SERIALIZE_HEADER_LEN + bytes.len()].copy_from_slice(&bytes);
        true
    }

    fn on_unserialize(&mut self, slice: &mut [u8], ctx: &mut UnserializeContext) -> bool {
        if slice.len() < SERIALIZE_HEADER_LEN {
            return false;
        }
        let len = u64::from_le_bytes(slice[..SERIALIZE_HEADER_LEN].try_into().unwrap()) as usize;
        let Some(payload) = slice.get(SERIALIZE_HEADER_LEN..SERIALIZE_HEADER_LEN + len) else {
            return false;
        };
        let payload = payload.to_vec();
        let Some(session) = self.session.as_mut() else {
            return false;
        };
        // Delegate the core-state restore (from_state_bytes → reattach the live
        // ROM → re-install audio → re-apply cheats + presentation) to the shared
        // session; keep the libretro-specific 8-byte length framing above.
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
        // `UnserializeContext` is itself a `GenericContext`.
        self.publish_memory_maps(&*ctx);
        true
    }
}
