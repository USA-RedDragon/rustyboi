//! DMA subsystem: the OAM-DMA (FF46) sprite-transfer engine and the CGB
//! VRAM DMA unit (GDMA + HBlank-HDMA, FF51-FF55). Relocated out of
//! `memory/mmio/`; this module owns the DMA type definitions, while the
//! bus-master transfer/scheduling logic lives in its `impl Mmio` child
//! modules (`oam`, `gdma`, `hdma`).
use serde::{Deserialize, Serialize};

mod oam;
mod gdma;
mod hdma;

fn default_pending_oam_zero() -> std::cell::Cell<i16> {
    std::cell::Cell::new(-1)
}

/// The DMA subsystem's owned state: the OAM-DMA (FF46) engine and the CGB
/// VRAM-DMA (GDMA + HBlank-HDMA, FF51-FF55) engine. `Mmio` holds exactly one
/// `dma: Dma` field; all DMA state lives here.
#[derive(Serialize, Deserialize, Clone, Default)]
pub(in crate::memory) struct Dma {
    pub(in crate::memory) oam: OamDmaEngine,
    pub(in crate::memory) hdma: HdmaEngine,
}

/// Source-region classification of the active OAM DMA, as decoded from the
/// FF46 source-high byte. Drives the per-region bus-conflict rules.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DmaSrcKind {
    Rom,
    Sram,
    Vram,
    Wram,
    /// CGB-only: source E000-FFFF drives the external bus with the RAM chip
    /// select asserted, so only the cartridge answers.
    ExternalBus,
}

/// CGB HDMA halt-state machine
/// Captured at HALT and consulted on unhalt to decide whether the next
/// Mode 0 should immediately fire an HDMA block.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[derive(Default)]
pub(crate) enum HaltHdmaState {
    /// Not in an HDMA period when halt was entered.
    #[default]
    Low,
    /// Halt entered while in HDMA period, HDMA armed, no block scheduled.
    High,
    /// Halt entered with a block already scheduled (req flagged).
    Requested,
}

