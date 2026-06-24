//! Link-time trap shims for `make coverage-web` ONLY. Compiled standalone by the
//! Makefile (`rustc --target=wasm32-unknown-unknown --crate-type=staticlib
//! --emit=obj`) into a wasm object that is `-Clink-arg`ed into the coverage
//! build. NOT a workspace crate — nothing else ever compiles it.
//!
//! Why it exists: the web wasm pulls in naga (via wgpu), whose shader
//! constant-folder references the libm hyperbolic functions `acosh`/`acoshf`/
//! `asinh`. `wasm32-unknown-unknown` has no libc, so nothing defines them. A
//! normal `make web` (release, fat LTO) folds the dead branches away before the
//! link ever sees them; the unoptimized coverage build does not, so the link
//! fails on the undefined symbols. These branches are never reached by the
//! headless web tests (egui's shaders use none of these ops), so each shim traps
//! (`unreachable`): a real call fails the test loudly instead of returning a
//! silently-wrong value. This object never ships.
#![no_std]

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

macro_rules! trap_shim {
    ($name:ident, $ty:ty) => {
        #[no_mangle]
        pub extern "C" fn $name(_x: $ty) -> $ty {
            core::arch::wasm32::unreachable()
        }
    };
}

trap_shim!(acosh, f64);
trap_shim!(acoshf, f32);
trap_shim!(asinh, f64);
trap_shim!(asinhf, f32);
trap_shim!(atanh, f64);
trap_shim!(atanhf, f32);
