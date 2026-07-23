//! OAM DMA (FF46): the sprite-attribute-table transfer engine.
//!
//! The continuously-running OAM-DMA cursor, its M-cycle cadence, and the
//! per-source-region bus-conflict model (`DmaSrcKind`) that governs what the
//! CPU reads/writes while a transfer holds the bus. Separate hardware unit
//! from the CGB VRAM DMA (see `hdma`/`gdma`).
//!
//! A child module of `mmio`, so it reaches `Mmio`'s private fields, the engine
//! structs and the parent's private helpers directly. Behaviour is identical
//! to the pre-split code — this is a pure relocation.
use super::DmaSrcKind;
use crate::memory::mmio::{
    Mmio, CARTRIDGE_BANK_END, CARTRIDGE_BANK_START, CARTRIDGE_END, CARTRIDGE_START, EMPTY_BYTE,
    EXTERNAL_RAM_END, EXTERNAL_RAM_START, OAM_SIZE, OAM_START, REG_DMA, VRAM_END, VRAM_START,
    WRAM_BANK_START, WRAM_START,
};
use crate::memory::{self, Addressable};

impl Mmio {
    /// Freeze/unfreeze the OAM-DMA across the STOP speed-switch unhalt window
    /// (hardware's halted branch — the OAM-DMA position stays put).
    pub(crate) fn set_oam_dma_stop_freeze(&mut self, freeze: bool) {
        self.oam_dma_stop_freeze = freeze;
        if freeze {
            // Hardware advances the OAM-DMA by the STOP's own
            // M-cycle before halting; arm the
            // single grace step that `step_dma` consumes (mirrors `halt.oam_grace`).
            //
            // SCOPED to a transfer at its FINAL byte (`dma.pos >= 158`). rustyboi's
            // eager per-dot OAM-DMA sits one M-cycle behind hardware's lazy
            // frozen position at the stop (pos 158 vs
            // hardware's 159) — the grace + `dma.pos==159` final-byte bypass below
            // completes the transfer (158 -> 159 -> 160 = OAM-DMA end) before the
            // freeze, so the post-window mode-2 sprite scan reads the COMPLETED OAM
            // (oamdma_late_speedchange_stat_2: the line's left-edge sprite maps,
            // mode-0 time +11, STAT read mode 3). A mid-transfer DMA (pos << 158, e.g.
            // the in-flight conflict-byte reads oamdmasrcC0_speedchange_readC000 /
            // hdma_transition_speedchange_oamdma) must stay frozen at its calibrated
            // position — those read the in-flight byte after the switch — so the
            // grace is gated OFF for them, keeping them byte-identical to baseline.
            if self.dma.active && self.dma.pos >= 158 {
                self.stop_oam_grace = 1;
            }
        }
    }

    /// The byte the OAM-DMA engine copies into `OAM[pos]`. Models the hardware
    /// OAM-DMA source pointer:
    ///   - invalid / off source -> disabled RAM (reads 0xFF).
    ///   - WRAM source -> the WRAM block selected by `src_high >> 4 & 1`, indexed by
    ///     the 12-bit offset (DMA source-high bit, NOT the CPU SVBK selection).
    ///   - rom/sram/vram -> normal read of `source_base + pos`.
    pub(in crate::memory) fn dma_source_byte(&self, pos: u8) -> u8 {
        match self.dma_src_kind() {
            // CGB src E000-FFFF: the DMA drives the external bus with the RAM
            // chip select asserted (gb-ctr "OAM DMA address decoding": all
            // A000-FFFF sources are external-RAM transfers), and on CGB the
            // WRAM is internal so only the cartridge answers - what it drives
            // is board-specific (`Cartridge::dma_sram_bus_read`): a lazy-CS
            // board returns SRAM[src & 0x1FFF] (AntonioND dma_valid_sources_gbc
            // real_gbc.sav rows E0-FF read the A000-BFFF fill: E0->A0..FF->BF),
            // a strict board leaves the bus floating 0xFF (the
            // srcE000_readFE00 cgb04c capture, RAMG on).
            DmaSrcKind::ExternalBus => {
                let src = self.dma.source_base.wrapping_add(pos as u16);
                match &self.cartridge {
                    Some(cart) => cart.dma_sram_bus_read(src),
                    None => 0xFF,
                }
            }
            DmaSrcKind::Wram => {
                let src = self.dma.source_base.wrapping_add(pos as u16);
                let wram_byte = self.dma_conflict_wram_read(src);
                // DMG src E000-FFFF: the echo fetch also asserts the external-RAM
                // /CS on the bus WRAM shares with the cartridge (gb-ctr: all
                // A000-FFFF DMA sources are external-RAM transfers, and the result
                // "depends on several factors, including the connected
                // cartridge"), so a lazy-/CS board contends with WRAM. Only the
                // DMG reaches this arm above E0 -- `dma_src_kind` routes CGB
                // E000+ to `ExternalBus`, where WRAM is not on the cart bus at
                // all. No-op below E0 and on strict boards / RAMG off.
                match (&self.cartridge, self.dma.source_base >> 8 >= 0xE0) {
                    (Some(cart), true) => wram_byte & cart.dma_sram_bus_read(src),
                    _ => wram_byte,
                }
            },
            _ => self.read_during_dma(self.dma.source_base.wrapping_add(pos as u16)),
        }
    }

