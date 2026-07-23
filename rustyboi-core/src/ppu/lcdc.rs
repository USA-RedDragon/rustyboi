use crate::memory::mmio;
use crate::ppu::fetcher;
use super::controller::{
    lcdc_has, sprite_tile_walk_cost, CGB_PIXEL_TRANSFER_WARMUP, DMG_PIXEL_TRANSFER_WARMUP,
    LCDCFlags, PendingLcdcEvent, PendingLcdcEventKind, Ppu, SpriteFetchPhase, State,
    OBJEN_APPLY_DOTS, OBJEN_APPLY_DOTS_CGB, OBJSIZE_APPLY_DOTS, WG_TRANSITION_DELAY,
    WIN_M3_PENALTY,
};

impl Ppu {
    pub(crate) fn handle_lcdc_write(&mut self, value: u8, mmio: &mmio::Mmio) {
        let display_enable = LCDCFlags::DisplayEnable as u8;
        let old_lcdc = self.lcdc.reg;
        let display_stays_enabled = (old_lcdc & display_enable) != 0 && (value & display_enable) != 0;

        // DMG window bus-glitch journal (see wg_apply): record the exact bus
        // transition time of a mid-mode-3 bit6/bit4 toggle. The address lines
        // reach the VRAM bus WG_TRANSITION_DELAY dots after the write's
        // register commit (the hardware transition dot lands on the window fetch
        // grid 3 dots after our register-visible apply cc).
        if !mmio.is_cgb_features_enabled()
            && display_stays_enabled
            && self.state == State::PixelTransfer
        {
            let wg_bits = (LCDCFlags::WindowTileMapDisplaySelect as u8)
                | (LCDCFlags::BGWindowTileDataSelect as u8)
                | (LCDCFlags::BGTileMapDisplaySelect as u8);
            if (old_lcdc ^ value) & wg_bits != 0 {
                let t_cc = self.write_cc(false) + WG_TRANSITION_DELAY;
                self.wg.wg_hist.push((t_cc, old_lcdc, value));
                self.bg_retro_repair(mmio);
            }
        }

        // The hardware window-pixel-insertion-disable glitch: a window-DISABLE
        // landing while a window tile fetch is in flight suppresses the
        // WE-off zero-pixel insert glitch for the remainder of this line
        // (reset at the next M3 arm).
        let win_en_bit = LCDCFlags::WindowDisplayEnable as u8;
        if !mmio.is_cgb()
            && display_stays_enabled
            && (old_lcdc & win_en_bit) != 0
            && (value & win_en_bit) == 0
            && self.win_being_fetched
        {
            self.we_insert_suppressed = true;
        }

        // Per-pixel BG-enable history. A mid-mode-3 LCDC.0 (BG-enable) toggle must
        // be applied per display column: the per-dot draw is flushed in bursts (the
        // mode-0 time flush draws all remaining FIFO pixels at one cc), so a
        // once-per-line / live `self.lcdc.reg & 1` read applies the final BG-enable to
        // every flushed column. Record each bit0 change as a (boundary_col, bgen)
        // entry — columns >= boundary_col see the new bit — so the renderer
        // reproduces the live per-tile `lcdc & lcdc_bgen` read. Only while pixel
        // transfer is active for this line.
        let bgen_bit = LCDCFlags::BGDisplay as u8;
        if display_stays_enabled
            && self.state == State::PixelTransfer
            && (old_lcdc & bgen_bit) != (value & bgen_bit)
        {
            // Column-space history keyed by the display column at which the
            // BG-enable change first becomes visible. `self.x` is the next display
            // column to be popped — the real pipeline plot position at the write
            // instant (it already carries the warmup/FIFO and window latency the
            // latency-free closed-form predictor lacks). The write commits `cc + 2`
            // PPU dots later, so the change first reaches the column plotted 2 dots
            // later: boundary = `self.x + 2`. When this line draws a window the
            // displayed column advances slower than 1/dot through the +6
            // StartWindowDraw stall, so the 2-dot commit spans ~2 extra display
            // columns; add +2 on window lines (net boundary self.x+4).
            let new_on = (value & bgen_bit) != 0;
            let win = self.window_started_this_line
                || self.win_draw_start
                || self.window_y_active(mmio);
            // DMG stall-aware boundary: the +2-dot commit is a POP-schedule
            // property, not a column offset. When pops are frozen at the write
            // (a sprite fetch stall in progress at column x, or one arming
            // there), column x itself pops after the commit and takes the new
            // bit; a sprite arming at x+1 pins the boundary to x+1 (the BG-off
            // span starts AT the stalled column). No stall keeps x+2.
            let cgb_compat = mmio.is_cgb() && !mmio.is_cgb_features_enabled();
            let stall_adj = if !mmio.is_cgb_features_enabled() {
                if cgb_compat && self.sprite_fetch_stall > 0 {
                    // CGB-compat: the sprite-fetch stall freezes the pipeline but
                    // the LCDC.0 commit dot keeps advancing toward the display
                    // column it lands on. The commit offset is GRADUATED by the
                    // remaining stall dots (2 - stall; with cgb_compat_adj=+1
                    // below the total is 3 - stall), not the binary 0/2 the DMG
                    // path uses (e.g. a BG-off write landing during the leftmost
                    // sprite's fetch stall: stall=3 -> boundary 0, stall=1 ->
                    // boundary 2). cgb_compat_adj below stays +1 for the stall
                    // case, so the total commit offset is 3 - stall.
                    3i32 - self.sprite_fetch_stall as i32
                } else if self.sprite_fetch_stall > 0 || self.dmg_unfetched_sprite_at(self.x) {
                    0
                } else if self.dmg_unfetched_sprite_at(self.x.saturating_add(1)) {
                    1
                } else {
                    2
                }
            } else {
                2
            };
            // CGB DMG-compat: the LCDC.0 commit lands one column later than DMG
            // in the plain no-stall case; but when a sprite fetch stalls OR an
            // unfetched sprite gates this column, the commit lands one column
            // EARLIER than DMG+1 (e.g. self.x=8 with an unfetched sprite wants
            // boundary 8, not 9).
            let cgb_compat_adj = if cgb_compat {
                let sprite_active = self.sprite_fetch_stall > 0
                    || self.dmg_unfetched_sprite_at(self.x)
                    || self.dmg_unfetched_sprite_at(self.x.saturating_add(1));
                if sprite_active { 0 } else { 1 }
            } else {
                0
            };
            let boundary_col = (self.x as i32 + stall_adj + cgb_compat_adj
                + if win { 2 } else { 0 })
            .clamp(0, 160) as u8;
            self.plot.bgen_history.push((boundary_col as u64, new_on));
        }

        // DMG mid-mode-3 OBJ-enable (LCDC.1) toggle: per-column pop gate +
        // in-progress fetch abort. Hardware gates each sprite pixel on LCDC.1
        // at that pixel's own pop dot, so the toggle covers an exact column
        // span; the boundary column mirrors the bgen model (the write becomes
        // visible to the mixer a couple of dots after `self.x`). Additionally
        // (the hardware "disabling objects while already fetching" behavior): a
        // disable landing while a sprite fetch is in progress ABORTS it — the
        // remaining stall dots are not consumed and the sprite's pixels never
        // reach the line. The closed-form mode-0 time refund for the same abort is
        // handled in set_lcdc_visible (remaining_sprite_cost, graduated).
        let objen_bit = LCDCFlags::SpriteDisplayEnable as u8;
        if !mmio.is_cgb_features_enabled()
            && display_stays_enabled
            && self.state == State::PixelTransfer
            && (old_lcdc & objen_bit) != (value & objen_bit)
        {
            let new_on = (value & objen_bit) != 0;
            // The write commits to the pixel gate OBJEN_APPLY_DOTS after the
            // hook (the hook runs before this dot's PPU step; the first gated
            // pop lands two dots out).
            let apply = if mmio.is_cgb() && !mmio.is_cgb_features_enabled() {
                OBJEN_APPLY_DOTS_CGB
            } else {
                OBJEN_APPLY_DOTS
            };
            self.plot.objen_history
                .push((self.ticks + apply, new_on));
            // Abort window = the sprite's own fetch bus activity
            // [match_dot, match_dot + penalty): a left-clipped sprite (spx < 8)
            // matched during the first-tile prologue, so its fetch ENDS before
            // the pipeline-refill tail of its stall — a disable landing in that
            // tail does NOT abort (the variant's k=0..2 bands keep the full
            // penalty). rec.arm_tick already carries the match adjustment. The
            // disable commits ~1 dot past the write hook; a fetch whose last
            // bus dot is the commit dot completes (obj_en k=15 keeps its
            // pixels), hence the strict compare with +1. On abort the stall
            // resumes pops at the commit dot: one residual stall dot remains.
            // Mid-fetch OBJ-disable aborts the in-progress sprite fetch only on DMG
            // silicon. On CGB hardware (including DMG-compat mode) the object fetch
            // treats OBJ_EN as always-on and never aborts ("disabling objects
            // while already fetching" is gated behind `!is_cgb`), so the sprite's
            // full fetch cost is spent regardless of the OBJ-disable — a short
            // OBJ-off pulse that re-enables mid-line does not shorten mode 3.
            if !mmio.is_cgb()
                && !new_on && self.sprite_fetch_stall > 0 && self.next_sprite_fetch_index > 0
                && let Some(rec) = self
                    .sprite_fetch_recs
                    .get_mut(self.next_sprite_fetch_index - 1)
                && rec.phase == SpriteFetchPhase::Fetched
            {
                let fetch_end = rec.arm_tick + rec.penalty as u128;
                if fetch_end > self.ticks + 1 {
                    rec.phase = SpriteFetchPhase::Aborted;
                    self.sprite_fetch_stall = self.sprite_fetch_stall.min(1);
                }
            }
        }

        // DMG mid-mode-3 OBJ-size (LCDC.2) toggle: record the apply dot so each
        // sprite tile-data byte samples the size bit at its own fetch dot (the
        // per-byte object-line-address recomputation, see obj_pixel_sized).
        let objsz_bit = LCDCFlags::SpriteSize as u8;
        if !mmio.is_cgb_features_enabled()
            && display_stays_enabled
            && self.state == State::PixelTransfer
            && (old_lcdc & objsz_bit) != (value & objsz_bit)
        {
            let apply_tick = self.ticks + OBJSIZE_APPLY_DOTS;
            self.plot.objsize_dot_history
                .push((apply_tick, (value & objsz_bit) != 0));
        }

        // Exact-cc OBJ-size (LCDC bit2) latch for the mode-2 OAM scan (PoC
        // extension). A sprite-size write during OAMSearch must become visible to
        // each OAM-scan slot as-of that slot's own abs_cc — not via the 2-dot
        // pending_lcdc_events queue plus the one-slot snapshot lag, which together
        // drop a late size change one OAM slot too far. Record the exact abs_cc
        // the change is visible (write_cc + 2*cgb, an LCDC write taking effect at cc+2 on hardware);
        // the scan samples bit2 against it per slot. Scoped to mode-2 writes; the
        // PixelTransfer mid-mode-3 size toggle keeps its closed-form recompute.
        let ssz = LCDCFlags::SpriteSize as u8;
        if display_stays_enabled
            && self.state == State::OAMSearch
            && mmio.is_cgb_features_enabled()
            && (old_lcdc & ssz) != (value & ssz)
        {
            // The OBJ-size change becomes visible to the fetcher/scan at
            // `write_cc + 2` (an LCDC write taking effect at cc+2 on hardware). The OAM scan samples
            // it per slot against this apply cc (objsize_large_at_cc), so a slot
            // read strictly past the apply cc sees the new size. ENABLE (8x8 ->
            // 8x16) lands at +2; DISABLE (8x16 -> 8x8) lands one OAM slot later
            // (+2 more cc): the hardware OAM scanner keeps the larger
            // already-latched height for the entry whose read straddles the
            // shrink, so the straddling sprite is still scanned 8x16. The
            // late_sizechange (disable) vs late_sizechange2 (enable) bracket pairs
            // require this asymmetry; with a symmetric offset the disable family
            // 1-for-1-swaps. (Verified across both speeds; DS landed at +2 for
            // both directions because the DS brackets only exercise the enable
            // side / the rounded odd-cc slot already absorbs the extra delay.)
            let ds = mmio.is_double_speed_mode();
            let disable = (old_lcdc & ssz) != 0 && (value & ssz) == 0;
            let off = if ds { 2 } else { 2 + if disable { 2 } else { 0 } };
            self.objsize_prev_large = self.objsize_large_at_cc(self.write_cc(ds));
            self.objsize_new_large = (value & ssz) != 0;
            self.objsize_apply_cc = (self.write_cc(ds) as i64 + off).max(0) as u64;
        }

        if mmio.is_cgb_features_enabled() && display_stays_enabled {
            // Exact-cc latch for the BG-fetch bit4 effect (PoC). When bit4
            // toggles during active pixel transfer, the per-dot queue quantizes
            // the commit to a dot boundary and lands it one fetch substep late.
            // Record the exact abs_cc the change should be visible to the
            // fetcher so each substep samples it on the correct side. Hardware
            // applies the new LCDC at `cc + 2` (PPU dots); a +2 abs_cc offset
            // lands the bit4 change exactly on the BG-fetch substep that should
            // first see it (verified against bgtiledata_spx08_ds_3/_4).
            let tds = LCDCFlags::BGWindowTileDataSelect as u8;
            let en = LCDCFlags::DisplayEnable as u8;
            if self.state == State::PixelTransfer && (old_lcdc & tds) != (value & tds) {
                let ds = mmio.is_double_speed_mode();
                let commit_cc = self.write_cc(ds) + 2;
                self.lcdc.lcdc_b4_exact = Some((commit_cc, value, old_lcdc));
                // Tile-index-is-tile-data glitch: a
                // falling LCDC.4 edge arms the glitch for exactly one CPU T-cycle
                // (on hardware the write sets the glitch flag for one T-cycle: set, advance 1,
                // clear). The single BG tile-data read that lands in that window
                // returns the tile INDEX instead of a VRAM byte (on hardware
                // this glitch is gated on tile < 0x80). Instrumented
                // CGB-C hardware places that read exactly 4 dots after the write in
                // its own grid. rustyboi's CPU-write dot sits at a substep- and
                // parity-dependent phase within its BG fetch grid, so the target
                // read (cc, k) is derived from the fetcher substep at the write:
                // a write about to run TileDataLow (substep 1) glitches that k=1
                // read (+2); a write on the tile boundary (substep 3) glitches the
                // next tile's k=2 read (+8) only when the write lands off the even
                // fetch cadence (odd abs_cc) — an on-cadence boundary write applies
                // the new addressing cleanly with no straddle. Verified dot-exact
                // vs CGB-C hardware on age m3-bg-lcdc (LOW-plane glitch) and
                // cgb-acid-hell (HIGH-plane glitch).
                let arm = (old_lcdc & tds) != 0
                    && (value & tds) == 0
                    && (old_lcdc & en) != 0
                    && (value & en) != 0;
                if arm && !ds {
                    let s = self.fetcher.fetch_substep();
                    let odd = self.abs_cc & 1 == 1;
                    let target = match s {
                        // About to read TileDataLow: glitch it (k=1), 2 dots out.
                        1 => Some((self.abs_cc + 2, 1u8)),
                        // About to read TileDataHigh: glitch it (k=2), 2 dots out.
                        2 => Some((self.abs_cc + 2, 2u8)),
                        // Tile boundary (Push next): an off-cadence write straddles
                        // into the next tile's HIGH read (+8); on-cadence is clean.
                        3 if odd => Some((self.abs_cc + 8, 2u8)),
                        _ => None,
                    };
                    if let Some(t) = target {
                        self.wg.tidxtd_glitch.push(t);
                    }
                }
            }
            // Window-enable (bit 5) toggle: record the exact hardware commit cc
            // (`write_cc + 2`, abs_cc units — same anchor as `lcdc_b4_exact`) so
            // the window-enable master checkpoints resolve the window-enable bit as-of their
            // own dot (see `we_win_bit_exact`).
            let we = LCDCFlags::WindowDisplayEnable as u8;
            if (old_lcdc & we) != (value & we) {
                let ds = mmio.is_double_speed_mode();
                // An LCDC write takes effect at cc+2 on hardware: the window bit is effective at
                // write_cc + 2 master cc. In rustyboi's abs_cc units the boundary
                // that aligns with the window-enable master checkpoint dot (write_ticks + 2 dots
                // ahead) is `write_cc + 3` (single speed) / `+4` (double speed) —
                // the abs_cc derive-phase plus the per-dot abs_cc factor. The
                // window-enable master event runs at the checkpoint BEFORE the LCDC commit, so equality
                // reads the OLD bit (the `<=` in `update_window_y_latch`).
                let commit_cc = self.write_cc(ds) + if ds { 4 } else { 3 };
                self.lcdc.we_win_bit_exact =
                    Some((commit_cc, (value & we) != 0, (old_lcdc & we) != 0));
            }
            self.lcdc.pending_lcdc_events.push(PendingLcdcEvent {
                cycles_remaining: 1,
                base_value: old_lcdc,
                value,
                kind: PendingLcdcEventKind::TileDataSelectOnly,
            });
            // Full lands 2 PPU dots after the write commits, matching the hardware
            // LCDC write taking effect at cc+2.
            self.lcdc.pending_lcdc_events.push(PendingLcdcEvent {
                cycles_remaining: 2,
                base_value: old_lcdc,
                value,
                kind: PendingLcdcEventKind::Full,
            });
        } else {
            self.lcdc.pending_lcdc_events.clear();
            self.set_lcdc_visible(value, mmio.is_cgb_features_enabled(), mmio.is_double_speed_mode());
        }
    }

