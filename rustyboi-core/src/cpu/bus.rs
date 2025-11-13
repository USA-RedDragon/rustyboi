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
        // Gate the PPU/audio step on the *persistent* T-phase parity so the
        // PPU's even-dot stepping stays aligned with the true accumulated cc
        // across instruction boundaries (per-instruction `dot` would re-anchor
        // the phase to the instruction start every M-cycle).
        if !double_speed || self.mmio.cpu_t_phase() % 2 == 0 {
            self.ppu.step_scheduled_stat_events(self.mmio);
            self.mmio.step_audio();
            self.ppu.step(self.mmio);
        } else {
            // Double-speed odd half-dot: the renderer steps once per pixel-dot
            // (the even phase above), but the CPU runs a second M-cycle here. Run
            // a STAT/IRQ sub-dot so events scheduled at an odd `abs_cc` fire at
            // the true half-dot instead of being rounded to the next render dot.
            self.ppu.step_subdot(self.mmio);
        }
        // HDMA triggers on the PPU's exact mode-0 (HBlank) entry, so check it
        // AFTER the PPU has stepped this dot. Prefer the renderer's cycle-exact
        // `hdma_period` predicate (Gambatte `isHdmaPeriod`); fall back to the
        // STAT mode-edge when no closed-form mode-0 dot is available.
        let period = self.ppu.hdma_period(double_speed);
        self.mmio.step_hdma(period);
        self.ppu.step_lcdc_events(self.mmio);

        self.mmio.advance_cpu_t_phase();
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
    fn ppu_locks_access(&self, addr: u16, access_cc: u64) -> bool {
        self.ppu_blocks(addr, true, access_cc)
    }

    /// Whether the PPU locks `addr` from a CPU access of the given direction.
    /// Boundary precision (the exact mode-2->3 and mode-3->0 transition dots)
    /// uses the renderer's cycle-exact predictor (`cpu_access_blocked`), which
    /// mirrors Gambatte's lineCycle thresholds. When no closed-form mode-0 dot
    /// is available (window / first line after enable) it falls back to the
    /// FF41 mode bits.
    fn ppu_blocks(&self, addr: u16, is_read: bool, access_cc: u64) -> bool {
        let is_vram = (0x8000..=0x9FFF).contains(&addr);
        let is_oam = (0xFE00..=0xFE9F).contains(&addr);
        let is_cgb_pal = (addr == 0xFF69 || addr == 0xFF6B) && self.mmio.is_cgb_features_enabled();
        if !(is_vram || is_oam || is_cgb_pal) {
            return false;
        }
        if self.mmio.read(ppu::LCD_CONTROL) & 0x80 == 0 {
            return false;
        }
        let kind: u8 = if is_vram { 0 } else if is_oam { 1 } else { 2 };
        let mode = self.mmio.read(ppu::LCD_STATUS) & 0x03;
        let mode_locked = if is_oam { mode == 2 || mode == 3 } else { mode == 3 };
        let ds = self.mmio.is_double_speed_mode();
        let is_cgb = self.mmio.is_cgb_features_enabled();
        if let Some(blocked) = self.ppu.cpu_access_blocked(kind, is_read, mode_locked, is_cgb, ds, access_cc) {
            return blocked;
        }
        mode_locked
    }

    pub fn read(&mut self, addr: u16) -> u8 {
        // APU reads (NRxx status, NR52, wave RAM) observe the channels at the
        // read M-cycle START cc (Gambatte resolves the read before advancing).
        // Snapshot the value before ticking; the per-dot step during tick_m would
        // otherwise let a length expiry scheduled within this M-cycle disable a
        // channel 4 dots early — making the cycle-exact `nr52` boundary tests
        // (length expiry at `((cc>>13)+len)<<13` vs the NR52 read cc) read 0 one
        // M-cycle too soon. NR52 status must reflect the pre-tick enabled state.
        let apu_read = if (0xFF10..=0xFF3F).contains(&addr) {
            // Resolve the APU length subsystem at the canonical per-access cc
            // (the SAME cc the timer register access resolves on), so the
            // length-expiry boundary is decided off one uniform clock with no
            // APU-specific phase constant (M7).
            let access_cc = self.mmio.access_cc();
            self.mmio.sync_apu_read_cc(access_cc);
            Some(self.mmio.read(addr))
        } else {
            None
        };
        // Serial registers observe serial state at the read's start cc; snapshot
        // before tick (mirrors the APU read hook).
        let serial_read = if matches!(addr, 0xFF01 | 0xFF02) {
            Some(self.mmio.snapshot_serial_read(addr))
        } else {
            None
        };
        // TIMA/TMA/TAC reads (FF05-07) derive against `abs_cc`; resolve the read
        // at the access START cc (Gambatte `read(addr,cc)`), before the M-cycle
        // ticks `abs_cc` forward 4. Snapshot here so the read anchor equals the
        // scheduled-IRQ delivery anchor (D1/D2). DIV (FF04) stays on the
        // post-tick path with serial/APU (not yet on start-cc).
        let timer_read = if matches!(addr, 0xFF04..=0xFF07) {
            Some(self.mmio.read(addr))
        } else {
            None
        };
        // IF read: the CPU resolves it at cc, but tick_m advances peripherals and
        // would let an IRQ flagged within this read M-cycle leak in 4 dots early.
        // Snapshot the VBlank (0), STAT (1), and serial (3) bits pre-tick so an
        // IRQ raised within this read cycle is observed at the read's start cc
        // (matching Gambatte's read-at-cc); the timer/joypad bits keep the
        // post-tick path, where their flag timing is already tuned to the full
        // M-cycle. Serial completion fires mid-tick_m on the boundary tests, so
        // snapshotting bit 3 makes the read resolve at cc like Gambatte.
        const IF_PRE_MASK: u8 = 0x0B;
        let if_pre = if addr == 0xFF0F {
            Some(self.mmio.snapshot_serial_read(addr) & IF_PRE_MASK)
        } else {
            None
        };
        // FF41 (STAT) read-at-cc: Gambatte resolves the mode bits with
        // `getStat(cc)` at the access START cc, where the mode-3 -> mode-0
        // boundary is `cc + 2 < m0Time`. The per-dot renderer sets the FF41 mode
        // register at a dot boundary that, at double speed, can round the odd-cc
        // sample; resolve it here against the closed-form m0Time so the straddle
        // pairs (m2int_*_m3stat_ds, etc.) sample the same sub-dot boundary
        // Gambatte does. Snapshot pre-tick so the read anchors at access_cc.
        let stat_mode_pre = if addr == ppu::LCD_STATUS {
            // Gambatte resolves FF41 at the raw master cc (== Gambatte `cc`);
            // boundary mode3 iff `master_cc + 2 < m0Time`.
            let access_cc = self.mmio.master_cc();
            let r = self.ppu.get_stat_mode3to0_at_cc(access_cc);
            if std::env::var("RB_DBG_M0").is_ok() {
                eprintln!("[FF41] master_cc={} m0t={:?} mode={:?} ly={} m3len={} lyt={}", access_cc, self.ppu.dbg_m0_time(), r, self.mmio.read(0xFF44), self.ppu.dbg_m3len(), self.ppu.dbg_lytime(&self.mmio));
            }
            r
        } else {
            None
        };
        // Snapshot the access cc at the read's START (Gambatte resolves PPU
        // access gating at `cc` before advancing). The cgbp begin/end boundary
        // is master-cc based and must anchor here, not at the post-tick cc.
        // CL1: PPU access-gating (VRAM/OAM/cgbp) resolves at the honest
        // start-of-access cc; the +4 vs the old `access_cc()` is folded into the
        // `cpu_access_blocked` boundary constants.
        let pre_access_cc = self.mmio.ppu_access_cc();
        self.tick_m();
        // VRAM is inaccessible to the CPU during Mode 3, OAM during Mode 2/3;
        // a blocked read returns open-bus 0xFF. Only while the LCD is on.
        if self.ppu_locks_access(addr, pre_access_cc) {
            return 0xFF;
        }
        if let Some(v) = apu_read {
            return v;
        }
        if let Some(v) = serial_read {
            return v;
        }
        if let Some(v) = timer_read {
            return v;
        }
        if let Some(pre) = if_pre {
            return pre | (self.mmio.read(addr) & !IF_PRE_MASK);
        }
        if let Some(mode) = stat_mode_pre {
            return (self.mmio.read(addr) & !0x03) | (mode & 0x03);
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
        if !self.mmio.dma_active() && self.ppu_blocks(addr, false, self.mmio.ppu_access_cc()) {
            self.tick_m();
            return;
        }

        // TIMA/TMA/TAC writes (FF05-07) resolve at the access START cc, then time
        // advances (Gambatte `write(addr,data,cc); cc += 4`). The scheduled-TIMA
        // model derives/IRQ-delivers against `abs_cc`, so the write must land
        // before the M-cycle ticks for its anchor to match the start-cc read
        // anchor (D1). FF04 (DIV) stays on the tick-before path below: its
        // `div_anchor` is shared by serial/APU-FS which are NOT yet on start-cc
        // (the atomic cluster move — serial WRITE_CC_OFFSET drop + APU root 2 —
        // is the follow-up step), so moving DIV alone would mix anchors.
        if matches!(addr, 0xFF04..=0xFF07) {
            self.mmio.write(addr, value);
            self.tick_m();
            return;
        }

        // SC (FF02) ABORT write resolves at the access START cc (M8 serial
        // merge): clearing the transfer-start/internal-clock bits must land
        // BEFORE this M-cycle's per-dot `step_serial` runs — otherwise the
        // in-flight transfer completes during the tick and raises the serial IF
        // the abort is meant to suppress. A START write (bits set) keeps the
        // tick-before path so its scheduled completion cc is unchanged (the
        // nopx/late_div_write completion-timing cases are tuned to it).
        let sc_abort = addr == 0xFF02 && (value & 0x81) != 0x81;
        if sc_abort {
            self.mmio.write(addr, value);
            self.tick_m();
            return;
        }

        let tick_before = matches!(addr, 0xFF01..=0xFF02 | 0xFF46 | 0xFF4A | 0xFF4B)
            || self.mmio.dma_active();
        if tick_before {
            // FF4A (WY): schedule Gambatte's `wy2` at the write's cc (read-at-
            // cc-start phase, like the STAT path) before the M-cycle ticks.
            if addr == ppu::WY {
                self.ppu.set_write_subdot(self.mmio.cpu_t_phase());
                self.ppu.on_wy_write(value, self.mmio);
            }
            self.tick_m();
            self.mmio.write(addr, value);
        } else {
            // The write resolves at the current persistent T-phase, before this
            // M-cycle's dots tick. Pass that phase's sub-dot parity so the PPU
            // STAT/LYC hooks place the event on the correct half-dot at DS.
            self.ppu.set_write_subdot(self.mmio.cpu_t_phase());
            self.mmio.write(addr, value);
            // FF42/FF43 (SCY/SCX): the CPU readback above is immediate, but the
            // BG fetcher must see the new value ~N dots later (write-side analog
            // of the wy1/wy2 latches). The mmio write phase is unchanged (kept in
            // this else branch, pre-tick) so steady-state rendering is identical;
            // only the fetcher's mid-M3 view is delayed via on_sc{y,x}_write.
            if addr == ppu::SCY {
                self.ppu.on_scy_write(value, self.mmio);
            }
            if addr == ppu::SCX {
                self.ppu.on_scx_write(value, self.mmio);
            }
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
