//! OAM DMA (FF46) and CGB HDMA/GDMA (FF51-FF55) transfer engines.
//!
//! Extracted verbatim from the parent `mmio` module to keep `Mmio`'s core
//! register/dispatch surface readable. This is a child module of `mmio`, so it
//! reaches `Mmio`'s private fields, the engine structs (`HdmaEngine`,
//! `OamDmaEngine`, `DmaSrcKind`) and the parent's private helpers directly; the
//! register-backed state these methods drive still lives on `Mmio` and its
//! embedded engines. Behaviour is unchanged by the move.
use super::*;

impl Mmio {
    /// Copy a single byte from `src` to the VRAM destination corresponding
    /// to `dst`. Shared by GDMA and HDMA. Caller advances `hdma.source` /
    /// `hdma.dest`. Models the hardware transfer inner loop:
    ///   - Source reads from VRAM (0x8000-0x9FFF) or >=0xE000 (WRAM
    ///     mirror / OAM / IO / HRAM) return 0xFF (open bus).
    ///   - Destination wraps within the currently selected VRAM bank
    ///     (modulo 0x2000), written at 0x8000 | (dst & 0x1FFF).
    fn copy_dma_byte(&mut self, src: u16, dst: u16) -> u8 {
        // Bypass DMA-active gating while we drive the bus read internally:
        // GDMA / HDMA are separate transfer engines from OAM DMA.
        let saved_dma_active = self.dma.active;
        self.dma.active = false;

        let byte = if (0x8000..=0x9FFF).contains(&src) {
            0xFF
        } else if src >= 0xE000 {
            // gb-ctr: E000+ HDMA/GDMA sources are external-bus reads with the
            // RAM chip select asserted; what responds is board-specific (see
            // `Cartridge::dma_sram_bus_read`). AntonioND hdma_valid_sources
            // real_gbc.sav rows E000/FE00/FEA0/FFE0 read the lazy-CS cart's
            // SRAM[src & 0x1FFF] (A000/BE00/BEA0/BFE0 fills); a strict board
            // floats 0xFF (the previous constant).
            match &self.cartridge {
                Some(cart) => cart.dma_sram_bus_read(src),
                None => 0xFF,
            }
        } else {
            <Self as memory::Addressable>::read(self, src)
        };

        let vram_addr = VRAM_START | (dst & 0x1FFF);
        if self.cgb_features_enabled && self.vram_bank == 1 {
            self.vram_bank1.write(vram_addr, byte);
        } else {
            self.vram.write(vram_addr, byte);
        }

        self.dma.active = saved_dma_active;
        byte
    }

    /// Resolve a single HDMA byte WITHOUT committing the VRAM write. Reads the
    /// source byte at the current (fire) cc — matching hardware's read-at-cc —
    /// and returns `(vram_addr, value, into_bank1)` for a deferred apply. Used by
    /// the deferred-block model so the VRAM write lands at the correct sub-M-cycle
    /// (`fire_cc + hdma.write_delay`) rather than coincident with the trigger.
    fn resolve_dma_byte(&mut self, src: u16, dst: u16) -> (u16, u8, bool) {
        let saved_dma_active = self.dma.active;
        self.dma.active = false;
        let byte = if (0x8000..=0x9FFF).contains(&src) {
            0xFF
        } else if src >= 0xE000 {
            // Board-specific external-bus response (see `copy_dma_byte`).
            match &self.cartridge {
                Some(cart) => cart.dma_sram_bus_read(src),
                None => 0xFF,
            }
        } else {
            <Self as memory::Addressable>::read(self, src)
        };
        self.dma.active = saved_dma_active;
        let vram_addr = VRAM_START | (dst & 0x1FFF);
        let into_bank1 = self.cgb_features_enabled && self.vram_bank == 1;
        (vram_addr, byte, into_bank1)
    }

    /// Commit one deferred HDMA byte write into VRAM (bank captured at fire).
    fn apply_dma_write(&mut self, vram_addr: u16, byte: u8, into_bank1: bool) {
        if into_bank1 {
            self.vram_bank1.write(vram_addr, byte);
        } else {
            self.vram.write(vram_addr, byte);
        }
    }

    /// Record the first destination byte's pre-transfer VRAM value for the
    /// PC-in-DMA-dest prefetch-absorption case. Reads the current VRAM byte at
    /// the block's first dest address (in the bank that will be written) before
    /// the transfer overwrites it.
    fn snapshot_dma_dest0_pre(&mut self) {
        let vram_addr = VRAM_START | (self.hdma.dest & 0x1FFF);
        let into_bank1 = self.cgb_features_enabled && self.vram_bank == 1;
        let pre = if into_bank1 {
            self.vram_bank1.read(vram_addr)
        } else {
            self.vram.read(vram_addr)
        };
        self.hdma.fire_dest0 = Some(vram_addr);
        self.hdma.fire_dest0_prebyte = pre;
        self.hdma.fire_cc = self.master_cc();
    }

    /// If the just-fired DMA block's first destination byte equals `pc` (the
    /// CPU's next opcode-fetch address), consume and return its pre-transfer
    /// value together with the dma-event fire cc (for the prefetch's VRAM-lock
    /// decision). One-shot. Returns None when no fire is pending or `pc` is not
    /// the block's first dest byte.
    pub(crate) fn take_dma_prefetch_shadow(&mut self, pc: u16) -> Option<(u8, u64)> {
        if self.hdma.fire_dest0 == Some(pc) {
            self.hdma.fire_dest0 = None;
            return Some((self.hdma.fire_dest0_prebyte, self.hdma.fire_cc));
        }
        None
    }

    /// Consume the pending VRAM-source GDMA first-word latch (see the field
    /// doc); returns the first dest word's VRAM address + bank flag.
    pub(crate) fn take_gdma_vram_src_fixup(&mut self) -> Option<(u16, bool)> {
        self.gdma_vram_src_fixup.take()
    }

    /// Patch the latched first word with the absorbed-prefetch byte.
    pub(crate) fn apply_gdma_vram_src_fixup(&mut self, addr: u16, byte: u8, into_bank1: bool) {
        for a in [addr, addr.wrapping_add(1)] {
            let a = VRAM_START | (a & 0x1FFF);
            if into_bank1 {
                self.vram_bank1.write(a, byte);
            } else {
                self.vram.write(a, byte);
            }
        }
    }

    /// Clear any stale DMA prefetch-shadow (called once the next opcode has been
    /// fetched without consuming it, so it cannot leak to a later access).
    pub(crate) fn clear_dma_prefetch_shadow(&mut self) {
        self.hdma.fire_dest0 = None;
        self.hdma.snapshot_armed = false;
    }

    /// True while deferred HDMA block writes are still in their
    /// per-dot countdown (`step_hdma_deferred` must run each dot to commit them at
    /// the right cc). Blocks the idle bulk-skip.
    pub(crate) fn has_pending_hdma_deferred(&self) -> bool {
        !self.hdma.pending_writes.is_empty()
    }

    /// Drain the deferred-HDMA write buffer one dot. When the delay expires the
    /// resolved bytes are committed to VRAM in order.
    #[inline]
    pub(crate) fn step_hdma_deferred(&mut self) {
        if self.hdma.pending_writes.is_empty() {
            return;
        }
        self.step_hdma_deferred_slow();
    }

