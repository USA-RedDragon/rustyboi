//! Deterministic per-frame gameplay input, shared by the `sweep` harness and
//! the `bench` PGO-drive mode. Stateless: input is a pure function of
//! (frame, seed), so it needs no history and is identical across runs and
//! thread counts.

use rustyboi_core_lib::input::ButtonState;

/// Frames 0..600: fixed title-clearing taps (START/A alternating, 6-frame
/// holds so edge-triggered menus see press and release). Frames 600+: 30-frame
/// slots from a per-ROM seeded xorshift — hold a weighted gameplay combo for 15
/// frames, release 15. START/SELECT are excluded from the gameplay phase (pause
/// screens are static and would skew toward idle menus) except paired START taps
/// (tap + counter-tap 60 frames later) so a long intro still gets entry chances
/// while a game already in play pauses for at most ~60 frames per pair.
pub fn masher(frame: usize, seed: u64) -> ButtonState {
    let mut b = ButtonState::default();
    let tap = |at: usize| frame >= at && frame < at + 6;
    if frame < 600 {
        if tap(120) || tap(280) || tap(440) {
            b.start = true;
        }
        if tap(200) || tap(360) || tap(520) {
            b.a = true;
        }
        return b;
    }
    if tap(800) || tap(860) || tap(1600) || tap(1660) || tap(2400) || tap(2460) {
        b.start = true;
        return b;
    }
    let slot = ((frame - 600) / 30) as u64;
    if (frame - 600) % 30 >= 15 {
        return b; // release half of the slot
    }
    // xorshift64* on seed ^ slot: stable per (ROM, slot), order-independent.
    let mut x = seed ^ slot.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    let r = x.wrapping_mul(0x2545_F491_4F6C_DD1D) % 100;
    match r {
        0..=24 => b.right = true,
        25..=39 => b.left = true,
        40..=49 => b.down = true,
        50..=59 => b.up = true,
        60..=79 => b.a = true,
        80..=89 => {
            b.a = true;
            b.right = true;
        }
        _ => b.b = true,
    }
    b
}