/// CGB HDMA/GDMA engine: the FF51-FF55 registers plus the block-timing,
/// arm/kick and deferred-write machinery. Most of this is `#[serde(skip)]`
/// sub-dot scheduling state; only the six register-backed fields persist.
#[derive(Serialize, Deserialize, Clone)]
pub(in crate::memory) struct HdmaEngine {
    // CGB HDMA-tracker sleep: master cc below which the per-dot step_hdma
    // maintenance is a proven no-op (the PPU sets it at its own mode
    // transitions — mode-2 entry until just before the pixel-transfer arm,
    // and the arm until just before the closed-form mode-0 time; no period
    // edge, LY change, or block fire can occur inside). Cleared by any IO
    // write and by set_hdma_req (halt-exit reflags). Not serialized: 0 = no
    // sleep.
    #[serde(skip)]
    #[serde(default)]
    pub(in crate::memory) tracker_sleep_until: u64,
    // CGB HDMA state
    pub(in crate::memory) source: u16,       // HDMA source address (advances per byte)
    pub(in crate::memory) dest: u16,         // HDMA destination (advances per byte; low 13 bits used for VRAM offset)
    // Blocks remaining, minus one.
    // 0x7F means "fully done": FF55 reads as 0xFF.
    // Pan Docs: CGB Registers (VRAM DMA) — https://gbdev.io/pandocs/CGB_Registers.html
    pub(in crate::memory) length: u8,
    // True while HDMA is armed (FF55 bit7 written as 1, not yet completed
    // or cancelled).
    #[serde(default)]
    pub(in crate::memory) enabled: bool,
    // True while a 0x10-byte block is scheduled to fire on the next CPU
    // cycle. Mirrors the hardware's pending-DMA request. Set by the PPU at
    // Mode 3->0 boundary (when `enabled`) and by LCD enable/disable
    // edges; cleared after `run_hdma_block` runs.
    #[serde(default)]
    pub(in crate::memory) req_pending: bool,
    // Cached `Ppu::is_hdma_period()` value, refreshed each PPU step. Read
    // by the HALT opcode handler so it does not need a `&Ppu` borrow.
    #[serde(skip, default)]
    pub(in crate::memory) is_in_period_cached: bool,
    // Previous STAT mode observed by `step_hdma`, used to detect the Mode 3->0
    // (HBlank) edge that arms an HDMA block (fallback path). Not part of save state.
    #[serde(skip, default)]
    pub(in crate::memory) prev_stat_mode: u8,
    // Previous `Ppu::hdma_period` value, used to detect the rising edge of the
    // cycle-exact HDMA-eligibility window. Not part of save state.
    #[serde(skip, default)]
    pub(in crate::memory) prev_period: bool,
    // Enforces Pan Docs' "one 0x10-byte HBlank-DMA block per HBlank": set when a
    // block fires, cleared on every LY change so a second edge (or an in-HBlank
    // FF55 kick that coincides with the closed-form mode-0 edge) can't arm a
    // second block for the same scanline. Keyed off LY (not the CPU-visible STAT
    // mode, which lags the cycle-exact predicate). Transient timing state.
    #[serde(skip, default)]
    pub(in crate::memory) block_fired_this_hblank: bool,
    // None until the first block; LY is a u8 register, so no sentinel is needed.
    #[serde(skip, default)]
    pub(in crate::memory) last_dma_ly: Option<u8>,
    // True while the bus is advancing the world in
    // lockstep through an HDMA block's transfer cc (the event-interleaved dma()).
    // While set, `step_hdma` must NOT fire/arm a new block (the lockstep advance
    // only ticks timer/PPU through the in-flight transfer; the next m0-edge is
    // handled by the normal per-dot crank after the lockstep).
    #[serde(skip, default)]
    pub(in crate::memory) lockstep_active: bool,
    // Armed at a Requested-context (multi-block) HDMA unhalt so the
    // per-dot lockstep advance (run_to_min_event) applies ONLY to the block that
    // fires during the resume instruction, not to the
    // normal m0-edge / GDMA-calibration blocks (which keep the deferred-stall
    // path). Cleared when the resume instruction completes.
    #[serde(skip, default)]
    pub(in crate::memory) resume_lockstep_window: bool,
    // Sticky: FF55 (HDMA5) has been written at least once since power-on, i.e.
    // this ROM drives the HDMA/GDMA machinery. The CGB LCD-woken halt-exit
    // stall (sm83.rs) is scoped away from such ROMs: the engine's entire
    // GDMA/HDMA cc-web (block fire cc's, dma_prefetch STAT biases, the
    // hdma_start/late_* race models) is co-tuned end-to-end to the un-stalled
    // wake cc, and the wake-time hdma predicates cannot see a GDMA / late-armed
    // HDMA that the woken stream will fire only after the wake. On real
    // hardware those streams stall too; modeling that requires re-anchoring
    // the whole DMA web (documented debt; no test ROM currently distinguishes it).
    // Serialized (additive `default`) so a state saved after the ROM first
    // touched FF55 preserves the sticky exactly rather than self-healing.
    #[serde(default)]
    pub(in crate::memory) machinery_used: bool,
    // Whether the HDMA block owed for the *current* eligibility period has
    // already been serviced. rustyboi fires the period block immediately at the
    // rising edge, whereas hardware defers it to the DMA event; this
    // flag lets `on_cpu_halt` recover hardware's distinction between "in period,
    // block already done" (hdma_high) and "in period, block still owed"
    // (hdma_requested -> fires on the deferred/unhalt path). Reset on the period
    // falling edge.
    #[serde(skip, default)]
    pub(in crate::memory) block_done_this_period: bool,
    // An HDMA-period rising edge that occurred WHILE the CPU was halted and whose
    // block was ALREADY serviced this period (the halt was entered in-period with
    // the block done -> `halt.hdma_state == High`). Hardware's during-halt period
    // HDMA request is suppressed (by the halted gate) AND consumed —
    // it never re-fires after unhalt; the next-line m0 edge fires the next block.
    // rustyboi's per-dot edge machine can resurrect that suppressed edge via the
    // STAT mode-3->0 fallback the first dot after unhalt (when the renderer's
    // closed-form `hdma_period` has handed off to None mid-HBlank). This flag marks
    // such a consumed edge so the STAT fallback skips it. NOT set for a Low-at-halt
    // period entry (`late_hdma_vs_tima_*_halt`: the halt was out-of-period, so the
    // post-unhalt m0 edge is a genuine first block and MUST fire). Cleared once the
    // suppressed edge has been consumed or on the next falling edge.
    #[serde(skip, default)]
    pub(in crate::memory) halt_edge_consumed: bool,
    // High-at-halt unhalt: the next-line m0 edge consume that lands JUST AFTER the
    // unhalt (not during the halt window, so `halt_edge_consumed` was never
    // set for it). When a HALT was entered in-period with the block already served
    // (`halt.hdma_state == High`), hardware suppresses+consumes the during-halt m0
    // HDMA request for the immediately-following line; rustyboi's unhalt cc can land
    // ONE dot before that line's m0 (vs hardware's unhalt landing just after it), so
    // the edge fires through the post-unhalt STAT 3->0 fallback instead of being
    // consumed (`hdma_late_m0halt_*_lcdoffset*_1`: a spurious extra block one line
    // early). This flag, set at the High-unhalt site, suppresses exactly the first
    // post-unhalt m0 fire; unlike `halt_edge_consumed` it is NOT cleared by an
    // intervening `period == Some(false)` dot, so it survives the unhalt-to-m0 gap.
    #[serde(skip, default)]
    pub(in crate::memory) high_unhalt_consume: bool,
    // When a Requested-at-halt HDMA block is reflagged+fired at unhalt, the NEXT
    // line's m0 rising edge that re-arms the following block may fall WITHIN that
    // block's transfer span. In hardware that m0 HDMA event is
    // absorbed by the in-flight transfer (the event is processed at the block's end
    // cc, its HDMA request reschedules to the line AFTER), so the genuine next
    // block fires a full line later. rustyboi fires synchronously at the per-dot m0
    // edge, so without this it arms the next block at that absorbed edge — one line
    // early. The absorption window is `[block1_fire_cc, block1_fire_cc + 16*(2+2*ds)]`
    // (the dma() transfer length, inclusive end — an edge AT the transfer end is
    // still absorbed); armed at the Requested unhalt reflag, `step_hdma` consumes
    // every m0 arm inside it and disarms on the first arm strictly past it.
    // HDMA transfers one block per H-Blank (base): TCAGBD §9.6.2. The m0-edge
    // absorption-window sub-cycle timing is not in Pan Docs/TCAGBD/GBCTR — test-ROM refs.
    #[serde(skip, default)]
    pub(in crate::memory) peraccess_consume_pending: bool,
    // Deferred HDMA block byte writes. Hardware reads each byte
    // at `cc` but writes it to VRAM at `cc + (2 + 2*ds)`,
    // so byte 0 lands one sub-M-cycle AFTER the trigger/prefetch boundary and
    // after VRAM unlocks. rustyboi fires the block synchronously; to place the
    // VRAM writes at the correct sub-M-cycle (the 4cc window the hdma_start /
    // hdma_late read tests probe) the resolved (vram_addr, value, into_bank1)
    // triples are read at fire time and drained `write_delay` dots later.
    // The `into_bank1` flag records the VBK bank captured at fire so a mid-delay
    // VBK switch cannot retarget the in-flight bytes. Not part of save state
    // (the buffer always drains within a few dots of the trigger).
    #[serde(skip, default)]
    pub(in crate::memory) pending_writes: Vec<(u16, u8, bool)>,
    #[serde(skip, default)]
    pub(in crate::memory) write_delay: u32,
    // PC-in-DMA-dest prefetch absorption (hardware runs
    // the next-opcode fetch at the instruction boundary, BEFORE the DMA event
    // overwrites VRAM). When a synchronous GDMA/HDMA block fires and the CPU's
    // very next opcode fetch lands on the block's first destination byte
    // (pc straddles ROM bank0->VRAM at 0x7FFE->0x8000), that opcode must read the
    // PRE-transfer VRAM byte while the instruction's operands (dest+1..) read the
    // POST-transfer bytes. Records the first-dest address and its pre-transfer
    // byte at fire; sm83 consults it for exactly the next prefetch.
    // (hdma_pc_7ffe / late_gdma_pc_7ffe.)
    #[serde(skip, default)]
    pub(in crate::memory) fire_dest0: Option<u16>,
    #[serde(skip, default)]
    pub(in crate::memory) fire_dest0_prebyte: u8,
    // The dma-event cc at which the block fired. The hardware
    // prefetch reads the next opcode at THIS cc (before the transfer), so the
    // prefetch's VRAM-lock decision must be taken here, not at rustyboi's
    // post-stall prefetch cc (which trails the fire by the whole transfer stall).
    #[serde(skip, default)]
    pub(in crate::memory) fire_cc: u64,
    // Armed by the FF55-write immediate kick (in-period HDMA enable on the same
    // instruction). Only such an instruction-driven block can flow the CPU's PC
    // straight into its VRAM destination (pc 0x7FFE -> 0x8000), so the
    // prefetch-absorption snapshot is gated on it: an m0-edge block firing inside
    // a HALT window (no kick this instruction) must NOT arm the shadow.
    #[serde(skip, default)]
    pub(in crate::memory) snapshot_armed: bool,
    // Resume-read pre-transfer shadow. When an HDMA block fires inside the
    // Requested-context HALT-bug resume window (`resume_lockstep_window`),
    // hardware runs the resume instruction's reads at the dma-event cc,
    // BEFORE the DMA commits the dest writes. So a resume read of any in-block VRAM
    // dest byte must observe the PRE-transfer value (the lockstep advances the PPU
    // to mode-0 readable, but the dest byte read at 0x80FA must still be its
    // pre-write value, not the just-transferred byte). Capture the pre-transfer
    // bytes of the whole dest range at fire; `read()` serves them for the window's
    // duration (one resume instruction). 1FFF-masked VRAM offset -> pre-byte.
    #[serde(skip, default)]
    pub(in crate::memory) resume_pre_shadow: std::collections::HashMap<u16, u8>,
    // Armed for BOTH IME states (unlike the !ime lockstep window); scopes the
    // pre-transfer shadow capture/serve through the resume read (HALT-bug double
    // execute OR the IME-on interrupt-service ISR read).
    #[serde(skip, default)]
    pub(in crate::memory) resume_shadow_window: bool,
    /// (CGB dma-due deferral) cc added to a VRAM WRITE's PPU
    /// mode-block check for the deferred post-HALT `ld (nn),a`. The hardware's
    /// DMA event advances the PPU across block1's transfer before the CPU
    /// resumes, so that write lands in the post-transfer mode-0 window. rustyboi
    /// defers block1's stall (block2's next/same-line timing depends on that
    /// deferral), so instead of advancing the world it biases only this write's
    /// mode check by the pending transfer span. One-shot (cleared on consume).
    #[serde(skip, default)]
    pub(in crate::memory) dma_due_write_cc_bias: u64,
    // FF55-kick fire-timing: set when an FF55 bit7=1 write (enable or
    // restart) wants to arm the first block immediately. Hardware
    // gates that immediate flag on the LIVE in-HBlank-period predicate (at cc+4), not
    // the 1-dot-lagged renderer period cache. The bus resolves this flag after
    // the FF55 write by evaluating the PPU's `hdma_period` at the write access cc;
    // if not in period the kick is dropped (the block then arms on the next
    // Mode 3->0 edge). The enable-vs-restart distinction the two write paths
    // once encoded here was never consumed -- every reader only asks whether a
    // kick is pending -- so this is a plain flag.
    #[serde(skip, default)]
    pub(in crate::memory) kick_eval_pending: bool,
    // FF55=00 disable-vs-m0-edge race: a FF55 bit7=0
    // write only clears the FUTURE m0-edge HDMA schedule; it CANNOT un-flag a
    // block whose m0 edge already fired (the DMA event latched -> the transfer still
    // runs). The bus sets this BEFORE the FF55 write by evaluating the PPU's
    // `disable_fires(cc)` (true => m0 edge already passed => the block must
    // still run despite the disable). The write handler reads it: true =>
    // keep the request and let the block fire (then HDMA ends normally),
    // false => the historical unconditional cancel. Consumed once. The PPU
    // reports this as an Option, but "no opinion" and "does not fire" drive the
    // identical cancel, so only the fires/does-not distinction is stored.
    #[serde(skip, default)]
    pub(in crate::memory) disable_fires: bool,
    // Interrupt-vs-dma precedence: while an interrupt service is
    // mid-flight (its PC pushes not yet complete), the M-cycle-boundary HDMA fire
    // is suppressed so the block fires AFTER the pushes. Set
    // by `service_interrupt` around the pushes, cleared once it fires the block.
    #[serde(skip, default)]
    pub(in crate::memory) mcycle_fire_suppressed: bool,
    // Late-hdma-vs-interrupt unhalt precedence: set at unhalt when a Low-at-halt
    // HDMA block did NOT reflag (the reflag gate was false at unhalt), so
    // its m0-edge falls within the immediately-following interrupt service window.
    // The service then suppresses+reorders that block to fire AFTER its PC pushes
    // (the `late_hdma_vs_tima_*_halt_2` content tests: copy the pushed 0x11C9).
    // Cleared once consumed by the service (or the next unhalt).
    #[serde(skip, default)]
    pub(in crate::memory) unhalt_noreflag_deferred: bool,
    // Next-M-cycle dma() scheduling for the IME-off HALT-bug resume. A block
    // reflagged at unhalt fires (in hardware) at the instruction boundary AFTER
    // the resume instruction (the DMA event runs after the opcode completes), so
    // its VRAM write lands AFTER the resume instruction's own memory read. The
    // synchronous m0-edge fire instead lands DURING the resume instruction,
    // ahead of that read (hdma_late_if_and_ie_halt_1: the `ld a,(80FA)` read sees
    // the post-DMA byte 0x02 instead of the pre-DMA 0x00). Set at the unhalt
    // reflag, this suppresses the synchronous fire across the resume instruction
    // and fires the held block at the next boundary.
    #[serde(skip, default)]
    pub(in crate::memory) unhalt_reflag_deferred: bool,
    // Late-hdma-vs-interrupt re-order: the master_cc at which the most
    // recent m0-edge HDMA block fired (read its 16 source bytes). Hardware orders
    // the DMA event (HDMA, flagged at the m0 time) vs the interrupt event
    // race by event time: DMA wins only when the m0 time <= the interrupt's
    // serviceable cc. rustyboi fires the block greedily the dot the
    // m0-edge is reached — one or two cc BEFORE the interrupt-triggering
    // instruction's boundary — so when an interrupt dispatches within the same
    // M-cycle window the block wrongly read pre-push memory. `service_interrupt`
    // compares this against its access cc and, when the interrupt won the race,
    // re-runs the block AFTER the pushes (the `late_hdma_vs_*` content tests).
    // None when no block is in-flight for the current period.
    #[serde(skip, default)]
    pub(in crate::memory) last_fire_cc: Option<u64>,
    // Snapshot of (source, dest, length, enabled) captured immediately BEFORE the
    // last m0-edge block fired, so the late-hdma-vs re-order can restore the
    // pre-fire pointers and re-run the block reading post-push memory.
    #[serde(skip, default)]
    pub(in crate::memory) pre_fire_state: Option<(u16, u16, u8, bool)>,
    // True when the HDMA block was already set up (FF55 written, `enabled`) at
    // HALT entry. Distinguishes the `hdma_*halt_*_ly_*`/`inc_*` family (HDMA armed in
    // the m3halt ISR BEFORE the HALT; the value-read is a downstream post-unhalt FF44
    // -> drop the +6 stall fudge) from `hdma_cycles_2` (FF55 written in the wakeup
    // ISR AFTER the HALT; the immediate FF41 STAT read needs the +6).
    #[serde(skip, default)]
    pub(in crate::memory) enabled_at_halt: bool,
    // Back-to-back GDMA word-bus conflict: set true while an OAM DMA is active once a
    // GDMA-conflict pass has run in this OAM-DMA lifetime, so the NEXT GDMA block (no
    // OAM-DMA completion in between) is recognised as a back-to-back second block.
    // Not in Pan Docs, TCAGBD, or GBCTR; the 2x-GDMA word-bus first-word duplication
    // is from real-silicon oamdumper .dump captures (not modelled by prior emulators).
    #[serde(skip, default)]
    pub(in crate::memory) gdma_conflict_ran: bool,
    // CPU-cycle stall owed for HDMA/GDMA blocks already transferred; the CPU
    // idles these cycles (peripherals keep ticking) before its next fetch.
    // Serialized (additive `default`): owed cycles can straddle an instruction
    // boundary, so a state saved with a stall pending must resume with it.
    #[serde(default)]
    pub(in crate::memory) pending_dma_stall: u32,
    // VRAM-source GDMA first-word latch. A GDMA whose source is VRAM reads
    // nothing (same-bus read: floats 0xFF), EXCEPT the transfer's first 16-bit
    // word, which latches the byte the CPU's absorbed next-opcode prefetch
    // left on the data
    // bus - duplicated into both bytes by the word bus. AntonioND
    // hdma_valid_sources real_gbc.sav row 8000 reads `3E 3E FF FF ...` (0x3E =
    // the `ld a,` opcode following the FF55 write). The prefetch byte is only
    // known at the CPU's next fetch, so `execute_gdma` arms this with the
    // first dest word's VRAM address (+ bank), and `Bus::fetch_opcode` patches
    // the two bytes with the fetched opcode. Consume-once.
    // Base (VRAM-source DMA = "two unknown bytes then FFh"): TCAGBD §9.6.3; the
    // identity of the two bytes (the word-duplicated prefetch opcode) is from the
    // AntonioND hdma_valid_sources captures — not in Pan Docs/GBCTR.
    #[serde(skip, default)]
    pub(in crate::memory) gdma_vram_src_fixup: Option<(u16, bool)>,
    // OAM-DMA advance suppression for the GDMA/HDMA stall window. `execute_gdma`
    // / `run_hdma_block` fold the OAM-DMA's M-cycle advances INTO the transfer
    // loop. The same
    // transfer cc are then drained as a CPU `pending_dma_stall`, during which
    // `step_dma` would advance the OAM-DMA a SECOND time. This counts those
    // already-folded dots so `step_dma` skips them (the OAM-DMA stays frozen
    // at its post-loop position until the next OAM-DMA update).
    // Serialized (additive `default`): the suppression window drains across the
    // CPU stall, spanning instruction boundaries.
    #[serde(default)]
    pub(in crate::memory) oam_dma_stall_suppress: u32,
}

