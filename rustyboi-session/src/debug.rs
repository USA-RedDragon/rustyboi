//! A serializable read-model of the running machine for the egui debug panels.
//!
//! The panels used to read `&GB` directly, which only works on desktop where the
//! emulator and UI share a thread. On web the emulator lives in a Web Worker and
//! the egui UI on the main thread has no `&GB`. [`DebugSnapshot`] bridges that:
//! the worker builds it from its `Session` and posts it (bincode over the
//! `postMessage` boundary); the main thread deserializes and the panels render
//! from the snapshot. Desktop builds the same snapshot inline. Because it is a
//! read-only projection built from the core's existing debug accessors, it can
//! never perturb emulation — the 27 hardware-test suites stay byte-identical by
//! construction.
//!
//! The baseline (registers + core MMIO + breakpoints + PPU status) is kept small
//! (<~1 KiB) so it is cheap to post every frame. The large sections (full memory
//! image, VRAM, OAM, palettes, a stack window) are each an `Option` populated
//! only when the panel that needs them is open — see [`DebugDetail`].

use serde::{Deserialize, Serialize};

use rustyboi_core_lib::memory::mmio;
use rustyboi_core_lib::ppu;

use crate::session::Session;

/// Which heavy snapshot sections to populate. Computed by the frontend from
/// which debug panels are currently open, so a closed panel costs nothing to
/// build (and, on web, nothing to post). An all-false detail yields only the
/// small baseline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebugDetail {
    /// Full 64 KiB CPU-visible memory image (Memory Explorer).
    pub memory: bool,
    /// Both VRAM banks + tile data (Tile Explorer, PPU Debug, Sprite Debug).
    pub vram: bool,
    /// The 160-byte OAM table (Sprite Debug).
    pub oam: bool,
    /// DMG/CGB palette data (Palette Explorer, Tile/Sprite previews).
    pub palettes: bool,
    /// A stack window around SP (Stack Explorer).
    pub stack: bool,
}

impl DebugDetail {
    /// Nothing requested — the common case (no debug panel open).
    pub fn is_empty(&self) -> bool {
        !(self.memory || self.vram || self.oam || self.palettes || self.stack)
    }

    /// Pack the five section flags into a byte bitmask for the compact
    /// main-thread→worker web message (bit 0 memory … bit 4 stack).
    pub fn to_bits(self) -> u8 {
        (self.memory as u8)
            | (self.vram as u8) << 1
            | (self.oam as u8) << 2
            | (self.palettes as u8) << 3
            | (self.stack as u8) << 4
    }

    /// Inverse of [`DebugDetail::to_bits`].
    pub fn from_bits(bits: u8) -> DebugDetail {
        DebugDetail {
            memory: bits & 0x01 != 0,
            vram: bits & 0x02 != 0,
            oam: bits & 0x04 != 0,
            palettes: bits & 0x08 != 0,
            stack: bits & 0x10 != 0,
        }
    }

    /// Union of two detail sets (any section either wants is populated).
    pub fn union(self, other: DebugDetail) -> DebugDetail {
        DebugDetail {
            memory: self.memory || other.memory,
            vram: self.vram || other.vram,
            oam: self.oam || other.oam,
            palettes: self.palettes || other.palettes,
            stack: self.stack || other.stack,
        }
    }
}

/// Decoded CPU registers + flags. Mirrors the fields the CPU Registers / Stack
/// panels read off the live `Registers`, so those panels never touch `&GB`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuState {
    pub a: u8,
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    pub pc: u16,
    pub sp: u16,
    pub ime: bool,
}

impl CpuState {
    pub fn flag_z(&self) -> bool {
        self.f & 0x80 != 0
    }
    pub fn flag_n(&self) -> bool {
        self.f & 0x40 != 0
    }
    pub fn flag_h(&self) -> bool {
        self.f & 0x20 != 0
    }
    pub fn flag_c(&self) -> bool {
        self.f & 0x10 != 0
    }
}

