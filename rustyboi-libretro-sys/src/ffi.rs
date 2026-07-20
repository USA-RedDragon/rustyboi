//! Hand-written libretro FFI — exactly the surface this core uses.
//!
//! Deliberately NOT bindgen-generated: owning these declarations keeps the
//! build free of a build-time libclang dependency (which made cross-compiling
//! painful and mis-typed `retro_key` on Windows), and pins the C ABI to the
//! upstream `libretro.h` layout. Every struct/const here is verified against the
//! canonical header layout. See <https://github.com/libretro/libretro-common>.
#![allow(non_camel_case_types)]

use std::os::raw::{c_char, c_uint, c_void};

pub const RETRO_API_VERSION: c_uint = 1;

// --- retro_pixel_format / retro_rumble_effect (C enums; int-sized) ---
pub type retro_pixel_format = c_uint;
pub const RETRO_PIXEL_FORMAT_XRGB8888: retro_pixel_format = 1;

pub type retro_rumble_effect = c_uint;
pub const RETRO_RUMBLE_STRONG: retro_rumble_effect = 0;
pub const RETRO_RUMBLE_WEAK: retro_rumble_effect = 1;

// --- log levels ---
pub type retro_log_level = c_uint;
pub const RETRO_LOG_INFO: retro_log_level = 1;
pub const RETRO_LOG_WARN: retro_log_level = 2;
pub const RETRO_LOG_ERROR: retro_log_level = 3;

// --- device / input ids ---
pub const RETRO_DEVICE_JOYPAD: c_uint = 1;
pub const RETRO_DEVICE_ID_JOYPAD_B: c_uint = 0;
pub const RETRO_DEVICE_ID_JOYPAD_SELECT: c_uint = 2;
pub const RETRO_DEVICE_ID_JOYPAD_START: c_uint = 3;
pub const RETRO_DEVICE_ID_JOYPAD_UP: c_uint = 4;
pub const RETRO_DEVICE_ID_JOYPAD_DOWN: c_uint = 5;
pub const RETRO_DEVICE_ID_JOYPAD_LEFT: c_uint = 6;
pub const RETRO_DEVICE_ID_JOYPAD_RIGHT: c_uint = 7;
pub const RETRO_DEVICE_ID_JOYPAD_A: c_uint = 8;

// --- region / memory ids ---
pub const RETRO_REGION_NTSC: c_uint = 0;
pub const RETRO_MEMORY_SAVE_RAM: c_uint = 0;
pub const RETRO_MEMORY_RTC: c_uint = 1;
pub const RETRO_MEMORY_SYSTEM_RAM: c_uint = 2;
pub const RETRO_MEMORY_VIDEO_RAM: c_uint = 3;

// --- memory-descriptor flags ---
pub const RETRO_MEMDESC_SYSTEM_RAM: u64 = 4;
pub const RETRO_MEMDESC_SAVE_RAM: u64 = 8;
pub const RETRO_MEMDESC_VIDEO_RAM: u64 = 16;

// --- environment commands (the subset we issue) ---
pub const RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY: c_uint = 9;
pub const RETRO_ENVIRONMENT_SET_PIXEL_FORMAT: c_uint = 10;
pub const RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS: c_uint = 11;
pub const RETRO_ENVIRONMENT_GET_VARIABLE: c_uint = 15;
pub const RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE: c_uint = 17;
pub const RETRO_ENVIRONMENT_SET_SUPPORT_NO_GAME: c_uint = 18;
pub const RETRO_ENVIRONMENT_GET_RUMBLE_INTERFACE: c_uint = 23;
pub const RETRO_ENVIRONMENT_GET_LOG_INTERFACE: c_uint = 27;
pub const RETRO_ENVIRONMENT_SET_GEOMETRY: c_uint = 37;
pub const RETRO_ENVIRONMENT_SET_MEMORY_MAPS: c_uint = 65572; // 0x10000 EXPERIMENTAL | 36
pub const RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2: c_uint = 67;

// --- callback typedefs (Option<fn> to match nullable C function pointers) ---
pub type retro_environment_t =
    Option<unsafe extern "C" fn(cmd: c_uint, data: *mut c_void) -> bool>;
pub type retro_video_refresh_t =
    Option<unsafe extern "C" fn(data: *const c_void, width: c_uint, height: c_uint, pitch: usize)>;
pub type retro_audio_sample_t = Option<unsafe extern "C" fn(left: i16, right: i16)>;
pub type retro_audio_sample_batch_t =
    Option<unsafe extern "C" fn(data: *const i16, frames: usize) -> usize>;
pub type retro_input_poll_t = Option<unsafe extern "C" fn()>;
pub type retro_input_state_t =
    Option<unsafe extern "C" fn(port: c_uint, device: c_uint, index: c_uint, id: c_uint) -> i16>;
pub type retro_set_rumble_state_t =
    Option<unsafe extern "C" fn(port: c_uint, effect: retro_rumble_effect, strength: u16) -> bool>;
// The frontend's logger is C-variadic printf; we only ever call it as
// ("%s\n", cstr), which needs no format parsing on our side.
pub type retro_log_printf_t =
    Option<unsafe extern "C" fn(level: retro_log_level, fmt: *const c_char, ...)>;