/// OAM DMA (FF46) engine: the in-flight transfer cursor and the prefetch
/// STAT-read bias the absorbed prefetch M-cycle leaves behind.
#[derive(Serialize, Deserialize, Clone)]
pub(in crate::memory) struct OamDmaEngine {
    // OAM DMA state. Models the hardware's continuously-running OAM-DMA engine:
    // `pos` idles at 254 (-2). On an FF46 write
    // the engine is armed (`active`) and `start_pos = (pos + 2)`;
    // the transfer of byte 0 therefore begins two M-cycles after the write.
    // Each M-cycle (4 dots) advances `pos`; when it reaches `start_pos`
    // the transfer (re)starts at 0, copies bytes 0..=159, then ends at 160.
    pub(in crate::memory) active: bool,
    pub(in crate::memory) source_base: u16,
    #[serde(default)]
    pub(in crate::memory) pos: u8,
    #[serde(default)]
    pub(in crate::memory) start_pos: u8,
    #[serde(default)]
    pub(in crate::memory) subcycle: u8, // dots elapsed within the current M-cycle (0..=3)
    // DMA prefetch absorption: hardware runs GDMA/HDMA with a preceding opcode
    // prefetch that fetches the next opcode at the DMA event cc with NO extra cc —
    // the opcode-fetch M-cycle is folded into the DMA's trailing `+4`. rustyboi
    // copies the block synchronously and drains the cc as an idle stall, so the
    // FIRST access after the stall starts its M-cycle one dot higher than hardware
    // (the absorbed prefetch M-cycle is double-counted). This flag, set when the
    // stall is consumed, tells the next FF41 STAT-mode read to resolve at
    // `master_cc - 1` (hardware's true read cc) so the post-DMA mode-3 boundary
    // `_1`/`_2` brackets land on the same sub-dot hardware does. Cleared after the
    // first STAT mode read consumes it.
    // Not in Pan Docs, TCAGBD, or GBCTR; sub-cycle STAT-bias timing from test-ROM refs.
    #[serde(skip, default)]
    pub(in crate::memory) prefetch_stat_bias: bool,
    // Set when a CPU write lands in OAM (0xFE00-0xFE9F) this M-cycle, so the PPU
    // can fire the sprite snapshot on an OAM write.
    // Drained by the PPU each dot.
    #[serde(default)]
    pub(in crate::memory) oam_write_pending: bool,
    // CGB VRAM-source OAM-DMA conflict reads return OAM[dma.pos] and then
    // zero that OAM byte. The read path is &self,
    // so record the position here and apply the zero on the next DMA advance.
    // -1 = none.
    #[serde(skip, default = "default_pending_oam_zero")]
    pub(in crate::memory) pending_oam_zero: std::cell::Cell<i16>,
    // OAM-DMA-source VRAM bus-conflict model. The PPU pushes its BG fetcher's
    // current VRAM data-bus address/bank here each dot during mode 3 (VRAM locked).
    // A VRAM-source OAM-DMA read while `fetcher_bus_locked` is set returns
    // VRAM[(dma_src_addr & fetcher_bus_addr)] — both the fetcher and the DMA drive
    // the VRAM address bus, so the array is indexed by their bitwise-AND (the real
    // hardware "OAM DMA bus conflict"). Cleared each dot the PPU is not mode-3.
    // Not in Pan Docs, TCAGBD, or GBCTR (GBCTR marks "OAM DMA bus conflicts" TODO;
    // TCAGBD §9.6.3's VRAM-read corruption is the GDMA/HDMA engine, not OAM-DMA);
    // the AND-with-fetcher formula is from real-silicon .dump captures.
    #[serde(skip, default)]
    pub(in crate::memory) fetcher_bus_addr: u16,
    #[serde(skip, default)]
    pub(in crate::memory) fetcher_bus_bank: u8,
    #[serde(skip, default)]
    pub(in crate::memory) fetcher_bus_locked: bool,
    // The first OAM-DMA VRAM-source read after the fetcher bus lock engages (a
    // fresh mode-3 entry) still resolves to true VRAM: the BG fetcher has not yet
    // settled a displayed-tile byte on the conflict bus during the line's warmup,
    // so the first locked M-cycle reads cleanly (the dumps show the first mode-3
    // byte of each crossed line is identity). Set on the lock's rising edge,
    // consumed by the first conflicting read.
    #[serde(skip, default)]
    pub(in crate::memory) fetcher_bus_warmup: bool,
    // DMG-only mode-2 fetcher-prefetch onset. On DMG the BG fetcher's first
    // tile-NUMBER fetch begins one M-cycle (4 dots) BEFORE the official mode-3
    // VRAM lock — the mode-2->mode-3 prefetch. A VRAM-source OAM-DMA M-cycle in
    // that prefetch window already sees the fetcher driving the first tilemap
    // address, so the conflict engages one M-cycle EARLIER than the CGB
    // `state==PixelTransfer` lock: the onset byte is `VRAM[dma_addr & tilemap0]`
    // (tile-number address-line AND), not the clean source. The dumps show the
    // crossed line's first conflict byte at the LAST mode-2 M-cycle on DMG, while
    // the subsequent first locked M-cycle is still the warmup (clean) byte. The
    // PPU publishes the predicted first tilemap address here for the 4-dot window
    // preceding the mode-3 arm; `dmg_prefetch_addr` is 0 when inactive.
    #[serde(skip, default)]
    pub(in crate::memory) dmg_prefetch_active: bool,
    #[serde(skip, default)]
    pub(in crate::memory) dmg_prefetch_addr: u16,
    // Second-order conflict: when an OAM-DMA M-cycle reads VRAM while the BG
    // fetcher is driving a TILE-NUMBER (tilemap) address, both the DMA byte and the
    // tile number the fetcher would latch are `VRAM[tilemap_addr & dma_src]`. That
    // poisoned tile number shifts the tile-data address the fetcher drives on the
    // NEXT M-cycle, so the following tile-data DMA read sees
    // `VRAM[tiledata(poisoned_tile,row) & dma_src]`. We carry the poisoned tile's
    // tile-data base (0x8000 + tile*16) here from the tilemap read to the next
    // tile-data read; the row low bits come from that read's own fetcher address.
    #[serde(skip, default)]
    pub(in crate::memory) poison_tiledata_base: Option<u16>,
    // True while the CPU is parked in the STOP speed-switch unhalt window
    // (0x20000 cycles). The CPU is halted for this
    // window, so the OAM-DMA takes its halted branch (elapsed time is
    // consumed WITHOUT moving `dma.pos`). rustyboi drains the
    // window via `stop_unhalt_cycles` without setting `cpu_halted`, so the
    // OAM-DMA must be frozen here too. Set on STOP entry, cleared at unhalt.
    // Serialized (additive `default`): the freeze persists across the whole STOP
    // window, spanning instruction boundaries.
    #[serde(default)]
    pub(in crate::memory) oam_dma_stop_freeze: bool,
    // Mirror of the HALT-entry `halt.oam_grace`, but for the STOP speed-switch.
    // Hardware advances the OAM-DMA by the STOP instruction's own M-cycle before
    // halting, so that M-cycle advances the
    // OAM-DMA one step before the freeze, and a transfer whose final byte's
    // M-cycle lands in that window completes (OAM-DMA end) rather than stalling to
    // unhalt. Without it rustyboi froze one byte short (pos 158 vs hardware's 160),
    // so the post-window mode-2 sprite scan read the in-flight 0xFF source instead
    // of the completed OAM (oamdma_late_speedchange_stat_2: the line's left-edge
    // sprite goes unmapped, mode-0 time -11, STAT read mode 0 where hardware reads
    // mode 3). Set on STOP entry alongside the freeze; consumed in `step_dma`.
    // Serialized (additive `default`): persists across the STOP window like the
    // freeze it pairs with.
    #[serde(default)]
    pub(in crate::memory) stop_oam_grace: u8,
}

