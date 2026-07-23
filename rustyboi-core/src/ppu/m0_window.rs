use crate::ppu::stat_irq;
use super::controller::Ppu;

impl Ppu {
    /// Cycle-exact HDMA-eligibility predicate, mirroring the hardware
    /// HDMA-eligibility period: a visible line, the within-line dot is at or
    /// past the predicted mode-0 (HBlank) start, and there is still room before
    /// line end to run a block (`dot + 3 + 3*ds < line-end`). Returns None when
    /// no closed-form mode-0 dot is available (window/first line after enable),
    /// so callers can fall back to the STAT mode-edge model. Read-only.
    pub(crate) fn hdma_period(&self, double_speed: bool) -> Option<bool> {
        if self.disabled {
            return None;
        }
        let m0 = self.m0.scheduled_mode0_dot? as i128;
        let ly = self.clk.internal_ly_val;
        if ly >= 144 {
            return Some(false);
        }
        let ds = double_speed as i128;
        let dot = self.ticks as i128;
        // Hardware gates HDMA on `cc >= mode-0 time` but its eligibility call site
        // passes `cc + 4`; the +1 dot here aligns the renderer
        // tick with that access cc. Net +1 on the dma suite, no regressions.
        let m0n = m0 + self.dma_scx_m0_nudge(double_speed, false) as i128;
        Some(dot > m0n && dot + 3 + 3 * ds < 456)
    }

    #[allow(dead_code)] // no in-tree caller; `pub` was masking dead_code. Unwired-peripheral and
    // unfinished-feature code lives here — check the feature roadmap before deleting.
    /// DEFERRED-HDMA-FIRE late-HBlank predicate for the FF55-kick / unhalt
    /// resolution paths only (NOT the per-dot edge machine). Mirrors the hardware
    /// `the HDMA-enable path` -> `the HDMA-active check at cc+4` where `the current line's mode-0 (HBlank) time` returns
    /// the CURRENT line's mode-0 time (the last mode-0 time) even after the renderer has
    /// crossed it — so a FF55 enable written mid-HBlank, after mode-0 entry but
    /// still on the same line, resolves IN-PERIOD and arms its block immediately
    /// (`hdma_late_enable_*`). rustyboi previously nulled `scheduled_mode0_dot` at
    /// the mode-0 time crossing, returning None there, dropping those late enables.
    ///
    /// Anchored on `m0_time_master` (master cc, shares the access cc's phase, so it
    /// is robust to the STOP/lcd-offset line-phase residual that a renderer-dot
    /// test is not): a visible line, the access cc at/past the mode-0 start, and
    /// not so deep into mode-0 that the next line is imminent. Threshold per speed
    /// brackets the late-enable pairs (SS: arm `cc-m0t` 191/188, drop 195/192 ->
    /// `< 192`; DS: arm 394/391, drop 398/395 -> `< 395`). Returns None when no
    /// closed-form mode-0 anchor exists (window / first line / mid-M3 invalidation)
    /// so the caller falls back to the STAT-mode gate.
    /// COORDINATED piece #3 (HDMA-halt deferred held-flag): the unhalt re-flag
    /// gate's `the HDMA-active check at cc` at the unhalt access cc. Same closed-form mode-0
    /// anchor as `hdma_period_kick`, but the END (drop) bracket sits later: the
    /// unhalt-reflag boundary the `hdma_late_m0unhalt_{1,2}` straddle pairs probe
    /// is past the FF55-enable kick boundary (cctracer: SS depth 196 reflags /
    /// 200 does not; DS 398 reflags / 402 does not), so it carries its own limit.
    /// Returns None when no closed-form mode-0 anchor exists (caller falls back).
    pub(crate) fn hdma_period_unhalt(&self, access_cc: u64, double_speed: bool) -> Option<bool> {
        self.hdma_period_unhalt_adj(access_cc, double_speed, 0)
    }

    /// This line's closed-form mode-0 (HBlank) start in master cc, or None when no
    /// closed-form anchor exists (window / first line after enable). Used by the
    /// HALT-entry HDMA capture to derive a per-period "block already served" signal
    /// (the live `hdma_block_done_this_period` flag is reset too early by the per-dot
    /// period falling edge — see `Mmio::on_cpu_halt_with_period_done`).
    pub(crate) fn m0_time_master_cc(&self) -> Option<u64> {
        self.m0.m0_time_master
    }

