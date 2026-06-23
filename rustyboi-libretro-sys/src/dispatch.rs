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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AvInfo, Environment, Frame, Game, Geometry, SystemInfo};

    // A minimal Core that records what dispatch handed it, so the guard/decode
    // logic in the wrappers can be observed without a real emulator.
    #[derive(Default)]
    struct FakeCore {
        loaded_path: Option<String>,
        loaded_len: usize,
        load_calls: u32,
        cheats: Vec<(u32, bool, String)>,
        system_ram: Vec<u8>,
        empty_region: Vec<u8>,
    }

    impl Core for FakeCore {
        fn info() -> SystemInfo {
            SystemInfo {
                library_name: c"fake",
                library_version: c"0",
                valid_extensions: c"gb",
                need_fullpath: false,
                block_extract: false,
            }
        }
        fn new() -> Self {
            FakeCore { system_ram: vec![1, 2, 3, 4], ..Default::default() }
        }
        fn av_info(&self) -> AvInfo {
            AvInfo {
                geometry: Geometry {
                    base_width: 1,
                    base_height: 1,
                    max_width: 1,
                    max_height: 1,
                    aspect_ratio: 1.0,
                },
                fps: 60.0,
                sample_rate: 44100.0,
            }
        }
        fn load_game(&mut self, game: &Game, _env: &Environment) -> bool {
            self.load_calls += 1;
            self.loaded_path = game.path.map(str::to_owned);
            self.loaded_len = game.data.len();
            true
        }
        fn run(&mut self, _frame: &mut Frame) {}
        fn cheat_set(&mut self, index: u32, enabled: bool, code: &str) {
            self.cheats.push((index, enabled, code.to_owned()));
        }
        fn memory(&mut self, id: c_uint) -> Option<&mut [u8]> {
            match id {
                2 => Some(&mut self.system_ram),
                9 => Some(&mut self.empty_region),
                _ => None,
            }
        }
    }

    fn game_info(path: *const c_char, data: &[u8]) -> retro_game_info {
        retro_game_info {
            path,
            data: data.as_ptr() as *const c_void,
            size: data.len(),
            meta: std::ptr::null(),
        }
    }

    #[test]
    fn api_version_is_one() {
        assert_eq!(api_version(), 1);
    }

    #[test]
    fn get_system_info_null_is_ignored() {
        // Null out-ptr must early-return, not deref.
        get_system_info::<FakeCore>(std::ptr::null_mut());

        let mut info: retro_system_info = unsafe { std::mem::zeroed() };
        get_system_info::<FakeCore>(&mut info);
        assert_eq!(info.library_name, FakeCore::info().library_name.as_ptr());
    }

    #[test]
    fn load_game_null_and_empty_guards() {
        let mut core = FakeCore::new();
        assert!(!load_game(&mut core, std::ptr::null()));

        let mut null_data = game_info(std::ptr::null(), &[]);
        null_data.data = std::ptr::null();
        null_data.size = 4;
        assert!(!load_game(&mut core, &null_data));

        let data = [0u8; 4];
        let mut zero_size = game_info(std::ptr::null(), &data);
        zero_size.size = 0;
        assert!(!load_game(&mut core, &zero_size));

        assert_eq!(core.load_calls, 0, "no guard-rejected call should reach the core");
    }

    #[test]
    fn load_game_path_decode() {
        let data = [0xAAu8; 8];

        // path set.
        let mut core = FakeCore::new();
        let path = std::ffi::CString::new("/roms/game.gb").unwrap();
        assert!(load_game(&mut core, &game_info(path.as_ptr(), &data)));
        assert_eq!(core.loaded_path.as_deref(), Some("/roms/game.gb"));
        assert_eq!(core.loaded_len, 8);

        // null path.
        let mut core = FakeCore::new();
        assert!(load_game(&mut core, &game_info(std::ptr::null(), &data)));
        assert_eq!(core.loaded_path, None);

        // invalid UTF-8 path decodes to None (0xFF is not valid UTF-8).
        let mut core = FakeCore::new();
        let bad: [c_char; 2] = [0xFFu8 as c_char, 0];
        assert!(load_game(&mut core, &game_info(bad.as_ptr(), &data)));
        assert_eq!(core.loaded_path, None);
    }

    #[test]
    fn serialize_unserialize_null_guards() {
        let mut core = FakeCore::new();
        assert!(!serialize(&mut core, std::ptr::null_mut(), 16));
        assert!(!unserialize(&mut core, std::ptr::null(), 16));
    }

    #[test]
    fn cheat_set_guards_and_decode() {
        let mut core = FakeCore::new();
        // null code pointer.
        cheat_set(&mut core, 0, true, std::ptr::null());
        // invalid UTF-8.
        let bad: [c_char; 2] = [0xFFu8 as c_char, 0];
        cheat_set(&mut core, 1, true, bad.as_ptr());
        assert!(core.cheats.is_empty());

        // valid code reaches the core with its args intact.
        let code = std::ffi::CString::new("ABCD-1234").unwrap();
        cheat_set(&mut core, 7, true, code.as_ptr());
        assert_eq!(core.cheats, vec![(7, true, "ABCD-1234".to_string())]);
    }

    #[test]
    fn memory_data_and_size_agree() {
        let mut core = FakeCore::new();

        // Present, non-empty region: data ptr non-null and matches size.
        let ptr = memory_data(&mut core, 2);
        assert!(!ptr.is_null());
        assert_eq!(memory_size(&mut core, 2), 4);
        assert_eq!(ptr as usize, core.system_ram.as_ptr() as usize);

        // Present but empty region collapses to null / 0.
        assert!(memory_data(&mut core, 9).is_null());
        assert_eq!(memory_size(&mut core, 9), 0);

        // Unknown id.
        assert!(memory_data(&mut core, 123).is_null());
        assert_eq!(memory_size(&mut core, 123), 0);
    }
}
