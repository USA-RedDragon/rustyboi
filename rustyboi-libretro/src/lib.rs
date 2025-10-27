//! libretro core frontend for the rustyboi Game Boy / Color emulator.
//!
//! Builds a `cdylib` that RetroArch (and other libretro frontends) can load.
//! Video is emitted as XRGB8888, audio as interleaved stereo i16 at 44.1 kHz,
//! input from the libretro joypad. Save states use bincode over the `GB` state.
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
use rustyboi_core_lib::ppu::{CgbColorConversion, FRAMEBUFFER_SIZE};

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
// 4194304 Hz CPU clock / 70224 dots per frame.
const FPS: f64 = 4194304.0 / 70224.0;
// rustyboi resamples APU output to a fixed host rate (see audio::controller).
const SAMPLE_RATE: f64 = 44100.0;

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
    samples: std::rc::Rc<std::cell::RefCell<Vec<(f32, f32)>>>,
}

impl rustyboi_core_lib::audio::AudioOutput for SampleBuffer {
    fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    fn add_samples(&mut self, samples: &[(f32, f32)]) {
        self.samples.borrow_mut().extend_from_slice(samples);
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
        { "cgb", "Game Boy Color" },
        { "dmg", "Game Boy (DMG)" },
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
}

#[derive(Clone, Copy, PartialEq)]
enum HardwarePref {
    Auto,
    Cgb,
    Dmg,
}

retro_core!(RustyboiCore {
    gb: None,
    audio: SampleBuffer::default(),
    hardware_pref: HardwarePref::Auto,
    dmg_palette: DmgPalette::Grayscale,
    color_correction: CgbColorConversion::Linear,
    framebuffer: vec![0u8; FRAMEBUFFER_SIZE * 4],
    gameshark_codes: Vec::new(),
    rumble_enabled: false,
});

impl RustyboiCore {
    fn pick_hardware(&self, rom: &[u8]) -> Hardware {
        match self.hardware_pref {
            HardwarePref::Cgb => Hardware::CGB,
            HardwarePref::Dmg => Hardware::DMG,
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

    fn read_options(&mut self, ctx: &mut OptionsChangedContext) {
        if let Some(value) = ctx.get_variable("rustyboi_hardware") {
            self.hardware_pref = match value {
                "cgb" => HardwarePref::Cgb,
                "dmg" => HardwarePref::Dmg,
                _ => HardwarePref::Auto,
            };
        }
        if let Some(value) = ctx.get_variable("rustyboi_dmg_palette") {
            self.dmg_palette = match value {
                "green" => DmgPalette::Green,
                "pocket" => DmgPalette::Pocket,
                _ => DmgPalette::Grayscale,
            };
        }
        if let Some(value) = ctx.get_variable("rustyboi_gbc_color_correction") {
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
        let mut push = |ptr: *mut u8, len: usize, start: usize, flags: u64, name: *const i8| {
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
/// input. Format reference: 9 hex nibbles `ABCD EF GHI` where the new value is
/// `AB`, the address is `FCDE` xor 0xF000, and the optional compare is derived
/// from `GHI`.
fn parse_game_genie(code: &str) -> Option<(u16, u8, Option<u8>)> {
    let hex: Vec<u8> = code
        .chars()
        .filter(|c| *c != '-')
        .map(|c| c.to_digit(16).map(|d| d as u8))
        .collect::<Option<Vec<u8>>>()?;
    if hex.len() != 6 && hex.len() != 9 {
        return None;
    }
    let n = |i: usize| hex[i] as u16;
    let new = (hex[0] << 4) | hex[1];
    // Address nibbles: hex[2] hex[4] hex[5] hex[3], with the high nibble xor 0xF.
    let address = (n(5) << 12) | (n(2) << 8) | (n(3) << 4) | n(4);
    let address = address ^ 0xF000;
    let compare = if hex.len() == 9 {
        // Compare byte: rotate the 6-nibble tail and xor 0xBA (standard GG).
        let raw = (hex[6] << 4) | hex[8];
        let rotated = raw.rotate_right(2);
        Some(rotated ^ 0xBA)
    } else {
        None
    };
    Some((address, new, compare))
}

/// Decode an 8-hex-digit GameShark code `ABCDGHIJ`:
/// `AB` = external RAM bank (ignored here), `CD` = new value,
/// `GHIJ` = little-endian target address. Returns (address, value).
fn parse_gameshark(code: &str) -> Option<GameSharkCode> {
    let trimmed: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if trimmed.len() != 8 || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let value = u8::from_str_radix(&trimmed[2..4], 16).ok()?;
    let addr_lo = u8::from_str_radix(&trimmed[4..6], 16).ok()?;
    let addr_hi = u8::from_str_radix(&trimmed[6..8], 16).ok()?;
    let address = ((addr_hi as u16) << 8) | (addr_lo as u16);
    Some(GameSharkCode { address, value })
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
        retro_system_av_info {
            geometry: retro_game_geometry {
                base_width: WIDTH,
                base_height: HEIGHT,
                max_width: WIDTH,
                max_height: HEIGHT,
                aspect_ratio: WIDTH as f32 / HEIGHT as f32,
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

        let rom = unsafe { std::slice::from_raw_parts(info.data as *const u8, info.size) }.to_vec();
        let mut cartridge = Cartridge::from_bytes(&rom)?;
        // RetroArch owns .srm/.rtc persistence via the memory-data hooks; make
        // sure the cart never opens or writes its own sidecar save file.
        cartridge.set_host_managed_saves(true);

        let hardware = self.pick_hardware(&rom);
        let mut gb = GB::new(hardware);
        gb.insert(cartridge);
        gb.skip_bios();
        gb.set_cgb_color_conversion(self.color_correction);
        gb.enable_audio(Box::new(self.audio.clone()))?;
        self.gb = Some(gb);
        self.gameshark_codes.clear();

        // Try to enable the rumble interface for MBC5 rumble carts.
        self.rumble_enabled = ctx.enable_rumble_interface().is_ok();

        let gctx: GenericContext = ctx.into();
        self.publish_memory_maps(&gctx);

        Ok(())
    }

    fn on_unload_game(&mut self, _ctx: &mut UnloadGameContext) {
        self.gb = None;
        self.gameshark_codes.clear();
        self.audio.samples.borrow_mut().clear();
    }

    fn on_reset(&mut self, _ctx: &mut ResetContext) {
        if let Some(gb) = self.gb.as_mut() {
            gb.reset();
        }
        self.audio.samples.borrow_mut().clear();
    }

    fn on_options_changed(&mut self, ctx: &mut OptionsChangedContext) {
        self.read_options(ctx);
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

        let (frame, _breakpoint) = gb.run_until_frame(true);
        let palette = self.dmg_palette.table();
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
        ctx.draw_frame(&self.framebuffer, WIDTH, HEIGHT, WIDTH as usize * 4);

        // Drive the rumble motor for MBC5 rumble carts.
        if self.rumble_enabled {
            let active = gb
                .cartridge()
                .map(|c| c.has_rumble() && c.rumble_active())
                .unwrap_or(false);
            let strength = if active { u16::MAX } else { 0 };
            let gctx: GenericContext = (&mut *ctx).into();
            gctx.set_rumble_state(0, retro_rumble_effect::RETRO_RUMBLE_STRONG, strength);
            gctx.set_rumble_state(0, retro_rumble_effect::RETRO_RUMBLE_WEAK, strength);
        }

        let drained: Vec<(f32, f32)> = self.audio.samples.borrow_mut().drain(..).collect();
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
        match self.gb.as_ref() {
            // Headroom for bincode framing; state size is stable for a given ROM.
            Some(gb) => bincode::serialized_size(gb).map(|n| n as usize + 4096).unwrap_or(0),
            None => 0,
        }
    }

    fn on_serialize(&mut self, slice: &mut [u8], _ctx: &mut SerializeContext) -> bool {
        let Some(gb) = self.gb.as_ref() else {
            return false;
        };
        match bincode::serialize(gb) {
            Ok(bytes) if bytes.len() <= slice.len() => {
                slice[..bytes.len()].copy_from_slice(&bytes);
                true
            }
            _ => false,
        }
    }

    fn on_unserialize(&mut self, slice: &mut [u8], _ctx: &mut UnserializeContext) -> bool {
        match bincode::deserialize::<GB>(slice) {
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