    fn step_hdma_deferred_slow(&mut self) {
        if self.hdma.write_delay > 0 {
            self.hdma.write_delay -= 1;
        }
        if self.hdma.write_delay == 0 {
            let pending = std::mem::take(&mut self.hdma.pending_writes);
            for (addr, byte, into_bank1) in pending {
                self.apply_dma_write(addr, byte, into_bank1);
            }
        }
    }

    /// Execute a CGB General-Purpose DMA (GDMA) transfer synchronously.
    /// Copies `length` bytes from `self.hdma.source` into VRAM starting at
    /// `self.hdma.dest`. Matches hardware:
    ///   - If the LCD is off, GDMA does not run.
    ///   - Destination clamped if it would overflow the 16-bit address
    ///     space.
    pub(super) fn execute_gdma(&mut self, length: usize) {
        // In hardware the `length` bytes are
        // transferred regardless of LCD state. The LCD-off branch only zeroes the
        // *remaining* HDMA block count, it does NOT skip the active
        // transfer. A pure GDMA kick therefore still copies its bytes (and
        // interleaves the OAM DMA) with the LCD off. Skipping it here used to drop
        // the GDMA conflict on LCD-off re-runs of the oamdumper tests, letting a
        // clean OAM-DMA pass overwrite the conflict bytes.

        self.snapshot_dma_dest0_pre();
        let mut src = self.hdma.source;
        let mut dst = self.hdma.dest;

        let effective_length = if (dst as usize) + length >= 0x10000 {
            0x10000 - dst as usize
        } else {
            length
        };

        // Arm the VRAM-source first-word latch (see `gdma_vram_src_fixup`).
        self.gdma_vram_src_fixup = if (0x8000..=0x9FFF).contains(&src) && effective_length >= 2 {
            let into_bank1 = self.cgb_features_enabled && self.vram_bank == 1;
            Some((VRAM_START | (dst & 0x1FFF), into_bank1))
        } else {
            None
        };

        let ds = self.is_double_speed_mode();
        let per_byte_cc: i64 = if ds { 4 } else { 2 };

        // OAM-DMA interleave. The OAM-DMA engine keeps
        // advancing one M-cycle (4 cc) per `loam += 4` step while the GDMA copies
        // bytes. The bus ran one `tick_m` (step_dma) before resolving this FF55
        // write, leaving rustyboi's `dma.pos` one M-cycle BEHIND the hardware
        // position at the kick instant. Catch up by one M-cycle (advance the
        // OAM-DMA position without a conflict write) so the gate below fires on
        // the same boundaries hardware does.
        // A block fired inside the STOP speed-switch unhalt window must NOT advance
        // the OAM-DMA: hardware freezes the OAM-DMA position (its halted branch)
        // while the CPU is
        // halted across the STOP. rustyboi's `step_dma` already honors
        // `oam_dma_stop_freeze`, but the synchronous HDMA-block interleave here
        // bypassed it, advancing `dma.pos` ~16 bytes and shifting the post-switch
        // in-flight conflict byte (hdma_transition_speedchange_oamdma: read 0x60
        // where hardware's frozen position reads 0x71).
        let interleave = self.dma.active && !self.oam_dma_stop_freeze;
        // The one-M-cycle catch-up corrects the `step_dma` tick that ran before this
        // FF55 write resolved. A back-to-back second block follows its predecessor
        // with no intervening `step_dma` (the two FF55 writes are adjacent), so its
        // OAM-DMA position was already caught up by the first block — catching up
        // again would end the OAM DMA one M-cycle early (drops the last conflict
        // clobber, e.g. OAM[45] in oamdmasrcC000_..._2xgdmalen09).
        if interleave && !self.gdma_conflict_ran {
            self.dma_advance_one_mcycle();
        }
        // Back-to-back second GDMA block: the OAM DMA is still mid-flight from a
        // FIRST GDMA-conflict pass in the same lifetime (no OAM-DMA completion in
        // between). Hardware's 16-bit word bus then holds the first block's already
        // word-written low OAM cells across the FF55-rewrite boundary gap, so this
        // block's low-address re-wrap must not re-clobber them (see
        // `dma_conflict_advance`). A single long GDMA (one pass) is never back-to-back.
        let back_to_back = interleave && self.gdma_conflict_ran;
        // `loam` tracks the OAM-DMA's relative update cursor: it starts at
        // `-dma.subcycle` (dots already elapsed in the current M-cycle) and the
        // per-byte cc advance is compared against `loam + 3` (gate `cc-3 > loam`).
        let mut cc: i64 = 0;
        let mut loam: i64 = -(self.dma.subcycle as i64);

        for _ in 0..effective_length {
            let data = self.copy_dma_byte(src, dst);
            cc += per_byte_cc;
            if interleave && self.dma.active && cc - 3 > loam {
                loam += 4;
                self.gdma_conflict_ran = true;
                self.dma_conflict_advance(src, data, back_to_back);
            }
            src = src.wrapping_add(1);
            dst = dst.wrapping_add(1);
        }
        // After the block, the OAM-DMA continues from the advanced position. The
        // residual `loam` phase becomes the next M-cycle's sub-cycle offset so
        // `step_dma` resumes on the correct dot (the OAM-DMA update cursor carries
        // the residual phase forward).
        if interleave && self.dma.active {
            // Dots elapsed since the last OAM-DMA M-cycle fired. `step_dma` fires
            // when `dma.subcycle` reaches 4, so the residual phase `(cc - loam)`
            // (mod 4) is exactly the count already accrued toward the next
            // M-cycle (the OAM-DMA update cursor carries `loam` forward and
            // recomputes the sub-cycle phase as `(cc - cursor) >> 2`).
            self.dma.subcycle = (cc - loam).rem_euclid(4) as u8;
        }

        self.hdma.source = src;
        self.hdma.dest = dst;

        // Hardware charges `2 + 2*ds` cc per byte for the entire
        // transfer plus a single trailing `cc += 4`, regardless of block count
        // (the +4 setup is NOT per-block). For one block this is 36 SS / 68 DS.
        // Hardware runs GDMA as an event preceded by an opcode prefetch
        // (the next opcode is fetched *before* the transfer's cc advance) and a
        // trailing `cc += 4`. Synchronous GDMA here charges the transfer up
        // front, so the post-stall return must absorb that prefetch/setup
        // overlap; +5 lands the next STAT-mode read on the exact mode-0 dot for
        // the gdma_cycles boundary pairs (the PPU position trailed the synced
        // master cc by ~1 dot at the read with the old +6 — see fix-gdma).
        //
        // Two back-to-back FF55=0 kicks (gdma_cycles_2xshort) drain as effectively
        // ONE prefetch sequence: the prefetch absorption
        // happens once across the pair, not per DMA event. The first kick set
        // `dma.prefetch_stat_bias` (its stall was drained, no STAT read has consumed
        // it yet); a second kick before that consumption must add only the raw
        // transfer + trailing setup (no second `+5`), else the post-stall STAT read
        // lands ~5 dots late at double speed (2xshort_ds_1 reads mode 0 where
        // hardware still reads mode 3 at `cc + 2 < mode-0 time`).
        //
        // The DS `setup` is 5 rather than the hardware trailing `+4` because a
        // single kick's prefetch overlap absorbs one cc. A back-to-back second
        // kick has no prefetch of its own to absorb (the pair shares one prefetch
        // sequence, as above), so it must charge the raw hardware `+4`. Charging 5
        // there put the following read one cc late: every other plain DS
        // gdma/hdma boundary pair brackets mode-0 time at `m0t-cc` = 5 (out3, mode 3)
        // and 1 (out0, mode 0) — 4 cc, one M-cycle apart — while 2xshort_ds sat at
        // 4 and 1, a 3 cc spread that no pair of reads one M-cycle apart can have.
        let (per_byte, mut setup) = if self.is_double_speed_mode() { (4, 5) } else { (2, 4) };
        let prefetch_fudge = if self.dma.prefetch_stat_bias { 0 } else { 5 };
        if self.is_double_speed_mode() && self.dma.prefetch_stat_bias {
            setup -= 1;
        }
        self.pending_dma_stall += (effective_length as u32) * per_byte + setup + prefetch_fudge;
        // The OAM-DMA M-cycles for the transfer were folded into the loop above.
        // Suppress `step_dma` for the true dma-event duration (the transfer
        // `per_byte` cc plus the single trailing `cc += 4`), NOT the extra `+5`
        // CPU-stall prefetch fudge. Hardware freezes the OAM-DMA cursor for the
        // event then catches the OAM-DMA up afterward; the residual
        // post-stall cc advance the OAM-DMA normally toward the next access.
        if interleave {
            self.oam_dma_stall_suppress = (effective_length as u32) * per_byte + 4;
        }
    }