    /// The byte an OAM-DMA M-cycle copies into `OAM[pos]`, modeling the VRAM-source
    /// bus conflict while the PPU has VRAM mode-3-locked. Both the OAM-DMA and the
    /// BG fetcher drive the VRAM address bus, so the array is indexed by the
    /// bitwise-AND of the two addresses (real-silicon "OAM DMA bus conflict").
    ///
    /// The fetcher address depends on which fetch substep is on the bus:
    ///   - tile-NUMBER read (`fetcher_bus_addr` in the 0x9800-0x9FFF tilemap range):
    ///     the byte is `VRAM[tilemap_addr & dma_addr]`. That same value is also the
    ///     POISONED tile number the fetcher latches, so we remember its tile-data
    ///     base for the next (tile-data) read.
    ///   - tile-DATA read (`fetcher_bus_addr` in 0x8000-0x97FF): the byte is
    ///     `VRAM[tiledata(poisoned_tile,row) & dma_addr]`, where the poisoned tile's
    ///     base was carried from the preceding tilemap read and the row low bits come
    ///     from this read's own fetcher address.
    ///
    /// The first locked M-cycle of a line (`fetcher_bus_warmup`) and any non-mode-3
    /// read fall back to the true VRAM source.
    ///
    /// Not in Pan Docs, TCAGBD, or GBCTR: GBCTR marks "OAM DMA bus conflicts" TODO,
    /// and TCAGBD §9.6.3's VRAM-read corruption describes the GDMA/HDMA engine, not
    /// OAM-DMA. The AND-with-fetcher model is from real-silicon .dump captures.
    fn dma_vram_conflict_or_source_byte(&mut self, pos: u8) -> u8 {
        let dma_addr = self.dma.source_base.wrapping_add(pos as u16);
        if self.dma_src_kind() != DmaSrcKind::Vram || !self.fetcher_bus_locked {
            // DMG mode-2 fetcher-prefetch onset: one M-cycle before the mode-3
            // lock, the fetcher already drives the first tile-NUMBER (tilemap)
            // address, so a VRAM-source DMA read here conflicts as the
            // address-line AND `VRAM[dma_addr & tilemap0]`. Only the LAST mode-2
            // M-cycle (`dmg_prefetch_active`, still unlocked) takes this; the
            // following first locked M-cycle is the warmup (clean) byte.
            if self.dma_src_kind() == DmaSrcKind::Vram
                && !self.cgb_features_enabled
                && self.dmg_prefetch_active
            {
                self.poison_tiledata_base = None;
                let eff_addr = dma_addr & self.dmg_prefetch_addr;
                return self.vram.read(eff_addr);
            }
            // Non-VRAM source, or VRAM free (HBlank/mode 0): true source byte.
            self.poison_tiledata_base = None;
            return self.dma_source_byte(pos);
        }
        if self.fetcher_bus_warmup {
            // First locked M-cycle of the line: the fetcher has not settled a
            // displayed-tile byte on the bus yet, so this reads clean VRAM.
            self.fetcher_bus_warmup = false;
            self.poison_tiledata_base = None;
            return self.dma_source_byte(pos);
        }
        let fa = self.fetcher_bus_addr;
        let bank = self.fetcher_bus_bank;
        let is_tile_number = (0x9800..=0x9FFF).contains(&fa);
        if !self.cgb_features_enabled {
            // DMG bus conflict. The DMG VRAM data bus behaves as an OR (not the
            // CGB address-line AND): a tile-DATA read returns
            // `VRAM[dma_addr | (fa & 0x0E)]` — the fetcher forces the tile-data
            // address's row bits high while the DMA address otherwise passes
            // through. A tile-NUMBER read on the bus does not conflict (reads true
            // VRAM), and does not poison.
            self.poison_tiledata_base = None;
            if is_tile_number {
                return self.dma_source_byte(pos);
            }
            return self.vram.read(dma_addr | (fa & 0x000E));
        }
        let eff_addr = if is_tile_number {
            // Tile-number read on the bus: AND the tilemap address. The resulting
            // byte is also the poisoned tile number, whose tile-data base feeds the
            // next read.
            let a = dma_addr & fa;
            let tile = self.read_vram_bank_internal(0, a);
            self.poison_tiledata_base = Some(0x8000u16.wrapping_add((tile as u16) << 4));
            a
        } else {
            // Tile-data read on the bus: substitute the poisoned tile's data base
            // (carried from the preceding tilemap read) for tile 0's, keeping this
            // read's own row low nibble, then AND the DMA address.
            let poisoned_fa = match self.poison_tiledata_base {
                Some(base) => base | (fa & 0x000F),
                None => fa,
            };
            dma_addr & poisoned_fa
        };
        if self.cgb_features_enabled && bank == 1 {
            self.vram_bank1.read(eff_addr)
        } else {
            self.vram.read(eff_addr)
        }
    }

