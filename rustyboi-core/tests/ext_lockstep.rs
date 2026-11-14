//! The external-CPU path (`GB::ext_*`) against the native CPU, in lockstep.
//!
//! An embedder that recompiles SM83 code runs each opcode body itself and
//! issues the body's bus accesses through `ext_read`, `ext_write`,
//! `ext_internal` and `ext_idu`, between `ext_begin` and `ext_end`. This test
//! plays that embedder: `body` below is a small SM83 core written against the
//! ext API with rustyboi's M-cycle placement, falling back to
//! `ext_exec_native` for the opcodes it does not model (CB prefix, HALT,
//! STOP, DI, EI, RETI, DAA, the SP+e forms, illegal opcodes). Two machines run
//! the same ROM, one on `step_instruction` and one on the ext path; registers
//! and the cycle position must match after every step, and memory, I/O and the
//! frame must match at the end.
//!
//! By default every 64th ROM of `gb-test-roms` runs for 10 frames on DMG,
//! CGB-C, CGB-E and AGB. `RB_EXT_LOCKSTEP=full` runs every ROM for 120 frames
//! (the release-mode sweep: `cargo test --release -p rustyboi-core --test
//! ext_lockstep`). Skips when `gb-test-roms` is absent.

use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::cpu::bus::ExtBusState;
use rustyboi_core_lib::cpu::registers::Registers;
use rustyboi_core_lib::gb::{GB, Hardware};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

const ZF: u8 = 0x80;
const NF: u8 = 0x40;
const HF: u8 = 0x20;
const CF: u8 = 0x10;

const MODELS: [Hardware; 4] = [Hardware::DMG, Hardware::CGB, Hardware::CGBE, Hardware::AGB];

/// One frame in master cc.
const FRAME_CC: u64 = 70224;
const MAX_STEPS: u64 = 3_000_000;

/// An external CPU: the registers it works on plus the bus counters of the
/// instruction in flight.
struct Ext<'a> {
    gb: &'a mut GB,
    st: ExtBusState,
    /// Deliberate fault for the self-check: every body reports 4 cc too many.
    skew: u32,
}

impl Ext<'_> {
    fn rd(&mut self, a: u16) -> u8 {
        self.gb.ext_read(&mut self.st, a)
    }
    fn wr(&mut self, a: u16, v: u8) {
        self.gb.ext_write(&mut self.st, a, v)
    }
    fn internal(&mut self) {
        self.gb.ext_internal(&mut self.st)
    }
    fn idu(&mut self, pre: u16) {
        self.gb.ext_idu(&mut self.st, pre)
    }
    fn imm(&mut self, r: &mut Registers) -> u8 {
        let v = self.rd(r.pc);
        r.pc = r.pc.wrapping_add(1);
        v
    }

    /// One step: the boundary, then the body (here or native), then the end.
    fn step(&mut self) {
        let Some(op) = self.gb.ext_begin(&mut self.st) else {
            return;
        };
        let mut r = self.gb.get_cpu_registers().clone();
        if let Some(total) = body(self, &mut r, op) {
            *self.gb.cpu_registers_mut() = r;
            self.gb.ext_end(&mut self.st, total + self.skew);
        } else {
            self.gb.ext_exec_native(&mut self.st, op);
        }
    }
}

fn reg8(r: &mut Registers, i: u8) -> &mut u8 {
    match i {
        0 => &mut r.b,
        1 => &mut r.c,
        2 => &mut r.d,
        3 => &mut r.e,
        4 => &mut r.h,
        5 => &mut r.l,
        _ => &mut r.a,
    }
}

fn cond(r: &Registers, cc: u8) -> bool {
    match cc {
        0 => r.f & ZF == 0,
        1 => r.f & ZF != 0,
        2 => r.f & CF == 0,
        _ => r.f & CF != 0,
    }
}

fn hl(r: &Registers) -> u16 {
    u16::from_be_bytes([r.h, r.l])
}

fn set_hl(r: &mut Registers, v: u16) {
    [r.h, r.l] = v.to_be_bytes();
}

