//! CGB HDMA (FF51-FF55): the HBlank-scheduled VRAM DMA engine, plus the
//! VRAM-DMA byte machinery shared with the general-purpose `gdma` mode.
//!
//! HDMA and GDMA are two modes of one FF51-FF55 VRAM-DMA unit sharing the same
//! `HdmaEngine` register state; the block/byte primitives (`resolve_dma_byte`,
//! `apply_dma_write`, `snapshot_dma_dest0_pre`, the prefetch-shadow) live here
//! because the HBlank engine is the primary owner and `gdma` reuses them. The
//! bulk of this file is the sub-dot arm/kick/halt scheduling web.
//!
//! A module under `memory::dma` holding the `impl Mmio` bus-master methods; it
//! reaches `Mmio`'s internals through their `pub(in crate::memory)` visibility
//! rather than as a child of `mmio`.
use super::HaltHdmaState;
use crate::memory::mmio::{Mmio, VRAM_START};
use crate::memory::{self, Addressable};
use crate::ppu;

impl Mmio {
    /// Resolve a single HDMA byte WITHOUT committing the VRAM write. Reads the
    /// source byte at the current (fire) cc — matching hardware's read-at-cc —
    /// and returns `(vram_addr, value, into_bank1)` for a deferred apply. Used by
    /// the deferred-block model so the VRAM write lands at the correct sub-M-cycle
    /// (`fire_cc + hdma.write_delay`) rather than coincident with the trigger.
    fn resolve_dma_byte(&mut self, src: u16, dst: u16) -> (u16, u8, bool) {
        let saved_dma_active = self.dma.oam.active;
        self.dma.oam.active = false;
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
        self.dma.oam.active = saved_dma_active;
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
    pub(super) fn snapshot_dma_dest0_pre(&mut self) {
        let vram_addr = VRAM_START | (self.dma.hdma.dest & 0x1FFF);
        let into_bank1 = self.cgb_features_enabled && self.vram_bank == 1;
        let pre = if into_bank1 {
            self.vram_bank1.read(vram_addr)
        } else {
            self.vram.read(vram_addr)
        };
        self.dma.hdma.fire_dest0 = Some(vram_addr);
        self.dma.hdma.fire_dest0_prebyte = pre;
        self.dma.hdma.fire_cc = self.master_cc();
    }

    /// If the just-fired DMA block's first destination byte equals `pc` (the
    /// CPU's next opcode-fetch address), consume and return its pre-transfer
    /// value together with the dma-event fire cc (for the prefetch's VRAM-lock
    /// decision). One-shot. Returns None when no fire is pending or `pc` is not
    /// the block's first dest byte.
    pub(crate) fn take_dma_prefetch_shadow(&mut self, pc: u16) -> Option<(u8, u64)> {
        if self.dma.hdma.fire_dest0 == Some(pc) {
            self.dma.hdma.fire_dest0 = None;
            return Some((self.dma.hdma.fire_dest0_prebyte, self.dma.hdma.fire_cc));
        }
        None
    }

    /// Clear any stale DMA prefetch-shadow (called once the next opcode has been
    /// fetched without consuming it, so it cannot leak to a later access).
    pub(crate) fn clear_dma_prefetch_shadow(&mut self) {
        self.dma.hdma.fire_dest0 = None;
        self.dma.hdma.snapshot_armed = false;
    }

    /// True while deferred HDMA block writes are still in their
    /// per-dot countdown (`step_hdma_deferred` must run each dot to commit them at
    /// the right cc). Blocks the idle bulk-skip.
    pub(crate) fn has_pending_hdma_deferred(&self) -> bool {
        !self.dma.hdma.pending_writes.is_empty()
    }

    /// Drain the deferred-HDMA write buffer one dot. When the delay expires the
    /// resolved bytes are committed to VRAM in order.
    #[inline]
    pub(crate) fn step_hdma_deferred(&mut self) {
        if self.dma.hdma.pending_writes.is_empty() {
            return;
        }
        self.step_hdma_deferred_slow();
    }

    fn step_hdma_deferred_slow(&mut self) {
        if self.dma.hdma.write_delay > 0 {
            self.dma.hdma.write_delay -= 1;
        }
        if self.dma.hdma.write_delay == 0 {
            let pending = std::mem::take(&mut self.dma.hdma.pending_writes);
            for (addr, byte, into_bank1) in pending {
                self.apply_dma_write(addr, byte, into_bank1);
            }
        }
    }

    // ----------------------------------------------------------------------
    // HDMA accessors used by gb.rs / cpu / ppu.
    // ----------------------------------------------------------------------

    pub(crate) fn hdma_is_enabled(&self) -> bool {
        self.cgb_features_enabled && self.dma.hdma.enabled
    }

    pub(crate) fn hdma_req_pending(&self) -> bool {
        self.dma.hdma.req_pending
    }

    /// Whether this HDMA period's block has already been serviced (the DMA event
    /// for this m0 edge already ran and acked, so no HDMA request is
    /// flagged — no block is owed/prefetched at a STOP).
    pub(crate) fn hdma_block_done_this_period(&self) -> bool {
        self.dma.hdma.block_done_this_period
    }

    /// Remaining HDMA blocks minus one (the FF55 length field). 0 => the next
    /// block completes the transfer.
    pub(crate) fn hdma_length(&self) -> u8 {
        self.dma.hdma.length
    }

    /// Arm the High-at-halt unhalt edge-consume: the first post-unhalt m0 HDMA edge
    /// is suppressed (it was the during-halt edge hardware already consumed). Called
    /// at the unhalt site when `halt.hdma_state == High`.
    pub(crate) fn arm_hdma_high_unhalt_consume(&mut self) {
        self.dma.hdma.high_unhalt_consume = true;
    }

    /// Arm the Requested-unhalt sub-block-cc consume.
    /// Called at the unhalt site when a `Requested`-at-halt block is reflagged so
    /// `step_hdma` can absorb the next-line m0 arm iff it falls within the freshly
    /// fired block's transfer span (hardware's m0 HDMA event consumed by the
    /// in-flight transfer), deferring the genuine next block one line.
    pub(crate) fn arm_hdma_peraccess_consume(&mut self) {
        self.dma.hdma.peraccess_consume_pending = true;
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
        if !self.dma.hdma.peraccess_consume_pending {
            return false;
        }
        let fire_cc = match self.dma.hdma.last_fire_cc {
            Some(c) => c,
            None => {
                self.dma.hdma.peraccess_consume_pending = false;
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
            self.dma.hdma.peraccess_consume_pending = false;
            false
        }
    }

    /// Master cc of the last HDMA block fire (None if none in-flight this period).
    pub(crate) fn hdma_last_fire_cc(&self) -> Option<u64> {
        self.dma.hdma.last_fire_cc
    }

    /// Whether an HDMA block is latched and would fire at the next
    /// M-cycle boundary (the `fire_pending_hdma_mcycle` precondition).
    pub(crate) fn hdma_fire_pending(&self) -> bool {
        self.dma.hdma.req_pending && self.dma.hdma.enabled
    }

    pub(crate) fn set_hdma_req(&mut self) {
        if self.cgb_features_enabled && self.dma.hdma.enabled {
            self.dma.hdma.req_pending = true;
        }
        // A requested block fires through step_hdma at any mode; wake the
        // tracker immediately.
        self.dma.hdma.tracker_sleep_until = 0;
    }

    /// Master cc below which the per-dot HDMA tracker may be skipped.
    #[inline]
    pub(crate) fn hdma_tracker_sleep_until(&self) -> u64 {
        self.dma.hdma.tracker_sleep_until
    }

    /// PPU-installed HDMA-tracker sleep bound (see the field doc).
    #[inline]
    pub(crate) fn set_hdma_tracker_sleep(&mut self, until: u64) {
        self.dma.hdma.tracker_sleep_until = until;
    }

    pub(crate) fn halt_hdma_state(&self) -> HaltHdmaState {
        self.halt.hdma_state
    }

    pub(crate) fn set_halt_hdma_state(&mut self, s: HaltHdmaState) {
        self.halt.hdma_state = s;
    }

    /// Sticky: this ROM has driven the HDMA/GDMA machinery (FF55 written).
    pub(crate) fn hdma_machinery_used(&self) -> bool {
        self.dma.hdma.machinery_used
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
        if !self.dma.hdma.kick_eval_pending {
            return false;
        }
        self.dma.hdma.kick_eval_pending = false;
        if in_period && self.dma.hdma.enabled {
            self.dma.hdma.req_pending = true;
            // Instruction-driven in-period kick: arm the prefetch-absorption
            // snapshot for the block this kick will fire (pc_7ffe). Cleared by
            // the snapshot or the next opcode fetch.
            self.dma.hdma.snapshot_armed = true;
            // DEFERRED-HDMA-FIRE: the kick services THIS period's block. Mark it
            // done so an immediately-following `halt` captures `halt.hdma_state =
            // High` (in-period + already-serviced) rather than
            // `Requested`, which would re-fire a SECOND block on unhalt
            // (hdma_late_m0halt_*). The per-dot `hdma_period` is false mid-HBlank
            // (post-mode-0 time crossing) so `step_hdma`'s own block_done set never
            // fires for a kick-armed block; set it here at the in-period kick.
            self.dma.hdma.block_done_this_period = true;
            // An in-HBlank kick services this HBlank's one block via the
            // cycle-exact closed-form predicate, which LEADS the CPU-visible STAT
            // mode. Mark the HBlank serviced so the STAT-mode-3->0 fallback in
            // `step_hdma` (which lags, firing on the register edge) does NOT arm a
            // SECOND block for the same scanline — the Pokémon Crystal Elm's-lab
            // arm-line double-fire. `step_hdma`'s own edge arms set it too (the
            // rise/fallback double-fire is the same lead/lag aliasing). Cleared on
            // the next LY change.
            self.dma.hdma.block_fired_this_hblank = true;
        }
        true
    }

    /// Whether an FF55 bit7=1 kick is awaiting the bus's live-period resolution.
    pub(crate) fn hdma_kick_eval_pending(&self) -> bool {
        self.dma.hdma.kick_eval_pending
    }

    pub(crate) fn set_hdma_disable_fires(&mut self, v: Option<bool>) {
        self.dma.hdma.disable_fires = v == Some(true);
    }

    pub(crate) fn hdma_is_in_period_cached(&self) -> bool {
        self.dma.hdma.is_in_period_cached
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
        if self.dma.hdma.is_in_period_cached {
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
        let interleave = self.dma.oam.active && !self.dma.oam.oam_dma_stop_freeze;
        // OAM-DMA catch-up. rustyboi resolves the FF55 write at the end of the
        // bus M-cycle; whether the current OAM-DMA byte for that M-cycle has
        // already been placed depends on the sub-cycle phase. When
        // `dma.subcycle == 0` an OAM-DMA M-cycle just completed, so rustyboi's
        // `dma.pos` lags hardware by one and must catch up;
        // when a byte is mid-flight (`dma.subcycle != 0`) the gated loop below
        // already advances `dma.pos` on the same boundary hardware does, so an
        // extra catch-up over-advances by one (suppresses the final conflict).
        if interleave && self.dma.oam.subcycle == 0 {
            self.dma_advance_one_mcycle();
        }
        // Snapshot the first destination byte's PRE-transfer value for the
        // PC-in-DMA-dest opcode-prefetch absorption. Only for a block fired by an
        // instruction-driven in-period kick (the only case where the CPU's next
        // opcode fetch can flow straight into the VRAM destination, pc_7ffe). An
        // m0-edge block firing inside a HALT window (no kick this instruction)
        // must NOT arm the shadow, else its unhalt-resume opcode at dest0 would
        // wrongly read the pre-transfer byte (hdma_transition_halt_hdmadst_unhalt).
        if self.dma.hdma.snapshot_armed {
            self.snapshot_dma_dest0_pre();
            self.dma.hdma.snapshot_armed = false;
        }
        let mut cc: i64 = 0;
        let mut loam: i64 = -(self.dma.oam.subcycle as i64);

        // In the HALT-bug resume window, snapshot each dest byte's
        // PRE-transfer VRAM value before the write is queued, so the resume read
        // (ordered before the DMA's commits in hardware) observes the old byte.
        let capture_resume_pre = self.dma.hdma.resume_shadow_window;
        for _ in 0..0x10 {
            let src = self.dma.hdma.source;
            let (vaddr, byte, into_bank1) =
                self.resolve_dma_byte(self.dma.hdma.source, self.dma.hdma.dest);
            if capture_resume_pre && !into_bank1 {
                let pre = self.vram.read(vaddr);
                self.dma.hdma.resume_pre_shadow.entry(vaddr & 0x1FFF).or_insert(pre);
            }
            self.dma.hdma.pending_writes.push((vaddr, byte, into_bank1));
            cc += per_byte_cc;
            if interleave && self.dma.oam.active && cc - 3 > loam {
                loam += 4;
                // HDMA blocks are single 16-byte transfers (one per H-blank), never
                // the back-to-back GDMA-block word-bus case.
                self.dma_conflict_advance(src, byte, false);
            }
            self.dma.hdma.source = self.dma.hdma.source.wrapping_add(1);
            self.dma.hdma.dest = self.dma.hdma.dest.wrapping_add(1);
        }
        if interleave && self.dma.oam.active {
            self.dma.oam.subcycle = (cc - loam).rem_euclid(4) as u8;
        }
        // The OAM-DMA M-cycles for this 0x10-byte block were folded into the loop
        // above; suppress `step_dma` for the true dma-event duration (the
        // 0x10-byte transfer plus the single trailing `cc += 4`) so the OAM-DMA is
        // not advanced twice (see `execute_gdma`).
        if interleave {
            self.dma.hdma.oam_dma_stall_suppress += (0x10u32) * (per_byte_cc as u32) + 4;
        }
        self.dma.hdma.write_delay = delay;

        if halted {
            // Hardware's halted branch: the
            // length is NOT recomputed — the FF55 length keeps its written value
            // and only bit 7 is set; the subsequent HDMA-disable step clears the enable.
            // `hdma.length` already holds the written `length_blocks_minus_1`, so
            // leaving it and clearing `hdma.enabled` makes FF55 read
            // `hdma.length | 0x80` (the written length with bit 7), not the 0xFF a
            // completing length-wrap would give.
            self.dma.hdma.enabled = false;
        } else {
            self.dma.hdma.length = self.dma.hdma.length.wrapping_sub(1) & 0x7F;
            // After underflow from 0x00 -> 0xFF -> masked = 0x7F the transfer
            // is complete: FF55 reads 0xFF.
            if self.dma.hdma.length == 0x7F {
                self.dma.hdma.enabled = false;
            }
        }
        self.dma.hdma.req_pending = false;

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
        let prefetch_fudge: u32 = if self.halt.wakeup_skew && self.dma.hdma.enabled_at_halt {
            0
        } else if self.halt.wakeup_skew
            && !self.dma.hdma.enabled_at_halt
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
        if self.dma.hdma.lockstep_active {
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
        self.dma.hdma.is_in_period_cached = in_period;

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
        if self.dma.hdma.last_dma_ly != Some(cur_ly) {
            self.dma.hdma.last_dma_ly = Some(cur_ly);
            self.dma.hdma.block_fired_this_hblank = false;
            // The per-period "block serviced" marker is per-HBlank too. In the
            // single-speed fallback regime (`period == None`) no closed-form falling
            // edge ever resets it, so clear it on the LY change alongside the
            // per-line flag — else it stays stale-true into the next line and a
            // HALT/STOP landing there would capture `High` for a block still owed.
            // Gated to the unhalted crank: during a HALT / STOP window the
            // halt-HDMA state machine owns block_done (High-vs-Requested capture and
            // its cross-line consume logic), so leave it alone there.
            if !self.cpu_halted && !self.in_stop_window {
                self.dma.hdma.block_done_this_period = false;
            }
        }
        // An LCD off/on cycle restarts the PPU: no HBlank periods exist while the
        // display is disabled, so the per-HBlank "block already fired" bookkeeping
        // must not survive into the first post-enable HBlank. LY can hold the same
        // value across the off/on cycle (it reads 0 throughout), so the LY-change
        // reset above would never clear a flag set at that LY — clear it while off.
        if !lcd_on {
            self.dma.hdma.block_fired_this_hblank = false;
        }
        let block_fired_this_hblank = lcd_on && self.dma.hdma.block_fired_this_hblank;

        // Reset the per-period "block already serviced" marker on the falling
        // edge so the next period's block is again owed.
        if self.dma.hdma.prev_period && !in_period {
            self.dma.hdma.block_done_this_period = false;
        }
        // A genuine period falling edge (closed-form `period` Some(true)->Some(false),
        // i.e. line-end past the HBlank window, not the Some->None renderer handoff)
        // means we are decisively out of the consumed-edge's period: drop the guard.
        if period == Some(false) {
            self.dma.hdma.halt_edge_consumed = false;
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
            if arm_allowed && !self.dma.hdma.prev_period && in_period && self.dma.hdma.enabled
                && !block_fired_this_hblank {
                // High-at-halt unhalt: consume the first post-unhalt m0 edge (the
                // during-halt edge hardware already consumed, landing one dot past
                // our slightly-early unhalt cc). Suppress this arm and clear.
                if self.dma.hdma.high_unhalt_consume {
                    self.dma.hdma.high_unhalt_consume = false;
                } else if self.peraccess_consume_m0_arm() {
                    // Requested-unhalt sub-block-cc consume: this m0 edge fell inside
                    // block1's transfer span; hardware absorbs it and defers the next
                    // block one line. Suppress this arm.
                } else {
                    self.dma.hdma.req_pending = true;
                    // One block per HBlank: the closed-form rise LEADS the STAT
                    // register, and the renderer hands `hdma_period` off Some->None
                    // at the mode-0 time crossing, so the STAT-3->0 fallback below
                    // would re-observe this SAME m0 edge and fire a SECOND block
                    // (FF55 readback then skips $00 — the Stuart Little middleware
                    // boot spin). Mark the HBlank serviced, exactly as the FF55
                    // in-period kick does.
                    self.dma.hdma.block_fired_this_hblank = true;
                }
            } else if !arm_allowed && !self.dma.hdma.prev_period && in_period && self.dma.hdma.enabled {
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
                    || self.dma.hdma.block_done_this_period
                {
                    self.dma.hdma.halt_edge_consumed = true;
                }
            }
            self.dma.hdma.prev_period = in_period;
            // Keep the STAT-mode tracker current so a later fallback line edges
            // cleanly rather than firing on a stale mode value.
            self.dma.hdma.prev_stat_mode = self.io_registers.read(ppu::LCD_STATUS) & 0x03;
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
                && !self.dma.hdma.halt_edge_consumed
                && !block_fired_this_hblank
                && self.dma.hdma.prev_stat_mode == 3
                && mode == 0
                && self.dma.hdma.enabled
            {
                // High-at-halt unhalt: consume the first post-unhalt m0 edge (see
                // the period-rising-edge branch). The lcdoffset m0halt tests fire
                // this edge through the STAT 3->0 fallback (period handed off to
                // None), one dot after the unhalt.
                if self.dma.hdma.high_unhalt_consume {
                    self.dma.hdma.high_unhalt_consume = false;
                } else if self.peraccess_consume_m0_arm() {
                    // Requested-unhalt sub-block-cc consume: see the
                    // period-rising-edge branch.
                } else {
                    self.dma.hdma.req_pending = true;
                    // Same one-block-per-HBlank marking as the rise path.
                    self.dma.hdma.block_fired_this_hblank = true;
                }
            }
            // The consumed-edge guard is single-use: it suppresses exactly the one
            // STAT 3->0 fallback that mirrors the consumed period edge, then clears
            // so subsequent lines arm normally.
            if self.dma.hdma.prev_stat_mode == 3 && mode == 0 {
                self.dma.hdma.halt_edge_consumed = false;
            }
            self.dma.hdma.prev_stat_mode = mode;
            self.dma.hdma.prev_period = in_period;
        }

        // HDMA event firing. Normally the block fires synchronously the dot the
        // request is latched (the byte-landing timing the hdma_start/late read
        // tests are calibrated to). The ONLY exception is the interrupt-vs-dma
        // precedence window: while an interrupt service is pushing PC
        // (`hdma.mcycle_fire_suppressed`), a block latched mid-service is HELD and
        // fired explicitly after the pushes so the pushed
        // return address is visible in the HDMA copy of that stack slot.
        if self.dma.hdma.req_pending && self.dma.hdma.enabled {
            // Mark this eligibility period's block serviced. At single speed the
            // renderer hands `hdma_period` off to None at the mode-0 crossing a hair
            // BEFORE this fire, so the ordinary per-line block fires with
            // `in_period == false` through the STAT-3->0 fallback. Keying the mark
            // off `hdma.block_fired_this_hblank` (set at whichever arm site latched
            // this line's block) marks the period serviced for a normally-fired
            // block too, closing the FF55=00-cancel / SS->DS-STOP re-fire gates that
            // test `!hdma.block_done_this_period` for a block that already ran.
            if in_period || self.dma.hdma.block_fired_this_hblank {
                self.dma.hdma.block_done_this_period = true;
            }
            if !self.dma.hdma.mcycle_fire_suppressed {
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
        if !(self.dma.hdma.req_pending && self.dma.hdma.enabled) {
            return;
        }
        // Snapshot the pre-fire block pointers so the late-hdma-vs-interrupt
        // re-order (see `reorder_late_hdma_after_pushes`) can restore them and
        // re-run the block reading post-push memory when an interrupt won the
        // m0-time-vs-interrupt-time race. Only meaningful while no OAM-DMA interleave
        // is active (the `late_hdma_vs_*` tests have none); a re-run with an
        // active OAM-DMA would double-advance its position, so the re-order is
        // gated on `!dma.active` at the service site.
        self.dma.hdma.pre_fire_state =
            Some((self.dma.hdma.source, self.dma.hdma.dest, self.dma.hdma.length, self.dma.hdma.enabled));
        self.dma.hdma.last_fire_cc = Some(self.master_cc());
        self.dma.hdma.pending_dma_stall += self.run_hdma_block();
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
        if !(self.dma.hdma.req_pending && self.dma.hdma.enabled) {
            return;
        }
        self.dma.hdma.pre_fire_state =
            Some((self.dma.hdma.source, self.dma.hdma.dest, self.dma.hdma.length, self.dma.hdma.enabled));
        self.dma.hdma.last_fire_cc = Some(self.master_cc());
        self.dma.hdma.pending_dma_stall += self.run_hdma_block_stop_halt();
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
        if self.dma.oam.active {
            return;
        }
        let Some(fire_cc) = self.dma.hdma.last_fire_cc else {
            return;
        };
        let Some((src, dst, len, en)) = self.dma.hdma.pre_fire_state else {
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
        self.dma.hdma.pending_writes.clear();
        self.dma.hdma.source = src;
        self.dma.hdma.dest = dst;
        self.dma.hdma.length = len;
        self.dma.hdma.enabled = en;
        self.dma.hdma.req_pending = true;
        self.run_hdma_block();
        self.dma.hdma.last_fire_cc = None;
        self.dma.hdma.pre_fire_state = None;
    }

    /// Whether the M-cycle-boundary HDMA fire is currently suppressed
    /// (an interrupt service is pushing PC; the block must fire after the pushes).
    pub(crate) fn hdma_mcycle_fire_suppressed(&self) -> bool {
        self.dma.hdma.mcycle_fire_suppressed
    }

    /// Begin/end suppression of the M-cycle-boundary HDMA fire around an
    /// interrupt service's PC pushes.
    pub(crate) fn set_hdma_mcycle_fire_suppressed(&mut self, v: bool) {
        self.dma.hdma.mcycle_fire_suppressed = v;
    }

    /// Late-hdma-vs-interrupt unhalt precedence: whether the just-unhalted HDMA
    /// block did NOT reflag at unhalt (its m0-edge falls within the following
    /// interrupt service and must fire AFTER the PC pushes).
    pub(crate) fn hdma_unhalt_noreflag_deferred(&self) -> bool {
        self.dma.hdma.unhalt_noreflag_deferred
    }

    #[allow(dead_code)] // no in-tree caller; `pub` was masking dead_code. Unwired-peripheral and
    // unfinished-feature code lives here — check the feature roadmap before deleting.
    pub(crate) fn hdma_unhalt_reflag_deferred(&self) -> bool {
        self.dma.hdma.unhalt_reflag_deferred
    }

    pub(crate) fn set_hdma_unhalt_reflag_deferred(&mut self, v: bool) {
        self.dma.hdma.unhalt_reflag_deferred = v;
    }

    pub(crate) fn set_hdma_unhalt_noreflag_deferred(&mut self, v: bool) {
        self.dma.hdma.unhalt_noreflag_deferred = v;
    }

    /// Mark/unmark the lockstep-transfer-advance window (suppresses
    /// `step_hdma` block arm/fire while the bus ticks the world through the
    /// in-flight block's transfer cc).
    pub(crate) fn set_hdma_lockstep_active(&mut self, v: bool) {
        self.dma.hdma.lockstep_active = v;
    }

    /// The Requested-context resume-instruction window in which a
    /// late-firing HDMA block must be advanced in lockstep (event-interleaved
    /// transfer) so the same-instruction resume read observes the extended line.
    pub(crate) fn set_hdma_resume_lockstep_window(&mut self, v: bool) {
        self.dma.hdma.resume_lockstep_window = v;
        if !v {
            // Resume instruction done — drop both the lockstep window and the
            // pre-transfer shadow + its window.
            self.dma.hdma.resume_shadow_window = false;
            self.dma.hdma.resume_pre_shadow.clear();
        }
    }
    pub(crate) fn hdma_resume_lockstep_window(&self) -> bool {
        self.dma.hdma.resume_lockstep_window
    }
    /// (CGB dma-due deferral) set/take the cc bias the deferred
    /// post-HALT VRAM write adds to its PPU mode-block check (block1's transfer
    /// span). One-shot — consumed by the first VRAM write on the resume step.
    pub(crate) fn set_hdma_dma_due_write_cc_bias(&mut self, v: u64) {
        self.dma.hdma.dma_due_write_cc_bias = v;
    }
    pub(crate) fn take_hdma_dma_due_write_cc_bias(&mut self) -> u64 {
        std::mem::take(&mut self.dma.hdma.dma_due_write_cc_bias)
    }
    /// Arm/clear the pre-transfer shadow window (armed for both IME states;
    /// the lockstep advance window is separate and !ime-gated).
    pub(crate) fn set_hdma_resume_shadow_window(&mut self, v: bool) {
        self.dma.hdma.resume_shadow_window = v;
        if !v {
            self.dma.hdma.resume_pre_shadow.clear();
        }
    }
    #[allow(dead_code)] // no in-tree caller; `pub` was masking dead_code. Unwired-peripheral and
    // unfinished-feature code lives here — check the feature roadmap before deleting.
    pub(crate) fn hdma_resume_shadow_window(&self) -> bool {
        self.dma.hdma.resume_shadow_window
    }

    /// Pre-transfer VRAM byte for a resume-window read of an in-block
    /// dest address (the resume read is ordered before dma()'s commits). Returns
    /// None outside the window or for an address not in the just-fired block.
    pub(crate) fn hdma_resume_pre_byte(&self, addr: u16) -> Option<u8> {
        if !self.dma.hdma.resume_shadow_window {
            return None;
        }
        self.dma.hdma.resume_pre_shadow.get(&(addr & 0x1FFF)).copied()
    }

    /// Whether this scanline's HBlank DMA block has already fired (the
    /// once-per-HBlank marker). Consumed by the inert-dot skip: an armed
    /// HBlank DMA whose block fired leaves the rest of the HBlank interior
    /// event-free.
    #[inline]
    pub(crate) fn hdma_block_fired_this_hblank(&self) -> bool {
        self.dma.hdma.block_fired_this_hblank
    }
}

impl Mmio {
    /// FF55 (HDMA5) read: the VRAM-DMA transfer-status byte (bit 7 set when
    /// idle/done, clear while a transfer is in progress). Extracted verbatim
    /// from the `Addressable::read` FF55 arm.
    pub(in crate::memory) fn hdma_status_byte(&self) -> u8 {
        if self.cgb_features_enabled {
            if self.dma.hdma.enabled {
                // In-progress: bit 7 clear, low 7 bits =
                // blocks remaining minus 1.
                self.dma.hdma.length & 0x7F
            } else {
                // Done / cancelled / never-armed: bit 7
                // set. `hdma.length == 0x7F` after a
                // completed transfer encodes 0xFF.
                self.dma.hdma.length | 0x80
            }
        } else {
            0xFF
        }
    }

    /// FF51 (HDMA1) write: VRAM-DMA source high byte.
    pub(in crate::memory) fn write_hdma_src_high(&mut self, value: u8) {
        if self.cgb_features_enabled {
            // Sticky HDMA/GDMA-machinery marker: src/dst
            // setup usually precedes the (one-time) vsync
            // halt in the DMA-preamble test ROMs, so mark
            // here too, not just on the FF55 trigger.
            self.dma.hdma.machinery_used = true;
            self.dma.hdma.source = (self.dma.hdma.source & 0x00FF) | ((value as u16) << 8);
        }
    }

    /// FF52 (HDMA2) write: VRAM-DMA source low byte.
    pub(in crate::memory) fn write_hdma_src_low(&mut self, value: u8) {
        if self.cgb_features_enabled {
            // Low nibble of source low byte is masked off on real hardware.
            // The low nibble is masked: `value & 0xF0`.
            self.dma.hdma.source = (self.dma.hdma.source & 0xFF00) | ((value as u16) & 0x00F0);
        }
    }

    /// FF53 (HDMA3) write: VRAM-DMA destination high byte.
    pub(in crate::memory) fn write_hdma_dst_high(&mut self, value: u8) {
        if self.cgb_features_enabled {
            self.dma.hdma.dest = (self.dma.hdma.dest & 0x00FF) | ((value as u16) << 8);
        }
    }

    /// FF54 (HDMA4) write: VRAM-DMA destination low byte.
    pub(in crate::memory) fn write_hdma_dst_low(&mut self, value: u8) {
        if self.cgb_features_enabled {
            // Low nibble of dest low byte is masked off on real hardware.
            // The low nibble is masked: `value & 0xF0`.
            self.dma.hdma.dest = (self.dma.hdma.dest & 0xFF00) | ((value as u16) & 0x00F0);
        }
    }

    /// FF55 (HDMA5) write: arm/kick/cancel the CGB VRAM DMA (GDMA vs HDMA).
    /// Extracted verbatim from the `Addressable::write` FF55 arm.
    pub(in crate::memory) fn write_hdma5(&mut self, value: u8) {
        if self.cgb_features_enabled {
            // Sticky HDMA/GDMA-machinery marker (see
            // `hdma.machinery_used`): scopes the CGB LCD
            // halt-exit stall away from the DMA cc-web.
            self.dma.hdma.machinery_used = true;
            let length_blocks_minus_1 = value & 0x7F;
            let new_mode = (value >> 7) & 0x01; // 0=GDMA, 1=HDMA
            let lcd_on = (self.io_registers.read(ppu::LCD_CONTROL)
                & (ppu::LCDCFlags::DisplayEnable as u8)) != 0;

            if self.dma.hdma.enabled {
                // HDMA already armed: bit7=0 cancels,
                // bit7=1 restarts with new length / src
                // / dst.
                if new_mode == 0 {
                    // FF55=00 disable-vs-m0-edge race:
                    // the disable
                    // only clears the FUTURE m0-edge schedule.
                    // A block whose m0 edge already fired
                    // (the DMA event latched) STILL runs. The
                    // bus stashes that decision in
                    // `hdma.disable_fires` by evaluating the
                    // PPU mode-0 time at this write's access cc.
                    // The race only exists while the period's
                    // block is still OWED (latched by the m0
                    // edge, not yet run). Once the block for
                    // this period has already been serviced
                    // (`hdma.block_done_this_period`, e.g. an
                    // in-period FF55 kick fired it), the next
                    // dma event is the NEXT line's m0 edge —
                    // in the future — so the disable always
                    // wins (SameSuite dma/hdma_mode0: enable+
                    // kick in mode 0, then disable a few
                    // M-cycles later must stop the transfer).
                    if self.dma.hdma.disable_fires
                        && !self.dma.hdma.block_done_this_period
                    {
                        // m0 edge already passed: keep the
                        // request latched so the block fires
                        // this M-cycle (step_hdma), exactly as
                        // hardware runs it despite the
                        // disable. The block-fire decrements
                        // length and ends HDMA normally.
                        self.dma.hdma.req_pending = true;
                        // Leave hdma.enabled = true so the
                        // M-cycle fire gate passes; the
                        // post-block length wrap clears it.
                    } else {
                        // Disable wins. Hardware latches the
                        // WRITTEN length bits on every FF55
                        // write, including the cancel (it stores
                        // `(value & 0x7F) + 1`
                        // before the abort early-return), so a
                        // later read returns 0x80|written, NOT
                        // the preserved remaining count
                        // (SameSuite dma/hdma_lcd_off expects
                        // 0x80 after FF55=00 with 3 blocks
                        // left).
                        self.dma.hdma.length = length_blocks_minus_1;
                        self.dma.hdma.enabled = false;
                        self.dma.hdma.req_pending = false;
                    }
                    self.dma.hdma.disable_fires = false;
                } else {
                    self.dma.hdma.length = length_blocks_minus_1;
                    if !lcd_on {
                        // LCD off: hardware fires immediately
                        // (no HDMA period concept).
                        self.dma.hdma.req_pending = true;
                    } else {
                        // LCD on: gate the immediate kick on the
                        // LIVE in-HBlank-period predicate (at cc+4), resolved by
                        // the bus after this write.
                        self.dma.hdma.kick_eval_pending = true;
                    }
                }
            } else if new_mode == 0 {
                // GDMA kick (synchronous).
                let total_bytes = (length_blocks_minus_1 as usize + 1) * 16;
                self.execute_gdma(total_bytes);
                self.dma.hdma.length = 0x7F; // FF55 reads 0xFF
            } else {
                // Arm HDMA. Fire the first block now if
                // LCD off; otherwise gate the immediate kick
                // on the live in-HBlank-period predicate (at cc+4, resolved by
                // the bus), else the Mode 3->0 trigger arms it.
                self.dma.hdma.enabled = true;
                self.dma.hdma.length = length_blocks_minus_1;
                if !lcd_on {
                    self.dma.hdma.req_pending = true;
                } else {
                    self.dma.hdma.kick_eval_pending = true;
                }
            }
            // Consume the per-write disable-race decision (only
            // the disable branch above uses it).
            self.dma.hdma.disable_fires = false;
        }
    }
}

#[cfg(test)]
mod hblank_dma_tests {
    //! Regression: Pan Docs specifies an HBlank DMA transfers exactly one
    //! 0x10-byte block per HBlank. When an in-HBlank FF55 kick fires this HBlank's
    //! block via the cycle-exact closed-form mode-0 predicate (which leads the
    //! CPU-visible STAT mode), the STAT-fallback edge must NOT arm a SECOND block
    //! for the same scanline — else the transfer finishes a line early. That early
    //! finish is what turned Pokémon Crystal's Elm's-lab HBlank-DMA cancel into a
    //! spurious mid-frame GDMA that corrupted the lower screen. The `hdma_block_
    //! fired_this_hblank` flag enforces one block per line and, keyed off LY,
    //! resets every scanline (so it never goes stale across frames).
    use crate::memory::dma::HaltHdmaState;
    use crate::memory::mmio::{Mmio, REG_HDMA5};
    use crate::memory::Addressable;
    use crate::ppu;

    /// A 5-block HBlank DMA (WRAM->VRAM), armed and enabled, positioned at the
    /// STAT mode-3->0 edge of line 46 that the fallback path arms on.
    fn armed_at_hblank_edge() -> Mmio {
        let mut m = Mmio::new();
        m.set_cgb_features_enabled(true);
        m.io_registers.write(ppu::LCD_CONTROL, ppu::LCDCFlags::DisplayEnable as u8);
        m.io_registers.write(ppu::LY, 46);
        m.io_registers.write(ppu::LCD_STATUS, 0); // mode 0 (HBlank)
        m.dma.hdma.source = 0xC000;
        m.dma.hdma.dest = 0x8000;
        m.dma.hdma.length = 4; // blocks-1 => 5 blocks
        m.dma.hdma.enabled = true;
        m.dma.hdma.prev_stat_mode = 3;
        m.dma.hdma.prev_period = false;
        m.dma.hdma.halt_edge_consumed = false;
        m.dma.hdma.last_dma_ly = Some(46); // already synced to this line
        m
    }

    #[test]
    fn second_block_on_same_line_is_suppressed() {
        let mut m = armed_at_hblank_edge();
        // The in-HBlank kick already fired this line's block.
        m.dma.hdma.block_fired_this_hblank = true;
        let len = m.dma.hdma.length;
        m.step_hdma(None); // STAT mode-3->0 fallback edge
        assert_eq!(m.dma.hdma.length, len, "a second block fired in the same HBlank");
        assert!(!m.dma.hdma.req_pending);
    }

    #[test]
    fn flag_resets_on_ly_change_so_next_line_fires() {
        let mut m = armed_at_hblank_edge();
        m.dma.hdma.block_fired_this_hblank = true;
        // A new scanline: LY advanced past the recorded line.
        m.io_registers.write(ppu::LY, 47);
        m.step_hdma(None);
        // The LY-change reset cleared the flag, so this line's edge arms its block.
        assert!(!m.dma.hdma.block_fired_this_hblank || m.dma.hdma.req_pending || m.dma.hdma.length < 4,
            "the flag must clear on LY change so the next HBlank's block still fires");
        assert_eq!(m.dma.hdma.last_dma_ly, Some(47), "LY tracker follows the live line");
    }

    #[test]
    fn same_ly_next_frame_still_fires_a_block() {
        // The flag keys off LY *changes*, not the LY value, so the same LY
        // recurring next frame is not mistaken for an already-serviced HBlank
        // (a raw-LY compare regressed this, dropping tiles like the "P" glyph).
        let mut m = armed_at_hblank_edge();
        m.dma.hdma.block_fired_this_hblank = true;
        // Simulate a full frame passing: LY walks away and returns to 46.
        for ly in [47u8, 48, 100, 0, 45, 46] {
            m.io_registers.write(ppu::LY, ly);
            m.step_hdma(None);
        }
        // Back on line 46 in a fresh frame, the flag is clear (it was reset on
        // every LY change), so an edge here would arm normally.
        assert!(!m.dma.hdma.block_fired_this_hblank, "flag must not persist across a frame");
    }

    // ---- Adversarial audit tests (double-fire campaign review) ----
    //
    // At single speed the renderer nulls `scheduled_mode0_dot` at the m0-time
    // crossing BEFORE the bus reads `hdma_period`, so the ordinary per-line block
    // fires through the STAT-3->0 fallback with `period == None` =>
    // `in_period == false` at the fire site. `step_hdma`'s bottom only set
    // `hdma.block_done_this_period` when `in_period` was true, so a normally
    // fired block left the period marked UN-serviced. Everything that gates a
    // re-fire on `!hdma.block_done_this_period` (the FF55=00 cancel race branch,
    // the SS->DS STOP synchronous fire, `on_stop_window_enter`) could then run a
    // SECOND block for a period whose block already ran.
    //
    // The two SILICON-OBSERVABLE consequences of this internal root cause split
    // between a ROM and host-side tests:
    //   - FF55=00 cancel after the block fired: the FF55 read-back value ($AC,
    //     the written low 7 bits with bit 7 latched) is host-side in
    //     `ff55_cancel_after_block_reads_back_written_length_with_bit7`; the ROM
    //     `hdma_ff55_cancel_after_block.cgb.mooneye.asm` keeps only its VRAM
    //     sentinels (block0 copied, blocks 1/2 untouched — no spurious extra
    //     block, no dropped disable).
    //   - SS->DS STOP after the block fired: fully host-side in
    //     `ssds_stop_after_block_fired_does_not_refire` (the ROM
    //     `hdma_ff55_ssds_stop_after_block` was removed — the STOP speed-switch
    //     window is not portable across the internal-suite runners).
    // The remaining host-side tests below pin state with no standalone silicon
    // observable: the root-cause flag contract, and the LCD-off case that is not
    // reachable from a cold ROM (see its comment).

    /// A block fired via the normal fallback edge must mark this period's block
    /// as serviced — that is the flag's documented contract ("whether the HDMA
    /// block owed for the *current* eligibility period has already been
    /// serviced"). Pure internal state (no standalone silicon observable), so it
    /// is pinned here rather than by a ROM.
    #[test]
    fn edge_fired_block_marks_period_serviced() {
        let mut m = armed_at_hblank_edge();
        m.step_hdma(None); // STAT 3->0 fallback fires this line's block
        assert_eq!(m.dma.hdma.length, 3, "the line's block fired");
        assert!(
            m.dma.hdma.block_done_this_period,
            "an edge-fired block must mark hdma_block_done_this_period; leaving it \
             clear re-opens every !block_done re-fire gate (FF55 cancel race, \
             SS->DS STOP fire) for a block that already ran"
        );
    }

    /// LCD disable/enable does not reset `hdma.block_fired_this_hblank`; when
    /// LY holds the same value across the off/on cycle (it reads 0 on hardware
    /// throughout), the stale flag suppresses the first post-enable HBlank
    /// block. Fixed by clearing the flag while the LCD is off (no HBlank periods
    /// exist there, so the per-HBlank marker is meaningless — a display restart
    /// begins a fresh frame).
    ///
    /// Kept host-side, NOT ROMed: this is not reachable from a cold ROM. The only
    /// LY at which the flag can go stale across an off/on cycle is LY 0 (any other
    /// LY self-heals — disabling the LCD drops LY to 0, and that LY change clears
    /// the flag). Firing the first block at LY 0 AND toggling LCD off/on before LY
    /// advances to 1 is not robustly achievable: the post-block spin-exit + LCDC
    /// write latency crosses into LY 1, which auto-resets the flag. A cold-ROM
    /// reproduction (block at LY 0, LCD off/on, then completion timing) produced
    /// byte-identical VRAM end-states with and without the fix — the divergence is
    /// a one-block TIMING shift (the suppressed block fires one HBlank later), so
    /// the end state is identical once the transfer completes; there is no
    /// end-state bus observable to grade.
    #[test]
    fn lcd_off_on_same_ly_stale_flag_suppresses_first_block() {
        let mut m = armed_at_hblank_edge();
        // This line's block fired just before the game disables the LCD.
        m.dma.hdma.block_fired_this_hblank = true;
        m.io_registers.write(ppu::LCD_CONTROL, 0);
        m.step_hdma(None); // an LCD-off tracker step; LY unchanged
        // LCD back on; LY unchanged across the off/on cycle. First HBlank edge:
        m.io_registers.write(ppu::LCD_CONTROL, ppu::LCDCFlags::DisplayEnable as u8);
        m.io_registers.write(ppu::LCD_STATUS, 0);
        m.dma.hdma.prev_stat_mode = 3;
        m.step_hdma(None);
        assert_eq!(
            m.dma.hdma.length, 3,
            "first post-enable HBlank block suppressed by a stale \
             hdma_block_fired_this_hblank (no reset on LCD disable/enable)"
        );
    }

    // ---- Silicon-observable re-fire consequences (re-homed from ROMs) ----
    //
    // The two host-side tests below re-home the silicon observables of the
    // double-fire root cause. `hdma_ff55_ssds_stop_after_block` is being deleted
    // as a ROM (its observable is A3 here); `hdma_ff55_cancel_after_block` keeps
    // only its VRAM sentinels as a ROM, and its $AC read-back (SameSuite-derived)
    // moves to A4 here.

    /// A single->double-speed STOP taken mid-HBlank AFTER this line's HBlank-DMA
    /// block already fired must NOT re-fire the serviced block. The fired block
    /// marks the period serviced, so the STOP captures the period as `High` (in
    /// period, block done, no reflag), not `Requested` — the synchronous-fire
    /// gate keys on `!block_done_this_period`. FF55 therefore still reads $01
    /// (block1 pending), not $FF (completed a block early). Re-homed from the
    /// deleted ROM `hdma_ff55_ssds_stop_after_block`.
    #[test]
    fn ssds_stop_after_block_fired_does_not_refire() {
        let mut m = armed_at_hblank_edge();
        m.dma.hdma.length = 2; // blocks-1 => 3 blocks (FF55 reads $02)
        // block0 fires through the single-speed STAT-3->0 fallback; the fix marks
        // the period serviced.
        m.step_hdma(None);
        assert_eq!(m.dma.hdma.length, 1, "block0 fired: 3 -> 2 blocks remaining");
        assert!(m.dma.hdma.block_done_this_period, "the fired block marked the period serviced");
        assert_eq!(m.read(REG_HDMA5), 0x01, "FF55 shows block1 pending after block0");

        // SS->DS STOP in the SAME HBlank: capture at entry, re-flag at unhalt.
        m.on_stop_window_enter(true);
        assert!(
            matches!(m.halt.hdma_state, HaltHdmaState::High),
            "an already-serviced in-period block must capture High, not Requested"
        );
        m.stop_window_exit_reflag(true);

        // The serviced block was NOT re-fired: length/enable untouched.
        assert_eq!(m.dma.hdma.length, 1, "SS->DS STOP must not re-fire the serviced block");
        assert!(m.dma.hdma.enabled, "the transfer is still in progress");
        assert!(!m.dma.hdma.req_pending, "no re-fire was queued");
        assert_eq!(
            m.read(REG_HDMA5),
            0x01,
            "FF55 stays $01 (not $FF) right after the switch"
        );
    }

    /// A mid-HBlank FF55=$00 cancel written as $2C (bit7=0), AFTER this line's
    /// block already fired, stops the transfer and latches the WRITTEN low 7 bits
    /// as the read-back length: FF55 reads back 0x80|$2C = $AC (NOT the preserved
    /// remaining count, NOT $FF). The serviced-period cancel fires no spurious
    /// extra block. Re-homed $AC read-back (SameSuite dma/hdma_lcd_off) from
    /// `hdma_ff55_cancel_after_block`, whose ROM keeps only the VRAM sentinels.
    #[test]
    fn ff55_cancel_after_block_reads_back_written_length_with_bit7() {
        let mut m = armed_at_hblank_edge();
        // block fires through the fallback edge, marking the period serviced.
        m.step_hdma(None);
        assert!(m.dma.hdma.block_done_this_period, "the fired block marked the period serviced");
        assert!(m.dma.hdma.enabled, "the transfer is still armed before the cancel");

        // FF55 = $2C: bit7 clear cancels; the low 7 bits latch as the read-back.
        m.write(REG_HDMA5, 0x2C);

        assert!(!m.dma.hdma.enabled, "FF55=00 cancels the transfer");
        assert!(!m.dma.hdma.req_pending, "a serviced-period cancel fires no spurious block");
        assert_eq!(
            m.read(REG_HDMA5),
            0xAC,
            "FF55 reads back 0x80 | written ($2C), not the remaining count"
        );
    }

    /// Disabling the LCD during an ACTIVE HBlank DMA fires exactly one block —
    /// documented hardware behavior, NOT a spurious rustyboi block. SameBoy
    /// `GB_lcd_off` runs a block on `hdma_on_hblank && (STAT & 3)`, and SameSuite
    /// `dma/hdma_lcd_off` (real hardware) confirms a single tile copies. With the
    /// LCD off the HDMA period is permanently active, so entering it services one
    /// owed block and then stops — it does not keep transferring.
    #[test]
    fn lcd_off_during_active_hblank_dma_fires_one_block() {
        let mut m = armed_at_hblank_edge();
        // Mid-frame, drawing (mode 3): not a serviced HBlank — the case SameBoy's
        // `(STAT & 3) != 0` gate fires on.
        m.io_registers.write(ppu::LCD_STATUS, 3);
        m.dma.hdma.prev_stat_mode = 3;
        m.dma.hdma.prev_period = false;
        m.dma.hdma.block_fired_this_hblank = false;
        m.dma.hdma.block_done_this_period = false;
        let before = m.dma.hdma.length;
        m.write_lcd_control(0); // LCD off
        m.step_hdma(None);
        assert_eq!(
            m.dma.hdma.length,
            before - 1,
            "LCD-off during an active HBlank DMA fires exactly one block"
        );
        // The permanent off-period must not keep firing (SameSuite: one tile).
        m.step_hdma(None);
        m.step_hdma(None);
        assert_eq!(
            m.dma.hdma.length,
            before - 1,
            "only ONE block fires on LCD-off, not continuously"
        );
    }

    /// SameBoy fires on LCD-off only when `(STAT & 3) != 0` — i.e. NOT when the
    /// current HBlank's block already fired. rustyboi's `block_done_this_period`
    /// guard stands in for that gate: LCD-off in an already-serviced HBlank adds
    /// no second block. (Equivalence check for the two double-fire guards.)
    #[test]
    fn lcd_off_after_serviced_hblank_does_not_double_fire() {
        let mut m = armed_at_hblank_edge();
        m.step_hdma(None); // block0 fires via the mode-0 fallback, marks the period serviced
        let after_one = m.dma.hdma.length;
        assert!(m.dma.hdma.block_done_this_period, "this HBlank's block is serviced");
        m.write_lcd_control(0); // LCD off in the same, already-serviced HBlank
        m.step_hdma(None);
        assert_eq!(
            m.dma.hdma.length, after_one,
            "no second block on LCD-off after this HBlank's block already fired"
        );
    }
}