    /// Advance the OAM-DMA engine by one M-cycle (one iteration of
    /// the hardware transfer loop). Advances `dma.pos`, (re)starts the
    /// transfer when it reaches `dma.start_pos`, copies the corresponding
    /// source byte into OAM, and ends the transfer at byte 160.
    pub(super) fn dma_advance_one_mcycle(&mut self) {
        // Apply any deferred CGB VRAM-source conflict-read OAM zero before this
        // M-cycle places a new byte (hardware zeroes inside the read itself).
        let pending = self.pending_oam_zero.get();
        if pending >= 0 {
            self.oam.write(OAM_START + pending as u16, 0);
            self.pending_oam_zero.set(-1);
        }

        self.dma.pos = self.dma.pos.wrapping_add(1);

        if self.dma.pos == self.dma.start_pos {
            // OAM-DMA start: transfer (re)starts from the top.
            self.dma.pos = 0;
            self.dma.start_pos = 0;
        }

        if self.dma.pos < 160 {
            let byte = self.dma_vram_conflict_or_source_byte(self.dma.pos);
            self.oam.write(OAM_START + self.dma.pos as u16, byte);
        } else if self.dma.pos == 160 {
            // OAM-DMA end: park the engine. Because no restart was requested
            // (`dma.start_pos == 0`), idle `dma.pos` at -2 and stop.
            if self.dma.start_pos == 0 {
                self.dma.pos = 0xFE;
                self.dma.active = false;
            }
        }
    }