    /// Per-dot LCDC delayed-commit pump. The queue is empty except for a few
    /// dots after a CPU FF40 write, so the hot path is the empty check alone;
    /// the drain loop lives out of line.
    #[inline]
    pub(crate) fn step_lcdc_events(&mut self, mmio: &mmio::Mmio) {
        if self.lcdc.pending_lcdc_events.is_empty() {
            return;
        }
        self.step_lcdc_events_slow(mmio);
    }

    fn step_lcdc_events_slow(&mut self, mmio: &mmio::Mmio) {
        let mut index = 0;
        while index < self.lcdc.pending_lcdc_events.len() {
            if self.lcdc.pending_lcdc_events[index].cycles_remaining > 0 {
                self.lcdc.pending_lcdc_events[index].cycles_remaining -= 1;
            }

            if self.lcdc.pending_lcdc_events[index].cycles_remaining == 0 {
                let event = self.lcdc.pending_lcdc_events.remove(index);
                match event.kind {
                    PendingLcdcEventKind::TileDataSelectOnly => {
                        let tile_data_select = LCDCFlags::BGWindowTileDataSelect as u8;
                        let value = (event.base_value & !tile_data_select) | (event.value & tile_data_select);
                        self.set_lcdc_visible(value, mmio.is_cgb_features_enabled(), mmio.is_double_speed_mode());
                        self.invalidate_fast_span();
                    }
                    PendingLcdcEventKind::Full => {
                        self.set_lcdc_visible(event.value, mmio.is_cgb_features_enabled(), mmio.is_double_speed_mode());
                        // The settled value now lives in self.lcdc.reg /
                        // cgb_tile_index_is_tile_data; drop the exact-cc override.
                        self.lcdc.lcdc_b4_exact = None;
                        // The commit changes what the mode-3 one-shot checks
                        // compare against; hold the preamble fast path off.
                        self.invalidate_fast_span();
                    }
                }
            } else {
                index += 1;
            }
        }
    }

