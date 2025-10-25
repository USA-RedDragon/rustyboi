//! libretro core frontend for the rustyboi Game Boy / Color emulator.
//!
//! Builds a `cdylib` that RetroArch (and other libretro frontends) can load.
//! Video is emitted as XRGB8888, audio as interleaved stereo i16 at 44.1 kHz,
//! input from the libretro joypad. Save states use bincode over the `GB` state.

use rust_libretro::{
    contexts::*, core::Core, env_version, input_descriptors, proc::CoreOptions, retro_core,
    sys::*, types::*,
};
use std::ffi::CString;

use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Frame, Hardware, GB};
use rustyboi_core_lib::input::ButtonState;
use rustyboi_core_lib::ppu::FRAMEBUFFER_SIZE;

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

// DMG grayscale shade -> XRGB8888 (matches the platform frontend's Grayscale).
const DMG_PALETTE: [[u8; 3]; 4] = [
    [0xFF, 0xFF, 0xFF],
    [0xAA, 0xAA, 0xAA],
    [0x55, 0x55, 0x55],
    [0x00, 0x00, 0x00],
];

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

#[derive(CoreOptions)]
#[categories({
    "system_settings",
    "System",
    "Hardware emulation options."
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
})]
struct RustyboiCore {
    gb: Option<GB>,
    audio: SampleBuffer,
    hardware_pref: HardwarePref,
    framebuffer: Vec<u8>,
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
    framebuffer: vec![0u8; FRAMEBUFFER_SIZE * 4],
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

    fn on_set_environment(&mut self, _initial: bool, _ctx: &mut SetEnvironmentContext) {}

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
        let cartridge = Cartridge::from_bytes(&rom)?;

        let hardware = self.pick_hardware(&rom);
        let mut gb = GB::new(hardware);
        gb.insert(cartridge);
        gb.skip_bios();
        gb.enable_audio(Box::new(self.audio.clone()))?;
        self.gb = Some(gb);

        Ok(())
    }

    fn on_unload_game(&mut self, _ctx: &mut UnloadGameContext) {
        self.gb = None;
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

        let (frame, _breakpoint) = gb.run_until_frame(true);
        match frame {
            Frame::Monochrome(data) => {
                for (i, &shade) in data.iter().enumerate() {
                    let rgb = DMG_PALETTE[(shade as usize) & 3];
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

        let drained: Vec<(f32, f32)> = self.audio.samples.borrow_mut().drain(..).collect();
        let mut interleaved = Vec::with_capacity(drained.len() * 2);
        for (l, r) in drained {
            interleaved.push((l.clamp(-1.0, 1.0) * 32767.0) as i16);
            interleaved.push((r.clamp(-1.0, 1.0) * 32767.0) as i16);
        }
        let audio_ctx: AudioContext = ctx.into();
        audio_ctx.batch_audio_samples(&interleaved);
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
                self.gb = Some(gb);
                true
            }
            Err(_) => false,
        }
    }
}