    /// One OAM-DMA M-cycle that fires *inside* a concurrent GDMA/HDMA transfer.
    /// Unlike `dma_advance_one_mcycle` (which writes the OAM-DMA's own source
    /// byte), the conflict path writes the GDMA-read byte `data` into
    /// `OAM[src & 0xFF]` — the GDMA source low byte — mirroring the hardware
    /// DMA inner loop. Cells the GDMA bus index
    /// touches get overwritten with GDMA data; cells the OAM-DMA already wrote
    /// keep their values.
    ///
    /// `back_to_back` engages the 16-bit-word-bus quirk of two consecutive GDMA
    /// blocks (undocumented CGB silicon, captured by the oamdumper .dump
    /// references; unmodelled by other emulators). When the SECOND block's source
    /// low byte re-wraps over the low OAM cells the FIRST block already word-wrote
    /// (`src` high byte non-zero, `src & 0xFF < first-pass frontier`), hardware's
    /// word bus keeps the first block's value instead of re-clobbering — and that
    /// first-pass value is the word LOOK-AHEAD `mem[src_lo + 1]` (the low byte of
    /// the next 16-bit word the GDMA drove), not `mem[src]`. Pan Docs: "the PPU
    /// reads whatever 16-bit word the DMA unit is writing to OAM".
    pub(super) fn dma_conflict_advance(&mut self, src: u16, data: u8, back_to_back: bool) {
        self.dma.pos = self.dma.pos.wrapping_add(1);

        if self.dma.pos == self.dma.start_pos {
            self.dma.pos = 0;
            self.dma.start_pos = 0;
        }

        if (self.dma.pos as usize) < OAM_SIZE {
            let p = (src & 0xFF) as usize;
            // Second-block re-wrap into the 8-byte word-bus gap shadow: the four
            // OAM-DMA M-cycles between the two back-to-back FF55 writes advance the
            // OAM DMA while the GDMA source is frozen, so once the second block's
            // source carries past its page (`src & 0xFF00 != 0`) the FIRST eight
            // source bytes (`src & 0xFF < 8`, = 4 M-cycles x 2-byte word) still see
            // the first block's word LOOK-AHEAD latched on the bus rather than this
            // block's re-clobber value. For the first three shadow words the latched
            // byte is the plain look-ahead `mem[src_lo + 1]`; on the FOURTH (the
            // block-boundary word, `src_lo == 7`) the GDMA source has just re-aligned
            // to a word boundary out of the frozen gap, so its low address bit shifts
            // up one and the latched byte is `mem[((src_lo + 1) << 1) | 1]` — the high
            // byte of the re-aligned word (0x11 for the len09 reference, matching both
            // the srcC000 and src8000 hardware captures).
            let lo = src & 0x00FF;
            // First block's source page: the second block advanced the source one page
            // (0x100 bytes) past it, so de-carry to reach the word the first block drove.
            let first_page = (src & 0xFF00).wrapping_sub(0x100);
            if back_to_back && (src & 0xFF00) != 0 && lo < 8 && p < OAM_SIZE {
                let la_off = if lo == 7 { ((lo + 1) << 1) | 1 } else { lo + 1 };
                let la = self.dma_conflict_source_byte(first_page | la_off);
                self.oam.write(OAM_START + p as u16, la);
            } else if p < OAM_SIZE {
                self.oam.write(OAM_START + p as u16, data);
            } else if self.cgb_features_enabled {
                // p >= 160 writes the OAM-shadow tail (0xFEA0-0xFEFF) masked
                // with 0xE7 (the non-AGB branch).
                self.oam_high[(p & 0xE7) - 0xA0] = data;
            }
        } else if self.dma.pos as usize == OAM_SIZE
            && self.dma.start_pos == 0 {
                self.dma.pos = 0xFE;
                self.dma.active = false;
            }
    }

    /// Read a GDMA conflict source byte (same open-bus rules as `copy_dma_byte`:
    /// VRAM/echo region reads 0xFF). Used for the back-to-back word look-ahead.
    fn dma_conflict_source_byte(&mut self, src: u16) -> u8 {
        if (0x8000..=0x9FFF).contains(&src) || src >= 0xE000 {
            return 0xFF;
        }
        let saved = self.dma.active;
        self.dma.active = false;
        let byte = <Self as memory::Addressable>::read(self, src);
        self.dma.active = saved;
        byte
    }

    /// Handle a CPU write to FF46. Arms the engine: the transfer of byte 0
    /// begins two M-cycles later (`dma.start_pos = dma.pos + 2`). A write while
    /// a transfer is already running schedules a restart at that point, leaving
    /// the in-flight transfer to continue until then (DMA-restart behavior).
    pub(in crate::memory) fn start_oam_dma(&mut self, value: u8) {
        self.dma.start_pos = self.dma.pos.wrapping_add(2);
        self.dma.subcycle = 0;
        self.dma.source_base = (value as u16) << 8;
        self.dma.active = true;
        // Fresh OAM-DMA lifetime: the next GDMA-conflict pass is the FIRST, not a
        // back-to-back second block.
        self.gdma_conflict_ran = false;
        self.io_registers.write(REG_DMA, value);
    }

    #[inline]
    pub(crate) fn step_dma(&mut self) {
        // Fast path (the common case: no OAM-DMA in flight): inlined into the
        // per-dot loop so no wasm call is made. Firefox pays a real cost per
        // wasm call on the ~4M-dots/sec hot path; the cold work is outlined.
        if self.oam_dma_stall_suppress == 0 && !self.dma.active {
            return;
        }
        self.step_dma_slow();
    }