fn rr_get(r: &Registers, i: u8) -> u16 {
    match i {
        0 => u16::from_be_bytes([r.b, r.c]),
        1 => u16::from_be_bytes([r.d, r.e]),
        2 => hl(r),
        _ => r.sp,
    }
}

fn rr_set(r: &mut Registers, i: u8, v: u16) {
    match i {
        0 => [r.b, r.c] = v.to_be_bytes(),
        1 => [r.d, r.e] = v.to_be_bytes(),
        2 => set_hl(r, v),
        _ => r.sp = v,
    }
}

fn inc8(r: &mut Registers, v: u8) -> u8 {
    let v = v.wrapping_add(1);
    r.f = (r.f & CF) | if v == 0 { ZF } else { 0 } | if v & 0xF == 0 { HF } else { 0 };
    v
}

fn dec8(r: &mut Registers, v: u8) -> u8 {
    let v = v.wrapping_sub(1);
    r.f = (r.f & CF) | NF | if v == 0 { ZF } else { 0 } | if v & 0xF == 0xF { HF } else { 0 };
    v
}

fn alu(r: &mut Registers, op: u8, v: u8) {
    let a = r.a;
    let c = u8::from(r.f & CF != 0);
    let z = |x: u8| if x == 0 { ZF } else { 0 };
    let flag = |b: bool, f: u8| if b { f } else { 0 };
    match op {
        0 | 1 => {
            let c = if op == 1 { c } else { 0 };
            let res = u16::from(a) + u16::from(v) + u16::from(c);
            r.a = res as u8;
            r.f = z(r.a) | flag((a & 0xF) + (v & 0xF) + c > 0xF, HF) | flag(res > 0xFF, CF);
        }
        2 | 3 | 7 => {
            let c = if op == 3 { c } else { 0 };
            let res = a.wrapping_sub(v).wrapping_sub(c);
            r.f = z(res)
                | NF
                | flag((a & 0xF) < (v & 0xF) + c, HF)
                | flag(u16::from(a) < u16::from(v) + u16::from(c), CF);
            if op != 7 {
                r.a = res;
            }
        }
        4 => {
            r.a = a & v;
            r.f = z(r.a) | HF;
        }
        5 => {
            r.a = a ^ v;
            r.f = z(r.a);
        }
        _ => {
            r.a = a | v;
            r.f = z(r.a);
        }
    }
}