/// PPU pipeline state (formerly read via `get_ppu_debug_info`'s live `&Ppu`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PpuMode {
    OamSearch,
    PixelTransfer,
    HBlank,
    VBlank,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct PpuState {
    pub disabled: bool,
    pub mode: PpuMode,
    pub ticks: u128,
    pub x: u8,
    pub has_frame: bool,
    pub sprites_on_line: usize,
    pub fetcher_pixels: [u8; 8],
}

/// Core MMIO registers the panels display. Read through the same `read_memory`
/// path the panels used, so blocking/open-bus behaviour is identical.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct MmioState {
    pub lcdc: u8,
    pub stat: u8,
    pub ly: u8,
    pub lyc: u8,
    pub scx: u8,
    pub scy: u8,
    pub wx: u8,
    pub wy: u8,
    pub bgp: u8,
    pub obp0: u8,
    pub obp1: u8,
    pub dma: u8,
    pub div: u8,
    pub tima: u8,
    pub tma: u8,
    pub tac: u8,
    pub ie: u8,
    pub iflags: u8,
    // CGB-only registers (0 on DMG).
    pub vbk: u8,
    pub svbk: u8,
    pub key1: u8,
    pub bcps: u8,
    pub ocps: u8,
}

/// Palette RAM for the palette / tile / sprite panels. DMG uses only the three
/// monochrome registers (already in [`MmioState`]); CGB fills the RGB555 tables.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PaletteData {
    /// 8 background palettes × 4 colors, RGB555.
    pub bg: Vec<u16>,
    /// 8 object palettes × 4 colors, RGB555.
    pub obj: Vec<u16>,
}

/// A window of stack memory around SP for the Stack Explorer.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StackWindow {
    /// Address of `bytes[0]`.
    pub base: u16,
    /// Raw bytes from `base` upward (covers the panel's scroll range).
    pub bytes: Vec<u8>,
}

/// The complete debug read-model. The baseline fields are always present and
/// small; the `Option` sections are populated per [`DebugDetail`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DebugSnapshot {
    pub cgb: bool,
    pub cpu: CpuState,
    pub ppu: PpuState,
    pub mmio: MmioState,
    /// Sorted active CPU breakpoints (Breakpoint Manager).
    pub breakpoints: Vec<u16>,
    /// A small instruction window starting at PC, for the CPU panel's inline
    /// disassembly (kept in the baseline so that panel needs no heavy section).
    /// `pc_bytes[i]` is the byte at `PC + i`.
    pub pc_bytes: [u8; PC_WINDOW],

    /// Full 64 KiB CPU-visible memory (Memory Explorer). `DebugDetail::memory`.
    pub memory: Option<Vec<u8>>,
    /// VRAM bank 0 then bank 1, each 0x8000..=0x9FFF (8 KiB). `DebugDetail::vram`.
    pub vram: Option<[Vec<u8>; 2]>,
    /// The 160-byte OAM table (0xFE00..). `DebugDetail::oam`.
    pub oam: Option<Vec<u8>>,
    /// CGB palette RAM. `DebugDetail::palettes` (DMG palettes live in `mmio`).
    pub palettes: Option<PaletteData>,
    /// A stack window around SP. `DebugDetail::stack`.
    pub stack: Option<StackWindow>,
}

/// Start of VRAM in the CPU address space.
const VRAM_START: u16 = 0x8000;
/// Length of one VRAM bank (0x8000..=0x9FFF).
const VRAM_LEN: usize = 0x2000;
/// Start of OAM.
const OAM_START: u16 = 0xFE00;
/// OAM length in bytes (40 sprites × 4).
const OAM_LEN: usize = 0xA0;
/// Bytes captured below `base` in the stack window (covers the panel's full
/// up-scroll: default `sp-8`, then up to 100 two-byte scroll steps).
const STACK_BELOW: u16 = 0xE0;
/// Total stack window length (covers the panel's full ±100-step scroll range).
const STACK_LEN: usize = 0x1C0;
/// Bytes captured from PC for the CPU panel's 5-instruction disassembly window
/// (covers 5 back-to-back 3-byte instructions plus the final operand fetch).
pub const PC_WINDOW: usize = 20;