    #[cold]
    fn step_dma_slow(&mut self) {
        // During the GDMA/HDMA stall the OAM-DMA was already advanced inside the
        // transfer loop (hardware folds it into the DMA event); skip the dots that
        // re-tick the same transfer time so the OAM-DMA is not double-advanced.
        if self.oam_dma_stall_suppress > 0 {
            self.oam_dma_stall_suppress -= 1;
            return;
        }
        if !self.dma.active {
            return;
        }

        // One source byte is transferred per M-cycle (4 dots), not per dot.
        self.dma.subcycle += 1;
        if self.dma.subcycle < 4 {
            return;
        }
        self.dma.subcycle = 0;
        // STOP speed-switch unhalt window: the CPU is halted for the
        // 0x20000 cycles, so the OAM-DMA takes its halted branch
        // and freezes the OAM position. Mid-transfer OAM-DMA must stay put across the
        // window (oamdma_*_speedchange_* read the in-flight conflict byte after the
        // switch).
        // STOP speed-switch freeze: mirror the HALT-entry grace. Hardware's
        // STOP handler advances the OAM-DMA by the STOP's own M-cycle before halting, so
        // the STOP's own M-cycle advances the OAM-DMA one step, and a transfer
        // whose final byte (pos 159 -> 160 = OAM-DMA end) lands in that window
        // completes before the freeze. Same shape as the `cpu_halted` branch
        // below: one grace M-cycle, plus the pos==159 final-byte bypass.
        if self.oam_dma_stop_freeze {
            if self.stop_oam_grace > 0 {
                self.stop_oam_grace -= 1;
            } else if self.dma.pos != 159 {
                return;
            }
            // grace M-cycle, or the final byte: fall through to advance.
        } else
        // While the CPU is halted the OAM-DMA position is FROZEN: the OAM-DMA's
        // halt branch consumes the elapsed M-cycles
        // WITHOUT advancing the OAM position. Keep
        // the sub-M-cycle phase (reset above) but do not place a byte. Hardware's
        // HALT handler still advances ONE M-cycle at halt entry
        // (before the CPU actually halts), i.e. the HALT
        // instruction's own M-cycle moves the OAM-DMA; only subsequent halt
        // M-cycles freeze. `halt.oam_grace` lets exactly that one through.
        if self.cpu_halted {
            if self.halt.oam_grace > 0 {
                self.halt.oam_grace -= 1;
            } else if self.dma.pos != 159 {
                // Freeze the OAM-DMA mid-transfer during HALT. EXCEPTION: the very
                // last byte (pos 159 -> 160 = OAM-DMA end). Hardware
                // advances the OAM-DMA twice at halt entry, before
                // halting, so a transfer whose final byte's M-cycle lands
                // inside the halt-entry window completes BEFORE the freeze rather
                // than stalling to unhalt. rustyboi's per-dot `step_dma` catch-up
                // sits one M-cycle behind hardware at the halt
                // instant (the FF46 two-M-cycle arm phase), so the grace M-cycle
                // only reaches pos 159; letting pos 159 -> 160 through here lands
                // OAM-DMA end at the same point hardware does. A mid-transfer DMA
                // (pos << 159, e.g. oamdmasrc80_halt_*: pos 11) stays frozen.
                // Gating on the final byte keeps every existing freeze boundary
                // (the read8000 / hdma_transition_oamdma cases) byte-identical,
                // while letting oamdma_late_halt_stat_2 finish so LY=4's mode-2
                // scan sees the real OAM sprite (mode-0 time +11, STAT read mode 3).
                return;
            }
        }
        self.dma_advance_one_mcycle();
    }

    /// Read the pending DMA stall without consuming it or arming the post-DMA
    /// STAT-read bias (unlike `take_dma_stall`).
    pub(crate) fn peek_dma_stall(&self) -> u32 {
        self.pending_dma_stall
    }

    /// Drop `amount` cc from the pending DMA stall (saturating). Used to absorb a
    /// deferred stop_halt HDMA block's transfer span into the STOP unhalt window
    /// rather than charging it as a separate post-window stall.
    pub(crate) fn reduce_dma_stall(&mut self, amount: u32) {
        self.pending_dma_stall = self.pending_dma_stall.saturating_sub(amount);
    }

    /// Consume the CPU-cycle stall owed for completed HDMA/GDMA transfers.
    pub(crate) fn take_dma_stall(&mut self) -> u32 {
        let stall = std::mem::take(&mut self.pending_dma_stall);
        if stall > 0 {
            // Arm the post-DMA STAT-read bias (prefetch absorption) so the
            // first FF41 mode read after the stall resolves at hardware's read cc.
            self.dma.prefetch_stat_bias = true;
        }
        stall
    }