    /// Mode-3 sprite cost (dots) of the sprites NOT yet rendered this line, under
    /// the given OBJ-enable state, using the one faithful tile-walk model. Sprites
    /// with index < `next_sprite_fetch_index` have already been drawn (their cost
    /// is already spent and fixed); only the remaining ones contribute. Drives the
    /// mid-mode-3 OBJ-toggle recompute so the closed-form mode-0 time is shifted by the
    /// exact remaining-sprite cost delta (matching the hardware next-mode-0 prediction
    /// re-run at the current `p.the next sprite`).
    fn remaining_sprite_cost(&self, scx: i32, obj_enabled: bool, use_fetch_index: bool) -> i32 {
        if !obj_enabled {
            return 0;
        }
        // The set of sprites whose cost is NOT yet committed (and so is affected by
        // a mid-mode-3 OBJ toggle). Two gates, matching how the live renderer
        // commits sprite fetches:
        // - DISABLE (`use_fetch_index`): OBJ was on up to here, so the fetch loop
        // has advanced `next_sprite_fetch_index` over every sprite whose stall
        // already armed (committed). Only sprites at index >= that count have
        // their cost removed. This gives the exact 1-cc disable boundary the
        // sprite_late_disable_*_{1,2} pairs bracket (the stall arms on the dot
        // the index advances).
        // - ENABLE: OBJ was off, so the fetch loop never advanced; a sprite will
        // still be fetched iff its trigger (display x = spx - 8) is not yet
        // passed, i.e. spx >= x + 8.
        if use_fetch_index {
            // DISABLE: the live renderer advances `next_sprite_fetch_index` at the
            // START of each sprite's stall and locks that sprite's cost into the
            // schedule GRADUALLY as the stall counts down -- the hardware
            // unrolled full-tile fetch charges the sprite's `max(11-dist,6)` dots one at
            // a time as `p.cycles` is consumed. A mid-mode-3 OBJ-disable therefore
            // refunds only the part of the in-progress sprite's stall that has NOT
            // yet elapsed, plus the full cost of every sprite whose stall has not yet
            // started (index >= nsfi). This makes the refunded mode-0 time depend 1:1 on
            // the disable cc (the later the disable, the less the refund), which the
            // sprite_late[_late]_disable_spx{18..1B}_{1,2} bracket pairs require:
            // their disable timings differ by single dots and the refunded mode-3 end
            // must cross the FF41 read cc by the matching fraction.
            //
            // Sprites at index >= nsfi: stall not yet started -> fully refundable.
            let mut tail: Vec<i32> = self
                .sprites_on_line
                .iter()
                .skip(self.next_sprite_fetch_index)
                .map(|s| s.x as i32)
                .collect();
            tail.sort_unstable();
            let mut cost = sprite_tile_walk_cost(&tail, scx, 167, 167, true);
            // In-progress sprite (index nsfi-1): its stall began at
            // `m3_last_sprite_commit_tick`; the dots remaining are its standalone
            // leading-rate cost minus the dots already counted down. Refund only the
            // remaining (clamped at 0 once fully drawn).
            if self.next_sprite_fetch_index > 0 {
                let in_prog = &self.sprites_on_line[self.next_sprite_fetch_index - 1];
                let single = sprite_tile_walk_cost(&[in_prog.x as i32], scx, 167, 167, true);
                // The live renderer consumes the in-progress sprite's first stall dot
                // on the same tick it advances `next_sprite_fetch_index` (the stall is
                // armed and immediately decremented), so the elapsed count includes
                // the commit tick itself: `ticks - commit_tick + 1`.
                let elapsed = self
                    .ticks
                    .saturating_sub(self.m3_last_sprite_commit_tick) as i32
                    + 1;
                cost += (single - elapsed).max(0);
            }
            return cost;
        }
        // ENABLE: a sprite will still be fetched iff the fetcher has NOT yet reached
        // its trigger (display x = spx - 8). At x == spx - 8 the fetcher is already
        // at the trigger and the sprite is missed, so the gate is strict: spx > x + 8.
        // (The sprite_late_enable_spx18_{1,2} pair brackets this single-dot boundary:
        // enabling at x = spx-9 still fetches, at x = spx-8 does not.)
        let cutoff = self.x as i32 + 8;
        let mut sprite_xs: Vec<i32> = self
            .sprites_on_line
            .iter()
            .map(|s| s.x as i32)
            .filter(|&spx| spx > cutoff)
            .collect();
        sprite_xs.sort_unstable();
        // The remaining group resumes the tile walk with no carried "first sprite"
        // (previous tile number = none), so the first remaining sprite in its tile gets the
        // leading rate, the rest 6 — the same sprite-cost accumulation continuation
        // hardware uses. No window split here (the window-bit is unchanged on this
        // path, so `nwx == targetx` collapses the split).
        sprite_tile_walk_cost(&sprite_xs, scx, 167, 167, true)
    }