// --- structs (repr(C), field-for-field with libretro.h) ---
#[repr(C)]
pub struct retro_system_info {
    pub library_name: *const c_char,
    pub library_version: *const c_char,
    pub valid_extensions: *const c_char,
    pub need_fullpath: bool,
    pub block_extract: bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct retro_game_geometry {
    pub base_width: c_uint,
    pub base_height: c_uint,
    pub max_width: c_uint,
    pub max_height: c_uint,
    pub aspect_ratio: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct retro_system_timing {
    pub fps: f64,
    pub sample_rate: f64,
}

#[repr(C)]
pub struct retro_system_av_info {
    pub geometry: retro_game_geometry,
    pub timing: retro_system_timing,
}

#[repr(C)]
pub struct retro_game_info {
    pub path: *const c_char,
    pub data: *const c_void,
    pub size: usize,
    pub meta: *const c_char,
}

#[repr(C)]
pub struct retro_input_descriptor {
    pub port: c_uint,
    pub device: c_uint,
    pub index: c_uint,
    pub id: c_uint,
    pub description: *const c_char,
}

#[repr(C)]
pub struct retro_memory_descriptor {
    pub flags: u64,
    pub ptr: *mut c_void,
    pub offset: usize,
    pub start: usize,
    pub select: usize,
    pub disconnect: usize,
    pub len: usize,
    pub addrspace: *const c_char,
}

#[repr(C)]
pub struct retro_memory_map {
    pub descriptors: *const retro_memory_descriptor,
    pub num_descriptors: c_uint,
}

#[repr(C)]
pub struct retro_variable {
    pub key: *const c_char,
    pub value: *const c_char,
}

#[repr(C)]
pub struct retro_rumble_interface {
    pub set_rumble_state: retro_set_rumble_state_t,
}

#[repr(C)]
pub struct retro_log_callback {
    pub log: retro_log_printf_t,
}

// --- core options v2 ---
#[repr(C)]
#[derive(Clone, Copy)]
pub struct retro_core_option_value {
    pub value: *const c_char,
    pub label: *const c_char,
}

#[repr(C)]
pub struct retro_core_option_v2_category {
    pub key: *const c_char,
    pub desc: *const c_char,
    pub info: *const c_char,
}

#[repr(C)]
pub struct retro_core_option_v2_definition {
    pub key: *const c_char,
    pub desc: *const c_char,
    pub desc_categorized: *const c_char,
    pub info: *const c_char,
    pub info_categorized: *const c_char,
    pub category_key: *const c_char,
    pub values: [retro_core_option_value; 128],
    pub default_value: *const c_char,
}

#[repr(C)]
pub struct retro_core_options_v2 {
    pub categories: *mut retro_core_option_v2_category,
    pub definitions: *mut retro_core_option_v2_definition,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, size_of};

    // The environment-command ids the wrappers pass must match libretro.h; a
    // wrong id silently talks to the wrong frontend command. SET_MEMORY_MAPS is
    // the EXPERIMENTAL bit (0x10000) OR'd with 36.
    #[test]
    fn environment_command_ids() {
        assert_eq!(RETRO_ENVIRONMENT_SET_MEMORY_MAPS, 65572);
        assert_eq!(RETRO_ENVIRONMENT_SET_MEMORY_MAPS, 0x10000 | 36);
        assert_eq!(RETRO_ENVIRONMENT_GET_LOG_INTERFACE, 27);
    }

    // Joypad button ids are the RETRO_DEVICE_ID_JOYPAD_* ordinals the frontend
    // reports state under; these are the fixed libretro.h numbering.
    #[test]
    fn joypad_id_numbering() {
        assert_eq!(RETRO_DEVICE_ID_JOYPAD_B, 0);
        assert_eq!(RETRO_DEVICE_ID_JOYPAD_SELECT, 2);
        assert_eq!(RETRO_DEVICE_ID_JOYPAD_START, 3);
        assert_eq!(RETRO_DEVICE_ID_JOYPAD_UP, 4);
        assert_eq!(RETRO_DEVICE_ID_JOYPAD_DOWN, 5);
        assert_eq!(RETRO_DEVICE_ID_JOYPAD_LEFT, 6);
        assert_eq!(RETRO_DEVICE_ID_JOYPAD_RIGHT, 7);
        assert_eq!(RETRO_DEVICE_ID_JOYPAD_A, 8);
    }

    // Memory-descriptor flag bits distinguish the region kinds in a memory map.
    #[test]
    fn memdesc_flag_bits() {
        assert_eq!(RETRO_MEMDESC_SYSTEM_RAM, 4);
        assert_eq!(RETRO_MEMDESC_SAVE_RAM, 8);
        assert_eq!(RETRO_MEMDESC_VIDEO_RAM, 16);
    }

    // repr(C) layout self-consistency for the fixed-width (pointer-free) structs:
    // these totals are identical on every target, so a stray field or reorder
    // relative to libretro.h shows up as a size/align mismatch.
    #[test]
    fn fixed_struct_layout() {
        // 4 u32 + 1 f32.
        assert_eq!(size_of::<retro_game_geometry>(), 20);
        assert_eq!(align_of::<retro_game_geometry>(), 4);
        // 2 f64.
        assert_eq!(size_of::<retro_system_timing>(), 16);
        assert_eq!(align_of::<retro_system_timing>(), 8);
        // geometry (20, pad to 8) + timing (16) => 24 + 16.
        assert_eq!(size_of::<retro_system_av_info>(), 40);
        assert_eq!(align_of::<retro_system_av_info>(), 8);
    }

    // Pointer-bearing structs are checked against the platform pointer width so
    // the assertions hold on both 32- and 64-bit targets.
    #[test]
    fn pointer_struct_layout() {
        let p = size_of::<*const c_char>();
        assert_eq!(size_of::<retro_variable>(), 2 * p);
        assert_eq!(size_of::<retro_core_option_value>(), 2 * p);
    }

    // The frontend indexes into a fixed 128-entry value array; the length is
    // part of the ABI contract, so pin it here.
    #[test]
    fn option_v2_definition_values_len() {
        let def: retro_core_option_v2_definition = unsafe { std::mem::zeroed() };
        assert_eq!(def.values.len(), 128);
    }
}