/// The body of `op` (fetch already charged by `ext_begin`), with the explicit
/// accesses in rustyboi's order; trailing internal cycles are left to
/// `ext_end`. Returns the instruction's total cc, or `None` for the
/// interpreter fallback.
fn body(x: &mut Ext, r: &mut Registers, op: u8) -> Option<u32> {
    let rel = |r: &mut Registers, off: u8| r.pc = r.pc.wrapping_add(off as i8 as u16);
    let total = match op {
        0x76 => return None,
        0x40..=0x7F => {
            let (d, s) = ((op >> 3) & 7, op & 7);
            if s == 6 {
                *reg8(r, d) = x.rd(hl(r));
                8
            } else if d == 6 {
                x.wr(hl(r), *reg8(r, s));
                8
            } else {
                *reg8(r, d) = *reg8(r, s);
                4
            }
        }
        0x80..=0xBF if op & 7 == 6 => {
            let v = x.rd(hl(r));
            alu(r, (op >> 3) & 7, v);
            8
        }
        0x80..=0xBF => {
            let v = *reg8(r, op & 7);
            alu(r, (op >> 3) & 7, v);
            4
        }
        0x34 => {
            let v = x.rd(hl(r));
            let v = inc8(r, v);
            x.wr(hl(r), v);
            12
        }
        0x35 => {
            let v = x.rd(hl(r));
            let v = dec8(r, v);
            x.wr(hl(r), v);
            12
        }
        0x36 => {
            let v = x.imm(r);
            x.wr(hl(r), v);
            12
        }
        _ if op < 0x40 && op & 7 == 4 => {
            let v = *reg8(r, op >> 3);
            *reg8(r, op >> 3) = inc8(r, v);
            4
        }
        _ if op < 0x40 && op & 7 == 5 => {
            let v = *reg8(r, op >> 3);
            *reg8(r, op >> 3) = dec8(r, v);
            4
        }
        _ if op < 0x40 && op & 7 == 6 => {
            let v = x.imm(r);
            *reg8(r, op >> 3) = v;
            8
        }
        _ if op < 0x40 && op & 0xF == 1 => {
            let lo = x.imm(r);
            let hi = x.imm(r);
            rr_set(r, op >> 4, u16::from_le_bytes([lo, hi]));
            12
        }
        _ if op < 0x40 && (op & 0xF == 3 || op & 0xF == 0xB) => {
            let t = rr_get(r, op >> 4);
            x.idu(t);
            let t = if op & 8 == 0 {
                t.wrapping_add(1)
            } else {
                t.wrapping_sub(1)
            };
            rr_set(r, op >> 4, t);
            8
        }
        _ if op < 0x40 && op & 0xF == 9 => {
            let (h, o) = (hl(r), rr_get(r, op >> 4));
            let res = u32::from(h) + u32::from(o);
            set_hl(r, res as u16);
            r.f = (r.f & ZF)
                | if (h & 0xFFF) + (o & 0xFFF) > 0xFFF {
                    HF
                } else {
                    0
                }
                | if res > 0xFFFF { CF } else { 0 };
            8
        }
        0xC6 | 0xCE | 0xD6 | 0xDE | 0xE6 | 0xEE | 0xF6 | 0xFE => {
            let v = x.imm(r);
            alu(r, (op >> 3) & 7, v);
            8
        }
        0xC7 | 0xCF | 0xD7 | 0xDF | 0xE7 | 0xEF | 0xF7 | 0xFF => {
            x.internal();
            push(x, r, r.pc);
            r.pc = u16::from(op & 0x38);
            16
        }
        0x00 => 4,
        0x02 | 0x12 => {
            x.wr(rr_get(r, op >> 4), r.a);
            8
        }
        0x0A | 0x1A => {
            r.a = x.rd(rr_get(r, op >> 4));
            8
        }
        0x22 | 0x32 => {
            let t = hl(r);
            x.wr(t, r.a);
            set_hl(
                r,
                if op == 0x22 {
                    t.wrapping_add(1)
                } else {
                    t.wrapping_sub(1)
                },
            );
            8
        }
        0x2A | 0x3A => {
            let t = hl(r);
            r.a = x.rd(t);
            set_hl(
                r,
                if op == 0x2A {
                    t.wrapping_add(1)
                } else {
                    t.wrapping_sub(1)
                },
            );
            8
        }
        0x07 => {
            let v = r.a >> 7;
            r.a = r.a << 1 | v;
            r.f = if v != 0 { CF } else { 0 };
            4
        }
        0x0F => {
            let v = r.a & 1;
            r.a = r.a >> 1 | v << 7;
            r.f = if v != 0 { CF } else { 0 };
            4
        }
        0x17 => {
            let v = r.a >> 7;
            r.a = r.a << 1 | u8::from(r.f & CF != 0);
            r.f = if v != 0 { CF } else { 0 };
            4
        }
        0x1F => {
            let v = r.a & 1;
            r.a = r.a >> 1 | if r.f & CF != 0 { 0x80 } else { 0 };
            r.f = if v != 0 { CF } else { 0 };
            4
        }
        0x2F => {
            r.a = !r.a;
            r.f |= NF | HF;
            4
        }
        0x37 => {
            r.f = (r.f & ZF) | CF;
            4
        }
        0x3F => {
            r.f = (r.f & (ZF | CF)) ^ CF;
            4
        }
        0x18 => {
            let off = x.imm(r);
            rel(r, off);
            12
        }
        0x20 | 0x28 | 0x30 | 0x38 => {
            let off = x.imm(r);
            if cond(r, (op >> 3) & 3) {
                rel(r, off);
                12
            } else {
                8
            }
        }
        0xC3 => {
            let lo = x.imm(r);
            let hi = x.imm(r);
            r.pc = u16::from_le_bytes([lo, hi]);
            16
        }
        0xC2 | 0xCA | 0xD2 | 0xDA => {
            if cond(r, (op >> 3) & 3) {
                let lo = x.imm(r);
                let hi = x.imm(r);
                r.pc = u16::from_le_bytes([lo, hi]);
                16
            } else {
                // rustyboi skips the operand reads when not taken.
                r.pc = r.pc.wrapping_add(2);
                12
            }
        }
        0xE9 => {
            r.pc = hl(r);
            4
        }
        0xCD | 0xC4 | 0xCC | 0xD4 | 0xDC => {
            let lo = x.imm(r);
            let hi = x.imm(r);
            if op != 0xCD && !cond(r, (op >> 3) & 3) {
                12
            } else {
                x.internal();
                push(x, r, r.pc);
                r.pc = u16::from_le_bytes([lo, hi]);
                24
            }
        }
        0xC9 => {
            r.pc = pop(x, r);
            16
        }
        0xC0 | 0xC8 | 0xD0 | 0xD8 => {
            if cond(r, (op >> 3) & 3) {
                x.internal();
                r.pc = pop(x, r);
                20
            } else {
                8
            }
        }
        0xC5 | 0xD5 | 0xE5 | 0xF5 => {
            let v = match op {
                0xC5 => rr_get(r, 0),
                0xD5 => rr_get(r, 1),
                0xE5 => hl(r),
                _ => u16::from_be_bytes([r.a, r.f]),
            };
            x.idu(r.sp);
            x.internal();
            push(x, r, v);
            16
        }
        0xC1 | 0xD1 | 0xE1 => {
            let v = pop(x, r);
            rr_set(r, (op >> 4) - 0xC, v);
            12
        }
        0xF1 => {
            let [a, f] = pop(x, r).to_be_bytes();
            (r.a, r.f) = (a, f & 0xF0);
            12
        }
        0xE0 => {
            let v = x.imm(r);
            x.wr(0xFF00 | u16::from(v), r.a);
            12
        }
        0xF0 => {
            let v = x.imm(r);
            r.a = x.rd(0xFF00 | u16::from(v));
            12
        }
        0xE2 => {
            x.wr(0xFF00 | u16::from(r.c), r.a);
            8
        }
        0xF2 => {
            r.a = x.rd(0xFF00 | u16::from(r.c));
            8
        }
        0xEA | 0xFA => {
            let lo = x.imm(r);
            let hi = x.imm(r);
            let a = u16::from_le_bytes([lo, hi]);
            if op == 0xEA {
                x.wr(a, r.a);
            } else {
                r.a = x.rd(a);
            }
            16
        }
        _ => return None,
    };
    Some(total)
}