    /// As `hdma_period_unhalt`, with the line-END (drop) bracket widened by
    /// `limit_adj` dots (the EI fast-dispatch ISR-phase compensation; see
    /// `Bus::hdma_in_period_for_unhalt_adj`). The compensation widens the END
    /// bracket ONLY — the START bracket (`cc >= m0t`, mode-0 entry) is left
    /// untouched, because the EI-fast ISR-phase shift inflates the unhalt-period
    /// DEPTH (`cc - m0t`) uniformly by 4: a Low-at-halt block deep in mode-0 (near
    /// the line end) must still reflag (depth 200 -> in), while a block at the
    /// mode-0 ENTRY (depth ~0, `hdma_ei_m3halt_m0unhalt_ly_*`) must still reflag
    /// too (hardware reflags) — which a m0t shift would wrongly push past the
    /// start bracket. `limit_adj == 0` is byte-identical to the calibrated
    /// baseline.
    pub(crate) fn hdma_period_unhalt_adj(
        &self,
        access_cc: u64,
        double_speed: bool,
        limit_adj: i64,
    ) -> Option<bool> {
        if self.disabled {
            return None;
        }
        if self.clk.internal_ly_val >= 144 {
            return Some(false);
        }
        let m0t = self.m0.m0_time_master? as i64;
        let cc = access_cc as i64;
        if cc < m0t {
            return Some(false);
        }
        let depth = cc - m0t;
        let limit: i64 = (if double_speed { 400 } else { 198 }) + limit_adj;
        Some(depth < limit)
    }

    /// HALT-ENTRY `the HDMA-active check at cc` for `halt-HDMA-state` (the hardware HALT handling).
    /// Same `m0_time_master`-anchored closed-form predicate as `hdma_period_unhalt`,
    /// but the line-end (drop) bracket sits a few cc LATER: the HALT instruction's
    /// access cc reaches the `cc + 3 + 3*ds < line-end` boundary at a different phase
    /// than the unhalt access cc, so the `hdma_late_m0halt_{1,2}` straddle pair
    /// (cctracer: HALT cc 4cc apart, period 1->0) bracket their own limit. Probed
    /// per speed via the `_1` (in-period -> High -> 1 block) / `_2` (past-boundary
    /// -> Low -> reflag -> 2 blocks) pairs: SS depth 206/204 in, 210/208 out -> 208;
    /// DS depth 408/407 in, 412/411 out -> 410. Returns None when no closed-form
    /// mode-0 anchor exists (caller falls back to the cached per-step period).
    pub(crate) fn hdma_period_halt(&self, access_cc: u64, double_speed: bool) -> Option<bool> {
        if self.disabled {
            return None;
        }
        if self.clk.internal_ly_val >= 144 {
            return Some(false);
        }
        let m0t = self.m0.m0_time_master? as i64;
        let cc = access_cc as i64;
        if cc < m0t {
            return Some(false);
        }
        let depth = cc - m0t;
        let limit: i64 = if double_speed { 410 } else { 208 };
        Some(depth < limit)
    }