impl Session {
    /// Build a read-only [`DebugSnapshot`] of the current machine. `detail`
    /// selects which heavy sections to populate; the baseline is always built.
    /// Reads only through the core's existing debug accessors, so it cannot
    /// affect emulation.
    pub fn debug_snapshot(&self, detail: DebugDetail) -> DebugSnapshot {
        let gb = self.gb();
        let cgb = gb.should_enable_cgb_features();
        let regs = gb.get_cpu_registers();

        let cpu = CpuState {
            a: regs.a,
            f: regs.f,
            b: regs.b,
            c: regs.c,
            d: regs.d,
            e: regs.e,
            h: regs.h,
            l: regs.l,
            pc: regs.pc,
            sp: regs.sp,
            ime: regs.ime,
        };

        let (ppu_ref, fetcher_pixels) = gb.get_ppu_debug_info();
        let mode = match ppu_ref.get_state() {
            ppu::State::OAMSearch => PpuMode::OamSearch,
            ppu::State::PixelTransfer => PpuMode::PixelTransfer,
            ppu::State::HBlank => PpuMode::HBlank,
            ppu::State::VBlank => PpuMode::VBlank,
        };
        let ppu_state = PpuState {
            disabled: ppu_ref.is_disabled(),
            mode,
            ticks: ppu_ref.get_ticks(),
            x: ppu_ref.get_x(),
            has_frame: ppu_ref.has_frame(),
            sprites_on_line: ppu_ref.get_sprites_on_line_count(),
            fetcher_pixels,
        };

        let r = |addr: u16| gb.read_memory(addr);
        let mmio_state = MmioState {
            lcdc: r(ppu::LCD_CONTROL),
            stat: r(ppu::LCD_STATUS),
            ly: r(ppu::LY),
            lyc: r(ppu::LYC),
            scx: r(ppu::SCX),
            scy: r(ppu::SCY),
            wx: r(ppu::WX),
            wy: r(ppu::WY),
            bgp: r(ppu::BGP),
            obp0: r(ppu::OBP0),
            obp1: r(ppu::OBP1),
            dma: r(mmio::REG_DMA),
            div: r(0xFF04),
            tima: r(0xFF05),
            tma: r(0xFF06),
            tac: r(0xFF07),
            ie: r(0xFFFF),
            iflags: r(0xFF0F),
            vbk: if cgb { r(mmio::REG_VBK) } else { 0 },
            svbk: if cgb { r(mmio::REG_SVBK) } else { 0 },
            key1: if cgb { r(0xFF4D) } else { 0 },
            bcps: if cgb { r(mmio::REG_BCPS) } else { 0 },
            ocps: if cgb { r(mmio::REG_OCPS) } else { 0 },
        };

        let mut breakpoints: Vec<u16> = gb.get_breakpoints().iter().copied().collect();
        breakpoints.sort_unstable();

        let mut pc_bytes = [0u8; PC_WINDOW];
        for (i, b) in pc_bytes.iter_mut().enumerate() {
            *b = r(regs.pc.wrapping_add(i as u16));
        }

        let memory = detail.memory.then(|| (0u16..=0xFFFF).map(&r).collect());

        let vram = detail.vram.then(|| {
            let bank = |b: u8| {
                (0..VRAM_LEN)
                    .map(|i| gb.read_vram_bank(b, VRAM_START + i as u16))
                    .collect::<Vec<u8>>()
            };
            [bank(0), bank(1)]
        });

        let oam = detail
            .oam
            .then(|| (0..OAM_LEN).map(|i| r(OAM_START + i as u16)).collect());

        let palettes = (detail.palettes && cgb).then(|| {
            let mut bg = Vec::with_capacity(32);
            let mut obj = Vec::with_capacity(32);
            for palette in 0..8u8 {
                for color in 0..4u8 {
                    bg.push(gb.read_bg_palette_data(palette, color));
                    obj.push(gb.read_obj_palette_data(palette, color));
                }
            }
            PaletteData { bg, obj }
        });

        let stack = detail.stack.then(|| {
            let base = regs.sp.saturating_sub(STACK_BELOW);
            let bytes = (0..STACK_LEN)
                .map(|i| r(base.saturating_add(i as u16)))
                .collect();
            StackWindow { base, bytes }
        });

        DebugSnapshot {
            cgb,
            cpu,
            ppu: ppu_state,
            mmio: mmio_state,
            breakpoints,
            pc_bytes,
            memory,
            vram,
            oam,
            palettes,
            stack,
        }
    }
}