/// High byte first, at sp-1 then sp-2.
fn push(x: &mut Ext, r: &mut Registers, v: u16) {
    let [hi, lo] = v.to_be_bytes();
    r.sp = r.sp.wrapping_sub(2);
    x.wr(r.sp.wrapping_add(1), hi);
    x.wr(r.sp, lo);
}

fn pop(x: &mut Ext, r: &mut Registers) -> u16 {
    let lo = x.rd(r.sp);
    let hi = x.rd(r.sp.wrapping_add(1));
    r.sp = r.sp.wrapping_add(2);
    u16::from_le_bytes([lo, hi])
}

fn machine(rom: &[u8], hw: Hardware) -> Option<GB> {
    let cart = Cartridge::from_bytes(rom).ok()?;
    let mut gb = GB::new(hw);
    gb.insert(cart);
    gb.skip_bios();
    Some(gb)
}

fn regs_eq(a: &Registers, b: &Registers) -> bool {
    (a.a, a.f, a.b, a.c, a.d, a.e, a.h, a.l, a.pc, a.sp, a.ime)
        == (b.a, b.f, b.b, b.c, b.d, b.e, b.h, b.l, b.pc, b.sp, b.ime)
}

/// Lockstep one ROM on one model. `Err` names the first difference.
fn lockstep(rom: &[u8], hw: Hardware, frames: u64, skew: u32) -> Result<(), String> {
    let (Some(mut native), Some(mut ext)) = (machine(rom, hw), machine(rom, hw)) else {
        return Ok(());
    };
    let max_cc = native.ext_now() + frames * FRAME_CC;
    let mut x = Ext {
        gb: &mut ext,
        st: ExtBusState::default(),
        skew,
    };
    for i in 0..MAX_STEPS {
        if native.ext_now() >= max_cc {
            break;
        }
        native.step_instruction(false);
        x.step();
        let (ra, rb) = (native.get_cpu_registers(), x.gb.get_cpu_registers());
        let (ca, cb) = (native.ext_now(), x.gb.ext_now());
        if !regs_eq(ra, rb) || ca != cb {
            return Err(format!(
                "step {i}: native pc={:04x} cc={ca}, ext pc={:04x} cc={cb}",
                ra.pc, rb.pc
            ));
        }
    }
    native.sync_lazy_peripherals();
    ext.sync_lazy_peripherals();
    for (lo, hi, name) in [
        (0x8000u16, 0x9FFFu16, "VRAM"),
        (0xC000, 0xDFFF, "WRAM"),
        (0xFE00, 0xFE9F, "OAM"),
        (0xFF00, 0xFF7F, "IO"),
        (0xFF80, 0xFFFF, "HRAM+IE"),
    ] {
        let diff = (lo..=hi)
            .filter(|&a| native.read_memory(a) != ext.read_memory(a))
            .count();
        if diff != 0 {
            return Err(format!("{name}: {diff} bytes differ"));
        }
    }
    if native.get_current_frame().rgb() != ext.get_current_frame().rgb() {
        return Err("frame differs".into());
    }
    Ok(())
}

