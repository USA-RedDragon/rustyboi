//! The bridge the [`libretro_core!`](crate::libretro_core) macro calls into:
//! each function takes the core instance (and raw C args), does the unsafe
//! pointer work, and calls the safe [`Core`] trait with a context. Keeping this
//! here means the consumer's entry points (macro-generated) carry no `unsafe`.
#![allow(clippy::missing_safety_doc)]

use crate::ffi::*;
use crate::{env, env_cb, environment, frame, poll_input, Core};
use std::ffi::{c_char, c_uint, c_void, CStr};

pub fn api_version() -> c_uint {
    RETRO_API_VERSION
}

pub fn get_system_info<C: Core>(info: *mut retro_system_info) {
    if info.is_null() {
        return;
    }
    let si = C::info();
    unsafe {
        (*info).library_name = si.library_name.as_ptr();
        (*info).library_version = si.library_version.as_ptr();
        (*info).valid_extensions = si.valid_extensions.as_ptr();
        (*info).need_fullpath = si.need_fullpath;
        (*info).block_extract = si.block_extract;
    }
}

pub fn set_environment<C: Core>(core: &mut C, cb: retro_environment_t) {
    crate::set_env_cb(cb);
    core.set_environment(&environment());
}

pub fn init<C: Core>(core: &mut C) {
    core.init(&environment());
}

pub fn deinit() {
    crate::clear();
}

pub fn get_system_av_info<C: Core>(core: &mut C, out: *mut retro_system_av_info) {
    if out.is_null() {
        return;
    }
    let av = core.av_info();
    unsafe {
        *out = retro_system_av_info {
            geometry: av.geometry.into(),
            timing: retro_system_timing {
                fps: av.fps,
                sample_rate: av.sample_rate,
            },
        };
    }
}

pub fn region<C: Core>(core: &mut C) -> c_uint {
    core.region()
}

pub fn load_game<C: Core>(core: &mut C, info: *const retro_game_info) -> bool {
    if info.is_null() {
        return false;
    }
    let info = unsafe { &*info };
    if info.data.is_null() || info.size == 0 {
        return false;
    }
    let data = unsafe { std::slice::from_raw_parts(info.data as *const u8, info.size) };
    let path = if info.path.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(info.path) }.to_str().ok()
    };
    core.load_game(&crate::Game { data, path }, &environment())
}

pub fn load_game_special() -> bool {
    false
}

pub fn unload_game<C: Core>(core: &mut C) {
    core.unload_game(&environment());
}

pub fn reset<C: Core>(core: &mut C) {
    core.reset(&environment());
}

pub fn run<C: Core>(core: &mut C) {
    poll_input();
    // Raw libretro has no options-changed callback: poll the update flag and
    // re-read options when the frontend reports a change.
    if unsafe { env::get_variable_update(env_cb()) } {
        core.options_changed(&environment());
    }
    core.run(&mut frame());
}

pub fn serialize_size<C: Core>(core: &mut C) -> usize {
    core.serialize_size()
}

pub fn serialize<C: Core>(core: &mut C, data: *mut c_void, size: usize) -> bool {
    if data.is_null() {
        return false;
    }
    let slice = unsafe { std::slice::from_raw_parts_mut(data as *mut u8, size) };
    core.serialize(slice)
}

pub fn unserialize<C: Core>(core: &mut C, data: *const c_void, size: usize) -> bool {
    if data.is_null() {
        return false;
    }
    let slice = unsafe { std::slice::from_raw_parts(data as *const u8, size) };
    core.unserialize(slice, &environment())
}

pub fn cheat_reset<C: Core>(core: &mut C) {
    core.cheat_reset();
}

pub fn cheat_set<C: Core>(core: &mut C, index: c_uint, enabled: bool, code: *const c_char) {
    if code.is_null() {
        return;
    }
    if let Ok(code) = unsafe { CStr::from_ptr(code) }.to_str() {
        core.cheat_set(index, enabled, code);
    }
}

pub fn memory_data<C: Core>(core: &mut C, id: c_uint) -> *mut c_void {
    match core.memory(id) {
        Some(region) if !region.is_empty() => region.as_mut_ptr() as *mut c_void,
        _ => std::ptr::null_mut(),
    }
}

pub fn memory_size<C: Core>(core: &mut C, id: c_uint) -> usize {
    core.memory(id).map_or(0, |r| r.len())
}