    // ----------------------------------------------------------------------
    // HDMA accessors used by gb.rs / cpu / ppu.
    // ----------------------------------------------------------------------

    pub(crate) fn hdma_is_enabled(&self) -> bool {
        self.cgb_features_enabled && self.hdma.enabled
    }

    pub(crate) fn hdma_req_pending(&self) -> bool {
        self.hdma.req_pending
    }

    /// Whether this HDMA period's block has already been serviced (the DMA event
    /// for this m0 edge already ran and acked, so no HDMA request is
    /// flagged — no block is owed/prefetched at a STOP).
    pub(crate) fn hdma_block_done_this_period(&self) -> bool {
        self.hdma.block_done_this_period
    }

    /// Remaining HDMA blocks minus one (the FF55 length field). 0 => the next
    /// block completes the transfer.
    pub(crate) fn hdma_length(&self) -> u8 {
        self.hdma.length
    }

    /// Arm the High-at-halt unhalt edge-consume: the first post-unhalt m0 HDMA edge
    /// is suppressed (it was the during-halt edge hardware already consumed). Called
    /// at the unhalt site when `halt.hdma_state == High`.
    pub(crate) fn arm_hdma_high_unhalt_consume(&mut self) {
        self.hdma.high_unhalt_consume = true;
    }

    /// Arm the Requested-unhalt sub-block-cc consume.
    /// Called at the unhalt site when a `Requested`-at-halt block is reflagged so
    /// `step_hdma` can absorb the next-line m0 arm iff it falls within the freshly
    /// fired block's transfer span (hardware's m0 HDMA event consumed by the
    /// in-flight transfer), deferring the genuine next block one line.
    pub(crate) fn arm_hdma_peraccess_consume(&mut self) {
        self.hdma.peraccess_consume_pending = true;
    }

    /// When a post-unhalt m0 rising edge would arm the
    /// next HDMA block, decide whether it must instead be CONSUMED because it falls
    /// within the just-fired (Requested-unhalt reflag) block's transfer span. Hardware
    /// processes the m0 HDMA event at the in-flight transfer's end cc, so an edge
    /// landing inside `[fire_cc, fire_cc + 16*(2+2*ds))` is absorbed and its block
    /// deferred to the next line; an edge at/after that span fires this line. Returns
    /// true (consume this arm, clearing the pending flag) iff a Requested-unhalt
    /// consume is armed and the current dot cc is strictly inside the span. Otherwise
    /// leaves the arm to proceed. The pending flag is one-shot: it is cleared whether
    /// the arm is consumed (inside span) or allowed (past span), so it only ever gates
    /// the single immediate post-unhalt m0 edge.
    fn peraccess_consume_m0_arm(&mut self) -> bool {
        if !self.hdma.peraccess_consume_pending {
            return false;
        }
        let fire_cc = match self.hdma.last_fire_cc {
            Some(c) => c,
            None => {
                self.hdma.peraccess_consume_pending = false;
                return false;
            }
        };
        let ds = self.is_double_speed_mode() as u64;
        let span: u64 = 0x10 * (2 + 2 * ds);
        let now = self.master_cc();
        // Inclusive end: hardware's m0 HDMA event landing AT the in-flight
        // block's transfer-end cc is still absorbed (the edge == block end is
        // consumed; only an edge strictly PAST the transfer fires its own block).
        let end = fire_cc.wrapping_add(span);
        if now >= fire_cc && now <= end {
            // Inside block1's transfer span: absorb this m0 arm (and any further
            // re-detect of the SAME edge through the period->STAT-fallback handoff).
            // Keep pending armed so every in-span dot is consumed.
            true
        } else {
            // At/past the span end: the genuine next-line block. Let it arm and
            // disarm the consume so subsequent lines are untouched.
            self.hdma.peraccess_consume_pending = false;
            false
        }
    }

    /// Master cc of the last HDMA block fire (None if none in-flight this period).
    pub(crate) fn hdma_last_fire_cc(&self) -> Option<u64> {
        self.hdma.last_fire_cc
    }

    /// Whether an HDMA block is latched and would fire at the next
    /// M-cycle boundary (the `fire_pending_hdma_mcycle` precondition).
    pub(crate) fn hdma_fire_pending(&self) -> bool {
        self.hdma.req_pending && self.hdma.enabled
    }

    pub(crate) fn set_hdma_req(&mut self) {
        if self.cgb_features_enabled && self.hdma.enabled {
            self.hdma.req_pending = true;
        }
        // A requested block fires through step_hdma at any mode; wake the
        // tracker immediately.
        self.hdma.tracker_sleep_until = 0;
    }

    /// Master cc below which the per-dot HDMA tracker may be skipped.
    #[inline]
    pub(crate) fn hdma_tracker_sleep_until(&self) -> u64 {
        self.hdma.tracker_sleep_until
    }

    /// PPU-installed HDMA-tracker sleep bound (see the field doc).
    #[inline]
    pub(crate) fn set_hdma_tracker_sleep(&mut self, until: u64) {
        self.hdma.tracker_sleep_until = until;
    }

    pub(crate) fn halt_hdma_state(&self) -> HaltHdmaState {
        self.halt.hdma_state
    }

    pub(crate) fn set_halt_hdma_state(&mut self, s: HaltHdmaState) {
        self.halt.hdma_state = s;
    }

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

    /// Sticky: this ROM has driven the HDMA/GDMA machinery (FF55 written).
    pub(crate) fn hdma_machinery_used(&self) -> bool {
        self.hdma.machinery_used
    }

    pub(crate) fn set_halt_wakeup_hdma(&mut self, v: bool) {
        self.halt.wakeup_hdma = v;
    }

    pub(crate) fn halt_wakeup_hdma(&self) -> bool {
        self.halt.wakeup_hdma
    }