    // The CGB tile-index-is-tile-data glitch for the BG data read about to run
    // (`self.abs_cc`, substep `k`): true iff a falling LCDC.4 write armed exactly
    // this (cc, k) read (see handle_lcdc_write / tidxtd_glitch). The glitch is a
    // single-read event, not a sustained level, so only the one read the hardware
    // 1-T-cycle tile-select-glitch window catches returns the tile index as data.
    fn tidxtd_quirk_at_fetch(&self) -> bool {
        let k = self.fetcher.fetch_substep();
        self.wg.tidxtd_glitch
            .iter()
            .any(|&(cc, tk)| cc == self.abs_cc && tk == k)
    }

    pub(in crate::ppu) fn fetcher_lcdc_state(&self) -> fetcher::FetcherLcdcState {
        // The tile-index-is-tile-data quirk is resolved per fetch dot from the
        // history (independent of the tdsel-address split below), so a falling
        // edge landing between a tile's TileDataLow and TileDataHigh reads
        // quirks the HIGH byte only.
        let quirk = self.tidxtd_quirk_at_fetch();
        // Exact-cc resolution of a pending mid-mode-3 bit4 toggle (PoC). If a
        // bit4 change is latched and this substep's abs_cc has not yet reached
        // its exact commit cc, present the PRE-commit bit4. This lets a single
        // tile straddle the change: TileDataLow before the commit uses the old
        // addressing method, TileDataHigh after it uses the new one.
        if let Some((commit_cc, new_val, old_val)) = self.lcdc.lcdc_b4_exact {
            let tds = LCDCFlags::BGWindowTileDataSelect as u8;
            if self.abs_cc < commit_cc {
                // Pre-commit: old bit4.
                let lcdc = (self.lcdc.reg & !tds) | (old_val & tds);
                return fetcher::FetcherLcdcState {
                    lcdc,
                    cgb_tile_index_is_tile_data: quirk,
                    or_lcdc: None,
                    scy_bus: None,
                scx_bus: None,
                };
            } else {
                // Post-commit: new bit4.
                let lcdc = (self.lcdc.reg & !tds) | (new_val & tds);
                return fetcher::FetcherLcdcState {
                    lcdc,
                    cgb_tile_index_is_tile_data: quirk,
                    or_lcdc: None,
                    scy_bus: None,
                scx_bus: None,
                };
            }
        }
        fetcher::FetcherLcdcState {
            lcdc: self.lcdc.reg,
            cgb_tile_index_is_tile_data: quirk,
            or_lcdc: None,
            scy_bus: None,
                scx_bus: None,
        }
    }