impl Default for HdmaEngine {
    fn default() -> Self {
        Self {
            tracker_sleep_until: 0,
            source: 0,
            dest: 0,
            length: 0,
            enabled: false,
            req_pending: false,
            machinery_used: false,
            is_in_period_cached: false,
            prev_stat_mode: 0,
            prev_period: false,
            block_fired_this_hblank: false,
            last_dma_ly: None,
            lockstep_active: false,
            resume_lockstep_window: false,
            block_done_this_period: false,
            halt_edge_consumed: false,
            high_unhalt_consume: false,
            peraccess_consume_pending: false,
            pending_writes: Vec::new(),
            fire_dest0: None,
            fire_dest0_prebyte: 0xFF,
            fire_cc: 0,
            snapshot_armed: false,
            resume_pre_shadow: std::collections::HashMap::new(),
            resume_shadow_window: false,
            dma_due_write_cc_bias: 0,
            write_delay: 0,
            kick_eval_pending: false,
            disable_fires: false,
            mcycle_fire_suppressed: false,
            unhalt_noreflag_deferred: false,
            unhalt_reflag_deferred: false,
            last_fire_cc: None,
            pre_fire_state: None,
            enabled_at_halt: false,
            gdma_conflict_ran: false,
            pending_dma_stall: 0,
            gdma_vram_src_fixup: None,
            oam_dma_stall_suppress: 0,
        }
    }
}

impl Default for OamDmaEngine {
    fn default() -> Self {
        Self {
            active: false,
            source_base: 0,
            pos: 0xFE,
            start_pos: 0,
            subcycle: 0,
            prefetch_stat_bias: false,
            oam_write_pending: false,
            pending_oam_zero: std::cell::Cell::new(-1),
            fetcher_bus_addr: 0,
            fetcher_bus_bank: 0,
            fetcher_bus_locked: false,
            fetcher_bus_warmup: false,
            dmg_prefetch_active: false,
            dmg_prefetch_addr: 0,
            poison_tiledata_base: None,
            oam_dma_stop_freeze: false,
            stop_oam_grace: 0,
        }
    }
}