fn corpus(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(d) = dirs.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x.eq_ignore_ascii_case("gb") || x.eq_ignore_ascii_case("gbc"))
            {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn test_roms() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../gb-test-roms");
    if root.is_dir() {
        Some(root)
    } else {
        eprintln!("gb-test-roms absent; skipping");
        None
    }
}

#[test]
fn ext_path_matches_native_cpu() {
    let Some(root) = test_roms() else { return };
    let full = std::env::var("RB_EXT_LOCKSTEP").is_ok_and(|v| v == "full");
    let (stride, frames) = if full { (1, 120) } else { (64, 10) };
    let roms: Vec<PathBuf> = corpus(&root).into_iter().step_by(stride).collect();
    let jobs: Vec<(PathBuf, Hardware)> = MODELS
        .iter()
        .flat_map(|&hw| roms.iter().map(move |p| (p.clone(), hw)))
        .collect();
    let next = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                while let Some((path, hw)) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let Ok(rom) = std::fs::read(path) else {
                        continue;
                    };
                    if let Err(e) = lockstep(&rom, *hw, frames, 0) {
                        failures
                            .lock()
                            .unwrap()
                            .push(format!("{hw:?} {}: {e}", path.display()));
                    }
                }
            });
        }
    });
    let mut failures = failures.into_inner().unwrap();
    failures.sort();
    eprintln!(
        "ext lockstep: {} of {} runs match",
        jobs.len() - failures.len(),
        jobs.len()
    );
    assert!(
        failures.is_empty(),
        "ext path diverged:\n{}",
        failures.join("\n")
    );
}

/// The harness must notice a body that misreports its length.
#[test]
fn lockstep_detects_a_cycle_error() {
    let Some(root) = test_roms() else { return };
    let Ok(rom) = std::fs::read(root.join("dmg-acid2/dmg-acid2.gb")) else {
        return;
    };
    assert!(lockstep(&rom, Hardware::DMG, 2, 0).is_ok());
    assert!(lockstep(&rom, Hardware::DMG, 2, 4).is_err());
}
