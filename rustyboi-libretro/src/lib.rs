//! libretro core frontend for the rustyboi Game Boy / Color emulator.
//!
//! Builds a `cdylib` that RetroArch (and other libretro frontends) can load.
//! Video is emitted as XRGB8888, audio as interleaved stereo i16 at 44.1 kHz,
//! input from the libretro joypad. Save states use the core's own
//! `GB::to_state_bytes` / `from_state_bytes` (length-prefixed) so RetroArch's
//! rewind, netplay and manual states all round-trip the full machine.
//!
//! Beyond A/V and input the core wires up: battery SRAM and MBC3 RTC persistence
//! (`RETRO_MEMORY_SAVE_RAM` / `RETRO_MEMORY_RTC`), Game Genie + GameShark cheats,
//! memory maps for RetroAchievements / RAM tools, MBC5 rumble, and DMG palette /
//! CGB colour-correction core options.

use rust_libretro::{
    contexts::*, core::Core, env_version, environment, input_descriptors, proc::CoreOptions,
    retro_core, sys::*, types::*,
};
use std::ffi::{c_void, CStr, CString};

use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Frame, Hardware, GB};
use rustyboi_core_lib::input::ButtonState;
use rustyboi_core_lib::ppu::{
    CgbColorConversion, SGB_FRAME_HEIGHT, SGB_FRAME_SIZE, SGB_FRAME_WIDTH,
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

// DMG four-shade palettes -> RGB. Index 0 = lightest, 3 = darkest.
const PALETTE_GRAYSCALE: [[u8; 3]; 4] = [
    [0xFF, 0xFF, 0xFF],
    [0xAA, 0xAA, 0xAA],
    [0x55, 0x55, 0x55],
    [0x00, 0x00, 0x00],
];
// Classic green "DMG" LCD tint.
const PALETTE_GREEN: [[u8; 3]; 4] = [
    [0xE0, 0xF8, 0xD0],
    [0x88, 0xC0, 0x70],
    [0x34, 0x68, 0x56],
    [0x08, 0x18, 0x20],
];
// Game Boy Pocket's cooler grayscale.
const PALETTE_POCKET: [[u8; 3]; 4] = [
    [0xC4, 0xCF, 0xA1],
    [0x8B, 0x95, 0x6D],
    [0x4D, 0x53, 0x3C],
    [0x1F, 0x1F, 0x1F],
];

#[derive(Clone, Copy, PartialEq)]
enum DmgPalette {
    Grayscale,
    Green,
    Pocket,
}

impl DmgPalette {
    fn table(self) -> &'static [[u8; 3]; 4] {
        match self {
            DmgPalette::Grayscale => &PALETTE_GRAYSCALE,
            DmgPalette::Green => &PALETTE_GREEN,
            DmgPalette::Pocket => &PALETTE_POCKET,
        }
    }
}

/// Audio sink registered with the core; the `GB` pushes generated samples here
/// during a frame, and `on_run` drains them afterwards.
#[derive(Clone, Default)]
struct SampleBuffer {
    // Arc<Mutex> (not Rc<RefCell>) so this AudioOutput sink is `Send` — required
    // since `GB::enable_audio` takes `Box<dyn AudioOutput + Send>` (so a cloned
    // GB stays Send for off-thread savestate serialization). Uncontended.
    samples: std::sync::Arc<std::sync::Mutex<Vec<(f32, f32)>>>,
}

impl rustyboi_core_lib::audio::AudioOutput for SampleBuffer {
    fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    fn add_samples(&mut self, samples: &[(f32, f32)]) {
        self.samples.lock().unwrap().extend_from_slice(samples);
    }
}

/// A parsed GameShark code: write `value` to RAM `address` every frame.
#[derive(Clone, Copy)]
struct GameSharkCode {
    address: u16,
    value: u8,
}

