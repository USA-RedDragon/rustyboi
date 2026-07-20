//! Optional, off-by-default "dirty line" observer for the scanline-renderer
//! feasibility study.
//!
//! A scanline (fast) renderer draws each visible line ONCE from the register
//! state latched at some point in the line, so it necessarily MISSES any write
//! to a render-affecting PPU register that lands DURING that line's mode-3
//! (pixel transfer) window. This probe counts exactly those events.
//!
//! It is a pure OBSERVER: it only reads state the bus already computed (the
//! PPU mode + LY at the write's issue cc) and increments counters. It NEVER
//! mutates emulation state. When a `Ppu` has no probe attached (the default,
//! `None`), the register-write path skips the `if let Some(probe)` block
//! entirely and emulation is byte-identical — this is what every test suite
//! run relies on.
//!
//! Detection mechanism (a): hook the register-write path and, at the write's
//! issue cc (before the M-cycle ticks), check whether the PPU is in mode 3 of
//! a visible line. This catches transient writes (a value written and restored
//! within the same mode-3 window still marks the line dirty), which a snapshot
//! at mode-3 entry/exit (mechanism (b)) would miss. Tradeoff: a write whose
//! net effect is nil (write X then write back the old value) is still counted
//! dirty — correct for the scanline-renderer question, since a single-latch
//! renderer would still render the wrong intermediate value if it happened to
//! latch between the two writes; but it means the count is an over-count of
//! "lines whose FINAL state differs", which is the conservative (pessimistic
//! for the fast path) direction, i.e. it will not overstate fast-path coverage.

/// The render-affecting registers we watch. CGB palette writes (BCPD/OCPD) are
/// tracked separately because on real hardware a mid-mode-3 write to them is
/// BLOCKED by the PPU (the palette bus is owned during pixel transfer) and thus
/// dropped — it does NOT affect the current line. We record the ATTEMPT so the
/// report can distinguish "tried to write mid-m3 (dropped, harmless to a fast
/// renderer)" from an effective mid-m3 write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchedReg {
    Lcdc,
    Scy,
    Scx,
    Bgp,
    Obp0,
    Obp1,
    Wx,
    /// CGB BCPD ($FF69) — background palette data.
    Bcpd,
    /// CGB OCPD ($FF6B) — object palette data.
    Ocpd,
}

impl WatchedReg {
    /// Map a bus address to a watched register, or `None` if not watched.
    #[inline]
    pub fn from_addr(addr: u16) -> Option<WatchedReg> {
        Some(match addr {
            0xFF40 => WatchedReg::Lcdc,
            0xFF42 => WatchedReg::Scy,
            0xFF43 => WatchedReg::Scx,
            0xFF47 => WatchedReg::Bgp,
            0xFF48 => WatchedReg::Obp0,
            0xFF49 => WatchedReg::Obp1,
            0xFF4B => WatchedReg::Wx,
            0xFF69 => WatchedReg::Bcpd,
            0xFF6B => WatchedReg::Ocpd,
            _ => return None,
        })
    }

    pub const ALL: [WatchedReg; 9] = [
        WatchedReg::Lcdc,
        WatchedReg::Scy,
        WatchedReg::Scx,
        WatchedReg::Bgp,
        WatchedReg::Obp0,
        WatchedReg::Obp1,
        WatchedReg::Wx,
        WatchedReg::Bcpd,
        WatchedReg::Ocpd,
    ];

    fn idx(self) -> usize {
        match self {
            WatchedReg::Lcdc => 0,
            WatchedReg::Scy => 1,
            WatchedReg::Scx => 2,
            WatchedReg::Bgp => 3,
            WatchedReg::Obp0 => 4,
            WatchedReg::Obp1 => 5,
            WatchedReg::Wx => 6,
            WatchedReg::Bcpd => 7,
            WatchedReg::Ocpd => 8,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            WatchedReg::Lcdc => "LCDC",
            WatchedReg::Scy => "SCY",
            WatchedReg::Scx => "SCX",
            WatchedReg::Bgp => "BGP",
            WatchedReg::Obp0 => "OBP0",
            WatchedReg::Obp1 => "OBP1",
            WatchedReg::Wx => "WX",
            WatchedReg::Bcpd => "BCPD",
            WatchedReg::Ocpd => "OCPD",
        }
    }
}