    /// Resolve a pending FF55 bit7=1 kick (`hdma.kick_eval_pending`)
    /// against the LIVE HDMA-period predicate the bus evaluates at the write
    /// access cc (the in-HBlank-period predicate at cc+4). If in period
    /// the first block is armed immediately; otherwise the kick is dropped and the
    /// block arms on the next Mode 3->0 edge (hardware schedules the HDMA event
    /// to the next m0 without flagging now). Returns whether a kick
    /// was pending (so the bus knows it consumed it).
    pub(crate) fn resolve_hdma_kick(&mut self, in_period: bool) -> bool {
        if !self.hdma.kick_eval_pending {
            return false;
        }
        self.hdma.kick_eval_pending = false;
        if in_period && self.hdma.enabled {
            self.hdma.req_pending = true;
            // Instruction-driven in-period kick: arm the prefetch-absorption
            // snapshot for the block this kick will fire (pc_7ffe). Cleared by
            // the snapshot or the next opcode fetch.
            self.hdma.snapshot_armed = true;
            // DEFERRED-HDMA-FIRE: the kick services THIS period's block. Mark it
            // done so an immediately-following `halt` captures `halt.hdma_state =
            // High` (in-period + already-serviced) rather than
            // `Requested`, which would re-fire a SECOND block on unhalt
            // (hdma_late_m0halt_*). The per-dot `hdma_period` is false mid-HBlank
            // (post-mode-0 time crossing) so `step_hdma`'s own block_done set never
            // fires for a kick-armed block; set it here at the in-period kick.
            self.hdma.block_done_this_period = true;
            // An in-HBlank kick services this HBlank's one block via the
            // cycle-exact closed-form predicate, which LEADS the CPU-visible STAT
            // mode. Mark the HBlank serviced so the STAT-mode-3->0 fallback in
            // `step_hdma` (which lags, firing on the register edge) does NOT arm a
            // SECOND block for the same scanline — the Pokémon Crystal Elm's-lab
            // arm-line double-fire. `step_hdma`'s own edge arms set it too (the
            // rise/fallback double-fire is the same lead/lag aliasing). Cleared on
            // the next LY change.
            self.hdma.block_fired_this_hblank = true;
        }
        true
    }

    /// Whether an FF55 bit7=1 kick is awaiting the bus's live-period resolution.
    pub(crate) fn hdma_kick_eval_pending(&self) -> bool {
        self.hdma.kick_eval_pending
    }

    pub(crate) fn set_hdma_disable_fires(&mut self, v: Option<bool>) {
        self.hdma.disable_fires = v == Some(true);
    }

    pub(crate) fn hdma_is_in_period_cached(&self) -> bool {
        self.hdma.is_in_period_cached
    }

    /// "In HDMA period" as seen by the unhalt re-flag gate. Uses the cycle-exact
    /// renderer period when available, else falls back to the FF41 STAT mode-0
    /// gate (matching `step_hdma`'s fallback edge model) so unhalt re-flagging
    /// works on the window / first-line paths where no closed-form mode-0 dot
    /// exists. LCD-off counts as permanently in period.
    pub(crate) fn hdma_in_period_for_unhalt(&self) -> bool {
        let lcdc = self.io_registers.read(ppu::LCD_CONTROL);
        let lcd_on = lcdc & (ppu::LCDCFlags::DisplayEnable as u8) != 0;
        if !lcd_on {
            return true;
        }
        if self.hdma.is_in_period_cached {
            return true;
        }
        (self.io_registers.read(ppu::LCD_STATUS) & 0x03) == 0
    }

    /// Execute one 0x10-byte HDMA block. Caller must have verified
    /// `hdma.req_pending && hdma.enabled`. Bytes are copied synchronously;
    /// callers charge the returned CPU-cycle stall via the outer per-cycle
    /// loop so PPU/timer/audio continue to tick during the transfer.
    pub(crate) fn run_hdma_block(&mut self) -> u32 {
        self.run_hdma_block_inner(false)
    }

    /// Execute one HDMA block whose DMA event fires while the CPU is in the
    /// STOP speed-switch halt window (hardware's halted branch). The 0x10 source
    /// bytes are still copied (the
    /// `read_hdmadst00` destination-content tests depend on it), but FF55 is NOT
    /// decremented: the halted branch leaves the FF55 length at its written
    /// value and only sets bit 7 (`| 0x80`), then the HDMA-disable step clears the
    /// enable. So a single-block HDMA caught mid-stop reads back the written
    /// length with bit 7 set (`hdma_late_m3speedchange_hdma5_scx*_2` -> out80),
    /// not the completed 0xFF the normal length-wrap would produce.
    pub(crate) fn run_hdma_block_stop_halt(&mut self) -> u32 {
        self.run_hdma_block_inner(true)
    }