    // DMG mid-mode-3 window VRAM-bus glitch. The hardware window fetch grid
    // differs from the renderer's anchored grid when sprites stall the line, so
    // each window fetch read is re-evaluated at its reconstructed HARDWARE dot
    // `h` against the exact LCDC.6/LCDC.4 bus-transition times (`wg_hist`):
    // Not in Pan Docs, TCAGBD (§8.16.1 Window and §8.17.3 VRAM-in-mode-3 are TODO stubs),
    // or GBCTR; sub-dot render timing from mealybug-tearoom-tests refs.
    // - h = F + D_pre + 8*tile + 2*substep + midline sprite shifts, where F is
    // the undelayed window-restart TileNumber dot (`wg_anchor_cc`).
    // - An offscreen-left sprite (OAM X <= 7) is fetched BEFORE the window
    // restart and delays the whole grid by D_pre = max(7, 13 - 2*ceil(X/2))
    // (2-dot fetcher-boundary quantized; single-sprite case).
    // - An on-screen sprite at window position pos = X - 8 >= 0 lets the
    // in-progress tile fetch complete through TileDataHigh, then inserts its
    // stall: tiles >= pos/8 + 2 shift by the sprite's actually-charged
    // penalty, read from its live fetch record (`sprite_fetch_recs` — the
    // classic max(11 - dist, 6) leading rate / flat-6 follower, or nothing
    // if the walk dropped/aborted the sprite).
    // - A read strictly between the transitions sees the post-write bits; a
    // read ON a transition dot returns the OR of both addresses' bytes (the
    // address lines change mid-read; both cells drive 1-bits onto the bus).
    // Derive the hardware window fetch-grid origin F at a DMG x==0 window
    // start (the immediate TileNumber catch-up runs on the current dot, `chop`
    // dots after the conceptual grid origin). See wg_apply.
    // The window draw-start state transition shared by all three activation
    // sites (early WX 1..6, the DMG deferred WX commit, and the main trigger):
    // hardware increments the window Y position here — once per line the window
    // actually begins drawing, not per-line in M2 — and restarts the fetcher in
    // window mode at `window_x`.
    //
    // Deliberately NOT part of this helper, because the three sites genuinely
    // differ past this point and the differences are load-bearing:
    //   - `m3_sprite_prev_tile` reset and the `win_start_dot` latch happen only
    //     at the early-WX site;
    //   - `win_first_tile_chop` is `7 - wx` / `0` / a `!is_cgb`-gated `chop`;
    //   - `wg_set_anchor` and the fetcher catch-up are unconditional at the
    //     first two sites but nested under `!is_cgb` at the main trigger, and
    //     the main trigger runs a multi-phase catch-up loop rather than a
    //     single substep.
    // Test one LCDC bit in the PPU's live LCDC latch.
    #[inline]
    pub(in crate::ppu) fn lcdc_has(&self, f: LCDCFlags) -> bool {
        lcdc_has(self.lcdc.reg, f)
    }