impl DebugSnapshot {
    /// Serialize for transport across the web worker→main `postMessage` boundary.
    /// Uses bincode (already the savestate format) so the same round-trip is
    /// exercised by both the web bridge and the unit test.
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap_or_default()
    }

    /// Inverse of [`DebugSnapshot::to_bytes`].
    pub fn from_bytes(bytes: &[u8]) -> Option<DebugSnapshot> {
        bincode::deserialize(bytes).ok()
    }

    /// RGB888 for a CGB background palette color, or `None` if not captured / DMG.
    pub fn cgb_bg_rgb(&self, palette: u8, color: u8) -> Option<(u8, u8, u8)> {
        let idx = palette as usize * 4 + color as usize;
        self.palettes.as_ref().and_then(|p| p.bg.get(idx)).map(|&c| rgb555_to_888(c))
    }

    /// RGB888 for a CGB object palette color, or `None` if not captured / DMG.
    pub fn cgb_obj_rgb(&self, palette: u8, color: u8) -> Option<(u8, u8, u8)> {
        let idx = palette as usize * 4 + color as usize;
        self.palettes.as_ref().and_then(|p| p.obj.get(idx)).map(|&c| rgb555_to_888(c))
    }

    /// Raw RGB555 for a CGB background palette color (for the hex readout).
    pub fn cgb_bg_rgb555(&self, palette: u8, color: u8) -> Option<u16> {
        let idx = palette as usize * 4 + color as usize;
        self.palettes.as_ref().and_then(|p| p.bg.get(idx)).copied()
    }

    /// Raw RGB555 for a CGB object palette color (for the hex readout).
    pub fn cgb_obj_rgb555(&self, palette: u8, color: u8) -> Option<u16> {
        let idx = palette as usize * 4 + color as usize;
        self.palettes.as_ref().and_then(|p| p.obj.get(idx)).copied()
    }

    /// A byte for the CPU panel's disassembly around PC. Reads from the small
    /// baseline PC window when `addr` falls inside it, else from the full memory
    /// image if captured, else 0. Lets the CPU panel disassemble without needing
    /// the heavy 64 KiB section.
    pub fn code_byte(&self, addr: u16) -> u8 {
        let off = addr.wrapping_sub(self.cpu.pc) as usize;
        if off < PC_WINDOW {
            self.pc_bytes[off]
        } else {
            self.mem(addr)
        }
    }

    /// A byte from the captured full-memory image, or 0 if memory was not
    /// requested / the address is out of range.
    pub fn mem(&self, addr: u16) -> u8 {
        self.memory.as_ref().and_then(|m| m.get(addr as usize)).copied().unwrap_or(0)
    }

    /// A byte from captured VRAM `bank` (0/1), or 0 if VRAM was not requested.
    pub fn vram_byte(&self, bank: u8, addr: u16) -> u8 {
        let off = addr.wrapping_sub(VRAM_START) as usize;
        self.vram
            .as_ref()
            .and_then(|banks| banks.get(bank as usize & 1))
            .and_then(|b| b.get(off))
            .copied()
            .unwrap_or(0)
    }

    /// A byte from the captured OAM table, or 0 if OAM was not requested.
    /// `addr` is a CPU address in 0xFE00..0xFEA0.
    pub fn oam_byte(&self, addr: u16) -> u8 {
        let off = addr.wrapping_sub(OAM_START) as usize;
        self.oam.as_ref().and_then(|o| o.get(off)).copied().unwrap_or(0)
    }

    /// A byte from the captured stack window, or 0 outside it.
    pub fn stack_byte(&self, addr: u16) -> u8 {
        let Some(w) = self.stack.as_ref() else { return 0 };
        let off = addr.wrapping_sub(w.base) as usize;
        w.bytes.get(off).copied().unwrap_or(0)
    }
}