    fn run_hdma_block_inner(&mut self, halted: bool) -> u32 {
        // Deferred byte-write placement. Hardware reads each byte at the dma-event `cc` but commits the VRAM write at
        // `cc + (2 + 2*ds)` — so byte 0 lands a precise sub-M-cycle AFTER the
        // trigger/prefetch boundary and after VRAM unlocks. rustyboi resolves CPU
        // reads at the POST-tick cc (one M-cycle later than hardware's read-at-cc),
        // so a VRAM read in the same window only sees the new byte once the write
        // has actually landed. Reading the 16 source bytes NOW (read-at-cc) and
        // deferring their VRAM commits by `delay` dots reproduces the byte-0
        // boundary the hdma_start / hdma_late read tests probe: the SS offset is
        // 3 dots, the DS offset 5 (the `2 + 2*ds` ratio rescaled for the post-tick
        // read granularity). The OAM-DMA interleave still advances at fire time —
        // it is tuned independently and reads its own source, not the deferred
        // VRAM bytes.
        let delay: u32 = if self.is_double_speed_mode() { 5 } else { 3 };

        // OAM-DMA interleave: HDMA and GDMA share the SAME
        // `dma()` inner loop`; only the
        // byte count differs). When an OAM-DMA is concurrently active each gated
        // HDMA byte writes the HDMA-read `data` into `OAM[src & 0xFF]`
        //, NOT the OAM-DMA's own source byte. `execute_gdma`
        // already mirrors this; `run_hdma_block` previously advanced the OAM-DMA
        // with `dma_advance_one_mcycle` (its own source), dropping the conflict
        // overwrite the oamdma-transition tests probe. Use the same gated cadence.
        let ds = self.is_double_speed_mode();
        let per_byte_cc: i64 = if ds { 4 } else { 2 };
        // A block firing inside the STOP speed-switch halt window must NOT advance
        // the OAM-DMA: hardware freezes the OAM-DMA position (its halted branch)
        // while the CPU is
        // halted across the STOP. Without this gate the block's
        // interleave advanced `dma.pos` ~16 bytes, shifting the post-switch
        // in-flight conflict byte (hdma_transition_speedchange_oamdma: read 0x60
        // where the frozen position reads 0x71).
        let interleave = self.dma.active && !self.oam_dma_stop_freeze;
        // OAM-DMA catch-up. rustyboi resolves the FF55 write at the end of the
        // bus M-cycle; whether the current OAM-DMA byte for that M-cycle has
        // already been placed depends on the sub-cycle phase. When
        // `dma.subcycle == 0` an OAM-DMA M-cycle just completed, so rustyboi's
        // `dma.pos` lags hardware by one and must catch up;
        // when a byte is mid-flight (`dma.subcycle != 0`) the gated loop below
        // already advances `dma.pos` on the same boundary hardware does, so an
        // extra catch-up over-advances by one (suppresses the final conflict).
        if interleave && self.dma.subcycle == 0 {
            self.dma_advance_one_mcycle();
        }
        // Snapshot the first destination byte's PRE-transfer value for the
        // PC-in-DMA-dest opcode-prefetch absorption. Only for a block fired by an
        // instruction-driven in-period kick (the only case where the CPU's next
        // opcode fetch can flow straight into the VRAM destination, pc_7ffe). An
        // m0-edge block firing inside a HALT window (no kick this instruction)
        // must NOT arm the shadow, else its unhalt-resume opcode at dest0 would
        // wrongly read the pre-transfer byte (hdma_transition_halt_hdmadst_unhalt).
        if self.hdma.snapshot_armed {
            self.snapshot_dma_dest0_pre();
            self.hdma.snapshot_armed = false;
        }
        let mut cc: i64 = 0;
        let mut loam: i64 = -(self.dma.subcycle as i64);

        // In the HALT-bug resume window, snapshot each dest byte's
        // PRE-transfer VRAM value before the write is queued, so the resume read
        // (ordered before the DMA's commits in hardware) observes the old byte.
        let capture_resume_pre = self.hdma.resume_shadow_window;
        for _ in 0..0x10 {
            let src = self.hdma.source;
            let (vaddr, byte, into_bank1) =
                self.resolve_dma_byte(self.hdma.source, self.hdma.dest);
            if capture_resume_pre && !into_bank1 {
                let pre = self.vram.read(vaddr);
                self.hdma.resume_pre_shadow.entry(vaddr & 0x1FFF).or_insert(pre);
            }
            self.hdma.pending_writes.push((vaddr, byte, into_bank1));
            cc += per_byte_cc;
            if interleave && self.dma.active && cc - 3 > loam {
                loam += 4;
                // HDMA blocks are single 16-byte transfers (one per H-blank), never
                // the back-to-back GDMA-block word-bus case.
                self.dma_conflict_advance(src, byte, false);
            }
            self.hdma.source = self.hdma.source.wrapping_add(1);
            self.hdma.dest = self.hdma.dest.wrapping_add(1);
        }
        if interleave && self.dma.active {
            self.dma.subcycle = (cc - loam).rem_euclid(4) as u8;
        }
        // The OAM-DMA M-cycles for this 0x10-byte block were folded into the loop
        // above; suppress `step_dma` for the true dma-event duration (the
        // 0x10-byte transfer plus the single trailing `cc += 4`) so the OAM-DMA is
        // not advanced twice (see `execute_gdma`).
        if interleave {
            self.oam_dma_stall_suppress += (0x10u32) * (per_byte_cc as u32) + 4;
        }
        self.hdma.write_delay = delay;

        if halted {
            // Hardware's halted branch: the
            // length is NOT recomputed — the FF55 length keeps its written value
            // and only bit 7 is set; the subsequent HDMA-disable step clears the enable.
            // `hdma.length` already holds the written `length_blocks_minus_1`, so
            // leaving it and clearing `hdma.enabled` makes FF55 read
            // `hdma.length | 0x80` (the written length with bit 7), not the 0xFF a
            // completing length-wrap would give.
            self.hdma.enabled = false;
        } else {
            self.hdma.length = self.hdma.length.wrapping_sub(1) & 0x7F;
            // After underflow from 0x00 -> 0xFF -> masked = 0x7F the transfer
            // is complete: FF55 reads 0xFF.
            if self.hdma.length == 0x7F {
                self.hdma.enabled = false;
            }
        }
        self.hdma.req_pending = false;

        // Stall: hardware advances `cc` by `(2 + 2*ds) * 16` per
        // byte (= 32 / 64) plus a trailing `cc += 4`. Hardware runs the block as
        // an event preceded by an opcode prefetch (next opcode fetched
        // before the transfer's cc advance); synchronous HDMA here absorbs that
        // prefetch/setup overlap with +6 so the post-block stall return lands
        // the next STAT-mode read on the exact mode-0 dot (36+6 / 68+6).
        // A block that fires on a HALT-woken instruction stream drops that +6 fudge:
        // its downstream value-read is the post-unhalt FF44 (the `hdma_*_ly_*` /
        // `inc_*` glyph reads), many instructions past the block, not an immediate
        // STAT read. The +6 there is a spurious persistent skew that pins the read
        // 6cc late (one LY high). Covers both the unhalt re-fire of a rolled-back
        // HALT-coincident block (the REFLAG side) and a NOREFLAG block that latches
        // on its m0 edge during the post-unhalt sled. The synchronous (non-HALT)
        // blocks `hdma_cycles` measures keep the +6.
        let prefetch_fudge: u32 = if self.halt.wakeup_skew && self.hdma.enabled_at_halt {
            0
        } else if self.halt.wakeup_skew
            && !self.hdma.enabled_at_halt
            && matches!(self.halt.hdma_state, HaltHdmaState::Low)
            && self.key1_switch_armed
        {
            // A halt-woken Low block fired while a CGB speed switch is armed (a
            // `stop` is pending downstream): its cost is drained as a timer-ticking
            // idle slice, so it shifts the cc of every downstream instruction —
            // including the post-STOP TIMA read. That read resolves against the 2nd
            // STOP's `DIV reset` anchor (identical either way), so the block cost sets
            // its phase directly: the +6 leaves `read - anchor = 131162`, one TIMA
            // tick below the 131168 boundary (ds_6 reads F8 vs hardware F9). The full
            // 12cc CPU-prefetch overlap lands the read on 131168 — and the byte-exact
            // F3..F9 sequence across ds_1..ds_6
            12
        } else if self.io_registers.read(ppu::LCD_CONTROL)
            & (ppu::LCDCFlags::DisplayEnable as u8)
            == 0
        {
            // LCD off: the block fires straight out of the FF55 write with no
            // PPU period to synchronise to, so the CPU reclaims one M-cycle of
            // the prefetch overlap the LCD-on path pays (AntonioND hdma_start_3,
            // LCD off + HDMA5=$80: the TIMA sample transitions 0B->0C at
            // REPETITIONS=3 and 0C->0D at 7; the +6 lands both one NOP early).
            2
        } else {
            6
        };
        // A post-STOP-unhalt HDMA block (the prefetched request fired
        // at the speed-switch unhalt; `halt.hdma_state == Requested`) charges only the
        // pure transfer cc (32 SS / 64 DS) — NEITHER the trailing +4 NOR the +6
        // CPU-prefetch fudge. Those are faithful only for a STAT/LY-read-downstream
        // block (the `hdma_cycles`/`frame*_count` calibration tests, which are `Low`);
        // the Requested block's downstream value-read is a TIMA read several
        // instructions later (hdma_late_m3speedchange_tima), so the fudge pinned it one
        // TIMA tick high. The `_3` reference: faithful cc-tlu == 131132
        // (8195 = F6); the old 36+6 lands 131142 (8196 = F7).
        if matches!(self.halt.hdma_state, HaltHdmaState::Requested) {
            return 16 * (2 + 2 * self.is_double_speed_mode() as u32);
        }
        let base = if self.is_double_speed_mode() { 68 } else { 36 };
        base + prefetch_fudge
    }

