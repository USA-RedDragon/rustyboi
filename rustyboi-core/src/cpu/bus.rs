use crate::memory::Addressable;
use crate::memory::mmio::Mmio;
use crate::ppu::{self, Ppu};
use std::ops::{Deref, DerefMut};

/// A tick-aware view over the system. CPU memory accesses go through `read`/
/// `write`, which advance every peripheral one M-cycle (4 dots) so each access
/// observes/mutates live state at its true intra-instruction cycle. Everything
/// else on `Mmio` is reached transparently via `Deref`.
pub struct Bus<'a> {
    pub mmio: &'a mut Mmio,
    pub ppu: &'a mut Ppu,
    // Dots elapsed since this instruction started; drives the double-speed PPU
    // gate (resets per instruction, matching the old per-`cpu_cycle` loop).
    dot: u32,
    ticked: u32,
}

impl<'a> Bus<'a> {
    pub fn new(mmio: &'a mut Mmio, ppu: &'a mut Ppu) -> Self {
        Bus {
            mmio,
            ppu,
            dot: 0,
            ticked: 0,
        }
    }

    pub fn ticked_dots(&self) -> u32 {
        self.ticked
    }

    /// Advance every peripheral by one dot.
    fn tick_t(&mut self) {
        self.mmio.step_timer();
        self.mmio.step_serial();
        self.mmio.step_dma();

        let double_speed = self.mmio.is_double_speed_mode();
        if !double_speed || self.dot % 2 == 0 {
            self.ppu.step_scheduled_stat_events(self.mmio);
            self.mmio.step_audio();
            self.ppu.step(self.mmio);
        }
        // HDMA triggers on the PPU's Mode 3->0 edge, so check it AFTER the PPU
        // has stepped this dot (otherwise the STAT mode it reads lags one dot).
        self.mmio.step_hdma();
        self.ppu.step_lcdc_events(self.mmio);

        self.dot = self.dot.wrapping_add(1);
        self.ticked += 1;
    }

    /// Tick the remaining internal (non-memory) cycles of an instruction.
    pub fn tick_remaining(&mut self, total_cycles: u32) {
        for _ in 0..total_cycles.saturating_sub(self.ticked) {
            self.tick_t();
        }
    }

    fn tick_m(&mut self) {
        for _ in 0..4 {
            self.tick_t();
        }
    }

    /// Tick one internal (non-memory) M-cycle, for opcodes that need their
    /// internal cycles placed at the right point (e.g. CALL's SP-dec before the
    /// stack pushes) rather than batched at instruction end.
    pub fn internal_cycle(&mut self) {
        self.tick_m();
    }

    /// Whether the PPU currently locks CPU access to `addr`: VRAM during Mode 3,
    /// OAM during Mode 2/3, and (CGB) the palette-data ports FF69/FF6B during
    /// Mode 3. Only while the LCD is on. Blocked reads return 0xFF; blocked
    /// writes are dropped.
    fn ppu_locks_access(&self, addr: u16) -> bool {
        let is_vram = (0x8000..=0x9FFF).contains(&addr);
        let is_oam = (0xFE00..=0xFE9F).contains(&addr);
        let is_cgb_pal = (addr == 0xFF69 || addr == 0xFF6B) && self.mmio.is_cgb_features_enabled();
        if !(is_vram || is_oam || is_cgb_pal) {
            return false;
        }
        if self.mmio.read(ppu::LCD_CONTROL) & 0x80 == 0 {
            return false;
        }
        let mode = self.mmio.read(ppu::LCD_STATUS) & 0x03;
        if is_oam { mode == 2 || mode == 3 } else { mode == 3 }
    }

    pub fn read(&mut self, addr: u16) -> u8 {
        self.tick_m();
        // VRAM is inaccessible to the CPU during Mode 3, OAM during Mode 2/3;
        // a blocked read returns open-bus 0xFF. Only while the LCD is on.
        if self.ppu_locks_access(addr) {
            return 0xFF;
        }
        self.mmio.read(addr)
    }

    pub fn write(&mut self, addr: u16, value: u8) {
        // Registers belonging to peripherals we tick inline (timer/serial/DMA)
        // latch at the end of the write M-cycle, so advance first. Everything
        // else (PPU registers, RAM) takes effect as the access is issued.
        //
        // While an OAM DMA transfer is running, the DMA engine advances during
        // this M-cycle *before* the CPU's write is resolved (Gambatte calls
        // `updateOamDma(cc)` at the top of `nontrivial_write`). A write into the
        // DMA's conflict area is then redirected into OAM[oamDmaPos_]. Ticking
        // the M-cycle first reproduces that ordering so `dma_pos` is the value
        // for this cycle when `mmio.write` resolves the conflict.
        // VRAM/OAM/CGB-palette writes are ignored while the PPU owns those
        // resources (see `ppu_locks_access`). Drop the write but still tick.
        // OAM-DMA conflicts are resolved separately in the tick-before path.
        if !self.mmio.dma_active() && self.ppu_locks_access(addr) {
            self.tick_m();
            return;
        }

        let tick_before = matches!(addr, 0xFF01..=0xFF02 | 0xFF04..=0xFF07 | 0xFF46 | 0xFF4A | 0xFF4B)
            || self.mmio.dma_active();
        if tick_before {
            self.tick_m();
            self.mmio.write(addr, value);
        } else {
            self.mmio.write(addr, value);
            if addr == ppu::LCD_CONTROL {
                self.ppu.handle_lcdc_write(value, self.mmio);
            }
            if self.mmio.take_stat_register_write_pending() {
                self.ppu.on_stat_register_write(self.mmio);
            }
            self.tick_m();
        }
    }
}

impl<'a> Deref for Bus<'a> {
    type Target = Mmio;
    fn deref(&self) -> &Mmio {
        self.mmio
    }
}

impl<'a> DerefMut for Bus<'a> {
    fn deref_mut(&mut self) -> &mut Mmio {
        self.mmio
    }
}
