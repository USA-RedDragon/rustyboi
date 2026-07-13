//! Safe-ish wrappers over the libretro environment callback, for the handful of
//! `RETRO_ENVIRONMENT_*` commands this core issues. Each takes the raw callback
//! (`retro_environment_t`) the frontend gave us in `retro_set_environment`.

use crate::ffi::*;
use std::ffi::{c_uint, c_void, CStr, CString};
use std::path::PathBuf;

/// Invoke the environment callback with a command + data pointer.
///
/// # Safety
/// `cb` must be the live environment callback; `data` must point at a value of
/// the type `cmd` expects (or be null where the command permits).
unsafe fn call(cb: retro_environment_t, cmd: c_uint, data: *mut c_void) -> bool {
    match cb {
        Some(f) => unsafe { f(cmd, data) },
        None => false,
    }
}

/// # Safety
/// `cb` must be the live environment callback.
pub unsafe fn set_support_no_game(cb: retro_environment_t, supported: bool) {
    let mut v = supported;
    unsafe {
        call(cb, RETRO_ENVIRONMENT_SET_SUPPORT_NO_GAME, &mut v as *mut bool as *mut c_void);
    }
}

/// # Safety
/// `cb` must be the live environment callback.
pub unsafe fn set_pixel_format(cb: retro_environment_t, fmt: retro_pixel_format) -> bool {
    let mut f = fmt;
    unsafe { call(cb, RETRO_ENVIRONMENT_SET_PIXEL_FORMAT, &mut f as *mut _ as *mut c_void) }
}

/// # Safety
/// `cb` must be the live environment callback; `descriptors` must be
/// NUL-terminated (a final all-zero `retro_input_descriptor`) and outlive
/// content (the frontend keeps the `description` pointers).
pub unsafe fn set_input_descriptors(cb: retro_environment_t, descriptors: &[retro_input_descriptor]) {
    unsafe {
        call(
            cb,
            RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS,
            descriptors.as_ptr() as *mut c_void,
        );
    }
}

/// Whether any core-option value changed since the last query.
///
/// # Safety
/// `cb` must be the live environment callback.
pub unsafe fn get_variable_update(cb: retro_environment_t) -> bool {
    let mut updated = false;
    unsafe {
        call(
            cb,
            RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE,
            &mut updated as *mut bool as *mut c_void,
        );
    }
    updated
}

/// Query a core-option value by key. Returns the frontend's current selection.
///
/// # Safety
/// `cb` must be the live environment callback.
pub unsafe fn get_variable(cb: retro_environment_t, key: &str) -> Option<String> {
    let ckey = CString::new(key).ok()?;
    let mut var = retro_variable {
        key: ckey.as_ptr(),
        value: std::ptr::null(),
    };
    let ok = unsafe {
        call(cb, RETRO_ENVIRONMENT_GET_VARIABLE, &mut var as *mut _ as *mut c_void)
    };
    if !ok || var.value.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(var.value) }.to_str().ok().map(str::to_owned)
}

/// # Safety
/// `cb` must be the live environment callback; `opts` (and everything it points
/// at) must outlive the call — the frontend copies the strings.
pub unsafe fn set_core_options_v2(cb: retro_environment_t, opts: *const retro_core_options_v2) {
    unsafe { call(cb, RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2, opts as *mut c_void) };
}

/// # Safety
/// `cb` must be the live environment callback; the descriptor array referenced
/// by `map` must outlive the call, and the pointers it holds must stay valid for
/// as long as the frontend may read them (the lifetime of the content).
pub unsafe fn set_memory_maps(cb: retro_environment_t, mut map: retro_memory_map) {
    unsafe { call(cb, RETRO_ENVIRONMENT_SET_MEMORY_MAPS, &mut map as *mut _ as *mut c_void) };
}

/// # Safety
/// `cb` must be the live environment callback.
pub unsafe fn set_game_geometry(cb: retro_environment_t, mut geom: retro_game_geometry) {
    unsafe { call(cb, RETRO_ENVIRONMENT_SET_GEOMETRY, &mut geom as *mut _ as *mut c_void) };
}

/// The frontend's system directory, if it exposes one.
///
/// # Safety
/// `cb` must be the live environment callback.
pub unsafe fn get_system_directory(cb: retro_environment_t) -> Option<PathBuf> {
    let mut dir: *const std::os::raw::c_char = std::ptr::null();
    let ok = unsafe {
        call(cb, RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY, &mut dir as *mut _ as *mut c_void)
    };
    if !ok || dir.is_null() {
        return None;
    }
    let s = unsafe { CStr::from_ptr(dir) }.to_str().ok()?;
    Some(PathBuf::from(s))
}

/// Ask for the rumble interface; returns the `set_rumble_state` fn if available.
///
/// # Safety
/// `cb` must be the live environment callback.
pub unsafe fn get_rumble_interface(cb: retro_environment_t) -> Option<retro_set_rumble_state_t> {
    let mut iface = retro_rumble_interface { set_rumble_state: None };
    let ok = unsafe {
        call(cb, RETRO_ENVIRONMENT_GET_RUMBLE_INTERFACE, &mut iface as *mut _ as *mut c_void)
    };
    if ok && iface.set_rumble_state.is_some() {
        Some(iface.set_rumble_state)
    } else {
        None
    }
}