    pub(in crate::ppu) fn set_lcdc_visible(&mut self, value: u8, cgb_features_enabled: bool, ds: bool) {
        let old_lcdc = self.lcdc.reg;
        let tile_data_select = LCDCFlags::BGWindowTileDataSelect as u8;
        let display_enable = LCDCFlags::DisplayEnable as u8;
        self.lcdc.cgb_tile_index_is_tile_data = cgb_features_enabled
            && (old_lcdc & tile_data_select) != 0
            && (value & tile_data_select) == 0
            && (old_lcdc & display_enable) != 0
            && (value & display_enable) != 0;
        // A mid-mode-3 window-enable toggle invalidates the closed-form mode-0
        // schedule (computed at M3 start from the initial WX/LCDC). Fall back to
        // the live emergent x==160 transition, which tracks the change.
        let win_bit = LCDCFlags::WindowDisplayEnable as u8;
        // A mid-mode-3 sprite-enable (bit 1) or sprite-size (bit 2) toggle also
        // changes the closed-form sprite-fetch penalty; invalidate and fall back
        // to the live emergent transition.
        let spr_bits = (LCDCFlags::SpriteDisplayEnable as u8) | (LCDCFlags::SpriteSize as u8);
        // A mid-mode-3 sprite-enable (bit 1) toggle, with no window change, keeps
        // the closed-form schedule but RECOMPUTES the not-yet-drawn sprite cost
        // from the single tile-walk model (the hardware next-mode-0 prediction re-runs the
        // predictor with `LCDC OBJ-enable(p)` live and the current `p.the next sprite`, so the
        // remaining sprites' cost is added/removed precisely). Shift both the
        // mode-0 dot and the read-at-cc mode-0 time by the cost delta rather than
        // nulling and falling back to the live x==160 transition.
        let obj_bit = LCDCFlags::SpriteDisplayEnable as u8;
        let only_obj_toggle = (old_lcdc & win_bit) == (value & win_bit)
            && (old_lcdc & (LCDCFlags::SpriteSize as u8)) == (value & (LCDCFlags::SpriteSize as u8))
            && (old_lcdc & obj_bit) != (value & obj_bit);
        if self.state == State::PixelTransfer
            && only_obj_toggle
            && self.m0.scheduled_mode0_dot.is_some()
        {
            let scx = (self.m3.m3_arm_scx & 0x07) as i32;
            let old_obj = (old_lcdc & obj_bit) != 0 || cgb_features_enabled;
            let new_obj = (value & obj_bit) != 0 || cgb_features_enabled;
            // DISABLE (old OBJ on): committed sprites are those whose cost the live
            // fetch loop has already locked into the schedule -> gate by the
            // lock-aware committed index. ENABLE (old OBJ off): gate by display
            // position. `use_fetch_index = old_obj` selects the right gate for
            // whichever side is non-zero.
            let use_fetch_index = old_obj && !new_obj;
            let old_rem = self.remaining_sprite_cost(scx, old_obj, use_fetch_index);
            let new_rem = self.remaining_sprite_cost(scx, new_obj, false);
            let delta = new_rem - old_rem; // dots; negative on disable
            // KEEP the closed-form schedule, shifting it by the (graduated) cost
            // delta. delta < 0 refunds the not-yet-drawn portion of the remaining
            // sprites (the next-mode-0 prediction re-run with the new OBJ-enable at the current
            // `p.the next sprite`); delta == 0 means every remaining sprite's cost is
            // already drawn, so the original closed-form mode-0 time (which includes the
            // full sprite cost) is already correct and must be kept -- nulling it and
            // falling back to the live x==160 transition would mis-resolve the FF41
            // read for the fully-committed bracket variants (sprite_late_late_disable
            // spx1B_2). The graduated `remaining_sprite_cost` makes the refund (and so
            // the resulting mode-0 time) depend 1:1 on the disable cc, which is what the
            // sprite_late[_late]_disable bracket pairs require.
            if let Some(dot) = self.m0.scheduled_mode0_dot {
                self.m0.scheduled_mode0_dot = Some((dot as i64 + delta as i64).max(0) as u128);
            }
            if let Some(m0t) = self.m0.m0_time_master {
                let dsf = ds as i64;
                self.m0.m0_time_master =
                    Some((m0t as i64 + ((delta as i64) << dsf)).max(0) as u64);
            }
            self.lcdc.reg = value;
            return;
        }
        if self.state == State::PixelTransfer
            && ((old_lcdc & win_bit) != (value & win_bit)
                || (old_lcdc & spr_bits) != (value & spr_bits))
        {
            self.m0.scheduled_mode0_dot = None;
            // A mid-mode-3 window-ENABLE toggle (not sprite) is the symmetric
            // counterpart to the disable refund below: the closed-form m0_time_master
            // was captured at M3 arm WITHOUT the window (it was off), so it lacks the
            // StartWindowDraw mode-3 penalty. If the window will now actually start
            // this line (window-Y gate holds and the fetcher has not yet passed the
            // window-start x = max(0, WX-7)), the hardware next-mode-0 prediction re-runs
            // with the window included and the boundary moves WIN_M3_PENALTY dots
            // later. ADD that penalty to m0_time_master so the FF41 read resolves the
            // window-inclusive mode-3 end, instead of nulling and falling back to the
            // live no-window-at-arm pipeline (which lands the boundary too early).
            // Scoped to no-sprite lines (CGB and DMG alike) so the sprite-fetch
            // geometry is unchanged; sprite-bit toggles still null below.
            let win_enable_clean = (old_lcdc & spr_bits) == (value & spr_bits)
                && (old_lcdc & win_bit) == 0
                && (value & win_bit) != 0
                && self.sprites_on_line.is_empty();
            let mut win_enable_handled = false;
            if win_enable_clean {
                win_enable_handled = true;
                // Window-Y gate: the window can start this line iff WY has triggered
                // (`window_y_triggered`, set at the line-450/454 window-enable master checkpoints
                // when LY==WY). set_lcdc_visible has no mmio handle, so use the
                // cached arm-time geometry: m3_scheduled_wx (WX latched at M3 arm)
                // and the window-Y trigger latch.
                let wx = self.m0.m3_scheduled_wx as i32;
                // Window-Y gate, mirroring `window_y_active`: the window-enable master trigger
                // latch (`window_y_triggered`, set at the line-450/454 checkpoints)
                // OR the immediate `wy2 == LY` fallback. The latter is required on
                // the first line after enable (LY=0), where the previous line's
                // checkpoints never ran so `window_y_triggered` is still false even
                // when WY==0 — exactly the late_enable_ly0 case.
                let wy_ok = self.window_y_triggered || self.latch.wy2 == self.internal_ly_val;
                let wx_in_range = (0..=166).contains(&wx) && (cgb_features_enabled || wx != 166);
                // The window penalty applies iff the enable lands BEFORE the
                // fetcher reaches the window-tile commit dot. The window draws from
                // visible x == max(0, WX-7); x begins advancing `WARMUP + 8` dots
                // past the M3 arm (the first BG tile fill) plus the SCX fine-scroll
                // discard. The penalty commits one dot ahead of the first window
                // pixel reaching x (the `-1`), mirroring `predicted_win_start_dot`.
                // The late_enable_ly0_ds_{1,2} pair brackets this commit dot to a
                // single cycle: _1 (write 1 cycle earlier) takes the +6, _2 does not.
                let x_at_start = (wx - 7).max(0);
                let warmup = if cgb_features_enabled {
                    CGB_PIXEL_TRANSFER_WARMUP as i64
                } else {
                    DMG_PIXEL_TRANSFER_WARMUP as i64
                };
                // SCX==5 fine-scroll phase: the hardware mode-3-start dispatch runs the
                // window-tile fetch one dot later than the linear discard model at
                // this single phase (the same +1 the closed-form mode-3 length applies
                // at scx==5, compute_m3_length_win). For x==0 windows (WX<=7) the
                // commit dot is therefore one dot later; without it a window-enable on
                // the boundary dot wrongly drops the penalty (late_reenable_scx5_2),
                // while scx3 stays on the linear boundary (late_reenable_scx3_2).
                let win_fine = if wx <= 7 && (self.m3.m3_arm_scx & 7) == 5 { 1 } else { 0 };
                let commit_dot = self.m3.m3_arm_dot as i64
                    + warmup
                    + 8
                    + self.m3.m3_arm_scx as i64
                    + x_at_start as i64
                    + win_fine
                    - 1;
                let will_start = wy_ok && wx_in_range && (self.ticks as i64) < commit_dot;
                if will_start
                    && let Some(m0t) = self.m0.m0_time_master {
                        let pen = (WIN_M3_PENALTY as i64) << ds as i64;
                        self.m0.m0_time_master = Some((m0t as i64 + pen).max(0) as u64);
                    }
                // else: keep the no-window m0_time_master as captured at arm.
            }
            // A mid-mode-3 window-DISABLE toggle (not sprite) interacts with the
            // StartWindowDraw mode-3 penalty captured at M3 arm. Hardware locks
            // the penalty once the window has drawn for WIN_M3_PENALTY dots
            // (StartWindowDraw::inc spans those dots); a disable BEFORE that lock
            // refunds the whole window penalty, a disable after keeps it. The
            // read-at-cc mode-0 time captured at arm already includes the penalty, so:
            // - disable >= win_start_dot + WIN_M3_PENALTY: keep mode-0 time as-is.
            // - disable < win_start_dot + WIN_M3_PENALTY: subtract the penalty
            // (refund) so the FF41 read resolves the no-window boundary.
            // - window never started: null (fall back; live no-window path).
            // The live pipeline (scheduled_mode0_dot) is invalidated above either
            // way; only the read-at-cc mode-0 time is adjusted. Sprite-bit toggles
            // null mode-0 time (the sprite-fetch penalty genuinely changes).
            let only_win_toggle = (old_lcdc & spr_bits) == (value & spr_bits)
                && (old_lcdc & win_bit) != (value & win_bit)
                && (value & win_bit) == 0; // disable
            // GRADUATED StartWindowDraw refund: the window mode-3 penalty accrues
            // one dot per drawn window dot, capped at WIN_M3_PENALTY. A mid-M3
            // window-disable at dot `ticks` has accrued
            // accrued = min(WIN_M3_PENALTY, ticks - win_start)
            // dots; the unaccrued remainder is refunded from the read-at-cc
            // mode-0 time captured (full-penalty) at arm. This generalises the
            // refund/keep across SCX phase and WX (each phase shifts win_start
            // and mode-0 time together). Scoped CGB / no sprites / single speed; DS
            // keeps the calibrated binary lock below. The live pipeline
            // (scheduled_mode0_dot) is invalidated above regardless.
            // Single-speed window-disable handling for both CGB and DMG. The
            // StartWindowDraw mode-3 penalty is captured (full) at M3 arm in
            // m0_time_master. CGB refunds the not-yet-drawn window dots gradually;
            // DMG is binary (full keep once committed, else null) — see the two
            // branches below. The DMG late_disable cluster reads the STAT mode
            // after the disable and expects mode 3 to persist whenever the window
            // had already committed, which the binary keep provides; the prior
            // null-and-fall-back-to-live-no-window path reported mode 0 too early.
            let clean_ss = !ds && self.sprites_on_line.is_empty();
            let clean_ds = cgb_features_enabled
                && ds
                && self.sprites_on_line.is_empty();
            // On DMG the LCDC-write hook fires one PPU step before the
            // PixelTransfer code latches `win_start_dot`, so a disable landing
            // exactly on the window-start dot still sees
            // `window_started_this_line == false`. Bridge that one-step race with
            // the M3-arm prediction: the window is effectively started once the
            // current tick has reached the predicted start dot. The graduated
            // refund then uses the predicted dot as the start (drawn==0 at the
            // boundary -> full penalty kept).
            // CGB single-speed window-disable WITH a sprite on the line: the
            // window_started_this_line latch lags the closed-form StartWindowDraw
            // commit (it flips only when the visible window x is reached), so a
            // disable landing at/after the window-tile fetch commit still sees it
            // false and would wrongly null (mode 0). Bridge with the predicted
            // commit dot `m3_arm_dot + CGB_WARMUP + 8 + scx&7 + max(0, WX-7) - 1`
            // (mirroring the LCDC window-ENABLE commit), so the binary keep branch
            // below fires once the window has committed. The late_disable_spx10_wx0f
            // _{1,2} CGB reps bracket it (disable at dot 98 = before -> out0 via the
            // null below; dot 102 = at commit -> out3 keep).
            let cgb_spr_commit = if cgb_features_enabled
                && !ds
                && !self.sprites_on_line.is_empty()
                && self.m0.m3_scheduled_win
            {
                let x_at_start = (self.m0.m3_scheduled_wx as i64 - 7).max(0);
                Some(self.m3.m3_arm_dot as i64
                    + CGB_PIXEL_TRANSFER_WARMUP as i64
                    + 8
                    + (self.m3.m3_arm_scx & 7) as i64
                    + x_at_start
                    - 1)
            } else {
                None
            };
            let win_started_for_refund = self.window_started_this_line
                || (!cgb_features_enabled
                    && self
                        .predicted_win_start_dot
                        .is_some_and(|p| self.ticks >= p))
                || cgb_spr_commit.is_some_and(|c| (self.ticks as i64) >= c);
            // CGB keeps the graduated refund (predicted_win_start_dot is DMG-only,
            // so this is just win_start_dot on CGB); DMG uses the binary keep below.
            let refund_start_dot = self.win_start_dot.or(self.predicted_win_start_dot);
            if win_enable_handled {
                // The clean window-ENABLE adjusted m0_time_master above; skip the
                // disable-refund / null path (which would otherwise null it because
                // `only_win_toggle` is false for an enable).
            } else if !only_win_toggle || !win_started_for_refund {
                self.m0.m0_time_master = None;
            } else if !ds
                && !cgb_features_enabled
                && !self.sprites_on_line.is_empty()
                && win_started_for_refund
            {
                // DMG late window-disable WITH a sprite on the line (late_disable_spx10
                // cluster). The StartWindowDraw penalty is binary on DMG exactly as in
                // the no-sprite branch below; the sprite cost is already baked into the
                // M3-arm m0_time_master and is unaffected by the window toggle. Once the
                // window has committed (win_started_for_refund) the disable keeps the
                // full window-inclusive mode-0 time (mode 3 persists -> out3); a disable
                // before the commit took the `!win_started_for_refund` null path above
                // (no penalty -> mode 0 -> out0). The spx10_wx0f_{1,2} reps bracket this
                // boundary. Keep m0_time_master as captured (no-op).
            } else if !ds
                && cgb_features_enabled
                && !self.sprites_on_line.is_empty()
                && win_started_for_refund
            {
                // CGB single-speed late window-disable WITH a sprite on the line
                // (late_disable_spx10_wx0f_2). Binary like the DMG-sprite branch: the
                // sprite cost is baked into the M3-arm m0_time_master and the window
                // StartWindowDraw penalty locks once the fetcher fetches the window
                // tile. `win_started_for_refund` already gated the commit dot via
                // `cgb_spr_commit`, so reaching here means the disable landed at/after
                // the commit -> keep the full window-inclusive mode-0 time (mode 3 -> out3).
                // A disable before the commit took the `!win_started_for_refund` null
                // path above (-> mode 0 -> out0, the passing _1 rep). Keep (no-op).
            } else if clean_ss && !cgb_features_enabled {
                // DMG: the StartWindowDraw penalty is binary, not graduated. Once
                // the window has reached its commit dot (win_started_for_refund),
                // a mid-M3 window-disable keeps the FULL window-inclusive mode-0 time
                // (mode 3 persists through the read); a disable before the commit
                // dot already nulled above (no penalty -> mode 0). The
                // late_disable_* DMG cluster (out0 just-before vs out3 at/after)
                // brackets exactly this binary boundary; a graduated refund here
                // over-shortens the at/after cases at SCX>0 / higher WX. Keep the
                // window-inclusive m0_time_master as captured at M3 arm (no-op).
            } else if clean_ss {
                if let (Some(m0t), Some(ws)) = (self.m0.m0_time_master, refund_start_dot) {
                    // The StartWindowDraw penalty does not begin accruing until the
                    // fetcher reaches the window tile, which the SCX fine-scroll
                    // discard delays by `scx&7` dots past `win_start_dot`. Without
                    // this shift the accrual is `scx&7` dots early, so a disable in
                    // the `scx&7` dots just after win_start over-accrues (refund
                    // truncated) — the late_disable_scx{2,3,5}_1 CGB cluster reads
                    // mode 3 (out3) where the hardware's later lock still refunds to
                    // mode 0 (out0). Shifting the reference by scx&7 lands all phases
                    // (scx0 unchanged; scx5_1 at the same dot as scx0_2 now refunds).
                    // The StartWindowDraw penalty does not begin accruing until the
                    // fetcher reaches the window tile. For a window that starts at
                    // x==0 (WX<=7), `win_start_dot` is latched at the start of the
                    // x==0 region — BEFORE the SCX fine-scroll discard (which still
                    // consumes scx&7 dots). So the accrual reference is scx&7 dots
                    // early, and a disable in those dots over-accrues (refund
                    // truncated): the late_disable_scx{2,3,5}_1 CGB reps read mode 3
                    // (out3) where the hardware's later lock still refunds to mode 0
                    // (out0). Shift the reference by scx&7 for x==0 windows only.
                    // For WX>7 the window starts AFTER the discard, so `win_start_dot`
                    // already reflects post-discard time (no shift — the scx03_wx1x
                    // reps keep their out3 boundary).
                    let win_fine = if self.m0.m3_scheduled_wx <= 7 {
                        (self.m3.m3_arm_scx & 7) as i64
                    } else {
                        0
                    };
                    let drawn = (self.ticks as i64) - ws as i64 - win_fine;
                    let accrued = drawn.clamp(0, WIN_M3_PENALTY as i64);
                    let refund = WIN_M3_PENALTY as i64 - accrued;
                    self.m0.m0_time_master = Some((m0t as i64 - refund).max(0) as u64);
                } else {
                    self.m0.m0_time_master = None;
                }
            } else if clean_ds {
                if let (Some(m0t), Some(ws)) = (self.m0.m0_time_master, self.win_start_dot) {
                    // GRADUATED refund (as in the single-speed branch): the window
                    // penalty accrues one dot per drawn window dot, capped at
                    // WIN_M3_PENALTY; the unaccrued remainder is refunded. At double
                    // speed each dot is 2 cc. (Was a binary full-or-none refund,
                    // which over-refunded an early disable by the 2 already-drawn
                    // window dots -> the late_disable_early_*_ds reads flipped.)
                    // SCX fine-scroll shift for x==0 windows (WX<=7), same as the
                    // single-speed branch: win_start_dot is latched before the scx&7
                    // discard completes, so the accrual reference is scx&7 dots early.
                    // Generalising the former `m3_arm_scx&7==0` gate to all phases
                    // covers the late_disable_scx5_ds_1 CGB rep.
                    let win_fine = if self.m0.m3_scheduled_wx <= 7 {
                        (self.m3.m3_arm_scx & 7) as i64
                    } else {
                        0
                    };
                    let drawn = (self.ticks as i64) - ws as i64 - win_fine;
                    let accrued = drawn.clamp(0, WIN_M3_PENALTY as i64);
                    let refund = (WIN_M3_PENALTY as i64 - accrued) << 1;
                    self.m0.m0_time_master = Some((m0t as i64 - refund).max(0) as u64);
                } else {
                    self.m0.m0_time_master = None;
                }
            } else {
                self.m0.m0_time_master = None;
            }
        }
        // On an LCDC write: a WE (window-enable) toggle with
        // the LCD already on updates the window-draw state. rustyboi splits the hardware
        // 2-bit window-draw state into `win_draw_start` (bit win_draw_start) and
        // `win_draw_started` (bit win_draw_started); reproduce the exact bit
        // arithmetic here. `xpos == xpos_end` (the line's pixel transfer is
        // done) holds whenever we are not actively in PixelTransfer, or x has
        // reached the line end inside it.
        if (old_lcdc & display_enable) != 0 && (old_lcdc & win_bit) != (value & win_bit) {
            let at_line_end = !matches!(self.state, State::PixelTransfer) || self.x >= 160;
            if (value & win_bit) == 0 {
                // WE-off: clear win_draw_started iff the window-draw state == win_draw_started
                // (started but not armed) OR the line is finished. win_draw_start
                // (the arm bit) survives, so a re-enable can resume next line.
                if (self.win_draw_started && !self.win_draw_start) || at_line_end {
                    self.win_draw_started = false;
                    // If the fetcher is actively drawing the window mid-line, the
                    // window stops here and the next tile fetch reverts to BG
                    // (the hardware window-tile fetch gates on `window-draw-state & win_draw_started`).
                    if self.fetcher.is_fetching_window() {
                        // The hardware tile-fetch f0 stage commits each window
                        // tile's window-vs-BG choice against the fetch grid's
                        // DISPLAY-COLUMN counter (`xpos`), with the WE bit
                        // sampled one dot late — not against the write's dot
                        // directly. The BG fetch grid reaches display column C
                        // at `bg_anchor_dot + 8 + C` (its first TileNumber
                        // leads its own column by 8), so a WE-off written on
                        // dot D keeps every window tile whose column satisfies
                        //     bg_anchor_dot + 8 + C <= D + 1,
                        // i.e. C <= D - bg_anchor_dot - 7, and reverts to BG
                        // from the first window-tile column past it.
                        //
                        // The columns are the window's own grid: the fetcher is
                        // currently filling column `x + fifo_size` (what the
                        // renderer has drawn plus what is still queued), and
                        // the following window tiles sit every 8 columns after
                        // it. `stop_window_with_extra` counts TileNumber steps,
                        // and the in-flight tile only has one left to run when
                        // it has not reached TileNumber yet, hence the substep
                        // correction.
                        //
                        // Not in Pan Docs, TCAGBD or GBCTR: fitted to
                        // SameBoy CGB-C/CGB-E over the window_bg_reprise probe
                        // family swept over both the window column (WX 8..25)
                        // and the write dot (the WE-off store moved through the
                        // line in 4-dot steps), 38/38 exact.
                        //
                        // Scoped to CGB: the hardware mid-tile boundary
                        // completion for a WE-off lives in StartWindowDraw::inc
                        // behind an explicit `&& p.cgb` gate. On DMG the revert
                        // is NOT latched at the write at all: the fetcher
                        // re-samples the WE bit at each TileNumber step (the
                        // tile-number fetch-step kill, see we_dot_hist) — a
                        // pulse that misses every TileNumber leaves the window
                        // running.
                        if cgb_features_enabled {
                            let extra = self.cgb_weoff_extra_tiles();
                            self.fetcher.stop_window_with_extra(extra);
                            self.window_started_this_line = false;
                            self.win_weoff_deferred_tail = true;
                        } else if at_line_end {
                            // DMG at line end (the wxA6 xpos-166 dance): no
                            // TileNumber will run again this line, so the
                            // deferred kill cannot land; stop immediately as
                            // The hardware window-draw-state clear does.
                            self.fetcher.stop_window_with_extra(0);
                            self.window_started_this_line = false;
                        }
                    }
                }
            } else {
                // WE-on: if the window-draw state == win_draw_start (armed but not started),
                // promote to started and advance the window Y line.
                if self.win_draw_start && !self.win_draw_started {
                    self.win_draw_started = true;
                    self.win_y_pos = self.win_y_pos.wrapping_add(1);
                }
            }
        }
        self.lcdc.reg = value;
        // An LCDC store re-runs the scheduled window-Y comparison the same way
        // a WY store does, so a window-enable that is only briefly set while
        // WY == LY still arms the window for the rest of the frame.
        let cc = self.write_cc(ds);
        self.arm_wy_recheck(cc, ds);
        self.stat_sched_touched();
    }

