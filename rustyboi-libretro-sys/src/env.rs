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
pub(crate) unsafe fn set_support_no_game(cb: retro_environment_t, supported: bool) {
    let mut v = supported;
    unsafe {
        call(cb, RETRO_ENVIRONMENT_SET_SUPPORT_NO_GAME, &mut v as *mut bool as *mut c_void);
    }
}

/// # Safety
/// `cb` must be the live environment callback.
pub(crate) unsafe fn set_pixel_format(cb: retro_environment_t, fmt: retro_pixel_format) -> bool {
    let mut f = fmt;
    unsafe { call(cb, RETRO_ENVIRONMENT_SET_PIXEL_FORMAT, &mut f as *mut _ as *mut c_void) }
}

/// # Safety
/// `cb` must be the live environment callback; `descriptors` must be
/// NUL-terminated (a final all-zero `retro_input_descriptor`) and outlive
/// content (the frontend keeps the `description` pointers).
pub(crate) unsafe fn set_input_descriptors(cb: retro_environment_t, descriptors: &[retro_input_descriptor]) {
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
pub(crate) unsafe fn get_variable_update(cb: retro_environment_t) -> bool {
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
pub(crate) unsafe fn get_variable(cb: retro_environment_t, key: &str) -> Option<String> {
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
pub(crate) unsafe fn set_core_options_v2(cb: retro_environment_t, opts: *const retro_core_options_v2) {
    unsafe { call(cb, RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2, opts as *mut c_void) };
}

/// # Safety
/// `cb` must be the live environment callback; the descriptor array referenced
/// by `map` must outlive the call, and the pointers it holds must stay valid for
/// as long as the frontend may read them (the lifetime of the content).
pub(crate) unsafe fn set_memory_maps(cb: retro_environment_t, mut map: retro_memory_map) {
    unsafe { call(cb, RETRO_ENVIRONMENT_SET_MEMORY_MAPS, &mut map as *mut _ as *mut c_void) };
}

/// # Safety
/// `cb` must be the live environment callback.
pub(crate) unsafe fn set_game_geometry(cb: retro_environment_t, mut geom: retro_game_geometry) {
    unsafe { call(cb, RETRO_ENVIRONMENT_SET_GEOMETRY, &mut geom as *mut _ as *mut c_void) };
}

/// The frontend's system directory, if it exposes one.
///
/// # Safety
/// `cb` must be the live environment callback.
pub(crate) unsafe fn get_system_directory(cb: retro_environment_t) -> Option<PathBuf> {
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
pub(crate) unsafe fn get_rumble_interface(cb: retro_environment_t) -> Option<retro_set_rumble_state_t> {
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

/// Ask for the frontend's logging interface; returns its `printf` fn if it has
/// one. Optional by design — a frontend without a logger is not an error, the
/// caller just falls back to stderr.
///
/// # Safety
/// `cb` must be the live environment callback.
pub(crate) unsafe fn get_log_interface(cb: retro_environment_t) -> retro_log_printf_t {
    let mut iface = retro_log_callback { log: None };
    let ok =
        unsafe { call(cb, RETRO_ENVIRONMENT_GET_LOG_INTERFACE, &mut iface as *mut _ as *mut c_void) };
    if ok {
        iface.log
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::raw::c_char;
    use std::sync::atomic::{AtomicU32, Ordering};

    // A None callback (frontend never installed one) must make every wrapper
    // report "unavailable" via the `call` early-out, never dereference.
    #[test]
    fn none_callback_is_inert() {
        unsafe {
            assert_eq!(get_variable(None, "any"), None);
            assert_eq!(get_system_directory(None), None);
            assert!(get_rumble_interface(None).is_none());
            assert!(get_log_interface(None).is_none());
            assert!(!get_variable_update(None));
        }
    }

    // The callback answering `false` (command unsupported) yields no value.
    unsafe extern "C" fn env_reject(_cmd: c_uint, _data: *mut c_void) -> bool {
        false
    }

    // Answers GET_VARIABLE but never fills in `value` (null) — must be None.
    unsafe extern "C" fn env_var_null(cmd: c_uint, _data: *mut c_void) -> bool {
        cmd == RETRO_ENVIRONMENT_GET_VARIABLE
    }

    static GET_VAR_CMD: AtomicU32 = AtomicU32::new(0);
    // Happy path: records the cmd id and returns a decodable value.
    unsafe extern "C" fn env_var_ok(cmd: c_uint, data: *mut c_void) -> bool {
        GET_VAR_CMD.store(cmd, Ordering::SeqCst);
        if cmd != RETRO_ENVIRONMENT_GET_VARIABLE {
            return false;
        }
        let var = unsafe { &mut *(data as *mut retro_variable) };
        var.value = c"chosen".as_ptr();
        true
    }

    #[test]
    fn get_variable_paths() {
        unsafe {
            // cmd rejected.
            assert_eq!(get_variable(Some(env_reject), "key"), None);
            // ok but value left null.
            assert_eq!(get_variable(Some(env_var_null), "key"), None);
            // interior NUL in the key never reaches the callback.
            assert_eq!(get_variable(Some(env_var_ok), "ba\0d"), None);
            // happy path decodes the frontend's selection.
            assert_eq!(get_variable(Some(env_var_ok), "key"), Some("chosen".to_string()));
        }
        assert_eq!(GET_VAR_CMD.load(Ordering::SeqCst), RETRO_ENVIRONMENT_GET_VARIABLE);
    }

    unsafe extern "C" fn env_sysdir_null(cmd: c_uint, _data: *mut c_void) -> bool {
        cmd == RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY
    }
    unsafe extern "C" fn env_sysdir_ok(cmd: c_uint, data: *mut c_void) -> bool {
        if cmd != RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY {
            return false;
        }
        unsafe { *(data as *mut *const c_char) = c"/rb/system".as_ptr() };
        true
    }

    #[test]
    fn get_system_directory_paths() {
        unsafe {
            assert_eq!(get_system_directory(Some(env_reject)), None);
            assert_eq!(get_system_directory(Some(env_sysdir_null)), None);
            assert_eq!(
                get_system_directory(Some(env_sysdir_ok)),
                Some(PathBuf::from("/rb/system"))
            );
        }
    }

    unsafe extern "C" fn dummy_rumble(_port: c_uint, _effect: retro_rumble_effect, _s: u16) -> bool {
        true
    }
    // ok, but leaves the interface fn None => treated as unavailable.
    unsafe extern "C" fn env_rumble_none(cmd: c_uint, _data: *mut c_void) -> bool {
        cmd == RETRO_ENVIRONMENT_GET_RUMBLE_INTERFACE
    }
    unsafe extern "C" fn env_rumble_ok(cmd: c_uint, data: *mut c_void) -> bool {
        if cmd != RETRO_ENVIRONMENT_GET_RUMBLE_INTERFACE {
            return false;
        }
        let iface = unsafe { &mut *(data as *mut retro_rumble_interface) };
        iface.set_rumble_state = Some(dummy_rumble);
        true
    }

    #[test]
    fn get_rumble_interface_paths() {
        unsafe {
            assert!(get_rumble_interface(Some(env_rumble_none)).is_none());
            assert!(get_rumble_interface(Some(env_rumble_ok)).is_some());
        }
    }

    unsafe extern "C" fn dummy_log_fixed(_level: retro_log_level, _fmt: *const c_char) {}
    // Rust can't *define* a C-variadic fn on stable (only declare the pointer
    // type), so the mock logger is a fixed-arity fn transmuted to the variadic
    // type. It is only stored and inspected here, never called.
    fn dummy_log() -> retro_log_printf_t {
        type Fixed = unsafe extern "C" fn(retro_log_level, *const c_char);
        type Variadic = unsafe extern "C" fn(retro_log_level, *const c_char, ...);
        Some(unsafe { std::mem::transmute::<Fixed, Variadic>(dummy_log_fixed) })
    }
    // ok, but leaves the log fn None => treated as unavailable.
    unsafe extern "C" fn env_log_none(cmd: c_uint, _data: *mut c_void) -> bool {
        cmd == RETRO_ENVIRONMENT_GET_LOG_INTERFACE
    }
    unsafe extern "C" fn env_log_ok(cmd: c_uint, data: *mut c_void) -> bool {
        if cmd != RETRO_ENVIRONMENT_GET_LOG_INTERFACE {
            return false;
        }
        let iface = unsafe { &mut *(data as *mut retro_log_callback) };
        iface.log = dummy_log();
        true
    }

    // A frontend with no logger (rejects the command, or accepts it but leaves
    // the fn null) must yield None so the caller falls back to stderr.
    #[test]
    fn get_log_interface_paths() {
        unsafe {
            assert!(get_log_interface(Some(env_reject)).is_none());
            assert!(get_log_interface(Some(env_log_none)).is_none());
            assert!(get_log_interface(Some(env_log_ok)).is_some());
        }
    }
}
