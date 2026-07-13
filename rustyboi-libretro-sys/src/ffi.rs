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