    /// Late-hdma-vs-interrupt unhalt precedence. On unhalt
    /// with a Low-at-halt HDMA block, the hardware unhalt interrupt event flags the block
    /// iff `the HDMA-active check at cc` (`cc >= mode-0 time`) at the unhalt cc. rustyboi's
    /// `m0_time_master` folds a +1 dot phase vs the raw mode-0 time, so the equivalent
    /// START boundary here is `cc + 1 >= m0t`. When TRUE the
    /// block's dma event is flagged (event time 0) and FIRES IMMEDIATELY at unhalt,
    /// i.e. BEFORE the interrupt's PC pushes — the dma-wins races
    /// (`late_hdma_vs_tima_*_halt_1`, copy the pre-push 0x1234). When FALSE the
    /// block is NOT yet in period at unhalt; its m0-edge falls during/after the
    /// interrupt service, so the block fires AFTER the pushes and copies the pushed
    /// return address (`*_halt_2`, 0x11C9). This predicate reports the former (fire
    /// AT unhalt / before pushes) decision so the service can suppress+reorder the
    /// latter. Anchored on `m0_time_master` (shares the access cc phase). None when
    /// no closed-form mode-0 anchor exists (caller keeps the synchronous fire).
    pub(crate) fn hdma_unhalt_fires_before_pushes(
        &self,
        access_cc: u64,
        double_speed: bool,
    ) -> Option<bool> {
        if self.disabled {
            return None;
        }
        if self.clk.internal_ly_val >= 144 {
            return Some(false);
        }
        let m0t = self.m0.m0_time_master? as i64;
        let cc = access_cc as i64;
        // REFLAG (fire-at-unhalt / before pushes) iff the unhalt access cc has
        // reached mode-0 start AND is not past the line-end. The START anchor is
        // `cc + 1 >= m0t` — the SAME +1 dot phase the per-dot `hdma_period`
        // predicate folds (`dot >= m0n + 1`); a bare `cc >= m0t` or the looser
        // `cc + 4` mis-brackets the scx-shifted mode-0 time. cctracer boundary at unhalt
        // cc=C: REFLAG for m0t<=C+1 (`scx{1,2}_halt_1`), NOREFLAG for m0t>=C+2
        // (`scx{1,2}_halt_2`).
        let in_start = cc + 1 >= m0t;
        let in_end = (cc - m0t) < (if double_speed { 400 } else { 198 });
        Some(in_start && in_end)
    }

    pub(crate) fn hdma_period_kick(&self, access_cc: u64, double_speed: bool) -> Option<bool> {
        if self.disabled {
            return None;
        }
        if self.clk.internal_ly_val >= 144 {
            return Some(false);
        }
        let m0t = self.m0.m0_time_master? as i64;
        let cc = access_cc as i64;
        // Line-identity staleness guard (same as `hdma_disable_fires`):
        // `m0_time_master` is rebased at the mode-3 arm, so during the NEXT
        // line's mode 2 (and early mode 3) it still holds the PREVIOUS line's
        // m0 time. An FF55 enable written there is a mode-2/3 arm, not a
        // late-in-HBlank same-line arm — hardware schedules it to the coming
        // m0 edge with no immediate block. Without this, a window-active line
        // (closed-form `hdma_period` = None, so only this predicate gates the
        // kick) whose mode-3 runs long enough that the next line's mode-2 arm
        // write lands < `limit` past the STALE anchor fires a spurious 37th
        // block — Pokémon Crystal's 37-block HBlank tilemap transfer then
        // completes a line early, so its readback-and-rewrite cancel
        // (`ld a,[rHDMA5] / and $7f / ldh [rHDMA5],a`) sees 0xFF instead of
        // 0x00 and becomes a 2KB GDMA over the displayed 9C00 freeze-frame
        // map (Elm's-lab textbox corruption).
        if m0t < self.line_start_master_cc(double_speed) {
            return Some(false);
        }
        // Start: in-period once the access cc reaches the mode-0 time. (the hardware
        // `cc + 4 >= mode-0 time`; the renderer-tick mode-0 time already folds the +4 phase
        // for the dma cluster, so a bare `cc >= m0t` brackets the enable pairs.)
        if cc < m0t {
            return Some(false);
        }
        // End: drop once the access cc is within `~12 master cc` of the next line
        // (i.e. too deep into mode-0). Empirical per-speed bracket on `cc - m0t`.
        let depth = cc - m0t;
        let limit: i64 = if double_speed { 395 } else { 192 };
        Some(depth < limit)
    }

    /// The shared LY-time gate phase: the DS->SS speed-change bridge drops the
    /// `+1` the LY counter correction carries, and every consumer of an LY time
    /// must sample the same phase.
    #[inline]
    pub(in crate::ppu) fn ly_plus1(&self) -> i64 {
        if self.speed.lytime_no_plus1 { 0 } else { 1 }
    }

