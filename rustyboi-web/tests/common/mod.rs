//! Shared fixtures for the headless web tests.

/// A minimal but *runnable* 32 KiB no-MBC cartridge, built in-code so the tests
/// need no external ROM (the `gb-test-roms` set is fetched at suite-run time and
/// is NOT vendored, so `include_bytes!`-ing it breaks the `web-headless` CI job,
/// which doesn't fetch it). The program loops incrementing `A` and writing it to
/// BGP, so the machine genuinely advances frame-to-frame — rewind snapshots
/// differ and `run_frame` produces distinct states. Mirrors the session tests'
/// `test_rom` helper.
pub fn test_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    // Entry point: JP 0x0150.
    rom[0x100] = 0xC3;
    rom[0x101] = 0x50;
    rom[0x102] = 0x01;
    let prog: &[u8] = &[
        0x3E, 0x00, // LD A, 0x00
        0xE0, 0x47, // LDH (0x47), A  ; BGP = A
        0x3C, //       INC A
        0xC3, 0x54, 0x01, // JP 0x0154
    ];
    rom[0x150..0x150 + prog.len()].copy_from_slice(prog);
    rom[0x147] = 0x00; // ROM only
    rom[0x148] = 0x00; // 32 KiB
    rom[0x149] = 0x00; // no RAM
    // Header checksum over 0x134..=0x14C.
    let mut checksum: u8 = 0;
    for &b in &rom[0x134..0x14D] {
        checksum = checksum.wrapping_sub(b).wrapping_sub(1);
    }
    rom[0x14D] = checksum;
    rom
}