#[derive(CoreOptions)]
#[categories({
    "system_settings",
    "System",
    "Hardware emulation options."
},{
    "video_settings",
    "Video",
    "Palette and colour options."
})]
#[options({
    "rustyboi_hardware",
    "System > Hardware Model",
    "Hardware Model",
    "Which Game Boy model to emulate. 'Auto' picks CGB unless the ROM header marks it DMG-only. Takes effect on content reload.",
    "Which Game Boy model to emulate. Takes effect on content reload.",
    "system_settings",
    {
        { "auto", "Auto (CGB / DMG by header)" },
        { "dmg", "Game Boy (DMG)" },
        { "mgb", "Game Boy Pocket (MGB)" },
        { "sgb", "Super Game Boy (SGB)" },
        { "cgb", "Game Boy Color (CGB)" },
        { "agb", "Game Boy Advance (AGB / GBC mode)" },
    }
},{
    "rustyboi_real_boot_rom",
    "System > Use Real Boot ROM",
    "Use Real Boot ROM",
    "When enabled, run the real boot ROM from the frontend's system directory (e.g. 'dmg_boot.bin', 'cgb_boot.bin', 'sgb_boot.bin') instead of a synthetic post-boot state. Falls back to the built-in skip-boot state if the file is missing. Takes effect on content reload.",
    "Run the real boot ROM from the system directory if present.",
    "system_settings",
    {
        { "disabled", "Disabled" },
        { "enabled", "Enabled" },
    }
},{
    "rustyboi_sgb_border",
    "Video > Super Game Boy Border",
    "Super Game Boy Border",
    "On Super Game Boy hardware, output the 256x224 composited frame with the game's border. No effect on non-SGB models or until the game uploads a border.",
    "Show the Super Game Boy border (256x224 output).",
    "video_settings",
    {
        { "disabled", "Disabled" },
        { "enabled", "Enabled" },
    }
},{
    "rustyboi_dmg_palette",
    "Video > DMG Palette",
    "DMG Palette",
    "Colour palette used when rendering original Game Boy (monochrome) output.",
    "Colour palette for monochrome output.",
    "video_settings",
    {
        { "grayscale", "Grayscale" },
        { "green", "Green (DMG)" },
        { "pocket", "Game Boy Pocket" },
    }
},{
    "rustyboi_gbc_color_correction",
    "Video > GBC Colour Correction",
    "GBC Colour Correction",
    "Colour conversion for Game Boy Color output. 'Gambatte' approximates the real LCD; 'Linear' is the raw RGB555 values.",
    "Colour conversion for Game Boy Color output.",
    "video_settings",
    {
        { "linear", "Linear (raw)" },
        { "gambatte", "Gambatte (LCD)" },
    }
})]
struct RustyboiCore {
    gb: Option<GB>,
    audio: SampleBuffer,
    hardware_pref: HardwarePref,
    dmg_palette: DmgPalette,
    color_correction: CgbColorConversion,
    framebuffer: Vec<u8>,
    gameshark_codes: Vec<GameSharkCode>,
    rumble_enabled: bool,
    use_real_boot_rom: bool,
    sgb_border_enabled: bool,
    // Tracks the geometry currently advertised to the frontend so `on_run` only
    // issues a SET_GEOMETRY when the SGB border toggles the output dimensions.
    sgb_border_active: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum HardwarePref {
    Auto,
    Dmg,
    Mgb,
    Sgb,
    Cgb,
    Agb,
}

retro_core!(RustyboiCore {
    gb: None,
    audio: SampleBuffer::default(),
    hardware_pref: HardwarePref::Auto,
    dmg_palette: DmgPalette::Grayscale,
    color_correction: CgbColorConversion::Linear,
    // Sized for the largest possible frame (SGB 256x224) so the same buffer
    // serves both the plain 160x144 and the composited SGB paths.
    framebuffer: vec![0u8; SGB_FRAME_SIZE * 4],
    gameshark_codes: Vec::new(),
    rumble_enabled: false,
    use_real_boot_rom: false,
    sgb_border_enabled: false,
    sgb_border_active: false,
});

impl RustyboiCore {
    fn pick_hardware(&self, rom: &[u8]) -> Hardware {
        match self.hardware_pref {
            HardwarePref::Dmg => Hardware::DMG,
            HardwarePref::Mgb => Hardware::MGB,
            HardwarePref::Sgb => Hardware::SGB,
            HardwarePref::Cgb => Hardware::CGB,
            HardwarePref::Agb => Hardware::AGB,
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
    /// given model (matches the Gambatte / SameBoy core naming).
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
    /// change live (colour correction). Hardware model, real-boot-ROM and SGB
    /// border only take effect on the next content load / geometry check, which
    /// is why this is safe to call from both `on_load_game` and
    /// `on_options_changed`. Works off the raw environment callback so any
    /// context can drive it.
    ///
    /// # Safety
    /// `callback` must be a valid environment callback for the current session.
    unsafe fn read_options(&mut self, callback: retro_environment_t) {
        let get = |key: &'static str| unsafe { environment::get_variable(callback, key) };
        if let Some(value) = get("rustyboi_hardware") {
            self.hardware_pref = match value {
                "dmg" => HardwarePref::Dmg,
                "mgb" => HardwarePref::Mgb,
                "sgb" => HardwarePref::Sgb,
                "cgb" => HardwarePref::Cgb,
                "agb" => HardwarePref::Agb,
                _ => HardwarePref::Auto,
            };
        }
        if let Some(value) = get("rustyboi_real_boot_rom") {
            self.use_real_boot_rom = value == "enabled";
        }
        if let Some(value) = get("rustyboi_sgb_border") {
            self.sgb_border_enabled = value == "enabled";
        }
        if let Some(value) = get("rustyboi_dmg_palette") {
            self.dmg_palette = match value {
                "green" => DmgPalette::Green,
                "pocket" => DmgPalette::Pocket,
                _ => DmgPalette::Grayscale,
            };
        }
        if let Some(value) = get("rustyboi_gbc_color_correction") {
            self.color_correction = match value {
                "gambatte" => CgbColorConversion::Gambatte,
                _ => CgbColorConversion::Linear,
            };
            if let Some(gb) = self.gb.as_mut() {
                gb.set_cgb_color_conversion(self.color_correction);
            }
        }
    }

    /// Publish the GB's live RAM regions to the frontend so RetroAchievements
    /// and RAM tools (and SAVE_RAM persistence) can see them. Pointers are into
    /// the heap-owned `GB`, which does not move for the lifetime of the content.
    fn publish_memory_maps(&mut self, ctx: &GenericContext) {
        let Some(gb) = self.gb.as_mut() else {
            return;
        };

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
        // lifetime of `self.gb`.
        unsafe {
            environment::set_memory_maps(*ctx.environment_callback(), map);
        }
    }
}

/// Decode a Game Genie code (`AAA-BBB` or `AAA-BBB-CCC`) into
/// (address, new value, optional compare value). Returns None on malformed
/// input. Decoding is delegated to the single canonical implementation in
/// [`rustyboi_core_lib::cheats::decode_game_genie`] (mGBA-derived nibble
/// layout), shared with the session frontend.
fn parse_game_genie(code: &str) -> Option<(u16, u8, Option<u8>)> {
    let gg = rustyboi_core_lib::cheats::decode_game_genie(code)?;
    Some((gg.addr, gg.value, gg.compare))
}

/// Decode an 8-hex-digit GameShark code via the canonical core decoder
/// ([`rustyboi_core_lib::cheats::decode_gameshark`]): `AB` = external RAM bank
/// (ignored here), `CD` = new value, `GHEF` = little-endian target address.
fn parse_gameshark(code: &str) -> Option<GameSharkCode> {
    let gs = rustyboi_core_lib::cheats::decode_gameshark(code)?;
    Some(GameSharkCode { address: gs.addr, value: gs.value })
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

    fn on_set_environment(&mut self, _initial: bool, ctx: &mut SetEnvironmentContext) {
        // Game Boy always needs content; advertise no-content as unsupported.
        let gctx: GenericContext = ctx.into();
        unsafe {
            environment::set_support_no_game(*gctx.environment_callback(), false);
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
        // synthetic post-boot state so content always loads.
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

        gb.set_cgb_color_conversion(self.color_correction);
        gb.enable_audio(Box::new(self.audio.clone()))?;
        self.gb = Some(gb);
        self.gameshark_codes.clear();
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
        self.gb = None;
        self.gameshark_codes.clear();
        self.audio.samples.lock().unwrap().clear();
        // The rumble state sent to the frontend persists until changed and
        // `on_run` stops driving it once the game is gone: a rumble cart
        // unloaded with the motor latched on would buzz forever. Stop it.
        self.stop_rumble(ctx);
    }

    fn on_reset(&mut self, ctx: &mut ResetContext) {
        if let Some(gb) = self.gb.as_mut() {
            gb.reset();
        }
        self.audio.samples.lock().unwrap().clear();
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
        self.gameshark_codes.clear();
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
        // A single cheat entry may contain several "+"-separated codes.
        for part in text.split(['+', '\n', ' ']).map(str::trim).filter(|s| !s.is_empty()) {
            if part.contains('-') {
                // Game Genie ROM patch.
                if let Some((addr, new, compare)) = parse_game_genie(part)
                    && let Some(gb) = self.gb.as_mut()
                    && let Some(cart) = gb.cartridge_mut()
                {
                    cart.apply_rom_patch(addr, new, compare);
                }
            } else if let Some(gs) = parse_gameshark(part) {
                // GameShark RAM poke, applied every frame in on_run.
                self.gameshark_codes.push(gs);
            }
        }
    }

    #[inline]
    fn on_run(&mut self, ctx: &mut RunContext, _delta_us: Option<i64>) {
        let Some(gb) = self.gb.as_mut() else {
            return;
        };

        let pressed = |id| ctx.get_input_state(0, RETRO_DEVICE_JOYPAD, 0, id) != 0;
        gb.set_input_state(ButtonState {
            a: pressed(RETRO_DEVICE_ID_JOYPAD_A),
            b: pressed(RETRO_DEVICE_ID_JOYPAD_B),
            start: pressed(RETRO_DEVICE_ID_JOYPAD_START),
            select: pressed(RETRO_DEVICE_ID_JOYPAD_SELECT),
            up: pressed(RETRO_DEVICE_ID_JOYPAD_UP),
            down: pressed(RETRO_DEVICE_ID_JOYPAD_DOWN),
            left: pressed(RETRO_DEVICE_ID_JOYPAD_LEFT),
            right: pressed(RETRO_DEVICE_ID_JOYPAD_RIGHT),
        });

        // Apply GameShark RAM pokes before the frame runs.
        for gs in &self.gameshark_codes {
            gb.write_memory(gs.address, gs.value);
        }

        // RTC persistence handshake: adopt `.rtc` data the frontend memcpy'd
        // into the RETRO_MEMORY_RTC region (with wall-clock catch-up), and
        // refresh the region so frontend (auto)saves read the current clock.
        if let Some(cart) = gb.cartridge_mut() {
            cart.rtc_memory_frame_sync();
        }

        let (frame, _breakpoint) = gb.run_until_frame(true);

        // SGB border compositing: only when the option is on and the game has
        // uploaded a border (else `None` and we render the plain 160x144 GB
        // frame). The composited buffer is RGB888, 256x224.
        let sgb_border = if self.sgb_border_enabled {
            gb.sgb_composited_frame()
        } else {
            None
        };
        // Rumble state is read here while `gb` is still borrowed; applied below.
        let rumble_active = gb
            .cartridge()
            .map(|c| c.has_rumble() && c.rumble_active())
            .unwrap_or(false);

        let palette = self.dmg_palette.table();
        let (draw_w, draw_h) = if let Some(border) = sgb_border {
            for (i, chunk) in border.chunks_exact(3).enumerate() {
                let o = i * 4;
                self.framebuffer[o] = chunk[2]; // B
                self.framebuffer[o + 1] = chunk[1]; // G
                self.framebuffer[o + 2] = chunk[0]; // R
                self.framebuffer[o + 3] = 0xFF;
            }
            (SGB_WIDTH, SGB_HEIGHT)
        } else {
            match frame {
                Frame::Monochrome(data) => {
                    for (i, &shade) in data.iter().enumerate() {
                        let rgb = palette[(shade as usize) & 3];
                        let o = i * 4;
                        self.framebuffer[o] = rgb[2]; // B
                        self.framebuffer[o + 1] = rgb[1]; // G
                        self.framebuffer[o + 2] = rgb[0]; // R
                        self.framebuffer[o + 3] = 0xFF;
                    }
                }
                Frame::Color(data) => {
                    for (i, chunk) in data.chunks_exact(3).enumerate() {
                        let o = i * 4;
                        self.framebuffer[o] = chunk[2]; // B
                        self.framebuffer[o + 1] = chunk[1]; // G
                        self.framebuffer[o + 2] = chunk[0]; // R
                        self.framebuffer[o + 3] = 0xFF;
                    }
                }
            }
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

        // Drive the rumble motor for MBC5 rumble carts.
        if self.rumble_enabled {
            let strength = if rumble_active { u16::MAX } else { 0 };
            let gctx: GenericContext = (&mut *ctx).into();
            gctx.set_rumble_state(0, retro_rumble_effect::RETRO_RUMBLE_STRONG, strength);
            gctx.set_rumble_state(0, retro_rumble_effect::RETRO_RUMBLE_WEAK, strength);
        }

        let drained: Vec<(f32, f32)> = self.audio.samples.lock().unwrap().drain(..).collect();
        let mut interleaved = Vec::with_capacity(drained.len() * 2);
        for (l, r) in drained {
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
        let Some(gb) = self.gb.as_mut() else {
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
        let Some(gb) = self.gb.as_mut() else {
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
        match self.gb.as_ref() {
            Some(gb) => match gb.to_state_bytes() {
                Ok(bytes) => SERIALIZE_HEADER_LEN + bytes.len() + bytes.len() / 64 + 64 * 1024,
                Err(_) => 0,
            },
            None => 0,
        }
    }

    fn on_serialize(&mut self, slice: &mut [u8], _ctx: &mut SerializeContext) -> bool {
        let Some(gb) = self.gb.as_ref() else {
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

    fn on_unserialize(&mut self, slice: &mut [u8], _ctx: &mut UnserializeContext) -> bool {
        if slice.len() < SERIALIZE_HEADER_LEN {
            return false;
        }
        let len = u64::from_le_bytes(slice[..SERIALIZE_HEADER_LEN].try_into().unwrap()) as usize;
        let Some(payload) = slice.get(SERIALIZE_HEADER_LEN..SERIALIZE_HEADER_LEN + len) else {
            return false;
        };
        match GB::from_state_bytes(payload) {
            Ok(mut gb) => {
                // Audio sink isn't serialized; re-attach so sound resumes.
                let _ = gb.enable_audio(Box::new(self.audio.clone()));
                if let Some(cart) = gb.cartridge_mut() {
                    cart.set_host_managed_saves(true);
                }
                gb.set_cgb_color_conversion(self.color_correction);
                self.gb = Some(gb);
                true
            }
            Err(_) => false,
        }
    }
}