    /// Whether the next FF41 STAT-mode read should apply the post-DMA prefetch
    /// bias (resolve at `master_cc - 1`). Consumes the flag.
    pub(crate) fn take_dma_prefetch_stat_bias(&mut self) -> bool {
        std::mem::take(&mut self.dma.prefetch_stat_bias)
    }

    /// Whether the OAM-DMA engine is armed/running. Used by the bus to decide whether
    /// the DMA M-cycle must be advanced before resolving a CPU write.
    pub(crate) fn dma_active(&self) -> bool {
        self.dma.active
    }

    /// True while a transfer is actively placing bytes into OAM (the window in
    /// which the CPU bus conflicts with OAM DMA). Mirrors `dma.pos < 160`.
    pub(in crate::memory) fn dma_transfer_in_progress(&self) -> bool {
        self.dma.active && self.dma.pos < 160
    }

    /// Public view of the OAM-DMA "placing bytes" window (`OAM-DMA start` ..
    /// `OAM-DMA end`). The PPU's lazy sprite snapshot uses this to know when the
    /// OAM source reads as disabled RAM (0xFF), mirroring hardware pointing
    /// the OAM reader at the disabled-RAM source for the DMA window.
    pub(crate) fn oam_dma_window_active(&self) -> bool {
        self.dma_transfer_in_progress()
    }

    /// Take (and clear) the pending-CPU-OAM-write flag. The PPU drains this each
    /// dot to fire the sprite snapshot on an OAM write.
    pub(crate) fn take_oam_write_pending(&mut self) -> bool {
        let p = self.oam_write_pending;
        self.oam_write_pending = false;
        p
    }

    /// Copy the 80 OAM position bytes (Y at even index, X at odd index, for each
    /// of the 40 sprites) into `out`. Reads the raw OAM buffer directly,
    /// bypassing the DMA-conflict bus logic — the PPU sprite snapshot wants the
    /// true post-write OAM contents.
    pub(crate) fn peek_oam_pos(&self, out: &mut [u8; 80]) {
        for i in 0..40 {
            let base = OAM_START + (i as u16) * 4;
            let (y, x) = match self.mgb_frozen_oam_entry(i as u8) {
                Some([y, x, _, _]) => (y, x),
                None => (self.oam.read(base), self.oam.read(base + 1)),
            };
            out[2 * i] = y;
            out[2 * i + 1] = x;
        }
    }

    /// Source-region classification of the active OAM DMA. `ExternalBus` is
    /// CGB hardware only (see `dma_source_byte`).
    /// Bus topology is a *hardware* property, not a
    /// DMG-compat one: a DMG cart on CGB still has internal WRAM and the CGB
    /// external-bus decode (AntonioND dma_valid_sources_dmg_mode real_gbc.sav
    /// rows C0-FF match the native-CGB grid, not the DMG one).
    fn dma_src_kind(&self) -> DmaSrcKind {
        let cgb = self.is_cgb();
        let src_high = (self.dma.source_base >> 8) as u8;
        let wram_top: u16 = if cgb { 0xE0 } else { 0x100 };
        if src_high < 0xA0 {
            if src_high < 0x80 { DmaSrcKind::Rom } else { DmaSrcKind::Vram }
        } else if (src_high as u16) < wram_top {
            if src_high < 0xC0 { DmaSrcKind::Sram } else { DmaSrcKind::Wram }
        } else {
            DmaSrcKind::ExternalBus
        }
    }

    /// The WRAM "area" (bank slot) selected by the active OAM DMA during a CGB
    /// conflicting WRAM access. Mirrors hardware, where bit 4 of the DMA
    /// source-high byte in FF46 (NOT the CPU's SVBK selection) chooses between
    /// the fixed bank-0
    /// block (area 0) and the currently SVBK-banked block (area 1).
    fn dma_conflict_wram_area(&self) -> u8 {
        ((self.dma.source_base >> 8) >> 4 & 1) as u8
    }

    /// Bank index for an OAM-DMA-conflict WRAM access, or `None` for the base
    /// `wram_bank` buffer.
    ///
    /// Deliberately NOT [`Mmio::banked_wram_index`]: this path applies the
    /// SVBK select unconditionally, with no `cgb_features_enabled` gate. The
    /// conflict path is only reachable on CGB, so the gate is redundant rather
    /// than contradictory — but the two are not textually interchangeable and
    /// must not be merged without proving the gate can never differ here.
    fn dma_conflict_wram_index(&self) -> Option<usize> {
        match self.wram_bank_select {
            2..=7 => Some((self.wram_bank_select - 2) as usize),
            _ => None,
        }
    }