    /// How many further window TileNumber steps a mid-line CGB WE-off write
    /// leaves armed (`stop_window_with_extra`'s argument).
    ///
    /// The revert lands at the first window-tile column the fetch grid has not
    /// reached by one dot after the write; see the call site for the grid
    /// arithmetic. Falls back to the pre-grid boundary heuristic on a line with
    /// no BG fetch-grid anchor (a window that took over before the line's first
    /// BG TileNumber), which is the only case the anchor cannot describe.
    fn cgb_weoff_extra_tiles(&self) -> u8 {
        let substep_ran_tile_number = self.fetcher.fetch_substep() != 0;
        let Some(anchor) = self.wg.bg_anchor_dot else {
            let wxs = self.fetcher.window_x_start_dbg() as i32;
            let phase = (self.x as i32 + 2 - 2 * wxs).rem_euclid(8);
            return if phase == 0 { 0 } else { 1 };
        };
        // Last display column the grid resolves as window (see call site).
        let last_col = self.ticks as i64 - anchor as i64 - 7;
        // The column the fetcher is filling right now: drawn + still queued.
        let cur_col = self.x as i64 + self.fetcher.pixel_fifo.size() as i64;
        // Window tiles left, counting the in-flight one: cur_col, +8, +16, ...
        let left = if cur_col > last_col {
            0
        } else {
            ((last_col - cur_col) / 8 + 1) as u8
        };
        // The in-flight tile has already consumed its TileNumber unless the
        // fetcher is still sitting on it.
        left.saturating_sub(u8::from(substep_ran_tile_number))
    }
}
