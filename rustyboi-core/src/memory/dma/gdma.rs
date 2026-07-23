//! CGB GDMA (FF55 bit7=0): the general-purpose, immediately-blocking VRAM DMA
//! transfer.
//!
//! A thin mode of the FF51-FF55 VRAM-DMA unit: `execute_gdma` copies
//! 0x10*N bytes in one shot, reusing the shared byte machinery in `hdma`
//! (`snapshot_dma_dest0_pre`) for the PC-in-dest prefetch case. The immediate
//! byte copy (`copy_dma_byte`) and the GDMA-only VRAM source-conflict fixup
//! live here.
//!
//! A module under `memory::dma` holding the `impl Mmio` bus-master methods; it
//! reaches `Mmio`'s internals through their `pub(in crate::memory)` visibility
//! rather than as a child of `mmio`.
use crate::memory::mmio::{Mmio, VRAM_START};
use crate::memory::{self, Addressable};

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
        let saved_dma_active = self.dma.oam.active;
        self.dma.oam.active = false;

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

        self.dma.oam.active = saved_dma_active;
        byte
    }

    /// Consume the pending VRAM-source GDMA first-word latch (see the field
    /// doc); returns the first dest word's VRAM address + bank flag.
    pub(crate) fn take_gdma_vram_src_fixup(&mut self) -> Option<(u16, bool)> {
        self.dma.hdma.gdma_vram_src_fixup.take()
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

    /// Execute a CGB General-Purpose DMA (GDMA) transfer synchronously.
    /// Copies `length` bytes from `self.dma.hdma.source` into VRAM starting at
    /// `self.dma.hdma.dest`. Matches hardware:
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
        let mut src = self.dma.hdma.source;
        let mut dst = self.dma.hdma.dest;

        let effective_length = if (dst as usize) + length >= 0x10000 {
            0x10000 - dst as usize
        } else {
            length
        };

        // Arm the VRAM-source first-word latch (see `gdma_vram_src_fixup`).
        self.dma.hdma.gdma_vram_src_fixup = if (0x8000..=0x9FFF).contains(&src) && effective_length >= 2 {
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
        let interleave = self.dma.oam.active && !self.dma.oam.oam_dma_stop_freeze;
        // The one-M-cycle catch-up corrects the `step_dma` tick that ran before this
        // FF55 write resolved. A back-to-back second block follows its predecessor
        // with no intervening `step_dma` (the two FF55 writes are adjacent), so its
        // OAM-DMA position was already caught up by the first block — catching up
        // again would end the OAM DMA one M-cycle early (drops the last conflict
        // clobber, e.g. OAM[45] in oamdmasrcC000_..._2xgdmalen09).
        if interleave && !self.dma.hdma.gdma_conflict_ran {
            self.dma_advance_one_mcycle();
        }
        // Back-to-back second GDMA block: the OAM DMA is still mid-flight from a
        // FIRST GDMA-conflict pass in the same lifetime (no OAM-DMA completion in
        // between). Hardware's 16-bit word bus then holds the first block's already
        // word-written low OAM cells across the FF55-rewrite boundary gap, so this
        // block's low-address re-wrap must not re-clobber them (see
        // `dma_conflict_advance`). A single long GDMA (one pass) is never back-to-back.
        let back_to_back = interleave && self.dma.hdma.gdma_conflict_ran;
        // `loam` tracks the OAM-DMA's relative update cursor: it starts at
        // `-dma.subcycle` (dots already elapsed in the current M-cycle) and the
        // per-byte cc advance is compared against `loam + 3` (gate `cc-3 > loam`).
        let mut cc: i64 = 0;
        let mut loam: i64 = -(self.dma.oam.subcycle as i64);

        for _ in 0..effective_length {
            let data = self.copy_dma_byte(src, dst);
            cc += per_byte_cc;
            if interleave && self.dma.oam.active && cc - 3 > loam {
                loam += 4;
                self.dma.hdma.gdma_conflict_ran = true;
                self.dma_conflict_advance(src, data, back_to_back);
            }
            src = src.wrapping_add(1);
            dst = dst.wrapping_add(1);
        }
        // After the block, the OAM-DMA continues from the advanced position. The
        // residual `loam` phase becomes the next M-cycle's sub-cycle offset so
        // `step_dma` resumes on the correct dot (the OAM-DMA update cursor carries
        // the residual phase forward).
        if interleave && self.dma.oam.active {
            // Dots elapsed since the last OAM-DMA M-cycle fired. `step_dma` fires
            // when `dma.subcycle` reaches 4, so the residual phase `(cc - loam)`
            // (mod 4) is exactly the count already accrued toward the next
            // M-cycle (the OAM-DMA update cursor carries `loam` forward and
            // recomputes the sub-cycle phase as `(cc - cursor) >> 2`).
            self.dma.oam.subcycle = (cc - loam).rem_euclid(4) as u8;
        }

        self.dma.hdma.source = src;
        self.dma.hdma.dest = dst;

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
        let prefetch_fudge = if self.dma.oam.prefetch_stat_bias { 0 } else { 5 };
        if self.is_double_speed_mode() && self.dma.oam.prefetch_stat_bias {
            setup -= 1;
        }
        self.dma.hdma.pending_dma_stall += (effective_length as u32) * per_byte + setup + prefetch_fudge;
        // The OAM-DMA M-cycles for the transfer were folded into the loop above.
        // Suppress `step_dma` for the true dma-event duration (the transfer
        // `per_byte` cc plus the single trailing `cc += 4`), NOT the extra `+5`
        // CPU-stall prefetch fudge. Hardware freezes the OAM-DMA cursor for the
        // event then catches the OAM-DMA up afterward; the residual
        // post-stall cc advance the OAM-DMA normally toward the next access.
        if interleave {
            self.dma.hdma.oam_dma_stall_suppress = (effective_length as u32) * per_byte + 4;
        }
    }
}