    /// Read the WRAM byte seen on a CGB OAM-DMA conflicting access. The byte is
    /// taken from the DMA-selected WRAM area at `[p & 0xFFF]`, so the address's C/D range is
    /// ignored: only the 12-bit offset and the DMA-derived area matter.
    fn dma_conflict_wram_read(&self, addr: u16) -> u8 {
        let offset = addr & 0x0FFF;
        if self.dma_conflict_wram_area() == 0 {
            self.wram.read(WRAM_START + offset)
        } else {
            match self.dma_conflict_wram_index() {
                Some(bank_index) => self.wram_banks[bank_index].read(WRAM_BANK_START + offset),
                None => self.wram_bank.read(WRAM_BANK_START + offset),
            }
        }
    }

    /// Write the CPU byte into WRAM during a CGB OAM-DMA conflict, matching the
    /// DMA-selected-area `[p & 0xFFF]` routing used by `dma_conflict_wram_read`.
    fn dma_conflict_wram_write(&mut self, addr: u16, value: u8) {
        let offset = addr & 0x0FFF;
        if self.dma_conflict_wram_area() == 0 {
            self.wram.write(WRAM_START + offset, value);
        } else {
            match self.dma_conflict_wram_index() {
                Some(bank_index) => {
                    self.wram_banks[bank_index].write(WRAM_BANK_START + offset, value)
                },
                None => self.wram_bank.write(WRAM_BANK_START + offset, value),
            }
        }
    }

    /// Resolve a CPU write that lands in the OAM-DMA conflict area while a
    /// transfer is in progress. Mirrors the hardware conflict branch: the write
    /// is redirected onto the shared bus, so the
    /// DMA copies the CPU-driven byte into `OAM[dma.pos]` instead of the
    /// original source byte. Returns true if the write was consumed here (and
    /// must not reach normal memory).
    pub(in crate::memory) fn dma_write_conflict(&mut self, addr: u16, value: u8) -> bool {
        if !self.dma_transfer_in_progress() || !self.dma_address_conflicts(addr) {
            return false;
        }
        let pos = self.dma.pos as u16;
        if self.is_cgb() {
            if addr < WRAM_START {
                // rom/sram/vram source: OAM latches the CPU byte (0 for vram).
                let byte = if self.dma_src_kind() == DmaSrcKind::Vram { 0 } else { value };
                self.oam.write(OAM_START + pos, byte);
            } else if self.dma_src_kind() != DmaSrcKind::Wram {
                // WRAM region with a non-WRAM source: the write still reaches
                // WRAM, but on the bank slot chosen by the DMA source-high bit,
                // not the CPU's SVBK selection.
                self.dma_conflict_wram_write(addr, value);
            }
            // WRAM region with a WRAM source: write is swallowed (no effect).
        } else {
            // DMG: OAM latches the CPU byte; a WRAM source ANDs with the byte
            // the DMA already placed (bus conflict).
            let byte = if self.dma_src_kind() == DmaSrcKind::Wram {
                self.oam.read(OAM_START + pos) & value
            } else {
                value
            };
            self.oam.write(OAM_START + pos, byte);
        }
        true
    }

    /// As `dma_transfer_in_progress`, but using the read-observed position.
    pub(in crate::memory) fn dma_read_conflict_active(&self) -> bool {
        self.dma.active && self.dma.pos < 160
    }

    /// Byte the CPU sees on a conflicting bus read while OAM DMA is mid-transfer.
    /// Mirrors the hardware conflict branch: the read
    /// observes `OAM[dma.pos]`, the byte the DMA just placed this M-cycle (the
    /// bus tick already advanced the engine before this read resolves). On CGB,
    /// a read of the WRAM region with a non-WRAM source instead returns the live
    /// WRAM byte.
    pub(in crate::memory) fn dma_conflict_byte(&self, addr: u16) -> u8 {
        // Hardware-level CGB gates: the WRAM-quirk read and
        // VRAM-source zeroing are silicon bus behaviors, active in DMG-compat
        // too (AntonioND dma_valid_sources_dmg_mode real_gbc.sav probes D0/E8/
        // F8/FC/FD during E-src DMA read the area-routed WRAM quirk bytes).
        if self.is_cgb() && self.dma_src_kind() != DmaSrcKind::Wram && addr >= WRAM_START {
            return self.dma_conflict_wram_read(addr);
        }
        let byte = self.oam.read(OAM_START + self.dma.pos as u16);
        // CGB with a VRAM source: the conflict read returns OAM[pos] but then
        // zeroes that OAM byte. Defer the zero to
        // the next DMA advance so the &self read path can record it.
        if self.is_cgb() && self.dma_src_kind() == DmaSrcKind::Vram {
            self.pending_oam_zero.set(self.dma.pos as i16);
        }
        byte
    }