    /// The LY time in master cc, anchored on `abs_cc` plus the dots remaining in
    /// the current line.
    ///
    /// NOTE: this is NOT interchangeable with the `p_now + ly_counter(mmio).time`
    /// anchor used by `m0_time_exact` / `cgbp_begin_exact`. Both name "the LY
    /// time", but they reach it by different routes — this one from `abs_cc` and
    /// the live `line_cycle`, the other by reading the LY counter through mmio —
    /// and only the latter is enable-anchored. They are left as two formulas
    /// deliberately; collapsing them would be a semantic bet, not code motion.
    ///
    /// The `LCD_CYCLES_PER_LINE - self.clk.line_cycle` subtraction is u32 and is kept
    /// verbatim: it is the original's arithmetic, including its debug-overflow
    /// behaviour if `line_cycle` were ever to exceed the line length.
    #[inline(always)]
    fn ly_time_master(&self, ds: i64) -> i64 {
        let plus1 = self.ly_plus1();
        let dots_to_next = (stat_irq::LCD_CYCLES_PER_LINE - self.clk.line_cycle) as i64;
        self.clk.p_now as i64 + self.clk.abs_cc as i64 + (dots_to_next << ds) + plus1
    }

    /// The hardware `line cycles(cc) = 456 - ((the LY time - cc) >> ds)`.
    #[inline(always)]
    pub(in crate::ppu) fn line_cycles_at(&self, cc: i64, ds: i64) -> i64 {
        stat_irq::LCD_CYCLES_PER_LINE as i64 - ((self.ly_time_master(ds) - cc) >> ds)
    }

    /// The current line's start in master cc (the LY time anchor rebased one
    /// line back) — the line-identity reference `hdma_disable_fires` and
    /// `hdma_period_kick` use to reject a stale previous-line `m0_time_master`.
    fn line_start_master_cc(&self, double_speed: bool) -> i64 {
        let dsi = double_speed as i64;
        self.ly_time_master(dsi) - ((stat_irq::LCD_CYCLES_PER_LINE as i64) << dsi)
    }

    /// FF55=00 HDMA-DISABLE-vs-m0-edge race (the hardware HDMA-disable path): writing
    /// FF55 bit7=0 only clears the FUTURE `memevent_hdma` schedule; it does NOT
    /// un-flag a block whose m0-edge has ALREADY fired (`intevent_dma` is latched
    /// and `dma()` will still run). So a late disable cannot stop a block once the
    /// current line's mode-0 edge has passed. The boundary is exactly the m0-edge
    /// time: the hardware processes the HDMA memory event (which raises the HDMA request)
    /// before the FF55 write whenever the write cc has reached `mode-0 time`.
    /// Returns `Some(true)` when the disable is too late (the m0 edge already
    /// flagged -> the block must still fire), `Some(false)` when the disable wins
    /// (cancel before the edge), or `None` when no closed-form mode-0 anchor exists
    /// (caller falls back to the unconditional cancel).
    /// Boundary is the hardware exact m0-edge time (`the current line's mode-0 (HBlank) time` =
    /// the predicted next mode-0 time): the disable fires the block iff `disable_cc >=
    /// mode-0 time`. rustyboi's `m0_time_master` is the STAT-read anchor (calibrated for
    /// `abs_cc + 2 < mode-0 time` with the LY counter `+1` and renderer-tick phase), and
    /// it runs a fixed few cc ABOVE the hardware bare m0-edge time: cctracer pins the
    /// gap at +6 (single speed) / +4 (double speed), constant across SCX (the SCX
    /// m3-length delta already lives in `m0_time_master`). So the true edge is
    /// `m0_time_master - gap`.
    ///
    /// cctracer ground truth (CGB, [_1 cancel -> out0 / _2 fire -> out1] pairs,
    /// rustyboi-clock disable cc vs m0_time_master):
    /// SS base _1=12935 _2=12939 m0t=12944 edge=12938 (m0t-6)
    /// SS scx2 _1=12939 _2=12943 m0t=12946 edge=12940 (m0t-6)
    /// SS scx5 _1=12939 _2=12943 m0t=12949 edge=12943 (m0t-6)
    /// DS _1=158392 _2=158396 m0t=158398 edge=158394 (m0t-4)
    /// DS scx5 _1=158400 _2=158404 m0t=158408 edge=158404 (m0t-4)
    pub(crate) fn hdma_disable_fires(&self, access_cc: u64, double_speed: bool) -> Option<bool> {
        if self.disabled {
            return None;
        }
        if self.clk.internal_ly_val >= 144 {
            return Some(false);
        }
        let m0t = self.m0.m0_time_master? as i64;
        let gap: i64 = if double_speed { 4 } else { 6 };
        let edge = m0t - gap;
        let cc = access_cc as i64;
        // Staleness bound: `m0_time_master` is rebased at the mode-3 arm, so
        // during the NEXT line's mode 2 (and early mode 3) it still holds the
        // PREVIOUS line's m0 time. A disable write there is hundreds of cc past
        // that edge whose block long ran - the next edge is unscheduled and the
        // disable must win (AntonioND hdma_start_3: FF55=00 at LY3 mode 2 with
        // one block left reads HDMA5=0x80). The genuine race only exists within
        // a write-resolution beat of the edge (the latched block stalls the CPU
        // immediately after), so a small window past m0t keeps every
        // edge-racing bracket while rejecting stale-line reads.
        // Staleness guard: `m0_time_master` is rebased at the mode-3 arm, so
        // during the NEXT line's mode 2 (and early mode 3) it still holds the
        // PREVIOUS line's m0 time. A disable write there is far past an edge
        // whose block long ran - the next edge is unscheduled and the disable
        // must win (AntonioND hdma_start_3: FF55=00 at LY3 mode 2 with one
        // block left reads HDMA5=0x80). Detect it by line identity: an m0t
        // before the current line's start cc (the LY time anchor, same phase
        // `vram_readable_at_cc` uses) belongs to a completed line. Same-line
        // late writes (incl. the STOP-speedchange wakeup family, whose owed
        // block resolves ~129cc past m0t) keep the edge-fired answer.
        if m0t < self.line_start_master_cc(double_speed) {
            return Some(false);
        }
        Some(cc >= edge)
    }