/// Convert a Game Boy Color RGB555 value to RGB888 (matches the panels' scaling).
pub fn rgb555_to_888(rgb555: u16) -> (u8, u8, u8) {
    let r = ((rgb555 & 0x1F) * 255 / 31) as u8;
    let g = (((rgb555 >> 5) & 0x1F) * 255 / 31) as u8;
    let b = (((rgb555 >> 10) & 0x1F) * 255 / 31) as u8;
    (r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::ports::{MemRumble, MemStorage, MemWebcam};
    use crate::session::{Ports, Session};
    use rustyboi_core_lib::gb::Hardware;

    fn booted_session(hardware: Hardware) -> Session {
        let config = Config { hardware, ..Default::default() };
        let ports = Ports {
            storage: Box::new(MemStorage::new()),
            rumble: Box::new(MemRumble::default()),
            webcam: Box::new(MemWebcam::default()),
        };
        Session::new(config, ports, [0u8; 32])
    }

    #[test]
    fn baseline_reflects_registers_and_mmio() {
        let session = booted_session(Hardware::DMG);
        let snap = session.debug_snapshot(DebugDetail::default());

        // Baseline must match the live core reads the panels used.
        let gb = session.gb();
        let regs = gb.get_cpu_registers();
        assert_eq!(snap.cpu.pc, regs.pc);
        assert_eq!(snap.cpu.sp, regs.sp);
        assert_eq!(snap.cpu.a, regs.a);
        assert_eq!(snap.mmio.lcdc, gb.read_memory(ppu::LCD_CONTROL));
        assert_eq!(snap.mmio.ly, gb.read_memory(ppu::LY));
        assert_eq!(snap.mmio.ie, gb.read_memory(0xFFFF));

        // No detail requested → no heavy sections, and the baseline stays small.
        assert!(snap.memory.is_none());
        assert!(snap.vram.is_none());
        assert!(snap.oam.is_none());
        assert!(snap.stack.is_none());
        assert!(
            snap.to_bytes().len() < 1024,
            "baseline snapshot should be < 1 KiB, was {}",
            snap.to_bytes().len()
        );
    }

    #[test]
    fn detail_gates_heavy_sections() {
        let session = booted_session(Hardware::DMG);
        let detail = DebugDetail {
            memory: true,
            vram: true,
            oam: true,
            palettes: true,
            stack: true,
        };
        let snap = session.debug_snapshot(detail);
        assert_eq!(snap.memory.as_ref().map(Vec::len), Some(0x10000));
        let vram = snap.vram.as_ref().expect("vram populated");
        assert_eq!(vram[0].len(), VRAM_LEN);
        assert_eq!(vram[1].len(), VRAM_LEN);
        assert_eq!(snap.oam.as_ref().map(Vec::len), Some(OAM_LEN));
        assert!(snap.stack.is_some());
    }

    #[test]
    fn bincode_round_trip_is_lossless() {
        let session = booted_session(Hardware::CGB);
        let detail = DebugDetail {
            memory: true,
            vram: true,
            oam: true,
            palettes: true,
            stack: true,
        };
        let snap = session.debug_snapshot(detail);
        let bytes = snap.to_bytes();
        let round = DebugSnapshot::from_bytes(&bytes).expect("round-trip");

        assert_eq!(round.cpu, snap.cpu);
        assert_eq!(round.mmio.lcdc, snap.mmio.lcdc);
        assert_eq!(round.cgb, snap.cgb);
        assert_eq!(round.memory, snap.memory);
        assert_eq!(round.vram, snap.vram);
        assert_eq!(round.oam, snap.oam);
        assert_eq!(round.breakpoints, snap.breakpoints);
        assert_eq!(round.palettes.map(|p| p.bg), snap.palettes.map(|p| p.bg));
    }
}