/// Per-frame + cumulative dirty-line counters. Attach to a `Ppu` via
/// `Ppu::attach_dirty_probe`; read back with `Ppu::dirty_probe`.
#[derive(Debug, Clone)]
pub struct DirtyLineProbe {
    /// Bitmap of visible lines (LY 0..=143) marked dirty in the CURRENT frame.
    /// Reset at each frame boundary via `end_frame`.
    line_dirty: [bool; 144],
    /// Per-register: was this register the cause of a dirty mark on the given
    /// line THIS frame? (Used so per-register dirty-line counts don't
    /// double-count multiple writes to the same reg on the same line.)
    line_reg_dirty: [[bool; 9]; 144],

    // ---- cumulative across all frames processed ----
    /// Total visible lines the PPU actually rendered (LCD on, mode-3 reached).
    /// Counted at frame end from `line_seen`.
    pub total_visible_lines: u64,
    /// Total dirty visible lines (>=1 watched effective write during mode 3).
    pub total_dirty_lines: u64,
    /// Per-register cumulative dirty-line counts (a line counts once per reg).
    pub per_reg_dirty_lines: [u64; 9],
    /// Per-register cumulative count of RAW mid-mode-3 write events (every
    /// write, including repeats on the same line).
    pub per_reg_events: [u64; 9],
    /// CGB palette (BCPD/OCPD) mid-mode-3 writes that were BLOCKED/dropped by
    /// the PPU (do NOT affect the current line — a fast renderer gets these
    /// right). Recorded but excluded from `line_dirty`.
    pub palette_blocked_events: u64,
    /// Frames observed (each `end_frame` call).
    pub frames: u64,

    /// Which visible lines were actually rendered this frame (mode-3 reached).
    line_seen: [bool; 144],
}

impl Default for DirtyLineProbe {
    fn default() -> Self {
        DirtyLineProbe {
            line_dirty: [false; 144],
            line_reg_dirty: [[false; 9]; 144],
            total_visible_lines: 0,
            total_dirty_lines: 0,
            per_reg_dirty_lines: [0; 9],
            per_reg_events: [0; 9],
            palette_blocked_events: 0,
            frames: 0,
            line_seen: [false; 144],
        }
    }
}

impl DirtyLineProbe {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that visible line `ly` reached mode 3 (was rendered) this frame.
    /// Called from the mode-3-entry hook. Cheap; only sets a bool.
    #[inline]
    pub fn mark_line_rendered(&mut self, ly: u8) {
        if (ly as usize) < 144 {
            self.line_seen[ly as usize] = true;
        }
    }

    /// Record a watched-register write. `mode3_visible` is true iff the write
    /// lands while the PPU is in mode 3 of a visible line (LY 0..=143).
    /// `blocked` is true for a CGB palette write the PPU dropped (mid-mode-3
    /// BCPD/OCPD): counted separately, does NOT dirty the line.
    #[inline]
    pub fn record_write(&mut self, reg: WatchedReg, ly: u8, mode3_visible: bool, blocked: bool) {
        if !mode3_visible {
            return;
        }
        // A write during mode 3 means the line was rendered.
        self.mark_line_rendered(ly);
        let ri = reg.idx();
        if blocked {
            self.palette_blocked_events += 1;
            return;
        }
        let li = ly as usize;
        if li >= 144 {
            return;
        }
        self.per_reg_events[ri] += 1;
        if !self.line_reg_dirty[li][ri] {
            self.line_reg_dirty[li][ri] = true;
            self.per_reg_dirty_lines[ri] += 1;
        }
        self.line_dirty[li] = true;
    }

    /// Fold this frame's per-line marks into the cumulative totals and reset
    /// the per-frame state. Call once per rendered frame.
    pub fn end_frame(&mut self) {
        let mut seen_any = false;
        for ly in 0..144 {
            if self.line_seen[ly] {
                self.total_visible_lines += 1;
                seen_any = true;
                if self.line_dirty[ly] {
                    self.total_dirty_lines += 1;
                }
            }
            self.line_dirty[ly] = false;
            self.line_seen[ly] = false;
            for r in 0..9 {
                self.line_reg_dirty[ly][r] = false;
            }
        }
        if seen_any {
            self.frames += 1;
        }
    }

    /// Per-game dirty-line percentage.
    pub fn dirty_line_pct(&self) -> f64 {
        if self.total_visible_lines == 0 {
            return 0.0;
        }
        100.0 * self.total_dirty_lines as f64 / self.total_visible_lines as f64
    }

    pub fn ever_dirty(&self) -> bool {
        self.total_dirty_lines > 0
    }
}