    /// Whether a CPU access to `addr` conflicts with the in-progress OAM DMA.
    /// Faithful hardware model: classify the DMA
    /// source into rom/sram/vram/wram/invalid, then test a per-4KB-block
    /// conflict bitmask (which differs between DMG and CGB).
    pub(in crate::memory) fn dma_address_conflicts(&self, addr: u16) -> bool {
        if addr >= OAM_START {
            return false;
        }
        let cgb = self.is_cgb();
        let src = self.dma_src_kind();

        // Per-block conflict masks (bit n set => 4KB block n conflicts).
        let mask: u16 = match src {
            DmaSrcKind::Rom | DmaSrcKind::Sram => 0xFCFF,
            DmaSrcKind::Vram => 0x0300,
            DmaSrcKind::Wram => if cgb { 0xF000 } else { 0xFCFF },
            DmaSrcKind::ExternalBus => if cgb { 0xFCFF } else { 0x0000 },
        };
        (mask >> (addr >> 12)) & 1 != 0
    }

    // Private helper to read during DMA without triggering DMA conflicts.
    // Only reached from `dma_source_byte` for source kinds 0/1/2, so `addr` is
    // always below 0xC000 (ROM / VRAM / external RAM); WRAM and E000+ sources
    // take the conflict-WRAM / external-bus paths instead.
    fn read_during_dma(&self, addr: u16) -> u8 {
        debug_assert!(addr < 0xC000, "read_during_dma: source kind contract violated");
        match addr {
            CARTRIDGE_START..=CARTRIDGE_END => {
                // Boot-ROM overlay first (DMG 0x000-0x0FF, CGB 0x000-0x0FF +
                // 0x200-0x8FF), then fall through to the cartridge.
                if let Some(byte) = self.bios_overlay_read(addr) {
                    return byte;
                }
                match &self.cartridge {
                    Some(cart) => cart.read(addr),
                    None => EMPTY_BYTE,
                }
            },
            CARTRIDGE_BANK_START..=CARTRIDGE_BANK_END => {
                match &self.cartridge {
                    Some(cart) => cart.read(addr),
                    None => EMPTY_BYTE,
                }
            },
            // VRAM-source OAM DMA reads through the live VBK pointer, so a
            // mid-DMA VBK write retargets
            // subsequent source bytes. The mode-3 bus conflict is applied upstream
            // in `dma_vram_conflict_or_source_byte`; this path is the clean source.
            VRAM_START..=VRAM_END => {
                if self.cgb_features_enabled && self.vram_bank == 1 {
                    self.vram_bank1.read(addr)
                } else {
                    self.vram.read(addr)
                }
            },
            EXTERNAL_RAM_START..=EXTERNAL_RAM_END => {
                match &self.cartridge {
                    Some(cart) => cart.read(addr),
                    None => EMPTY_BYTE,
                }
            },
            _ => EMPTY_BYTE,
        }
    }

    /// Whether the PPU's per-dot OAM snapshot snoop could possibly observe an
    /// event this dot: an OAM-DMA window edge needs `dma.active` (or the
    /// PPU-side previous-window flag, checked by the caller) and a CPU OAM
    /// write needs `oam_write_pending`. When both are clear and the PPU saw no
    /// window last dot, `process_oam_reader_events` is a guaranteed no-op.
    #[inline]
    pub(crate) fn oam_snoop_event_possible(&self) -> bool {
        self.dma.active || self.oam_write_pending
    }

    /// Whether the per-dot OAM-DMA bus-conflict publish
    /// (`Ppu::update_dma_fetcher_bus`) has a possible consumer: the conflict
    /// resolution inside `step_dma_slow` runs only under this same predicate.
    #[inline]
    pub(crate) fn oam_dma_bus_snoop_needed(&self) -> bool {
        self.dma.active || self.oam_dma_stall_suppress != 0
    }
}
