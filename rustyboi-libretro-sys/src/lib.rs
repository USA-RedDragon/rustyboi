//! A small, safe libretro core framework for rustyboi.
//!
//! [`ffi`] is the hand-written C ABI (no bindgen → no build-time libclang, and
//! full control of the layout). Everything unsafe — the C entry points, the
//! frontend callback pointers, the environment-command plumbing — lives here,
//! behind a safe [`Core`] trait and the [`Environment`] / [`Frame`] contexts.
//! A libretro core is then just `impl Core for MyCore {}` plus
//! [`libretro_core!`], with no `unsafe` in the consumer.
//!
//! libretro drives a core single-threaded (one frontend thread issues every
//! `retro_*` call), so the frontend callbacks live in `static mut` cells set by
//! the `retro_set_*` entry points.
#![allow(static_mut_refs)]
// The callback cells are read via `*(&raw const CELL)` — an intentional raw
// read of the `static mut` (no reference formed). `deref_addrof` would rewrite
// that to a bare `CELL` access, defeating the explicit raw-pointer idiom.
#![allow(clippy::deref_addrof)]
// The `retro_*` entry points are the libretro C ABI: the frontend hands them
// raw pointers and guarantees validity + single-threaded calls (see the module
// docs). They deref those pointers by contract; marking each `unsafe fn` would
// not change how the C frontend calls them, so allow the deref crate-wide.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

pub mod dispatch;
pub mod ffi;
mod env;

use ffi::*;
use std::ffi::{c_uint, CStr, CString};
use std::path::PathBuf;

/// Button ids and `RETRO_MEMORY_*` ids a core matches on, re-exported so the
/// consumer needn't reach into [`ffi`].
pub use ffi::{
    RETRO_DEVICE_ID_JOYPAD_A, RETRO_DEVICE_ID_JOYPAD_B, RETRO_DEVICE_ID_JOYPAD_DOWN,
    RETRO_DEVICE_ID_JOYPAD_LEFT, RETRO_DEVICE_ID_JOYPAD_RIGHT, RETRO_DEVICE_ID_JOYPAD_SELECT,
    RETRO_DEVICE_ID_JOYPAD_START, RETRO_DEVICE_ID_JOYPAD_UP, RETRO_MEMORY_RTC,
    RETRO_MEMORY_SAVE_RAM, RETRO_MEMORY_SYSTEM_RAM, RETRO_MEMORY_VIDEO_RAM, RETRO_REGION_NTSC,
};

// --- frontend callbacks (set by the retro_set_* entry points) ---
static mut ENV: retro_environment_t = None;
static mut VIDEO: retro_video_refresh_t = None;
static mut AUDIO_BATCH: retro_audio_sample_batch_t = None;
static mut AUDIO_SAMPLE: retro_audio_sample_t = None;
static mut INPUT_POLL: retro_input_poll_t = None;
static mut INPUT_STATE: retro_input_state_t = None;
static mut RUMBLE: retro_set_rumble_state_t = None;
static mut LOG: retro_log_printf_t = None;
// Keeps the built core-option C table alive for the frontend's lifetime.
static mut OPTIONS: Option<OwnedOptions> = None;

#[inline]
pub(crate) fn env_cb() -> retro_environment_t {
    unsafe { *(&raw const ENV) }
}

// ===========================================================================
// Public value types (safe mirrors of the C structs the consumer produces).
// ===========================================================================

/// Static core identity for `retro_get_system_info`. The strings are read (and
/// may be cached) by the frontend, so they must be `'static`.
pub struct SystemInfo {
    pub library_name: &'static CStr,
    pub library_version: &'static CStr,
    pub valid_extensions: &'static CStr,
    pub need_fullpath: bool,
    pub block_extract: bool,
}

#[derive(Clone, Copy)]
pub struct Geometry {
    pub base_width: u32,
    pub base_height: u32,
    pub max_width: u32,
    pub max_height: u32,
    pub aspect_ratio: f32,
}

