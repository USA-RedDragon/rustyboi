//! Stack-hazard regression guards. `GB` is built BY VALUE on the calling
//! thread's stack at every savestate restore (`GB::from_state_bytes`), rewind
//! step, and clone — on threads as small as iOS's 1 MiB main thread, wasm's
//! shadow stack, and whatever RetroArch calls `retro_unserialize` on. The big
//! buffers (framebuffers, VRAM/WRAM) are heap-boxed precisely so those paths
//! move ~4 KiB, not ~208 KiB; historically the inline layout caused real
//! crashes (wasm restore trap, Android SIGSEGV, debug-test overflows).
//!
//! If one of these fails, a large inline array crept back into the named
//! struct — box it instead (see ppu framebuffers / mmio vram for the pattern).

use std::mem::size_of;

#[test]
fn gb_stays_stack_cheap() {
    let n = size_of::<rustyboi_core_lib::gb::GB>();
    assert!(n < 8 * 1024, "size_of::<GB>() = {n} (was ~4.1 KiB; keep big buffers boxed)");
}

#[test]
fn ppu_stays_stack_cheap() {
    let n = size_of::<rustyboi_core_lib::ppu::Ppu>();
    assert!(n < 4 * 1024, "size_of::<Ppu>() = {n} (framebuffers must stay Box<[u8; N]>)");
}

#[test]
fn mmio_stays_stack_cheap() {
    let n = size_of::<rustyboi_core_lib::memory::mmio::Mmio>();
    assert!(n < 4 * 1024, "size_of::<Mmio>() = {n} (vram/wram must stay Box<Memory>)");
}