    /// The byte the OAM-DMA engine copies into `OAM[pos]`. Models the hardware
    /// OAM-DMA source pointer:
    ///   - invalid / off source -> disabled RAM (reads 0xFF).
    ///   - WRAM source -> the WRAM block selected by `src_high >> 4 & 1`, indexed by
    ///     the 12-bit offset (DMA source-high bit, NOT the CPU SVBK selection).
    ///   - rom/sram/vram -> normal read of `source_base + pos`.
    pub(super) fn dma_source_byte(&self, pos: u8) -> u8 {
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
    fn dma_advance_one_mcycle(&mut self) {
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
    fn dma_conflict_advance(&mut self, src: u16, data: u8, back_to_back: bool) {
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
    pub(super) fn start_oam_dma(&mut self, value: u8) {
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

    /// Drive the CGB HBlank-DMA engine. Called once per dot by the bus.
    /// Detects the Mode 3->0 (HBlank entry) edge to arm a block while HDMA is
    /// enabled, then services any pending request by transferring one 0x10-byte
    /// block. `run_hdma_block` is otherwise never invoked, so without this the
    /// HDMA engine never moves bytes.
    pub(crate) fn step_hdma(&mut self, period: Option<bool>) {
        if !self.cgb_features_enabled {
            return;
        }
        // While ticking the world in lockstep through an in-flight
        // block transfer, do not arm/fire another block (the per-dot crank handles
        // the next m0-edge after the lockstep completes).
        if self.hdma.lockstep_active {
            return;
        }

        let lcdc = self.io_registers.read(ppu::LCD_CONTROL);
        let lcd_on = lcdc & (ppu::LCDCFlags::DisplayEnable as u8) != 0;

        // Cycle-exact HDMA-eligibility window from the PPU renderer (the
        // in-HBlank-period predicate). When the LCD is off, treat it as permanently in the
        // period (hardware fires HDMA immediately when armed). When the renderer
        // cannot supply a closed-form mode-0 dot (window/first line), fall back
        // to the STAT mode-3->0 edge below.
        let in_period = if !lcd_on { true } else { period.unwrap_or(false) };
        self.hdma.is_in_period_cached = in_period;

        // Pan Docs: an HBlank DMA transfers exactly 0x10 bytes ONCE per HBlank.
        // Clear the "already fired this HBlank" flag on every LY change — once per
        // scanline, so it can never go stale across frames the way a raw-LY value
        // compare would (that mis-fires when the same LY recurs, dropping tiles).
        // The reset keys off LY, NOT the CPU-visible STAT mode: the in-HBlank FF55
        // kick fires its block via the cycle-exact closed-form mode-0 predicate,
        // which LEADS the STAT register by a few dots (the register can still read
        // mode 2/3 while a block legitimately fires), so a STAT-mode reset would
        // wipe the flag mid-HBlank and let the STAT-fallback edge arm a second
        // block — completing the transfer a scanline early (this is exactly the
        // Pokémon Crystal Elm's-lab cutscene bug: a 37-block HBlank DMA whose last
        // block the game cancels finished a line early, turning the RES 7 cancel
        // into a spurious GDMA that corrupted the lower screen for one frame).
        let cur_ly = self.io_registers.read(ppu::LY);
        if self.hdma.last_dma_ly != Some(cur_ly) {
            self.hdma.last_dma_ly = Some(cur_ly);
            self.hdma.block_fired_this_hblank = false;
            // The per-period "block serviced" marker is per-HBlank too. In the
            // single-speed fallback regime (`period == None`) no closed-form falling
            // edge ever resets it, so clear it on the LY change alongside the
            // per-line flag — else it stays stale-true into the next line and a
            // HALT/STOP landing there would capture `High` for a block still owed.
            // Gated to the unhalted crank: during a HALT / STOP window the
            // halt-HDMA state machine owns block_done (High-vs-Requested capture and
            // its cross-line consume logic), so leave it alone there.
            if !self.cpu_halted && !self.in_stop_window {
                self.hdma.block_done_this_period = false;
            }
        }
        // An LCD off/on cycle restarts the PPU: no HBlank periods exist while the
        // display is disabled, so the per-HBlank "block already fired" bookkeeping
        // must not survive into the first post-enable HBlank. LY can hold the same
        // value across the off/on cycle (it reads 0 throughout), so the LY-change
        // reset above would never clear a flag set at that LY — clear it while off.
        if !lcd_on {
            self.hdma.block_fired_this_hblank = false;
        }
        let block_fired_this_hblank = lcd_on && self.hdma.block_fired_this_hblank;

        // Reset the per-period "block already serviced" marker on the falling
        // edge so the next period's block is again owed.
        if self.hdma.prev_period && !in_period {
            self.hdma.block_done_this_period = false;
        }
        // A genuine period falling edge (closed-form `period` Some(true)->Some(false),
        // i.e. line-end past the HBlank window, not the Some->None renderer handoff)
        // means we are decisively out of the consumed-edge's period: drop the guard.
        if period == Some(false) {
            self.hdma.halt_edge_consumed = false;
        }

        // the period-edge HDMA request is suppressed on hardware while the CPU is
        // halted: during HALT — and equally
        // during the CGB STOP speed-switch window (which also halts the CPU,
        // see `in_stop_window`) — the block is governed by the
        // halt-HDMA state machine and re-flagged only on unhalt, so the edge must
        // NOT auto-arm here. Edge trackers are still advanced so the rising edge is
        // detected cleanly once the CPU unhalts.
        let arm_allowed = !self.cpu_halted && !self.in_stop_window;
        if lcd_on && period.is_some() {
            // Rising edge of the eligibility window arms a block (unless this
            // HBlank's one block already fired — see `block_fired_this_hblank`).
            if arm_allowed && !self.hdma.prev_period && in_period && self.hdma.enabled
                && !block_fired_this_hblank {
                // High-at-halt unhalt: consume the first post-unhalt m0 edge (the
                // during-halt edge hardware already consumed, landing one dot past
                // our slightly-early unhalt cc). Suppress this arm and clear.
                if self.hdma.high_unhalt_consume {
                    self.hdma.high_unhalt_consume = false;
                } else if self.peraccess_consume_m0_arm() {
                    // Requested-unhalt sub-block-cc consume: this m0 edge fell inside
                    // block1's transfer span; hardware absorbs it and defers the next
                    // block one line. Suppress this arm.
                } else {
                    self.hdma.req_pending = true;
                    // One block per HBlank: the closed-form rise LEADS the STAT
                    // register, and the renderer hands `hdma_period` off Some->None
                    // at the mode-0 time crossing, so the STAT-3->0 fallback below
                    // would re-observe this SAME m0 edge and fire a SECOND block
                    // (FF55 readback then skips $00 — the Stuart Little middleware
                    // boot spin). Mark the HBlank serviced, exactly as the FF55
                    // in-period kick does.
                    self.hdma.block_fired_this_hblank = true;
                }
            } else if !arm_allowed && !self.hdma.prev_period && in_period && self.hdma.enabled {
                // A period rising edge while HALTED. Hardware suppresses (and
                // CONSUMES) the HDMA request here. Whether this consumed edge must
                // STILL fire its block after unhalt depends on the halt-HDMA state:
                //   - High (halt entered in-period, block already serviced this
                //     period): the unhalt does NOT reflag and this period's block is gone — the NEXT line's m0
                //     edge fires the next block. Mark the edge consumed so rustyboi's
                //     STAT-mode-3->0 fallback (which can resurrect this same m0 edge
                //     the first dot after unhalt, once the closed-form `hdma_period`
                //     has handed off to None) skips it (hdma_m0halt_late_m3unhalt_*).
                //   - Low / Requested (out-of-period at halt, or armed-and-owed): the
                //     unhalt reflag path fires the block; the post-unhalt edge is the
                //     genuine first block and MUST NOT be skipped
                //     (late_hdma_vs_tima_*_halt). Leave the flag clear.
                if self.halt.hdma_state == HaltHdmaState::High
                    || self.hdma.block_done_this_period
                {
                    self.hdma.halt_edge_consumed = true;
                }
            }
            self.hdma.prev_period = in_period;
            // Keep the STAT-mode tracker current so a later fallback line edges
            // cleanly rather than firing on a stale mode value.
            self.hdma.prev_stat_mode = self.io_registers.read(ppu::LCD_STATUS) & 0x03;
        } else {
            let mode = if lcd_on {
                self.io_registers.read(ppu::LCD_STATUS) & 0x03
            } else {
                0
            };
            // The STAT mode-3->0 fallback edge and the closed-form `period` rising
            // edge are the SAME mode-0 transition; when the renderer hands off from
            // its closed-form `hdma_period` (Some) to None mid-HBlank, `period`
            // flips Some(true)->None on the very dot the STAT register flips 3->0,
            // so the fallback can resurrect an m0 edge the closed-form path already
            // recognized. When that edge was a during-halt period entry whose block
            // was already serviced (`hdma.halt_edge_consumed`, set above for a
            // High-at-halt period re-entry), hardware has already consumed it; skip
            // the fallback arm so it is not re-fired post-unhalt (the spurious extra
            // block in hdma_m0halt_late_m3unhalt_*). A Low/Requested-at-halt period
            // entry leaves the flag clear, so its post-unhalt first block still fires
            // (late_hdma_vs_tima_*_halt). The genuine window / first-line fallback
            // paths never set the flag either.
            if arm_allowed
                && lcd_on
                && !self.hdma.halt_edge_consumed
                && !block_fired_this_hblank
                && self.hdma.prev_stat_mode == 3
                && mode == 0
                && self.hdma.enabled
            {
                // High-at-halt unhalt: consume the first post-unhalt m0 edge (see
                // the period-rising-edge branch). The lcdoffset m0halt tests fire
                // this edge through the STAT 3->0 fallback (period handed off to
                // None), one dot after the unhalt.
                if self.hdma.high_unhalt_consume {
                    self.hdma.high_unhalt_consume = false;
                } else if self.peraccess_consume_m0_arm() {
                    // Requested-unhalt sub-block-cc consume: see the
                    // period-rising-edge branch.
                } else {
                    self.hdma.req_pending = true;
                    // Same one-block-per-HBlank marking as the rise path.
                    self.hdma.block_fired_this_hblank = true;
                }
            }
            // The consumed-edge guard is single-use: it suppresses exactly the one
            // STAT 3->0 fallback that mirrors the consumed period edge, then clears
            // so subsequent lines arm normally.
            if self.hdma.prev_stat_mode == 3 && mode == 0 {
                self.hdma.halt_edge_consumed = false;
            }
            self.hdma.prev_stat_mode = mode;
            self.hdma.prev_period = in_period;
        }

        // HDMA event firing. Normally the block fires synchronously the dot the
        // request is latched (the byte-landing timing the hdma_start/late read
        // tests are calibrated to). The ONLY exception is the interrupt-vs-dma
        // precedence window: while an interrupt service is pushing PC
        // (`hdma.mcycle_fire_suppressed`), a block latched mid-service is HELD and
        // fired explicitly after the pushes so the pushed
        // return address is visible in the HDMA copy of that stack slot.
        if self.hdma.req_pending && self.hdma.enabled {
            // Mark this eligibility period's block serviced. At single speed the
            // renderer hands `hdma_period` off to None at the mode-0 crossing a hair
            // BEFORE this fire, so the ordinary per-line block fires with
            // `in_period == false` through the STAT-3->0 fallback. Keying the mark
            // off `hdma.block_fired_this_hblank` (set at whichever arm site latched
            // this line's block) marks the period serviced for a normally-fired
            // block too, closing the FF55=00-cancel / SS->DS-STOP re-fire gates that
            // test `!hdma.block_done_this_period` for a block that already ran.
            if in_period || self.hdma.block_fired_this_hblank {
                self.hdma.block_done_this_period = true;
            }
            if !self.hdma.mcycle_fire_suppressed {
                self.fire_pending_hdma_mcycle();
            }
        }
    }

    /// Fire any latched HDMA block at a CPU M-cycle boundary (the
    /// DMA-event body). Called by the bus after each access M-cycle so the
    /// copy lands one M-cycle after the trigger — and, when an interrupt service
    /// pushed to the block's source region during this M-cycle, AFTER those
    /// pushes. No-op when nothing is latched.
    pub(crate) fn fire_pending_hdma_mcycle(&mut self) {
        if !(self.hdma.req_pending && self.hdma.enabled) {
            return;
        }
        // Snapshot the pre-fire block pointers so the late-hdma-vs-interrupt
        // re-order (see `reorder_late_hdma_after_pushes`) can restore them and
        // re-run the block reading post-push memory when an interrupt won the
        // m0-time-vs-interrupt-time race. Only meaningful while no OAM-DMA interleave
        // is active (the `late_hdma_vs_*` tests have none); a re-run with an
        // active OAM-DMA would double-advance its position, so the re-order is
        // gated on `!dma.active` at the service site.
        self.hdma.pre_fire_state =
            Some((self.hdma.source, self.hdma.dest, self.hdma.length, self.hdma.enabled));
        self.hdma.last_fire_cc = Some(self.master_cc());
        self.pending_dma_stall += self.run_hdma_block();
        // The DMA event: after the block, a halt-time
        // `hdma_requested` collapses to `hdma_low` so a subsequent unhalt does
        // not re-fire it (the request has now been serviced).
        if self.halt.hdma_state == HaltHdmaState::Requested {
            self.halt.hdma_state = HaltHdmaState::Low;
        }
    }

    /// Fire the latched HDMA block whose `dma()` event lands inside the STOP
    /// speed-switch halt window. Same copy as `fire_pending_hdma_mcycle` but with
    /// the halted-branch FF55 semantics (no length decrement; see
    /// `run_hdma_block_stop_halt`).
    pub(crate) fn fire_pending_hdma_mcycle_stop_halt(&mut self) {
        if !(self.hdma.req_pending && self.hdma.enabled) {
            return;
        }
        self.hdma.pre_fire_state =
            Some((self.hdma.source, self.hdma.dest, self.hdma.length, self.hdma.enabled));
        self.hdma.last_fire_cc = Some(self.master_cc());
        self.pending_dma_stall += self.run_hdma_block_stop_halt();
        if self.halt.hdma_state == HaltHdmaState::Requested {
            self.halt.hdma_state = HaltHdmaState::Low;
        }
    }

    /// Late-hdma-vs-interrupt re-order. Hardware resolves
    /// the race by event time: the m0-edge HDMA (requested at `mode-0 time`) wins
    /// over the interrupt only when `mode-0 time <=` the interrupt's
    /// serviceable cc; otherwise the interrupt's PC pushes run first and the
    /// block fires AFTER, so its copy of the source stack slot carries the pushed
    /// return address (`late_hdma_vs_ei/ie/tima/m0` content tests).
    ///
    /// rustyboi fires the m0-edge block greedily the dot the edge is reached —
    /// which lands one or two cc BEFORE the interrupt-triggering instruction's
    /// boundary. When the interrupt then dispatches within the same M-cycle window
    /// (its boundary `access_cc` no more than a full M-cycle past the fire) the
    /// block already (wrongly) read pre-push memory. Re-run it here, after the
    /// pushes, restoring the pre-fire pointers and discarding the stale deferred
    /// writes so the post-push source bytes land instead. Gated on no active
    /// OAM-DMA interleave (the only safe re-run; the vs tests have none) and on
    /// the block actually having fired this M-cycle window.
    pub(crate) fn reorder_late_hdma_after_pushes(&mut self, service_access_cc: u64) {
        if self.dma.active {
            return;
        }
        let Some(fire_cc) = self.hdma.last_fire_cc else {
            return;
        };
        let Some((src, dst, len, en)) = self.hdma.pre_fire_state else {
            return;
        };
        // The interrupt won the race only when its dispatch boundary is within one
        // M-cycle (4cc) of the greedy fire on EITHER side: the failing vs tests
        // dispatch at fire+0 / fire+2, versus +46 for the genuine dma-wins races
        // (where the block fired a full instruction earlier and legitimately wins).
        // The two-sided window also rejects a stale `hdma.last_fire_cc` left by an
        // unrelated earlier block. `service_access_cc` is the pre-push boundary cc;
        // `fire_cc` is the post-tick master_cc of the fire.
        if fire_cc + 4 < service_access_cc || fire_cc > service_access_cc + 4 {
            return;
        }
        // Discard the stale (pre-push) deferred writes and re-run the block from
        // the restored pointers so its source reads see the just-pushed PC.
        self.hdma.pending_writes.clear();
        self.hdma.source = src;
        self.hdma.dest = dst;
        self.hdma.length = len;
        self.hdma.enabled = en;
        self.hdma.req_pending = true;
        self.run_hdma_block();
        self.hdma.last_fire_cc = None;
        self.hdma.pre_fire_state = None;
    }

    /// Whether the M-cycle-boundary HDMA fire is currently suppressed
    /// (an interrupt service is pushing PC; the block must fire after the pushes).
    pub(crate) fn hdma_mcycle_fire_suppressed(&self) -> bool {
        self.hdma.mcycle_fire_suppressed
    }

    /// Begin/end suppression of the M-cycle-boundary HDMA fire around an
    /// interrupt service's PC pushes.
    pub(crate) fn set_hdma_mcycle_fire_suppressed(&mut self, v: bool) {
        self.hdma.mcycle_fire_suppressed = v;
    }

    /// Late-hdma-vs-interrupt unhalt precedence: whether the just-unhalted HDMA
    /// block did NOT reflag at unhalt (its m0-edge falls within the following
    /// interrupt service and must fire AFTER the PC pushes).
    pub(crate) fn hdma_unhalt_noreflag_deferred(&self) -> bool {
        self.hdma.unhalt_noreflag_deferred
    }

    #[allow(dead_code)] // no in-tree caller; `pub` was masking dead_code. Unwired-peripheral and
    // unfinished-feature code lives here — check the feature roadmap before deleting.
    pub(crate) fn hdma_unhalt_reflag_deferred(&self) -> bool {
        self.hdma.unhalt_reflag_deferred
    }

    pub(crate) fn set_hdma_unhalt_reflag_deferred(&mut self, v: bool) {
        self.hdma.unhalt_reflag_deferred = v;
    }

    pub(crate) fn set_hdma_unhalt_noreflag_deferred(&mut self, v: bool) {
        self.hdma.unhalt_noreflag_deferred = v;
    }

    /// Read the pending DMA stall without consuming it or arming the post-DMA
    /// STAT-read bias (unlike `take_dma_stall`).
    pub(crate) fn peek_dma_stall(&self) -> u32 {
        self.pending_dma_stall
    }

    /// Mark/unmark the lockstep-transfer-advance window (suppresses
    /// `step_hdma` block arm/fire while the bus ticks the world through the
    /// in-flight block's transfer cc).
    pub(crate) fn set_hdma_lockstep_active(&mut self, v: bool) {
        self.hdma.lockstep_active = v;
    }

    /// The Requested-context resume-instruction window in which a
    /// late-firing HDMA block must be advanced in lockstep (event-interleaved
    /// transfer) so the same-instruction resume read observes the extended line.
    pub(crate) fn set_hdma_resume_lockstep_window(&mut self, v: bool) {
        self.hdma.resume_lockstep_window = v;
        if !v {
            // Resume instruction done — drop both the lockstep window and the
            // pre-transfer shadow + its window.
            self.hdma.resume_shadow_window = false;
            self.hdma.resume_pre_shadow.clear();
        }
    }
    pub(crate) fn hdma_resume_lockstep_window(&self) -> bool {
        self.hdma.resume_lockstep_window
    }
    /// (CGB dma-due deferral) set/take the cc bias the deferred
    /// post-HALT VRAM write adds to its PPU mode-block check (block1's transfer
    /// span). One-shot — consumed by the first VRAM write on the resume step.
    pub(crate) fn set_hdma_dma_due_write_cc_bias(&mut self, v: u64) {
        self.hdma.dma_due_write_cc_bias = v;
    }
    pub(crate) fn take_hdma_dma_due_write_cc_bias(&mut self) -> u64 {
        std::mem::take(&mut self.hdma.dma_due_write_cc_bias)
    }
    /// Arm/clear the pre-transfer shadow window (armed for both IME states;
    /// the lockstep advance window is separate and !ime-gated).
    pub(crate) fn set_hdma_resume_shadow_window(&mut self, v: bool) {
        self.hdma.resume_shadow_window = v;
        if !v {
            self.hdma.resume_pre_shadow.clear();
        }
    }
    #[allow(dead_code)] // no in-tree caller; `pub` was masking dead_code. Unwired-peripheral and
    // unfinished-feature code lives here — check the feature roadmap before deleting.
    pub(crate) fn hdma_resume_shadow_window(&self) -> bool {
        self.hdma.resume_shadow_window
    }

    /// Pre-transfer VRAM byte for a resume-window read of an in-block
    /// dest address (the resume read is ordered before dma()'s commits). Returns
    /// None outside the window or for an address not in the just-fired block.
    pub(crate) fn hdma_resume_pre_byte(&self, addr: u16) -> Option<u8> {
        if !self.hdma.resume_shadow_window {
            return None;
        }
        self.hdma.resume_pre_shadow.get(&(addr & 0x1FFF)).copied()
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
    pub(super) fn dma_transfer_in_progress(&self) -> bool {
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
    pub(super) fn dma_write_conflict(&mut self, addr: u16, value: u8) -> bool {
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
    pub(super) fn dma_read_conflict_active(&self) -> bool {
        self.dma.active && self.dma.pos < 160
    }

    /// Byte the CPU sees on a conflicting bus read while OAM DMA is mid-transfer.
    /// Mirrors the hardware conflict branch: the read
    /// observes `OAM[dma.pos]`, the byte the DMA just placed this M-cycle (the
    /// bus tick already advanced the engine before this read resolves). On CGB,
    /// a read of the WRAM region with a non-WRAM source instead returns the live
    /// WRAM byte.
    pub(super) fn dma_conflict_byte(&self, addr: u16) -> u8 {
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
    pub(super) fn dma_address_conflicts(&self, addr: u16) -> bool {
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

    /// Whether this scanline's HBlank DMA block has already fired (the
    /// once-per-HBlank marker). Consumed by the inert-dot skip: an armed
    /// HBlank DMA whose block fired leaves the rest of the HBlank interior
    /// event-free.
    #[inline]
    pub(crate) fn hdma_block_fired_this_hblank(&self) -> bool {
        self.hdma.block_fired_this_hblank
    }
}