/// Audio/video timing + geometry for `retro_get_system_av_info`.
pub struct AvInfo {
    pub geometry: Geometry,
    pub fps: f64,
    pub sample_rate: f64,
}

/// The content handed to [`Core::load_game`].
pub struct Game<'a> {
    pub data: &'a [u8],
    pub path: Option<&'a str>,
}

/// Which RAM region a [`MemoryDescriptor`] describes (RetroAchievements / tools).
#[derive(Clone, Copy)]
pub enum MemoryKind {
    SystemRam,
    SaveRam,
    VideoRam,
}

/// A live RAM region published to the frontend. Built from a slice, so the
/// consumer never writes a raw pointer; the frontend keeps the pointer for the
/// content's lifetime (the region must outlive the content — the caller's
/// contract, satisfied by regions inside the heap-owned machine).
pub struct MemoryDescriptor {
    ptr: *mut u8,
    len: usize,
    start: usize,
    kind: MemoryKind,
}
impl MemoryDescriptor {
    pub fn new(region: &mut [u8], start: usize, kind: MemoryKind) -> Self {
        MemoryDescriptor { ptr: region.as_mut_ptr(), len: region.len(), start, kind }
    }
}

// --- core options builder ---
pub struct OptionValue {
    pub value: String,
    pub label: String,
}
pub struct OptionDef {
    pub key: &'static str,
    pub desc: &'static str,
    pub desc_categorized: &'static str,
    pub info: &'static str,
    pub category: &'static str,
    pub values: Vec<OptionValue>,
    pub default: String,
}
pub struct OptionCategory {
    pub key: &'static str,
    pub desc: &'static str,
    pub info: &'static str,
}
/// A declarative option table; [`Environment::set_core_options`] turns it into
/// the C `retro_core_options_v2` structure and registers it.
#[derive(Default)]
pub struct CoreOptions {
    pub categories: Vec<OptionCategory>,
    pub options: Vec<OptionDef>,
}

// ===========================================================================
// The trait a core implements.
// ===========================================================================

pub trait Core: Sized {
    /// Static identity (name / version / extensions). Called before init.
    fn info() -> SystemInfo;
    /// Construct the core instance (lazily, on first use).
    fn new() -> Self;

    /// Register core options, no-content support, etc. (has the environment).
    fn set_environment(&mut self, _env: &Environment) {}
    /// One-time init (e.g. input descriptors).
    fn init(&mut self, _env: &Environment) {}

    fn av_info(&self) -> AvInfo;
    /// PAL/NTSC; defaults to NTSC.
    fn region(&self) -> u32 {
        RETRO_REGION_NTSC
    }

    /// Load content. Return `false` on any failure.
    fn load_game(&mut self, game: &Game, env: &Environment) -> bool;
    fn unload_game(&mut self, _env: &Environment) {}
    fn reset(&mut self, _env: &Environment) {}

    /// Produce one frame: read input, run, then draw + emit audio via `frame`.
    fn run(&mut self, frame: &mut Frame);

    fn options_changed(&mut self, _env: &Environment) {}
    fn cheat_reset(&mut self) {}
    fn cheat_set(&mut self, _index: u32, _enabled: bool, _code: &str) {}

    /// A directly-exposed RAM region for `RETRO_MEMORY_*` (SRAM/RTC/WRAM/VRAM),
    /// or `None`. Returned as a slice; the framework hands its pointer + length
    /// to the frontend.
    fn memory(&mut self, _id: u32) -> Option<&mut [u8]> {
        None
    }

    fn serialize_size(&mut self) -> usize {
        0
    }
    fn serialize(&mut self, _into: &mut [u8]) -> bool {
        false
    }
    fn unserialize(&mut self, _data: &[u8], _env: &Environment) -> bool {
        false
    }
}

// ===========================================================================
// Contexts — safe handles over the frontend callbacks.
// ===========================================================================

/// Access to the environment callback (queries + one-shot registrations).
pub struct Environment {
    _priv: (),
}