    /// The HDMA m0 (mode-3->0) trigger edge cc for the current line — the same
    /// `m0_time_master - gap` boundary `hdma_disable_fires` compares against,
    /// returned as a value. The STOP path uses it to measure how far before the
    /// stop the block's edge was crossed (deciding the halted-vs-completing FF55
    /// readback for `hdma_late_m3speedchange_hdma5_scx*_2` vs `_3`).
    pub(crate) fn hdma_m0_edge(&self, double_speed: bool) -> Option<i64> {
        let m0t = self.m0.m0_time_master? as i64;
        let gap: i64 = if double_speed { 4 } else { 6 };
        Some(m0t - gap)
    }

    /// SCX-phase-conditioned nudge to the mode-0 boundary dot used by the
    /// HDMA/VRAM-lock predictors (NOT the m0 STAT IRQ, which is calibrated
    /// separately). The closed-form `compute_m3_length` prefix `scx + (1-cgb)`
    /// is a dot-count model; at some SCX phases the hardware mode-3-start fine-scroll
    /// dispatch lands the actual HBlank one renderer dot off from that linear
    /// model, and that boundary feeds the HDMA trigger / VRAM-unlock the dma
    /// suite measures. Env-overridable, gated per SCX&7 phase and per speed so
    /// it cannot touch co-calibrated clusters at other phases.
    fn dma_scx_m0_nudge(&self, _double_speed: bool, vram: bool) -> i64 {
        let scx = self.m3.m3_arm_scx & 0x07;
        // Two surgical, phase-scoped boundary nudges, each a clean -1 on the dma
        // cluster with zero regressions across the co-calibrated clusters
        // (window / scx_during_m3 / cgbpal_m3 / enable_display / scy / oamdma):
        //
        // * HDMA-trigger boundary, SCX&7==1 (vram=false): the hardware mode-3-start
        // fine-scroll dispatch lands the actual HBlank one renderer dot before
        // the linear `scx + (1-cgb)` prefix model implies, so the HDMA block at
        // this phase arms one dot early in our model; -1 realigns it. Only the
        // HDMA consumer (dma cluster) sees this; VRAM-lock is untouched here.
        //
        // * VRAM-lock end boundary, SCX&7==3 (vram=true): at this phase the
        // cycle-exact mode-3->0 unblock the dma reads probe sits one dot late
        // vs hardware; -1 realigns it. Verified to fix 1 dma with no regression
        // in any co-calibrated VRAM/OAM/cgbpal-access test.
        //
        // SCX&7==0 was -2 on dma-only but regresses two window m2int_wxA6
        // busyread tests, so it is deliberately left unbiased (default 0).
        match (vram, scx) {
            (false, 1) => -1,
            (true, 3) => -1,
            _ => 0,
        }
    }
}