impl Environment {
    /// Current value the frontend has for a core option, if set.
    pub fn get_variable(&self, key: &str) -> Option<String> {
        unsafe { env::get_variable(env_cb(), key) }
    }
    /// Advertise whether the core can run with no content (Game Boy: no).
    pub fn set_support_no_game(&self, supported: bool) {
        unsafe { env::set_support_no_game(env_cb(), supported) }
    }
    /// The frontend's system directory (boot ROMs, etc.).
    pub fn system_directory(&self) -> Option<PathBuf> {
        unsafe { env::get_system_directory(env_cb()) }
    }
    /// Request XRGB8888 output; `false` if the frontend refuses it.
    pub fn set_pixel_format_xrgb8888(&self) -> bool {
        unsafe { env::set_pixel_format(env_cb(), RETRO_PIXEL_FORMAT_XRGB8888) }
    }
    /// Set joypad (port 0) button descriptions: `(RETRO_DEVICE_ID_JOYPAD_*, label)`.
    pub fn set_joypad_descriptors(&self, buttons: &[(u32, &'static CStr)]) {
        let mut descs: Vec<retro_input_descriptor> = buttons
            .iter()
            .map(|(id, label)| retro_input_descriptor {
                port: 0,
                device: RETRO_DEVICE_JOYPAD,
                index: 0,
                id: *id,
                description: label.as_ptr(),
            })
            .collect();
        // NUL terminator.
        descs.push(retro_input_descriptor {
            port: 0,
            device: 0,
            index: 0,
            id: 0,
            description: std::ptr::null(),
        });
        unsafe { env::set_input_descriptors(env_cb(), &descs) };
    }
    /// Build and register the core-option table (kept alive for the frontend).
    pub fn set_core_options(&self, opts: &CoreOptions) {
        let owned = OwnedOptions::build(opts);
        let v2 = owned.as_v2();
        unsafe {
            env::set_core_options_v2(env_cb(), &v2);
            OPTIONS = Some(owned);
        }
    }
    /// Publish RAM regions for RetroAchievements / RAM tools.
    pub fn set_memory_maps(&self, descriptors: &[MemoryDescriptor]) {
        let ffi_descs: Vec<retro_memory_descriptor> = descriptors
            .iter()
            .filter(|d| d.len != 0)
            .map(|d| retro_memory_descriptor {
                flags: match d.kind {
                    MemoryKind::SystemRam => RETRO_MEMDESC_SYSTEM_RAM,
                    MemoryKind::SaveRam => RETRO_MEMDESC_SAVE_RAM,
                    MemoryKind::VideoRam => RETRO_MEMDESC_VIDEO_RAM,
                },
                ptr: d.ptr as *mut std::ffi::c_void,
                offset: 0,
                start: d.start,
                select: 0,
                disconnect: 0,
                len: d.len,
                addrspace: std::ptr::null(),
            })
            .collect();
        let map = retro_memory_map {
            descriptors: ffi_descs.as_ptr(),
            num_descriptors: ffi_descs.len() as c_uint,
        };
        unsafe { env::set_memory_maps(env_cb(), map) };
    }
    /// Change the output geometry (e.g. toggling the SGB border).
    pub fn set_geometry(&self, g: Geometry) {
        unsafe { env::set_game_geometry(env_cb(), g.into()) };
    }
    /// Ask the frontend for a rumble interface; `true` if one is available (the
    /// framework stores it, so [`set_rumble`](Self::set_rumble) then works).
    pub fn enable_rumble(&self) -> bool {
        match unsafe { env::get_rumble_interface(env_cb()) } {
            Some(cb) => {
                unsafe { RUMBLE = cb };
                true
            }
            None => false,
        }
    }
    /// Drive the rumble motors outside a frame (e.g. stop them on unload/reset).
    pub fn set_rumble(&self, strong: u16, weak: u16) {
        set_rumble(strong, weak);
    }
    /// Write one line to the frontend's log (RetroArch's log window / file), or
    /// to stderr when the frontend offers no logging interface.
    pub fn log(&self, level: LogLevel, msg: &str) {
        log_line(level, msg);
    }
}

/// Severity for [`Environment::log`], mirroring `retro_log_level`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

impl LogLevel {
    fn to_ffi(self) -> retro_log_level {
        match self {
            LogLevel::Info => RETRO_LOG_INFO,
            LogLevel::Warn => RETRO_LOG_WARN,
            LogLevel::Error => RETRO_LOG_ERROR,
        }
    }
}

/// Log one line through the frontend, else stderr. Always sent as `("%s\n", s)`
/// so a `%` in the message can never be read as a format directive.
fn log_line(level: LogLevel, msg: &str) {
    match (unsafe { *(&raw const LOG) }, CString::new(msg)) {
        (Some(f), Ok(c)) => unsafe { f(level.to_ffi(), c"%s\n".as_ptr(), c.as_ptr()) },
        // No frontend logger (or an interior NUL the C side can't carry).
        _ => eprintln!("[rustyboi] {msg}"),
    }
}

/// Drive both rumble motors via the stored interface (0 = off).
fn set_rumble(strong: u16, weak: u16) {
    if let Some(f) = unsafe { *(&raw const RUMBLE) } {
        unsafe {
            f(0, RETRO_RUMBLE_STRONG, strong);
            f(0, RETRO_RUMBLE_WEAK, weak);
        }
    }
}

/// The per-frame context: input, video, audio, geometry, rumble.
pub struct Frame {
    _priv: (),
}

impl Frame {
    /// Whether a joypad button (port 0) is currently pressed.
    pub fn pressed(&self, id: u32) -> bool {
        match unsafe { *(&raw const INPUT_STATE) } {
            Some(f) => unsafe { f(0, RETRO_DEVICE_JOYPAD, 0, id) != 0 },
            None => false,
        }
    }
    /// Present one XRGB8888 frame (`pixels` is `width * height * 4` bytes).
    pub fn draw_xrgb8888(&mut self, pixels: &[u8], width: u32, height: u32) {
        if let Some(f) = unsafe { *(&raw const VIDEO) } {
            unsafe {
                f(
                    pixels.as_ptr() as *const std::ffi::c_void,
                    width,
                    height,
                    width as usize * 4,
                )
            };
        }
    }
    /// Submit interleaved stereo samples (L,R,L,R…).
    pub fn audio(&mut self, interleaved: &[i16]) {
        if let Some(f) = unsafe { *(&raw const AUDIO_BATCH) } {
            unsafe { f(interleaved.as_ptr(), interleaved.len() / 2) };
        }
    }
    /// Change output geometry mid-run (SGB border toggle).
    pub fn set_geometry(&mut self, g: Geometry) {
        unsafe { env::set_game_geometry(env_cb(), g.into()) };
    }
    /// Drive the rumble motors (0 = off, u16::MAX = full).
    pub fn set_rumble(&mut self, strong: u16, weak: u16) {
        set_rumble(strong, weak);
    }
}

// ===========================================================================
// Internal glue.
// ===========================================================================

impl From<Geometry> for retro_game_geometry {
    fn from(g: Geometry) -> Self {
        retro_game_geometry {
            base_width: g.base_width,
            base_height: g.base_height,
            max_width: g.max_width,
            max_height: g.max_height,
            aspect_ratio: g.aspect_ratio,
        }
    }
}

/// Owns every `CString` the option table points into plus the C arrays.
struct OwnedOptions {
    _strings: Vec<CString>,
    categories: Vec<retro_core_option_v2_category>,
    definitions: Vec<retro_core_option_v2_definition>,
}
impl OwnedOptions {
    fn as_v2(&self) -> retro_core_options_v2 {
        retro_core_options_v2 {
            categories: self.categories.as_ptr() as *mut _,
            definitions: self.definitions.as_ptr() as *mut _,
        }
    }
    fn build(opts: &CoreOptions) -> OwnedOptions {
        // libretro caps each value list (including the NUL terminator) at 128.
        const MAX_VALUES: usize = 128;
        let mut strings: Vec<CString> = Vec::new();
        let mut push = |s: &str| -> *const std::ffi::c_char {
            let c = CString::new(s).expect("option string has an interior NUL");
            let ptr = c.as_ptr();
            strings.push(c);
            ptr
        };

        let mut categories: Vec<retro_core_option_v2_category> = opts
            .categories
            .iter()
            .map(|c| retro_core_option_v2_category {
                key: push(c.key),
                desc: push(c.desc),
                info: push(c.info),
            })
            .collect();
        categories.push(unsafe { std::mem::zeroed() }); // NULL-key terminator

        let mut definitions: Vec<retro_core_option_v2_definition> = Vec::new();
        for opt in &opts.options {
            assert!(opt.values.len() < MAX_VALUES, "option {} has too many values", opt.key);
            let mut values: [retro_core_option_value; MAX_VALUES] = unsafe { std::mem::zeroed() };
            for (slot, v) in values.iter_mut().zip(opt.values.iter()) {
                slot.value = push(&v.value);
                slot.label = push(&v.label);
            }
            definitions.push(retro_core_option_v2_definition {
                key: push(opt.key),
                desc: push(opt.desc),
                desc_categorized: push(opt.desc_categorized),
                info: push(opt.info),
                info_categorized: std::ptr::null(),
                category_key: push(opt.category),
                values,
                default_value: push(&opt.default),
            });
        }
        definitions.push(unsafe { std::mem::zeroed() }); // NULL-key terminator

        OwnedOptions { _strings: strings, categories, definitions }
    }
}

// Callback setters (called by the macro-generated retro_set_* entry points).
#[doc(hidden)]
pub fn set_env_cb(cb: retro_environment_t) {
    unsafe { ENV = cb };
    // The logger is fetched here rather than lazily so it is already in place
    // for anything the core logs from `set_environment` / `init` onwards.
    unsafe { LOG = env::get_log_interface(cb) };
}
#[doc(hidden)]
pub fn set_video_cb(cb: retro_video_refresh_t) {
    unsafe { VIDEO = cb };
}
#[doc(hidden)]
pub fn set_audio_sample_cb(cb: retro_audio_sample_t) {
    unsafe { AUDIO_SAMPLE = cb };
}
#[doc(hidden)]
pub fn set_audio_batch_cb(cb: retro_audio_sample_batch_t) {
    unsafe { AUDIO_BATCH = cb };
}
#[doc(hidden)]
pub fn set_input_poll_cb(cb: retro_input_poll_t) {
    unsafe { INPUT_POLL = cb };
}
#[doc(hidden)]
pub fn set_input_state_cb(cb: retro_input_state_t) {
    unsafe { INPUT_STATE = cb };
}
/// Clear all globals on `retro_deinit`.
#[doc(hidden)]
pub fn clear() {
    unsafe {
        ENV = None;
        VIDEO = None;
        AUDIO_BATCH = None;
        AUDIO_SAMPLE = None;
        INPUT_POLL = None;
        INPUT_STATE = None;
        RUMBLE = None;
        LOG = None;
        OPTIONS = None;
    }
}

pub(crate) fn environment() -> Environment {
    Environment { _priv: () }
}

/// Poll input once, then hand the core a [`Frame`] (libretro convention: call
/// `retro_input_poll_t` before reading state).
pub(crate) fn poll_input() {
    if let Some(f) = unsafe { *(&raw const INPUT_POLL) } {
        unsafe { f() };
    }
}
pub(crate) fn frame() -> Frame {
    Frame { _priv: () }
}

/// Emit the libretro C entry points for a type implementing [`Core`].
///
/// The `#[no_mangle]` symbols must live in the final `cdylib`, so they are
/// generated here in the consumer crate; each is a thin wrapper that forwards to
/// [`dispatch`] with the (lazily-created) singleton instance. No `unsafe` or raw
/// FFI leaks into the consumer.
///
/// ```ignore
/// struct MyCore { /* … */ }
/// impl rustyboi_libretro_sys::Core for MyCore { /* … */ }
/// rustyboi_libretro_sys::libretro_core!(MyCore);
/// ```
#[macro_export]
macro_rules! libretro_core {
    ($core:ty) => {
        static mut __RB_INSTANCE: ::core::option::Option<$core> = ::core::option::Option::None;

        #[inline]
        fn __rb() -> &'static mut $core {
            unsafe {
                (*(&raw mut __RB_INSTANCE))
                    .get_or_insert_with(<$core as $crate::Core>::new)
            }
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn retro_api_version() -> ::core::ffi::c_uint {
            $crate::dispatch::api_version()
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_get_system_info(info: *mut $crate::ffi::retro_system_info) {
            $crate::dispatch::get_system_info::<$core>(info)
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_set_environment(cb: $crate::ffi::retro_environment_t) {
            $crate::dispatch::set_environment(__rb(), cb)
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_set_video_refresh(cb: $crate::ffi::retro_video_refresh_t) {
            $crate::set_video_cb(cb)
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_set_audio_sample(cb: $crate::ffi::retro_audio_sample_t) {
            $crate::set_audio_sample_cb(cb)
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_set_audio_sample_batch(
            cb: $crate::ffi::retro_audio_sample_batch_t,
        ) {
            $crate::set_audio_batch_cb(cb)
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_set_input_poll(cb: $crate::ffi::retro_input_poll_t) {
            $crate::set_input_poll_cb(cb)
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_set_input_state(cb: $crate::ffi::retro_input_state_t) {
            $crate::set_input_state_cb(cb)
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_init() {
            $crate::dispatch::init(__rb())
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_deinit() {
            unsafe { *(&raw mut __RB_INSTANCE) = ::core::option::Option::None };
            $crate::dispatch::deinit()
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_set_controller_port_device(
            _port: ::core::ffi::c_uint,
            _device: ::core::ffi::c_uint,
        ) {
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_get_system_av_info(info: *mut $crate::ffi::retro_system_av_info) {
            $crate::dispatch::get_system_av_info(__rb(), info)
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_get_region() -> ::core::ffi::c_uint {
            $crate::dispatch::region(__rb())
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_load_game(info: *const $crate::ffi::retro_game_info) -> bool {
            $crate::dispatch::load_game(__rb(), info)
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_load_game_special(
            _game_type: ::core::ffi::c_uint,
            _info: *const $crate::ffi::retro_game_info,
            _num_info: usize,
        ) -> bool {
            $crate::dispatch::load_game_special()
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_unload_game() {
            $crate::dispatch::unload_game(__rb())
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_reset() {
            $crate::dispatch::reset(__rb())
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_run() {
            $crate::dispatch::run(__rb())
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_serialize_size() -> usize {
            $crate::dispatch::serialize_size(__rb())
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_serialize(data: *mut ::core::ffi::c_void, size: usize) -> bool {
            $crate::dispatch::serialize(__rb(), data, size)
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_unserialize(data: *const ::core::ffi::c_void, size: usize) -> bool {
            $crate::dispatch::unserialize(__rb(), data, size)
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_cheat_reset() {
            $crate::dispatch::cheat_reset(__rb())
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_cheat_set(
            index: ::core::ffi::c_uint,
            enabled: bool,
            code: *const ::core::ffi::c_char,
        ) {
            $crate::dispatch::cheat_set(__rb(), index, enabled, code)
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_get_memory_data(id: ::core::ffi::c_uint) -> *mut ::core::ffi::c_void {
            $crate::dispatch::memory_data(__rb(), id)
        }
        #[unsafe(no_mangle)]
        pub extern "C" fn retro_get_memory_size(id: ::core::ffi::c_uint) -> usize {
            $crate::dispatch::memory_size(__rb(), id)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cstr(ptr: *const std::ffi::c_char) -> String {
        assert!(!ptr.is_null(), "pointer should be interned, not null");
        unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned()
    }

    fn sample_options() -> CoreOptions {
        CoreOptions {
            categories: vec![OptionCategory { key: "cat", desc: "Category", info: "cat info" }],
            options: vec![OptionDef {
                key: "opt",
                desc: "Option",
                desc_categorized: "Opt",
                info: "opt info",
                category: "cat",
                values: vec![
                    OptionValue { value: "a".into(), label: "Label A".into() },
                    OptionValue { value: "b".into(), label: "Label B".into() },
                ],
                default: "b".into(),
            }],
        }
    }

    // Both C tables are NUL-key terminated (the frontend scans until key==null),
    // and every string the definition points at is interned and readable.
    #[test]
    fn build_terminators_and_interning() {
        let owned = OwnedOptions::build(&sample_options());

        // A trailing all-zero (null-key) terminator on each table.
        assert_eq!(owned.categories.len(), 2);
        assert!(owned.categories.last().unwrap().key.is_null());
        assert_eq!(owned.definitions.len(), 2);
        assert!(owned.definitions.last().unwrap().key.is_null());

        let cat = &owned.categories[0];
        assert_eq!(cstr(cat.key), "cat");
        assert_eq!(cstr(cat.desc), "Category");
        assert_eq!(cstr(cat.info), "cat info");

        let def = &owned.definitions[0];
        assert_eq!(cstr(def.key), "opt");
        assert_eq!(cstr(def.desc), "Option");
        assert_eq!(cstr(def.desc_categorized), "Opt");
        assert_eq!(cstr(def.info), "opt info");
        assert_eq!(cstr(def.category_key), "cat");
        assert_eq!(cstr(def.default_value), "b");

        // Each value/label is interned in order; slots past the list stay null.
        assert_eq!(cstr(def.values[0].value), "a");
        assert_eq!(cstr(def.values[0].label), "Label A");
        assert_eq!(cstr(def.values[1].value), "b");
        assert_eq!(cstr(def.values[1].label), "Label B");
        assert!(def.values[2].value.is_null());
        assert!(def.values[2].label.is_null());
    }

    // An empty table still yields the two NUL-key terminators the frontend needs.
    #[test]
    fn build_empty_yields_terminators() {
        let owned = OwnedOptions::build(&CoreOptions::default());
        assert_eq!(owned.categories.len(), 1);
        assert!(owned.categories[0].key.is_null());
        assert_eq!(owned.definitions.len(), 1);
        assert!(owned.definitions[0].key.is_null());
    }

    // A value list of exactly MAX_VALUES leaves no room for the null terminator
    // in the fixed 128-slot array, so build must reject it.
    #[test]
    #[should_panic(expected = "too many values")]
    fn build_rejects_oversized_value_list() {
        let values = (0..128)
            .map(|i| OptionValue { value: i.to_string(), label: i.to_string() })
            .collect();
        let opts = CoreOptions {
            categories: vec![],
            options: vec![OptionDef {
                key: "k",
                desc: "",
                desc_categorized: "",
                info: "",
                category: "",
                values,
                default: "0".into(),
            }],
        };
        OwnedOptions::build(&opts);
    }

    // A C string cannot carry an interior NUL; build must panic rather than
    // silently truncate the option key.
    #[test]
    #[should_panic(expected = "interior NUL")]
    fn build_rejects_interior_nul() {
        let opts = CoreOptions {
            categories: vec![],
            options: vec![OptionDef {
                key: "bad\0key",
                desc: "",
                desc_categorized: "",
                info: "",
                category: "",
                values: vec![OptionValue { value: "v".into(), label: "l".into() }],
                default: "v".into(),
            }],
        };
        OwnedOptions::build(&opts);
    }

    // Geometry maps field-for-field into the C struct, aspect_ratio included.
    #[test]
    fn geometry_into_ffi() {
        let g = Geometry {
            base_width: 160,
            base_height: 144,
            max_width: 256,
            max_height: 224,
            aspect_ratio: 10.0 / 9.0,
        };
        let c: retro_game_geometry = g.into();
        assert_eq!(c.base_width, 160);
        assert_eq!(c.base_height, 144);
        assert_eq!(c.max_width, 256);
        assert_eq!(c.max_height, 224);
        assert_eq!(c.aspect_ratio, 10.0f32 / 9.0);
    }
}
