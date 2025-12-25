use crate::audio;
use crate::cartridge;
use crate::cpu;
use crate::input;
use crate::memory;
use crate::memory::Addressable;
use crate::ppu;
use crate::serial;
use crate::timer;
use serde::{Deserialize, Serialize};

use std::fs;
use std::io;

const EMPTY_BYTE: u8 = 0xFF;

fn default_oam_high() -> [u8; 0x60] {
    [0; 0x60]
}

fn default_pending_oam_zero() -> std::cell::Cell<i16> {
    std::cell::Cell::new(-1)
}

const BIOS_START: u16 = 0x0000;
const BIOS_SIZE: usize = 256; // 256 bytes
const BIOS_END: u16 = BIOS_START + BIOS_SIZE as u16 - 1;
pub const CARTRIDGE_START: u16 = 0x0000;
pub const CARTRIDGE_SIZE: usize = 16384; // 16KB
const CARTRIDGE_AFTER_BIOS_START: u16 = 0x0100; // After BIOS is disabled
pub const CARTRIDGE_END: u16 = CARTRIDGE_START + CARTRIDGE_SIZE as u16 - 1;
pub const CARTRIDGE_BANK_START: u16 = 0x4000;
pub const CARTRIDGE_BANK_SIZE: usize = 16384; // 16KB
pub const CARTRIDGE_BANK_END: u16 = CARTRIDGE_BANK_START + CARTRIDGE_BANK_SIZE as u16 - 1;
pub const VRAM_START: u16 = 0x8000;
const VRAM_SIZE: usize = 8192; // 8KB
const VRAM_END: u16 = VRAM_START + VRAM_SIZE as u16 - 1;
const EXTERNAL_RAM_START: u16 = 0xA000;
const EXTERNAL_RAM_SIZE: usize = 8192; // 8KB
const EXTERNAL_RAM_END: u16 = EXTERNAL_RAM_START + EXTERNAL_RAM_SIZE as u16 - 1;
const WRAM_START: u16 = 0xC000;
const WRAM_SIZE: usize = 4096; // 4KB
const WRAM_END: u16 = WRAM_START + WRAM_SIZE as u16 - 1;
const WRAM_BANK_START: u16 = 0xD000;
const WRAM_BANK_SIZE: usize = 4096; // 4KB
const WRAM_BANK_END: u16 = WRAM_BANK_START + WRAM_BANK_SIZE as u16 - 1;
const ECHO_RAM_START: u16 = 0xE000;
const ECHO_RAM_SIZE: usize = 7680; // 7.5KB
const ECHO_RAM_END: u16 = ECHO_RAM_START + ECHO_RAM_SIZE as u16 - 1;
const ECHO_RAM_MIRROR_END: u16 = 0xDDFF; // Echo RAM mirrors WRAM and most of WRAM_BANK
const OAM_START: u16 = 0xFE00;
const OAM_SIZE: usize = 160; // 160 bytes
const OAM_END: u16 = OAM_START + OAM_SIZE as u16 - 1;
const UNUSED_START: u16 = 0xFEA0;
const UNUSED_SIZE: usize = 96; // 96 bytes
const UNUSED_END: u16 = UNUSED_START + UNUSED_SIZE as u16 - 1;
const IO_REGISTERS_START: u16 = 0xFF00;
const IO_REGISTERS_SIZE: usize = 128; // 128 bytes
const IO_REGISTERS_END: u16 = IO_REGISTERS_START + IO_REGISTERS_SIZE as u16 - 1;
const HRAM_START: u16 = 0xFF80;
const HRAM_SIZE: usize = 127; // 127 bytes
const HRAM_END: u16 = HRAM_START + HRAM_SIZE as u16 - 1;
const IE_REGISTER: u16 = 0xFFFF; // Interrupt Enable Register

pub const REG_BOOT_OFF: u16 = 0xFF50; // Boot ROM disable
pub const REG_DMA: u16 = 0xFF46; // DMA Transfer and Start Address

// CGB-specific registers
pub const REG_KEY0: u16 = 0xFF4C;  // CGB CPU mode select (DMG compatibility)
pub const REG_KEY1: u16 = 0xFF4D;  // CGB Prepare speed switch
pub const REG_VBK: u16 = 0xFF4F;   // VRAM Bank select
pub const REG_HDMA1: u16 = 0xFF51; // HDMA Source High
pub const REG_HDMA2: u16 = 0xFF52; // HDMA Source Low
pub const REG_HDMA3: u16 = 0xFF53; // HDMA Destination High
pub const REG_HDMA4: u16 = 0xFF54; // HDMA Destination Low
pub const REG_HDMA5: u16 = 0xFF55; // HDMA Length/Mode/Start
pub const REG_SVBK: u16 = 0xFF70; // WRAM Bank select
pub const REG_BCPS: u16 = 0xFF68; // Background Color Palette Specification
pub const REG_BCPD: u16 = 0xFF69; // Background Color Palette Data
pub const REG_OCPS: u16 = 0xFF6A; // Object Color Palette Specification
pub const REG_OCPD: u16 = 0xFF6B; // Object Color Palette Data


/// CGB HDMA halt-state machine
/// Captured at HALT and consulted on unhalt to decide whether the next
/// Mode 0 should immediately fire an HDMA block.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum HaltHdmaState {
    /// Not in an HDMA period when halt was entered.
    Low,
    /// Halt entered while in HDMA period, HDMA armed, no block scheduled.
    High,
    /// Halt entered with a block already scheduled (req flagged).
    Requested,
}

impl Default for HaltHdmaState {
    fn default() -> Self {
        HaltHdmaState::Low
    }
}


#[derive(Serialize, Deserialize, Clone)]
struct DelayedMmioWrite {
    addr: u16,
    value: u8,
    cycles_remaining: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppliedMmioWrite {
    pub addr: u16,
    pub value: u8,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Mmio {
    #[serde(skip, default)]
    bios: Option<memory::Memory<BIOS_START, BIOS_SIZE>>,
    #[serde(skip, default)]
    cartridge: Option<cartridge::Cartridge>,
    input: input::Input,
    vram: memory::Memory<VRAM_START, VRAM_SIZE>,
    wram: memory::Memory<WRAM_START, WRAM_SIZE>,
    wram_bank: memory::Memory<WRAM_BANK_START, WRAM_BANK_SIZE>,
    oam: memory::Memory<OAM_START, OAM_SIZE>,
    // CGB-only shadow for the 0xFEA0-0xFEFF "unused" region, which on CGB
    // mirrors the OAM index space masked with 0xE7 (Gambatte ioamhram_ tail).
    // Indexed by `(addr & 0xFF) & 0xE7` minus 0xA0 (reachable indices are
    // 0xA0..=0xE7). Not present on DMG (writes ignored, reads 0xFF).
    #[serde(default = "default_oam_high", with = "serde_bytes")]
    oam_high: [u8; 0x60],
    timer: timer::Timer,
    #[serde(default = "serial::Serial::new")]
    serial: serial::Serial,
    #[serde(skip, default)]
    delayed_writes: Vec<DelayedMmioWrite>,
    io_registers: memory::Memory<IO_REGISTERS_START, IO_REGISTERS_SIZE>,
    hram: memory::Memory<HRAM_START, HRAM_SIZE>,
    ie_register: u8,
    audio: audio::Audio,
    // OAM DMA state. Modeled on Gambatte's continuously-running engine:
    // `dma_pos` mirrors `oamDmaPos_` and idles at 254 (-2). On an FF46 write
    // the engine is armed (`dma_active`) and `dma_start_pos = (dma_pos + 2)`;
    // the transfer of byte 0 therefore begins two M-cycles after the write.
    // Each M-cycle (4 dots) advances `dma_pos`; when it reaches `dma_start_pos`
    // the transfer (re)starts at 0, copies bytes 0..=159, then ends at 160.
    dma_active: bool,
    dma_source_base: u16,
    #[serde(default)]
    dma_pos: u8,
    #[serde(default)]
    dma_start_pos: u8,
    #[serde(default)]
    dma_subcycle: u8, // dots elapsed within the current M-cycle (0..=3)
    // Set when a CPU write lands in OAM (0xFE00-0xFE9F) this M-cycle, so the PPU
    // can fire the sprite-snapshot `change(cc)` (Gambatte `oamChange` on a write).
    // Drained by the PPU each dot.
    #[serde(default)]
    oam_write_pending: bool,
    // CGB VRAM-source OAM-DMA conflict reads return OAM[oamDmaPos_] and then
    // zero that OAM byte (Gambatte `nontrivial_read`). The read path is &self,
    // so record the position here and apply the zero on the next DMA advance.
    // -1 = none.
    #[serde(skip, default = "default_pending_oam_zero")]
    pending_oam_zero: std::cell::Cell<i16>,

    // Set true when the CPU writes to FF44 (LY). Consumed by the PPU on its
    // next step to reset internal scanline timing. Not part of save state.
    #[serde(skip, default)]
    ly_write_pending: bool,
    // Set true when the CPU writes to a register that affects the STAT line
    // (FF40 LCDC, FF41 STAT, FF45 LYC). Consumed by the PPU between CPU
    // instructions to re-run LYC compare and the STAT edge detector so that
    // enabling a STAT source mid-frame can fire IRQ immediately when a
    // matching condition is already true.
    #[serde(skip, default)]
    stat_register_write_pending: bool,
    // Set specifically by FF41 (STAT) writes, even when the value is unchanged.
    // The DMG STAT-write bug fires on any FF41 write regardless of value.
    #[serde(skip, default)]
    ff41_write_pending: bool,
    // Persistent CPU T-cycle phase. Survives instruction boundaries (unlike the
    // per-instruction `Bus::dot`). At double speed the PPU steps every other
    // T-cycle; this counter carries the true accumulated phase so the DS gate
    // and register-write sub-dot resolution stay aligned to the real cc parity.
    #[serde(default)]
    cpu_t_phase: u64,

    // CGB-specific state
    vram_bank: u8,          // VRAM bank select (0-1)
    wram_bank_select: u8,   // WRAM bank select (1-7)
    
    // CGB speed switching state
    key0_locked: bool,      // Whether KEY0 register is locked (after boot ROM finishes)
    key0_dmg_mode: bool,    // DMG compatibility mode (KEY0 bit 0)
    key1_current_speed: bool, // Current speed mode (KEY1 bit 7): false=normal, true=double
    key1_switch_armed: bool,  // Speed switch armed (KEY1 bit 0)
    
    // CGB VRAM bank 1 (bank 0 is the existing vram field)
    vram_bank1: memory::Memory<VRAM_START, VRAM_SIZE>,
    
    // CGB WRAM banks 2-7 (bank 1 is the existing wram_bank field)  
    wram_banks: Vec<memory::Memory<WRAM_BANK_START, WRAM_BANK_SIZE>>, // Banks 2-7
    
    // CGB HDMA state
    hdma_source: u16,       // HDMA source address (advances per byte)
    hdma_dest: u16,         // HDMA destination (advances per byte; low 13 bits used for VRAM offset)
    // Blocks-remaining-minus-one, matching Gambatte's `dmaLength/0x10 - 1`.
    // 0x7F means "fully done": FF55 reads as 0xFF.
    hdma_length: u8,
    // True while HDMA is armed (FF55 bit7 written as 1, not yet completed
    // or cancelled).
    #[serde(default)]
    hdma_enabled: bool,
    // True while a 0x10-byte block is scheduled to fire on the next CPU
    // cycle. Mirrors Gambatte's `hdma_req` intreq flag. Set by the PPU at
    // Mode 3->0 boundary (when `hdma_enabled`) and by LCD enable/disable
    // edges; cleared after `run_hdma_block` runs.
    #[serde(default)]
    hdma_req_pending: bool,
    // CPU-cycle stall owed for HDMA/GDMA blocks already transferred; the CPU
    // idles these cycles (peripherals keep ticking) before its next fetch.
    #[serde(skip, default)]
    pending_dma_stall: u32,
    // C7 (DMA prefetch absorption): Gambatte runs GDMA/HDMA as `intevent_dma` with
    // a preceding `Interrupter::prefetch` that fetches the next opcode at the dma
    // event cc with NO extra cc — the opcode-fetch M-cycle is folded into the dma's
    // trailing `+4`. rustyboi copies the block synchronously and drains the cc as an
    // idle stall, so the FIRST access after the stall starts its M-cycle one dot
    // higher than Gambatte's (the absorbed prefetch M-cycle is double-counted). This
    // flag, set when the stall is consumed, tells the next FF41 STAT-mode read to
    // resolve at `master_cc - 1` (Gambatte's true read cc) so the post-DMA mode-3
    // boundary `_1`/`_2` brackets land on the same sub-dot Gambatte does. Cleared
    // after the first STAT mode read consumes it.
    #[serde(skip, default)]
    dma_prefetch_stat_bias: bool,
    // OAM-DMA advance suppression for the GDMA/HDMA stall window. `execute_gdma`
    // / `run_hdma_block` fold the OAM-DMA's M-cycle advances INTO the transfer
    // loop (Gambatte's `cc - 3 > lOam` gate inside `Memory::dma`). The same
    // transfer cc are then drained as a CPU `pending_dma_stall`, during which
    // `step_dma` would advance the OAM-DMA a SECOND time. This counts those
    // already-folded dots so `step_dma` skips them (mirrors Gambatte freezing
    // `lastOamDmaUpdate_` at its post-loop value until the next `updateOamDma`).
    #[serde(skip, default)]
    oam_dma_stall_suppress: u32,
    // Allow the OAM-DMA to advance this many M-cycles at HALT entry before the
    // freeze takes hold (Gambatte `Memory::halt` runs `updateOamDma(cc + 4)`
    // before halting, so the HALT instruction's own M-cycle moves the OAM-DMA;
    // subsequent halt M-cycles freeze it). Set by `on_cpu_halt`, decremented by
    // `step_dma` advances.
    #[serde(skip, default)]
    halt_oam_grace: u8,
    // True while the CPU is parked in the STOP speed-switch unhalt window
    // (0x20000 cycles). Gambatte's `Memory::stop` `intreq_.halt()`s for this
    // window, so `updateOamDma` takes its `halted()` branch (advances
    // `lastOamDmaUpdate_` WITHOUT moving `oamDmaPos_`). rustyboi drains the
    // window via `stop_unhalt_cycles` without setting `cpu_halted`, so the
    // OAM-DMA must be frozen here too. Set on STOP entry, cleared at unhalt.
    #[serde(skip, default)]
    oam_dma_stop_freeze: bool,
    // Mirrors Gambatte's `haltHdmaState_`.
    #[serde(default)]
    halt_hdma_state: HaltHdmaState,
    // Cached `Ppu::is_hdma_period()` value, refreshed each PPU step. Read
    // by the HALT opcode handler so it does not need a `&Ppu` borrow.
    #[serde(skip, default)]
    hdma_is_in_period_cached: bool,
    // Previous STAT mode observed by `step_hdma`, used to detect the Mode 3->0
    // (HBlank) edge that arms an HDMA block (fallback path). Not part of save state.
    #[serde(skip, default)]
    hdma_prev_stat_mode: u8,
    // Previous `Ppu::hdma_period` value, used to detect the rising edge of the
    // cycle-exact HDMA-eligibility window. Not part of save state.
    #[serde(skip, default)]
    hdma_prev_period: bool,

    // Mirrors `intreq_.halted()`. Gambatte suppresses the period-edge
    // `flagHdmaReq` while halted (video.h:41 `if (!intreq_.halted())`); the
    // halt-time block is governed instead by the `haltHdmaState_` machine and
    // re-flagged only on unhalt. Set by the HALT opcode, cleared on unhalt.
    #[serde(skip, default)]
    cpu_halted: bool,

    // CGB STOP speed-switch unhalt window. Gambatte's `Memory::stop` calls
    // `intreq_.halt()` for the 0x20000-cycle unhalt window, so the HDMA
    // period-edge `flagHdmaReq` is suppressed during the bridge + window
    // (video.h:41 `if (!intreq_.halted())`). rustyboi's `cpu_halted` is only set
    // by the HALT opcode, not by STOP, so the m0-edge wrongly auto-arms a block
    // while the CPU is parked across the speed bridge. Set by `on_stop_window_enter`
    // / cleared by `stop_window_exit_reflag` so `step_hdma`'s arm gate (and edge
    // consumption) treats the STOP window as halted.
    #[serde(skip, default)]
    in_stop_window: bool,

    // C1 HALT-wakeup access-cc skew guard. rustyboi does not yet model the
    // HALT-wakeup prefetch cost (the documented +9cc HALT bug), so the master_cc
    // the bus snapshots for memory accesses in the instruction stream resumed by a
    // HALT-wakeup is one M-cycle too early. The FF41 (STAT) getStat-at-cc
    // resolution (`get_stat_mode_at_cc` mid-frame line tail) is the only consumer
    // sensitive to that sub-M-cycle skew, and the post-tick renderer register is
    // already correct there, so this flag tells the bus to defer the FF41 line-tail
    // override to the register while a HALT-woken stream is live. Set on HALT
    // wakeup, cleared when the CPU next halts again (re-arm). Without it the
    // `halt/m0int_m0stat_*` / `late_m0*_halt_m0stat_*` (out2) reads regress against
    // their HALT-free twins, which land the same access_cc but read mode 2.
    #[serde(skip, default)]
    halt_wakeup_skew: bool,

    // True when the current HALT wakeup involved HDMA (a block was Low/Requested
    // at halt or HDMA was enabled across the wakeup). The CGB halt-exit +4 getLyReg
    // bias is already folded into the HDMA wakeup phase (the in-halt block transfer
    // / unhalt reflag), so the plain-wakeup bias must be suppressed for these.
    #[serde(skip, default)]
    halt_wakeup_hdma: bool,

    // Whether the HDMA block owed for the *current* eligibility period has
    // already been serviced. rustyboi fires the period block immediately at the
    // rising edge, whereas Gambatte defers it to the `intevent_dma` event; this
    // flag lets `on_cpu_halt` recover Gambatte's distinction between "in period,
    // block already done" (hdma_high) and "in period, block still owed"
    // (hdma_requested -> fires on the deferred/unhalt path). Reset on the period
    // falling edge.
    #[serde(skip, default)]
    hdma_block_done_this_period: bool,

    // An HDMA-period rising edge that occurred WHILE the CPU was halted and whose
    // block was ALREADY serviced this period (the halt was entered in-period with
    // the block done -> `haltHdmaState_ == High`). Gambatte's during-halt period
    // `flagHdmaReq` is suppressed (video.h:41 `!intreq_.halted()`) AND consumed —
    // it never re-fires after unhalt; the next-line m0 edge fires the next block.
    // rustyboi's per-dot edge machine can resurrect that suppressed edge via the
    // STAT mode-3->0 fallback the first dot after unhalt (when the renderer's
    // closed-form `hdma_period` has handed off to None mid-HBlank). This flag marks
    // such a consumed edge so the STAT fallback skips it. NOT set for a Low-at-halt
    // period entry (`late_hdma_vs_tima_*_halt`: the halt was out-of-period, so the
    // post-unhalt m0 edge is a genuine first block and MUST fire). Cleared once the
    // suppressed edge has been consumed or on the next falling edge.
    #[serde(skip, default)]
    hdma_halt_edge_consumed: bool,

    // High-at-halt unhalt: the next-line m0 edge consume that lands JUST AFTER the
    // unhalt (not during the halt window, so `hdma_halt_edge_consumed` was never
    // set for it). When a HALT was entered in-period with the block already served
    // (`haltHdmaState_ == High`), Gambatte suppresses+consumes the during-halt m0
    // `flagHdmaReq` for the immediately-following line; rustyboi's unhalt cc can land
    // ONE dot before that line's m0 (vs Gambatte's unhalt landing just after it), so
    // the edge fires through the post-unhalt STAT 3->0 fallback instead of being
    // consumed (`hdma_late_m0halt_*_lcdoffset*_1`: a spurious extra block one line
    // early). This flag, set at the High-unhalt site, suppresses exactly the first
    // post-unhalt m0 fire; unlike `hdma_halt_edge_consumed` it is NOT cleared by an
    // intervening `period == Some(false)` dot, so it survives the unhalt-to-m0 gap.
    #[serde(skip, default)]
    hdma_high_unhalt_consume: bool,

    // per-access STAGE 2 (FACET 3): when a Requested-at-halt HDMA
    // block is reflagged+fired at unhalt, the NEXT line's m0 rising edge that
    // re-arms the following block may fall WITHIN that block's transfer span. In
    // Gambatte that m0 `memevent_hdma` is absorbed by the in-flight `dma()` (the
    // event is processed at the block's end cc, its `flagHdmaReq` reschedules to
    // the line AFTER), so the genuine next block fires a full line later. rustyboi
    // fires synchronously at the per-dot m0 edge, so without this it arms the next
    // block at that absorbed edge — one line early. This is the sub-block-cc
    // discriminator the `hdma_transition_halt_late_unhalt_scx1_1` (m0 edge inside
    // block span -> defer) vs `_2` (m0 edge past block span -> fire this line)
    // canary pair probes: the only differing quantity at the dot grid is the
    // absolute sub-dot phase of the post-unhalt m0 edge vs block1's transfer end.
    // The window is `[block1_fire_cc, block1_fire_cc + 16*(2+2*ds)]` (the dma()
    // transfer length, inclusive end — Gambatte's edge AT the transfer end is still
    // absorbed); armed at the Requested unhalt reflag, `step_hdma` consumes every m0
    // arm inside it and disarms on the first arm strictly past it.
    #[serde(skip, default)]
    hdma_peraccess_consume_pending: bool,

    // Deferred HDMA block byte writes. Gambatte's `Memory::dma` reads each byte
    // at `cc` but writes it to VRAM at `cc + (2 + 2*ds)` (memory.cpp:354/375),
    // so byte 0 lands one sub-M-cycle AFTER the trigger/prefetch boundary and
    // after VRAM unlocks. rustyboi fires the block synchronously; to place the
    // VRAM writes at the correct sub-M-cycle (the 4cc window the hdma_start /
    // hdma_late read tests probe) the resolved (vram_addr, value, into_bank1)
    // triples are read at fire time and drained `hdma_write_delay` dots later.
    // The `into_bank1` flag records the VBK bank captured at fire so a mid-delay
    // VBK switch cannot retarget the in-flight bytes. Not part of save state
    // (the buffer always drains within a few dots of the trigger).
    #[serde(skip, default)]
    hdma_pending_writes: Vec<(u16, u8, bool)>,
    #[serde(skip, default)]
    hdma_write_delay: u32,

    // C7-full FF55-kick fire-timing: set when an FF55 bit7=1 write (enable or
    // restart) wants to arm the first block immediately. Gambatte's `enableHdma`
    // gates that immediate flag on the LIVE `isHdmaPeriod(cc + 4)` predicate, not
    // the 1-dot-lagged renderer period cache. The bus resolves this flag after
    // the FF55 write by evaluating the PPU's `hdma_period` at the write access cc;
    // if not in period the kick is dropped (the block then arms on the next
    // Mode 3->0 edge). 0=no kick pending, 1=enable kick, 2=restart kick.
    #[serde(skip, default)]
    hdma_kick_eval_pending: u8,

    // FF55=00 disable-vs-m0-edge race (Gambatte `disableHdma`): a FF55 bit7=0
    // write only clears the FUTURE m0-edge HDMA schedule; it CANNOT un-flag a
    // block whose m0 edge already fired (`intevent_dma` latched -> `dma()` still
    // runs). The bus sets this BEFORE the FF55 write by evaluating the PPU's
    // `hdma_disable_fires(cc)` (true => m0 edge already passed => the block must
    // still run despite the disable). The write handler reads it: Some(true) =>
    // keep the request and let the block fire (then HDMA ends normally),
    // Some(false)/None => the historical unconditional cancel. Consumed once.
    #[serde(skip, default)]
    hdma_disable_fires: Option<bool>,

    // C7-full interrupt-vs-dma precedence: while an interrupt service is
    // mid-flight (its PC pushes not yet complete), the M-cycle-boundary HDMA fire
    // is suppressed so the block fires AFTER the pushes (memory.cpp:312-320). Set
    // by `service_interrupt` around the pushes, cleared once it fires the block.
    #[serde(skip, default)]
    hdma_mcycle_fire_suppressed: bool,

    // Late-hdma-vs-interrupt unhalt precedence: set at unhalt when a Low-at-halt
    // HDMA block did NOT reflag (Gambatte `isHdmaPeriod(cc)` false at unhalt), so
    // its m0-edge falls within the immediately-following interrupt service window.
    // The service then suppresses+reorders that block to fire AFTER its PC pushes
    // (the `late_hdma_vs_tima_*_halt_2` content tests: copy the pushed 0x11C9).
    // Cleared once consumed by the service (or the next unhalt).
    #[serde(skip, default)]
    hdma_unhalt_noreflag_deferred: bool,

    // Next-M-cycle dma() scheduling for the IME-off HALT-bug resume. A block
    // reflagged at unhalt fires (in Gambatte) at the instruction boundary AFTER
    // the resume instruction (`intevent_dma` runs after the opcode completes), so
    // its VRAM write lands AFTER the resume instruction's own memory read. The
    // synchronous m0-edge fire instead lands DURING the resume instruction,
    // ahead of that read (hdma_late_if_and_ie_halt_1: the `ld a,(80FA)` read sees
    // the post-DMA byte 0x02 instead of the pre-DMA 0x00). Set at the unhalt
    // reflag, this suppresses the synchronous fire across the resume instruction
    // and fires the held block at the next boundary.
    #[serde(skip, default)]
    hdma_unhalt_reflag_deferred: bool,

    // C7-full late-hdma-vs-interrupt re-order: the master_cc at which the most
    // recent m0-edge HDMA block fired (read its 16 source bytes). Gambatte orders
    // the `intevent_dma` (HDMA, flagged at `m0Time`) vs `intevent_interrupts`
    // race by event time: DMA wins only when `m0Time <= minIntTime_` (the
    // interrupt's serviceable cc). rustyboi fires the block greedily the dot the
    // m0-edge is reached — one or two cc BEFORE the interrupt-triggering
    // instruction's boundary — so when an interrupt dispatches within the same
    // M-cycle window the block wrongly read pre-push memory. `service_interrupt`
    // compares this against its access cc and, when the interrupt won the race,
    // re-runs the block AFTER the pushes (the `late_hdma_vs_*` content tests).
    // None when no block is in-flight for the current period.
    #[serde(skip, default)]
    hdma_last_fire_cc: Option<u64>,
    // Snapshot of (source, dest, length, enabled) captured immediately BEFORE the
    // last m0-edge block fired, so the late-hdma-vs re-order can restore the
    // pre-fire pointers and re-run the block reading post-push memory.
    #[serde(skip, default)]
    hdma_pre_fire_state: Option<(u16, u16, u8, bool)>,

    // True when the HDMA block was already set up (FF55 written, `hdma_enabled`) at
    // HALT entry. Distinguishes the `hdma_*halt_*_ly_*`/`inc_*` family (HDMA armed in
    // the m3halt ISR BEFORE the HALT; the value-read is a downstream post-unhalt FF44
    // -> drop the +6 stall fudge) from `hdma_cycles_2` (FF55 written in the wakeup
    // ISR AFTER the HALT; the immediate FF41 STAT read needs the +6).
    #[serde(skip, default)]
    hdma_enabled_at_halt: bool,

    // CGB palette state
    #[serde(with = "serde_bytes")]
    bg_palette_ram: [u8; 64],    // 8 palettes × 4 colors × 2 bytes = 64 bytes
    #[serde(with = "serde_bytes")]
    obj_palette_ram: [u8; 64],   // 8 palettes × 4 colors × 2 bytes = 64 bytes
    bg_palette_spec: u8,         // BCPS register
    obj_palette_spec: u8,        // OCPS register
    
    // CGB feature enablement
    cgb_features_enabled: bool, // Whether CGB-specific features should be active
}

impl Mmio {
    pub fn new() -> Self {
        Mmio {
            bios: None,
            cartridge: None,
            input: input::Input::new(),
            vram: memory::Memory::new(),
            wram: memory::Memory::new(),
            wram_bank: memory::Memory::new(),
            oam: memory::Memory::new(),
            oam_high: [0; 0x60],
            timer: timer::Timer::new(),
            serial: serial::Serial::new(),
            delayed_writes: Vec::new(),
            io_registers: memory::Memory::new(),
            hram: memory::Memory::new(),
            ie_register: 0,
            audio: audio::Audio::new(),
            dma_active: false,
            dma_source_base: 0,
            dma_pos: 0xFE,
            dma_start_pos: 0,
            dma_subcycle: 0,
            oam_write_pending: false,
            pending_oam_zero: std::cell::Cell::new(-1),
            ly_write_pending: false,
            stat_register_write_pending: false,
            ff41_write_pending: false,
            cpu_t_phase: 0,

            // CGB-specific fields initialization
            vram_bank: 0,
            wram_bank_select: 1, // CGB starts with WRAM bank 1 selected
            
            // CGB speed switching initialization
            key0_locked: false,    // Unlocked at boot, locked after boot ROM finishes
            key0_dmg_mode: false,  // Default to full CGB mode
            key1_current_speed: false, // Start in normal speed mode
            key1_switch_armed: false,  // No speed switch armed initially
            vram_bank1: memory::Memory::new(),
            wram_banks: (0..6).map(|_| memory::Memory::new()).collect(), // Banks 2-7
            hdma_source: 0,
            hdma_dest: 0,
            hdma_length: 0,
            hdma_enabled: false,
            pending_dma_stall: 0,
            dma_prefetch_stat_bias: false,
            oam_dma_stall_suppress: 0,
            halt_oam_grace: 0,
            oam_dma_stop_freeze: false,
            hdma_req_pending: false,
            halt_hdma_state: HaltHdmaState::Low,
            halt_wakeup_skew: false,
            halt_wakeup_hdma: false,
            hdma_is_in_period_cached: false,
            hdma_prev_stat_mode: 0,
            hdma_prev_period: false,
            cpu_halted: false,
            in_stop_window: false,
            hdma_block_done_this_period: false,
            hdma_halt_edge_consumed: false,
            hdma_high_unhalt_consume: false,
            hdma_peraccess_consume_pending: false,
            hdma_pending_writes: Vec::new(),
            hdma_write_delay: 0,
            hdma_kick_eval_pending: 0,
            hdma_disable_fires: None,
            hdma_mcycle_fire_suppressed: false,
            hdma_unhalt_noreflag_deferred: false,
            hdma_unhalt_reflag_deferred: false,
            hdma_last_fire_cc: None,
            hdma_pre_fire_state: None,
            hdma_enabled_at_halt: false,

            // CGB palette initialization
            bg_palette_ram: [0; 64],
            obj_palette_ram: [0; 64],
            bg_palette_spec: 0,
            obj_palette_spec: 0,
            
            cgb_features_enabled: false, // Will be set when cartridge is inserted
        }
    }

    pub fn reset(&mut self) {
        let mut new = Self::new();
        // Move (rather than clone) the bios and cartridge into the fresh
        // MMIO. The cartridge owns the open `.sav` file handle for
        // battery-backed carts; `Cartridge::Clone` deliberately drops that
        // handle, so cloning here would silently disable persistent save
        // writes after every reset/restart (including the implicit reset
        // performed by the GUI's "Load ROM" path).
        new.bios = self.bios.take();
        new.cartridge = self.cartridge.take();
        *self = new;
    }

    pub fn insert_cartridge(&mut self, cartridge: cartridge::Cartridge) {
        self.cartridge = Some(cartridge);
    }
    
    pub fn set_cgb_features_enabled(&mut self, enabled: bool) {
        self.cgb_features_enabled = enabled;
    }
    
    pub fn is_cgb_features_enabled(&self) -> bool {
        self.cgb_features_enabled
    }

    pub fn read_bg_palette_data(&self, palette_idx: u8, color_idx: u8) -> (u8, u8) {
        if !self.cgb_features_enabled || palette_idx >= 8 || color_idx >= 4 {
            return (0xFF, 0xFF); // Invalid access
        }
        
        let offset = (palette_idx * 8 + color_idx * 2) as usize;
        if offset + 1 < 64 {
            (self.bg_palette_ram[offset], self.bg_palette_ram[offset + 1])
        } else {
            (0xFF, 0xFF)
        }
    }
    
    pub fn read_obj_palette_data(&self, palette_idx: u8, color_idx: u8) -> (u8, u8) {
        if !self.cgb_features_enabled || palette_idx >= 8 || color_idx >= 4 {
            return (0xFF, 0xFF); // Invalid access
        }
        
        let offset = (palette_idx * 8 + color_idx * 2) as usize;
        if offset + 1 < 64 {
            (self.obj_palette_ram[offset], self.obj_palette_ram[offset + 1])
        } else {
            (0xFF, 0xFF)
        }
    }
    
    /// Seed the CGB power-on palette RAM (libgambatte initstate.cpp). The boot
    /// ROM leaves BG palette RAM all-white (0x7FFF) and OBJ palette RAM holding
    /// a fixed hardware power-on dump (`cgbObjpDump`). Games (and hwtests) that
    /// render sprites/BG without writing FF68-FF6B observe these values, so
    /// skip_bios must reproduce them instead of all-zero (black).
    pub fn set_post_bios_cgb_palettes(&mut self) {
        // BG palette RAM: every color = 0x7FFF (0xFF, 0x7F).
        for i in (0..64).step_by(2) {
            self.bg_palette_ram[i] = 0xFF;
            self.bg_palette_ram[i + 1] = 0x7F;
        }
        // OBJ palette RAM: Gambatte cgbObjpDump.
        const CGB_OBJP_DUMP: [u8; 64] = [
            0x00, 0x00, 0xF2, 0xAB, 0x61, 0xC2, 0xD9, 0xBA,
            0x88, 0x6E, 0xDD, 0x63, 0x28, 0x27, 0xFB, 0x9F,
            0x35, 0x42, 0xD6, 0xD4, 0x50, 0x48, 0x57, 0x5E,
            0x23, 0x3E, 0x3D, 0xCA, 0x71, 0x21, 0x37, 0xC0,
            0xC6, 0xB3, 0xFB, 0xF9, 0x08, 0x00, 0x8D, 0x29,
            0xA3, 0x20, 0xDB, 0x87, 0x62, 0x05, 0x5D, 0xD4,
            0x0E, 0x08, 0xFE, 0xAF, 0x20, 0x02, 0xD7, 0xFF,
            0x07, 0x6A, 0x55, 0xEC, 0x83, 0x40, 0x0B, 0x77,
        ];
        self.obj_palette_ram = CGB_OBJP_DUMP;
    }

    pub fn read_vram_bank1(&self, addr: u16) -> u8 {
        if !self.cgb_features_enabled || addr < VRAM_START || addr > VRAM_END {
            return 0xFF; // Invalid access
        }
        
        self.vram_bank1.read(addr)
    }

    /// Read from specific VRAM bank for debugging purposes
    pub fn read_vram_bank(&self, bank: u8, addr: u16) -> u8 {
        if addr < VRAM_START || addr > VRAM_END {
            return 0xFF; // Invalid address
        }
        
        match bank {
            0 => self.vram.read(addr),
            1 => {
                if self.cgb_features_enabled {
                    self.vram_bank1.read(addr)
                } else {
                    0xFF // Bank 1 doesn't exist on DMG
                }
            }
            _ => 0xFF, // Invalid bank
        }
    }

    pub fn get_cartridge(&self) -> Option<&cartridge::Cartridge> {
        self.cartridge.as_ref()
    }

    pub fn load_bios(&mut self, path: &str) -> Result<(), io::Error> {
        let data = fs::read(path)?;
        let mut bios = memory::Memory::new();
        if data.len() < BIOS_SIZE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "BIOS file too small"));
        }
        if data.len() > BIOS_SIZE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "BIOS file too large"));
        }
        for (i, &byte) in data.iter().take(BIOS_SIZE).enumerate() {
            bios.write(BIOS_START + i as u16, byte);
        }
        self.bios = Some(bios);
        Ok(())
    }

    pub fn has_bios(&self) -> bool {
        self.bios.is_some()
    }

    pub fn step_timer(&mut self) {
        let mut timer = self.timer.clone();
        timer.step(self);
        self.timer = timer;
    }

    /// per-access STAGE 1 (min-event idle fast path): true when the whole world is
    /// idle except the timer+serial, so a span of dots can be bulk-skipped to the
    /// next scheduled event without losing any per-dot peripheral side effect.
    /// Requires: LCD off (no PPU renderer / mode edges / STAT-IRQ schedule),
    /// no OAM-DMA in flight, no HDMA armed, APU powered off (its channels step per
    /// dot), no deferred HDMA block writes, no OAM-DMA stall catch-up, no halt OAM
    /// grace, and no queued delayed register writes. Under all of these only
    /// `step_timer` and `step_serial` advance, and both are span-collapsible
    /// (the timer via `Timer::step_to`, serial via its phase-based `step`). The CPU
    /// T-phase parity only gates the PPU, which is off here, so collapsing it is a
    /// no-op. This is purely an advance-mechanism optimization: the per-dot
    /// fallback in `Bus::run_to` handles every cc the guard rejects, so behavior is
    /// byte-identical to the per-dot crank.
    pub fn idle_bulk_skippable(&self) -> bool {
        let lcd_on = self.io_registers.read(ppu::LCD_CONTROL)
            & (ppu::LCDCFlags::DisplayEnable as u8)
            != 0;
        !lcd_on
            && !self.dma_active
            && self.oam_dma_stall_suppress == 0
            && self.halt_oam_grace == 0
            && !self.hdma_enabled
            && !self.hdma_req_pending
            && !self.audio.is_powered()
            && !self.serial.is_active()
            && self.delayed_writes.is_empty()
            && !self.has_pending_hdma_deferred()
    }

    /// per-access STAGE 1: bulk-advance the timer+serial to `target_cc` in one shot
    /// (only call when `idle_bulk_skippable()` held for the entire span). Mirrors
    /// the order `resolve_one_dot` uses (timer, then serial) so the net effect is
    /// byte-identical to cranking each dot. `master_cc` is `timer.abs_cc`, so the
    /// timer's `step_to` carries the master cc to the target and the serial step
    /// observes the final phase.
    pub fn bulk_advance_idle(&mut self, target_cc: u64) {
        let dots = target_cc.wrapping_sub(self.master_cc());
        let mut timer = self.timer.clone();
        timer.step_to(target_cc, self);
        self.timer = timer;
        // Serial is phase-based: stepping once at the final phase shifts the same
        // bits and (if it completed) fires the IRQ exactly as the per-dot path. The
        // guard already requires it inactive, so this is a defensive no-op.
        self.step_serial();
        // The per-dot path advances `cpu_t_phase` once per dot; collapse the same
        // count so the T-phase parity that gates the (currently off) PPU stays
        // exactly where the per-dot crank would have left it.
        self.cpu_t_phase = self.cpu_t_phase.wrapping_add(dots);
    }

    /// Write a timer register, then immediately deliver any glitch IRQ the write
    /// scheduled (Gambatte flags it inline at the write cc). The write resolves
    /// at the timer's current `abs_cc`, which the CPU positions at the access
    /// start cc.
    pub fn write_timer(&mut self, addr: u16, value: u8) {
        let mut timer = self.timer.clone();
        timer.write(addr, value);
        let irq = timer.take_pending_irq();
        self.timer = timer;
        if irq {
            self.request_interrupt(cpu::registers::InterruptFlag::Timer);
        }
    }

    pub fn step_serial(&mut self) {
        // Serial now runs on the master cc (`abs_cc`), the SAME clock the timer
        // DIV/TIMA and APU derive from — no separate `cpu_t_phase` parallel
        // clock (M8 serial merge). `abs_cc` is advanced at the start of the
        // timer step within this same dot's tick, so it is the live cc here.
        let phase = self.timer.abs_cc();
        let mut serial = self.serial.clone();
        serial.step(phase, self);
        self.serial = serial;
    }

    /// SC (FF02) write: latches the value, then (re)schedules the transfer event
    /// using the timer counter and the canonical WRITE access cc (M8). The write
    /// resolves at the access START cc (bus.rs routes FF02 to the start-cc path),
    /// so an abort (SC bit-0 cleared) lands before this access's `step_serial`
    /// can fire a completion the abort must suppress.
    pub fn write_serial_sc(&mut self, value: u8) {
        let divider = self.timer.internal_counter();
        let phase = self.timer.write_access_cc();
        self.serial.schedule_sc(value, divider, phase);
    }

    pub fn set_serial_cgb(&mut self, cgb: bool) {
        self.serial.set_cgb(cgb);
    }

    /// Snapshot a serial-influenced register (SB/SC/IF) at the read M-cycle
    /// start cc, mirroring `sync_apu_for_read`. The per-dot `step_serial` during
    /// `tick_m` can complete the transfer and set the serial IF bit within the
    /// read cycle; capturing the value before ticking makes the CPU observe
    /// serial state at the read's start (Gambatte resolves the read at cc).
    pub fn snapshot_serial_read(&self, addr: u16) -> u8 {
        self.read(addr)
    }

    /// Raise an interrupt by setting its IF bit. Equivalent to
    /// `SM83::set_interrupt_flag(flag, true, self)` but needs no CPU borrow, so
    /// peripherals (PPU) can request interrupts directly.
    pub fn request_interrupt(&mut self, flag: cpu::registers::InterruptFlag) {
        let current = self.read(cpu::registers::INTERRUPT_FLAG);
        self.write(cpu::registers::INTERRUPT_FLAG, current | flag as u8);
    }

    /// Queue a CPU write to land `cycles_until_write` T-cycles later (0 = now).
    /// Models the sub-instruction landing cycle of certain register writes.
    pub fn queue_delayed_write(&mut self, addr: u16, value: u8, cycles_until_write: u32) {
        if cycles_until_write > 0 {
            self.delayed_writes.push(DelayedMmioWrite {
                addr,
                value,
                cycles_remaining: cycles_until_write,
            });
        } else {
            self.write(addr, value);
        }
    }

    pub fn step_delayed_writes(&mut self) -> Vec<AppliedMmioWrite> {
        let mut applied = Vec::new();
        let mut index = 0;
        while index < self.delayed_writes.len() {
            if self.delayed_writes[index].cycles_remaining > 0 {
                self.delayed_writes[index].cycles_remaining -= 1;
            }
            if self.delayed_writes[index].cycles_remaining == 0 {
                let write = self.delayed_writes.remove(index);
                self.write(write.addr, write.value);
                applied.push(AppliedMmioWrite {
                    addr: write.addr,
                    value: write.value,
                });
            } else {
                index += 1;
            }
        }
        applied
    }

    pub fn clear_delayed_writes(&mut self) {
        self.delayed_writes.clear();
    }

    pub fn clock_apu_frame_sequencer(&mut self) {
        self.audio.clock_frame_sequencer();
    }

    /// Initialize the timer's internal 16-bit counter at boot. See
    /// `Timer::set_internal_counter`.
    pub fn set_timer_internal_counter(&mut self, value: u16) {
        self.timer.set_internal_counter(value);
    }

    /// Write a raw byte into the generic IO-register backing store, bypassing
    /// per-register write masking. Used by `skip_bios` to seed power-on values
    /// (e.g. RP unused bits) that the masked write path cannot set.
    pub fn set_io_register(&mut self, addr: u16, value: u8) {
        self.io_registers.write(addr, value);
    }

    /// Establish the post-`skip_bios` APU state. Syncs the APU cycle counter from
    /// the (already-set) timer counter first so the channel duty phase has the
    /// correct cc base, then applies Gambatte's post-boot state.
    pub fn set_post_bios_audio_state(&mut self, cgb: bool) {
        self.sync_apu_cc();
        self.audio.set_post_bios_state(cgb);
    }

    /// Record the CGB flag for the APU boot anchor. Must run before any audio
    /// register write or `sync_apu_cc` that would anchor the SPU clock.
    pub fn set_audio_boot_cgb(&mut self, cgb: bool) {
        self.audio.set_boot_cgb(cgb);
    }

    pub fn step_audio(&mut self) {
        self.sync_apu_cc();
        let mut audio = self.audio.clone();
        audio.step(self);
        self.audio = audio;
    }

    /// Push the APU's 2 MHz cycle-counter inputs to the audio unit: the
    /// frame-sequencer step (FS phase, maintained independently of DIV writes)
    /// and the timer's internal counter (sub-step position). The controller
    /// reconstructs Gambatte's `cycleCounter_` from these.
    fn sync_apu_cc(&mut self) {
        let ds = self.is_double_speed_mode();
        self.sync_apu_cc_with_ds(ds);
    }

    /// Like `sync_apu_cc`, but with an explicit double-speed flag. Gambatte's
    /// `PSG::speedChange` calls `generateSamples(cpuCc, isDoubleSpeed())` with
    /// the speed being LEFT, BEFORE the KEY1 toggle — so the flush to the switch
    /// cc must use the OLD speed's `>>(1+ds)` rate, not the just-toggled one.
    fn sync_apu_cc_with_ds(&mut self, ds: bool) {
        let abs_cc = self.timer.abs_cc();
        let div_resets = self.timer.div_reset_count();
        let div_anchor = self.timer.div_anchor();
        self.audio.sync_cc(abs_cc, div_resets, div_anchor, ds);
    }

    /// Sync the APU cycle counter to the exact CPU read cycle and advance the
    /// wave channel's fetch position, so an APU/wave-RAM read observes the
    /// channel at the precise sub-M-cycle (Gambatte evaluates waveRamRead with
    /// the live cc). Only used on the read path (0xFF10-0xFF3F).
    pub fn sync_apu_for_read(&mut self) {
        self.sync_apu_cc();
        self.audio.sync_wave_for_read();
    }

    /// Resolve the APU length subsystem at the canonical CPU-access cc (M7).
    /// `read_abs_cc` is the master cc at the access point — the SAME value the
    /// timer register access resolves on (`abs_cc + ACCESS_CC_OFF`). Drives the
    /// length-expiry comparison off one uniform per-access cc, with no
    /// APU-specific additive constant.
    pub fn sync_apu_read_cc(&mut self, read_abs_cc: u64) {
        self.sync_apu_cc();
        self.audio.sync_wave_for_read();
        self.audio.set_read_len_cc(read_abs_cc);
    }

    /// Resolve the APU length subsystem at the canonical CPU WRITE access cc
    /// (M8). Overlays `len_cc` to the write cc, then runs the actual register
    /// write (whose NRx1/NRx4 length math consumes the overlaid cc), then
    /// restores the steady-state base. Mirrors `sync_apu_read_cc` for the read
    /// side: the trigger's length-expiry boundary is anchored to one uniform
    /// per-access clock, dissolving the write/read phase asymmetry.
    pub fn write_apu(&mut self, addr: u16, value: u8) {
        self.sync_apu_cc();
        // Gambatte's `waveRamWrite` runs `updateWaveCounter(cc)` before the write
        // so the corruption/active-fetch window (`waveCounter_ == cc+1`) and the
        // overwritten wave-RAM byte are resolved at the live fetch position. The
        // per-dot `step` only advances the fetch on whole dots; sync it to the
        // exact write cc here, matching the read path.
        if (audio::WAV_START..=audio::WAV_END).contains(&addr) {
            self.audio.sync_wave_for_read();
        }
        let write_cc = self.timer.write_access_cc();
        self.audio.set_write_len_cc(write_cc);
        self.audio.write(addr, value);
        self.audio.restore_len_cc();
    }

    /// The canonical CPU-access cc the timer resolves register accesses on.
    /// Exposed so the bus can present the SAME cc to the APU/serial reads,
    /// dissolving the per-peripheral phase constants (M7).
    pub fn access_cc(&self) -> u64 {
        self.timer.access_cc()
    }

    /// STAGE 2 (RB_FAITHFUL) event-cc dispatch: the cc the most recent still-
    /// undispatched TIMA IRQ fired at, or `None`. The CPU gates timer-interrupt
    /// eligibility on the boundary access cc having reached this cc.
    pub fn pending_timer_fire_cc(&self) -> Option<u64> {
        self.timer.pending_fire_cc()
    }

    /// Delivery cc of the next scheduled timer overflow (EI-loop fast-dispatch).
    pub fn next_timer_overflow_cc(&self) -> Option<u64> {
        self.timer.next_overflow_deliver_cc()
    }

    /// EARLY (EI-loop) anchor cc of the next scheduled overflow (schedCc + IF_OFF).
    pub fn next_timer_overflow_ei_cc(&self) -> Option<u64> {
        self.timer.next_overflow_ei_cc()
    }

    /// per-access STAGE 1: the EXACT cc the next timer overflow's IF bit is raised
    /// at, with the same `fold` `step_to`/`update_irq_delivery` will apply. The
    /// min-event idle fast path lands on this cc so the overflow fires identically.
    pub fn next_timer_overflow_fire_cc(&self) -> Option<u64> {
        self.timer.next_overflow_fire_cc(self.cpu_is_halted())
    }

    /// EARLY (EI-loop) gate cc of the undispatched timer IRQ.
    pub fn pending_timer_fire_cc_ei(&self) -> Option<u64> {
        self.timer.pending_fire_cc_ei()
    }

    /// EI-loop fast timer delivery: fire any imminent overflow at the early anchor
    /// (`boundary >= schedCc + IF_OFF`) and raise its IF bit. Called by the CPU in
    /// a non-halt/non-stop EI loop so the serviced ISR runs on Gambatte's exact
    /// phase, ahead of the normal `CC_OFF`-late per-dot delivery.
    pub fn force_ei_timer_delivery(&mut self, boundary: u64) {
        let mut timer = self.timer.clone();
        let fired = timer.force_ei_delivery(boundary);
        self.timer = timer;
        if fired {
            let mut t = self.timer.clone();
            t.flush_pending_irq(self);
            self.timer = t;
        }
    }

    /// STAGE 2: clear the recorded timer fire cc once the CPU dispatches the IRQ.
    pub fn clear_timer_fire_cc(&mut self) {
        self.timer.clear_fire_cc();
    }

    /// Mirror of `intreq_.halted()`: true while the CPU is in HALT. The FAST EI-loop
    /// timer IF-set grid keeps the HALT-wakeup IF-set on the late (`CC_OFF`) anchor
    /// while non-halt (EI-loop) overflows use the early grid.
    pub fn cpu_is_halted(&self) -> bool {
        self.cpu_halted
    }

    /// FAST EI-loop: is the current ISR running on the early IF-set grid? The bus
    /// uses this to sample the timer IF bit at the access cc (rather than the
    /// M-cycle end) so a read-only early-grid ISR (tc00_irq_ds_1) still misses an
    /// overflow whose early IF-set has not yet been reached at the read cc.
    pub fn timer_isr_on_early_grid(&self) -> bool {
        self.timer.isr_on_early_grid()
    }

    /// CL1: the *honest* per-access cc — the true `abs_cc` at the START of the
    /// CPU access's M-cycle. `master_cc()` is incremented at the top of each
    /// dot-step, so before this access's `tick_m` it trails the M-cycle start by
    /// exactly one dot; the true start is `master_cc + 1` (Gambatte resolves the
    /// access at `cc`, then `cc += 4`). The old `access_cc()` = `master_cc + 5`
    /// resolved the access at its END (`+4`) plus the same `+1` lag — a fixed
    /// offset that is right on average but off by the intra-instruction position.
    /// The PPU read-cc / access-gating consumers anchor here so CL2 (ISR-dispatch
    /// cc) and CL3 (opcode granularity) can vary the true access cc and have the
    /// PPU respond at the exact point, instead of a baked-in `+5`. The four-dot
    /// difference vs `access_cc()` is folded into the PPU consumer constants
    /// (`get_stat_mode3to0_at_cc`, `cpu_access_blocked`) so this is net-zero.
    pub fn ppu_access_cc(&self) -> u64 {
        self.timer.abs_cc().wrapping_add(1)
    }

    /// The raw master clock (`cc`, T-cycles) the whole engine advances. The PPU
    /// derives its dot-cycles from this against the LCD-enable anchor `p_now`
    /// (Gambatte: PPU dot-cycles = `(cc - p_now) >> ds`).
    pub fn master_cc(&self) -> u64 {
        self.timer.abs_cc()
    }

    pub fn generate_audio_samples(&mut self, cpu_cycles: u32) -> Vec<(f32, f32)> {
        let mut audio = self.audio.clone();
        let samples = audio.generate_samples(self, cpu_cycles);
        self.audio = audio;
        samples
    }

    /// Copy a single byte from `src` to the VRAM destination corresponding
    /// to `dst`. Shared by GDMA and HDMA. Caller advances `hdma_source` /
    /// `hdma_dest`. Mirrors the inner-loop of Gambatte's `Memory::dma`:
    ///   - Source reads from VRAM (0x8000-0x9FFF) or >=0xE000 (WRAM
    ///     mirror / OAM / IO / HRAM) return 0xFF (open bus).
    ///   - Destination wraps within the currently selected VRAM bank
    ///     (modulo 0x2000), written at 0x8000 | (dst & 0x1FFF).
    fn copy_dma_byte(&mut self, src: u16, dst: u16) -> u8 {
        // Bypass DMA-active gating while we drive the bus read internally:
        // GDMA / HDMA are separate transfer engines from OAM DMA.
        let saved_dma_active = self.dma_active;
        self.dma_active = false;

        let byte = if (0x8000..=0x9FFF).contains(&src) || src >= 0xE000 {
            0xFF
        } else {
            <Self as memory::Addressable>::read(self, src)
        };

        let vram_addr = VRAM_START | (dst & 0x1FFF);
        if self.cgb_features_enabled && self.vram_bank == 1 {
            self.vram_bank1.write(vram_addr, byte);
        } else {
            self.vram.write(vram_addr, byte);
        }

        self.dma_active = saved_dma_active;
        byte
    }

    /// Resolve a single HDMA byte WITHOUT committing the VRAM write. Reads the
    /// source byte at the current (fire) cc — matching Gambatte's read-at-cc —
    /// and returns `(vram_addr, value, into_bank1)` for a deferred apply. Used by
    /// the deferred-block model so the VRAM write lands at the correct sub-M-cycle
    /// (`fire_cc + hdma_write_delay`) rather than coincident with the trigger.
    fn resolve_dma_byte(&mut self, src: u16, dst: u16) -> (u16, u8, bool) {
        let saved_dma_active = self.dma_active;
        self.dma_active = false;
        let byte = if (0x8000..=0x9FFF).contains(&src) || src >= 0xE000 {
            0xFF
        } else {
            <Self as memory::Addressable>::read(self, src)
        };
        self.dma_active = saved_dma_active;
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

    /// per-access STAGE 1: true while deferred HDMA block writes are still in their
    /// per-dot countdown (`step_hdma_deferred` must run each dot to commit them at
    /// the right cc). Blocks the idle bulk-skip.
    pub fn has_pending_hdma_deferred(&self) -> bool {
        !self.hdma_pending_writes.is_empty()
    }

    /// Drain the deferred-HDMA write buffer one dot. When the delay expires the
    /// resolved bytes are committed to VRAM in order.
    pub fn step_hdma_deferred(&mut self) {
        if self.hdma_pending_writes.is_empty() {
            return;
        }
        if self.hdma_write_delay > 0 {
            self.hdma_write_delay -= 1;
        }
        if self.hdma_write_delay == 0 {
            let pending = std::mem::take(&mut self.hdma_pending_writes);
            for (addr, byte, into_bank1) in pending {
                self.apply_dma_write(addr, byte, into_bank1);
            }
        }
    }

    /// Execute a CGB General-Purpose DMA (GDMA) transfer synchronously.
    /// Copies `length` bytes from `self.hdma_source` into VRAM starting at
    /// `self.hdma_dest`. Mirrors Gambatte's `Memory::dma`:
    ///   - If the LCD is off, GDMA does not run.
    ///   - Destination clamped if it would overflow the 16-bit address
    ///     space (memory.cpp:335-337).
    fn execute_gdma(&mut self, length: usize) {
        // Gambatte `Memory::dma` (memory.cpp:332-343): the `length` bytes are
        // transferred regardless of LCD state. The LCD-off branch only zeroes the
        // *remaining* HDMA block count (`dmaLength`), it does NOT skip the active
        // transfer. A pure GDMA kick therefore still copies its bytes (and
        // interleaves the OAM DMA) with the LCD off. Skipping it here used to drop
        // the GDMA conflict on LCD-off re-runs of the oamdumper tests, letting a
        // clean OAM-DMA pass overwrite the conflict bytes.

        let mut src = self.hdma_source;
        let mut dst = self.hdma_dest;

        let effective_length = if (dst as usize) + length >= 0x10000 {
            0x10000 - dst as usize
        } else {
            length
        };

        let ds = self.is_double_speed_mode();
        let per_byte_cc: i64 = if ds { 4 } else { 2 };

        // OAM-DMA interleave (Gambatte `Memory::dma`). The OAM-DMA engine keeps
        // advancing one M-cycle (4 cc) per `lOam += 4` step while the GDMA copies
        // bytes. The bus ran one `tick_m` (step_dma) before resolving this FF55
        // write, leaving rustyboi's `dma_pos` one M-cycle BEHIND Gambatte's
        // `oamDmaPos_` at the kick instant. Catch up by one M-cycle (advance the
        // OAM-DMA position without a conflict write) so the gate below fires on
        // the same boundaries Gambatte does.
        // A block fired inside the STOP speed-switch unhalt window must NOT advance
        // the OAM-DMA: Gambatte's `Memory::dma` interleaves via `updateOamDma`, which
        // takes its `halted()` branch (oamDmaPos_ frozen) while the CPU is
        // `intreq_.halt()`ed across the STOP. rustyboi's `step_dma` already honors
        // `oam_dma_stop_freeze`, but the synchronous HDMA-block interleave here
        // bypassed it, advancing oamDmaPos_ ~16 bytes and shifting the post-switch
        // in-flight conflict byte (hdma_transition_speedchange_oamdma: read 0x60
        // where Gambatte's frozen position reads 0x71).
        let interleave = self.dma_active && !self.oam_dma_stop_freeze;
        if interleave {
            self.dma_advance_one_mcycle();
        }
        // `lOam` mirrors Gambatte's relative `lastOamDmaUpdate_`: it starts at
        // `-dma_subcycle` (dots already elapsed in the current M-cycle) and the
        // per-byte cc advance is compared against `lOam + 3` (gate `cc-3 > lOam`).
        let mut cc: i64 = 0;
        let mut loam: i64 = -(self.dma_subcycle as i64);

        for _ in 0..effective_length {
            let data = self.copy_dma_byte(src, dst);
            cc += per_byte_cc;
            if interleave && self.dma_active && cc - 3 > loam {
                loam += 4;
                self.dma_conflict_advance(src, data);
            }
            src = src.wrapping_add(1);
            dst = dst.wrapping_add(1);
        }
        // After the block, the OAM-DMA continues from the advanced position. The
        // residual `lOam` phase becomes the next M-cycle's sub-cycle offset so
        // `step_dma` resumes on the correct dot (mirrors Gambatte storing
        // `lastOamDmaUpdate_ = lOam`).
        if interleave && self.dma_active {
            // Dots elapsed since the last OAM-DMA M-cycle fired. `step_dma` fires
            // when `dma_subcycle` reaches 4, so the residual phase `(cc - loam)`
            // (mod 4) is exactly the count already accrued toward the next
            // M-cycle (mirrors Gambatte storing `lastOamDmaUpdate_ = lOam` and
            // recomputing `(cc - lastOamDmaUpdate_) >> 2`).
            self.dma_subcycle = (cc - loam).rem_euclid(4) as u8;
        }

        self.hdma_source = src;
        self.hdma_dest = dst;

        // Gambatte `Memory::dma` charges `2 + 2*ds` cc per byte for the entire
        // transfer plus a single trailing `cc += 4`, regardless of block count
        // (the +4 setup is NOT per-block). For one block this is 36 SS / 68 DS.
        // Gambatte runs GDMA as an event preceded by `Interrupter::prefetch`
        // (the next opcode is fetched *before* the transfer's cc advance) and a
        // trailing `cc += 4`. Synchronous GDMA here charges the transfer up
        // front, so the post-stall return must absorb that prefetch/setup
        // overlap; +5 lands the next STAT-mode read on the exact mode-0 dot for
        // the gdma_cycles boundary pairs (the PPU position trailed the synced
        // master cc by ~1 dot at the read with the old +6 — see fix-gdma).
        //
        // Two back-to-back FF55=0 kicks (gdma_cycles_2xshort) drain as effectively
        // ONE prefetch sequence: Gambatte's `Interrupter::prefetch` absorption
        // happens once across the pair, not per `dma()` event. The first kick set
        // `dma_prefetch_stat_bias` (its stall was drained, no STAT read has consumed
        // it yet); a second kick before that consumption must add only the raw
        // transfer + trailing setup (no second `+5`), else the post-stall STAT read
        // lands ~5 dots late at double speed (2xshort_ds_1 reads mode 0 where
        // Gambatte still reads mode 3 at `cc + 2 < m0Time`).
        let (per_byte, setup) = if self.is_double_speed_mode() { (4, 5) } else { (2, 4) };
        let prefetch_fudge = if self.dma_prefetch_stat_bias { 0 } else { 5 };
        self.pending_dma_stall += (effective_length as u32) * per_byte + setup + prefetch_fudge;
        // The OAM-DMA M-cycles for the transfer were folded into the loop above.
        // Suppress `step_dma` for Gambatte's true dma-event duration (the transfer
        // `per_byte` cc plus the single trailing `cc += 4`), NOT the extra `+5`
        // CPU-stall prefetch fudge. Gambatte freezes `lastOamDmaUpdate_` for the
        // event then catches the OAM-DMA up on the next `updateOamDma`; the residual
        // post-stall cc advance the OAM-DMA normally toward the next access.
        if interleave {
            self.oam_dma_stall_suppress = (effective_length as u32) * per_byte + 4;
        }
    }

    // ----------------------------------------------------------------------
    // HDMA accessors used by gb.rs / cpu / ppu.
    // ----------------------------------------------------------------------

    pub fn hdma_is_enabled(&self) -> bool {
        self.cgb_features_enabled && self.hdma_enabled
    }

    pub fn hdma_req_pending(&self) -> bool {
        self.hdma_req_pending
    }

    /// Remaining HDMA blocks minus one (the FF55 length field). 0 => the next
    /// block completes the transfer.
    pub fn hdma_length(&self) -> u8 {
        self.hdma_length
    }

    /// Arm the High-at-halt unhalt edge-consume: the first post-unhalt m0 HDMA edge
    /// is suppressed (it was the during-halt edge Gambatte already consumed). Called
    /// at the unhalt site when `haltHdmaState_ == High`.
    pub fn arm_hdma_high_unhalt_consume(&mut self) {
        self.hdma_high_unhalt_consume = true;
    }

    /// per-access STAGE 2 (FACET 3): arm the Requested-unhalt sub-block-cc consume.
    /// Called at the unhalt site when a `Requested`-at-halt block is reflagged so
    /// `step_hdma` can absorb the next-line m0 arm iff it falls within the freshly
    /// fired block's transfer span (Gambatte's m0 `memevent_hdma` consumed by the
    /// in-flight `dma()`), deferring the genuine next block one line.
    pub fn arm_hdma_peraccess_consume(&mut self) {
        self.hdma_peraccess_consume_pending = true;
    }

    /// per-access STAGE 2 (FACET 3): when a post-unhalt m0 rising edge would arm the
    /// next HDMA block, decide whether it must instead be CONSUMED because it falls
    /// within the just-fired (Requested-unhalt reflag) block's transfer span. Gambatte
    /// processes the m0 `memevent_hdma` at the in-flight `dma()`'s end cc, so an edge
    /// landing inside `[fire_cc, fire_cc + 16*(2+2*ds))` is absorbed and its block
    /// deferred to the next line; an edge at/after that span fires this line. Returns
    /// true (consume this arm, clearing the pending flag) iff a Requested-unhalt
    /// consume is armed and the current dot cc is strictly inside the span. Otherwise
    /// leaves the arm to proceed. The pending flag is one-shot: it is cleared whether
    /// the arm is consumed (inside span) or allowed (past span), so it only ever gates
    /// the single immediate post-unhalt m0 edge.
    fn peraccess_consume_m0_arm(&mut self) -> bool {
        if !self.hdma_peraccess_consume_pending {
            return false;
        }
        let fire_cc = match self.hdma_last_fire_cc {
            Some(c) => c,
            None => {
                self.hdma_peraccess_consume_pending = false;
                return false;
            }
        };
        let ds = self.is_double_speed_mode() as u64;
        let span: u64 = 0x10 * (2 + 2 * ds);
        let now = self.master_cc();
        // Inclusive end: Gambatte's m0 `memevent_hdma` lands AT the in-flight
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
            self.hdma_peraccess_consume_pending = false;
            false
        }
    }

    /// Master cc of the last HDMA block fire (None if none in-flight this period).
    pub fn hdma_last_fire_cc(&self) -> Option<u64> {
        self.hdma_last_fire_cc
    }

    /// C7-full: whether an HDMA block is latched and would fire at the next
    /// M-cycle boundary (the `fire_pending_hdma_mcycle` precondition).
    pub fn hdma_fire_pending(&self) -> bool {
        self.hdma_req_pending && self.hdma_enabled
    }

    pub fn set_hdma_req(&mut self) {
        if self.cgb_features_enabled && self.hdma_enabled {
            self.hdma_req_pending = true;
        }
    }

    pub fn ack_hdma_req(&mut self) {
        self.hdma_req_pending = false;
    }

    pub fn halt_hdma_state(&self) -> HaltHdmaState {
        self.halt_hdma_state
    }

    pub fn set_halt_hdma_state(&mut self, s: HaltHdmaState) {
        self.halt_hdma_state = s;
    }

    /// Freeze/unfreeze the OAM-DMA across the STOP speed-switch unhalt window
    /// (Gambatte `updateOamDma` `halted()` branch — `oamDmaPos_` stays put).
    pub fn set_oam_dma_stop_freeze(&mut self, freeze: bool) {
        self.oam_dma_stop_freeze = freeze;
    }

    /// CPU has left HALT. Clears the `intreq_.halted()` mirror so the
    /// period-edge `flagHdmaReq` resumes (video.h:41).
    pub fn clear_cpu_halt(&mut self) {
        self.cpu_halted = false;
    }

    /// True while the CGB STOP speed-switch unhalt window is open (Gambatte
    /// `intreq_.halt()`): the HDMA period-edge `flagHdmaReq` is suppressed across
    /// the speed bridge and stall (video.h:41). Set by `on_stop_window_enter`,
    /// cleared by `stop_window_exit_reflag`.
    pub fn in_stop_window(&self) -> bool {
        self.in_stop_window
    }

    /// C1: mark/clear that the live instruction stream was resumed by a HALT
    /// wakeup (its access-cc is sub-M-cycle skewed; see field doc). Set on wakeup,
    /// cleared when the CPU halts again.
    pub fn set_halt_wakeup_skew(&mut self, v: bool) {
        self.halt_wakeup_skew = v;
    }

    /// C1: true while a HALT-woken instruction stream is live (FF41 getStat-at-cc
    /// line-tail override is deferred to the renderer register).
    pub fn halt_wakeup_skew(&self) -> bool {
        self.halt_wakeup_skew
    }

    pub fn set_halt_wakeup_hdma(&mut self, v: bool) {
        self.halt_wakeup_hdma = v;
    }

    pub fn halt_wakeup_hdma(&self) -> bool {
        self.halt_wakeup_hdma
    }

    pub fn update_hdma_period_cache(&mut self, in_period: bool) {
        self.hdma_is_in_period_cached = in_period;
    }

    /// C7-full: resolve a pending FF55 bit7=1 kick (`hdma_kick_eval_pending`)
    /// against the LIVE HDMA-period predicate the bus evaluates at the write
    /// access cc (Gambatte `enableHdma` -> `isHdmaPeriod(cc + 4)`). If in period
    /// the first block is armed immediately; otherwise the kick is dropped and the
    /// block arms on the next Mode 3->0 edge (matching Gambatte scheduling
    /// `memevent_hdma` to the next m0 without flagging now). Returns whether a kick
    /// was pending (so the bus knows it consumed it).
    pub fn resolve_hdma_kick(&mut self, in_period: bool) -> bool {
        if self.hdma_kick_eval_pending == 0 {
            return false;
        }
        self.hdma_kick_eval_pending = 0;
        if in_period && self.hdma_enabled {
            self.hdma_req_pending = true;
            // DEFERRED-HDMA-FIRE: the kick services THIS period's block. Mark it
            // done so an immediately-following `halt` captures `haltHdmaState_ =
            // High` (Gambatte: in-period + already-serviced) rather than
            // `Requested`, which would re-fire a SECOND block on unhalt
            // (hdma_late_m0halt_*). The per-dot `hdma_period` is false mid-HBlank
            // (post-m0Time crossing) so `step_hdma`'s own block_done set never
            // fires for a kick-armed block; set it here at the in-period kick.
            self.hdma_block_done_this_period = true;
        }
        true
    }

    /// Whether an FF55 bit7=1 kick is awaiting the bus's live-period resolution.
    pub fn hdma_kick_eval_pending(&self) -> bool {
        self.hdma_kick_eval_pending != 0
    }

    /// Bus-supplied decision for the NEXT FF55 disable write: `Some(true)` => the
    /// m0 edge has already fired so the block must still run (do not cancel),
    /// `Some(false)`/`None` => cancel as before. Set just before the write.
    pub fn set_hdma_disable_fires(&mut self, v: Option<bool>) {
        self.hdma_disable_fires = v;
    }

    pub fn hdma_is_in_period_cached(&self) -> bool {
        self.hdma_is_in_period_cached
    }

    /// "In HDMA period" as seen by the unhalt re-flag gate. Uses the cycle-exact
    /// renderer period when available, else falls back to the FF41 STAT mode-0
    /// gate (matching `step_hdma`'s fallback edge model) so unhalt re-flagging
    /// works on the window / first-line paths where no closed-form mode-0 dot
    /// exists. LCD-off counts as permanently in period.
    pub fn hdma_in_period_for_unhalt(&self) -> bool {
        let lcdc = self.io_registers.read(ppu::LCD_CONTROL);
        let lcd_on = lcdc & (ppu::LCDCFlags::DisplayEnable as u8) != 0;
        if !lcd_on {
            return true;
        }
        if self.hdma_is_in_period_cached {
            return true;
        }
        (self.io_registers.read(ppu::LCD_STATUS) & 0x03) == 0
    }

    /// CPU has just entered HALT. Mirrors Gambatte's `Memory::halt`
    /// (memory.cpp:407): records the halt-HDMA state and acks any
    /// currently flagged req so it does not double-fire on unhalt.
    /// Coarse fallback (no PPU access): uses the cached per-step period.
    pub fn on_cpu_halt(&mut self) {
        let in_period = self.hdma_is_in_period_cached;
        self.on_cpu_halt_with_period(Some(in_period));
    }

    /// HALT-entry with a caller-supplied cycle-exact `isHdmaPeriod(cc)` (the Bus
    /// path computes this via the renderer's `m0_time_master`-anchored predictor,
    /// the SAME predicate c04d78a uses for the unhalt re-flag). `None` => no
    /// closed-form mode-0 anchor; fall back to the cached per-step period.
    pub fn on_cpu_halt_with_period(&mut self, in_period: Option<bool>) {
        self.on_cpu_halt_with_period_done(in_period, None)
    }

    /// As `on_cpu_halt_with_period`, with a caller-supplied `block_done_override`:
    /// whether the CURRENT period's HDMA block has ALREADY been serviced, derived
    /// from the last block-fire cc vs this line's mode-0 time rather than the live
    /// `hdma_block_done_this_period` flag. The flag is cleared by the per-dot
    /// `hdma_period` falling edge, whose line-END dot (`dot + 3 + 3*ds < 456`) sits
    /// a hair EARLIER than the HALT-entry predicate's end bracket (`depth < 208/410`)
    /// — so a HALT landing in that sliver sees the flag already reset and wrongly
    /// captures `Requested` (re-firing a spurious second block at unhalt) where
    /// Gambatte captures `High` (in-period, block done, no reflag). The fire-cc
    /// override is robust across that boundary disagreement
    /// (`hdma_late_m0halt_*_lcdoffset*_1`). `None` keeps the legacy flag behaviour.
    pub fn on_cpu_halt_with_period_done(
        &mut self,
        in_period: Option<bool>,
        block_done_override: Option<bool>,
    ) {
        self.cpu_halted = true;
        // FAST EI-loop: entering HALT ends any prior EI fast-dispatch stream; the
        // HALT-woken ISR observes the timer IF re-flag on the LATE grid.
        self.timer.clear_isr_early_grid();
        // Gambatte advances the OAM-DMA one M-cycle at halt entry (the HALT
        // instruction's own M-cycle, `updateOamDma(cc + 4)` before
        // `intreq_.halt()`); allow that single advance through the freeze. The
        // FINAL completing byte (pos 159 -> 160 = endOamDma) is additionally let
        // through in `step_dma` even past the grace, because Gambatte's halt-entry
        // `updateOamDma(cc+4)` finishes a transfer whose last byte lands inside the
        // halt window — see the `dma_pos == 159` bypass there.
        self.halt_oam_grace = 1;
        // C1: a fresh HALT re-arms the wakeup-skew guard (the previous HALT-woken
        // stream has ended).
        self.halt_wakeup_skew = false;
        // A fresh HALT supersedes any pending High-unhalt edge-consume (the prior
        // unhalt's stream has ended); never let it span halts.
        self.hdma_high_unhalt_consume = false;
        self.hdma_peraccess_consume_pending = false;
        if !self.cgb_features_enabled {
            self.halt_hdma_state = HaltHdmaState::Low;
            return;
        }
        // Gambatte `Memory::halt`: haltHdmaState_ = (enabled && period) ? high : low,
        // then `requested` if a block is currently flagged. rustyboi services the
        // period block immediately at the edge instead of holding a flag, so a
        // block that is *owed but not yet serviced* this period (would still be
        // flagged in Gambatte) maps to `Requested`; one already serviced maps to
        // `High`.
        let mut period = in_period.unwrap_or(self.hdma_is_in_period_cached);
        let mut block_done = block_done_override.unwrap_or(self.hdma_block_done_this_period);
        // HALT-coincident HDMA fire rollback (Gambatte `Memory::halt` flag-then-event
        // ordering). rustyboi services an HBlank-DMA block greedily the dot its m0
        // edge latches; Gambatte instead FLAGS it (`flagHdmaReq`) and runs the block
        // as the `intevent_dma` event that follows the HALT's own prefetch M-cycle.
        // When the HALT instruction executes on the very M-cycle that m0 edge lands,
        // Gambatte therefore captures the block as `Requested` (held, NOT yet served)
        // and fires it at UNHALT — whereas rustyboi has already fired it this dot,
        // pinning the post-unhalt FF44 read 36cc early (the block's stall, which
        // Gambatte inserts right after unhalt, was instead spent during the HALT).
        // Detect that exact coincidence (`hdma_last_fire_cc == halt cc`), roll the
        // just-fired block back to its pre-fire pointers, drop its deferred VRAM
        // writes and un-charge its stall, then capture `Requested` so the unhalt
        // re-fires it on Gambatte's dot. Scoped to the same-M-cycle straddle so the
        // ordinary in-period (`High`) and out-of-period (`Low` -> reflag) HALT
        // captures, whose block fired on an earlier dot, are untouched.
        let halt_cc = self.master_cc();
        // Use the PRE-fire enabled flag: a final block (length underflow) clears
        // `hdma_enabled` inside `run_hdma_block`, but Gambatte still holds it enabled
        // and `Requested` at the coincident HALT.
        let pre_fire_enabled = self.hdma_pre_fire_state.map(|s| s.3).unwrap_or(false);
        // Record whether HDMA was armed at HALT entry (the value-read-downstream
        // family) vs requested only in the wakeup ISR (`hdma_cycles_2`).
        self.hdma_enabled_at_halt = self.hdma_enabled || pre_fire_enabled;
        // The m0 edge that latches the block can land anywhere within the HALT's own
        // prefetch M-cycle (4cc, or 8cc at double speed): scx shifts the mode-3->0
        // boundary a couple dots relative to the HALT cc. Treat a fire within that
        // one-M-cycle window before the HALT as coincident.
        let mcycle: u64 = 4u64 << (self.is_double_speed_mode() as u64);
        let coincident_fire = pre_fire_enabled
            // An interleaving OAM-DMA advanced its own position inside the fired
            // block; rolling the block back would double-advance it (the same guard
            // `reorder_late_hdma_after_pushes` uses). Leave the synchronous fire.
            && !self.dma_active
            && self
                .hdma_last_fire_cc
                .map(|fc| fc <= halt_cc && halt_cc - fc < mcycle)
                .unwrap_or(false);
        if coincident_fire {
            if let Some((src, dst, len, en)) = self.hdma_pre_fire_state {
                self.hdma_pending_writes.clear();
                self.hdma_source = src;
                self.hdma_dest = dst;
                self.hdma_length = len;
                self.hdma_enabled = en;
                self.pending_dma_stall = 0;
                self.hdma_write_delay = 0;
                self.hdma_last_fire_cc = None;
                self.hdma_pre_fire_state = None;
                self.hdma_block_done_this_period = false;
                period = true;
                block_done = false;
            }
        }
        self.halt_hdma_state = if self.hdma_req_pending {
            HaltHdmaState::Requested
        } else if self.hdma_enabled && period {
            if block_done {
                HaltHdmaState::High
            } else {
                HaltHdmaState::Requested
            }
        } else {
            HaltHdmaState::Low
        };
        // Gambatte does ackDmaReq after copying the flag.
        self.hdma_req_pending = false;
    }

    /// CGB STOP speed-switch entry (Gambatte `Memory::stop`, memory.cpp:453). Like
    /// `Memory::halt` it captures `haltHdmaState_` and `intreq_.halt()`s for the
    /// 0x20000 unhalt window, so the per-dot HDMA period edge is suppressed across
    /// the speed bridge and stall (`in_stop_window`). `in_period_now` is
    /// `hdmaIsEnabled() && isHdmaPeriod(stop_cc)` evaluated by the caller at the
    /// stop cc (the exact `m0_time_master - gap` edge). The block is (re)flagged or
    /// dropped by `stop_window_exit_reflag` at the unhalt cc.
    pub fn on_stop_window_enter(&mut self, in_period_now: bool) {
        if !self.cgb_features_enabled {
            self.halt_hdma_state = HaltHdmaState::Low;
            self.in_stop_window = true;
            return;
        }
        self.halt_hdma_state = if self.hdma_req_pending {
            HaltHdmaState::Requested
        } else if self.hdma_enabled && in_period_now {
            if self.hdma_block_done_this_period {
                HaltHdmaState::High
            } else {
                HaltHdmaState::Requested
            }
        } else {
            HaltHdmaState::Low
        };
        // Gambatte ackDmaReq after copying the flag.
        self.hdma_req_pending = false;
        self.in_stop_window = true;
    }

    /// CGB STOP unhalt (Gambatte `intevent_unhalt` reflag gate, memory.cpp:224/304):
    /// at the unhalt cc reflag the held block iff
    /// `(hdmaEnabled && isHdmaPeriod(cc) && state==low) || state==requested`.
    /// `in_period_unhalt` is `isHdmaPeriod(unhalt_cc)` (renderer-exact). Clears the
    /// stop-window suppression and fires the block when the gate passes.
    pub fn stop_window_exit_reflag(&mut self, in_period_unhalt: bool) {
        self.in_stop_window = false;
        let reflag = matches!(self.halt_hdma_state, HaltHdmaState::Requested)
            || (self.hdma_enabled
                && in_period_unhalt
                && matches!(self.halt_hdma_state, HaltHdmaState::Low));
        if reflag {
            self.set_hdma_req();
            self.fire_pending_hdma_mcycle();
        }
    }

    /// Execute one 0x10-byte HDMA block. Caller must have verified
    /// `hdma_req_pending && hdma_enabled`. Bytes are copied synchronously;
    /// callers charge the returned CPU-cycle stall via the outer per-cycle
    /// loop so PPU/timer/audio continue to tick during the transfer.
    pub fn run_hdma_block(&mut self) -> u32 {
        self.run_hdma_block_inner(false)
    }

    /// Execute one HDMA block whose `dma()` event fires while the CPU is in the
    /// STOP speed-switch halt window (Gambatte `Memory::dma` `halted()` branch,
    /// memory.cpp:384). The 0x10 source bytes are still copied (the
    /// `read_hdmadst00` destination-content tests depend on it), but FF55 is NOT
    /// decremented: the halted branch leaves `ioamhram_[0x155]` at its written
    /// value and only sets bit 7 (`| 0x80`), then `disableHdma` clears the
    /// enable. So a single-block HDMA caught mid-stop reads back the written
    /// length with bit 7 set (`hdma_late_m3speedchange_hdma5_scx*_2` -> out80),
    /// not the completed 0xFF the normal length-wrap would produce.
    pub fn run_hdma_block_stop_halt(&mut self) -> u32 {
        self.run_hdma_block_inner(true)
    }

    fn run_hdma_block_inner(&mut self, halted: bool) -> u32 {
        // Deferred byte-write placement. Gambatte's `Memory::dma` (memory.cpp:354/
        // 375) reads each byte at the dma-event `cc` but commits the VRAM write at
        // `cc + (2 + 2*ds)` — so byte 0 lands a precise sub-M-cycle AFTER the
        // trigger/prefetch boundary and after VRAM unlocks. rustyboi resolves CPU
        // reads at the POST-tick cc (one M-cycle later than Gambatte's read-at-cc),
        // so a VRAM read in the same window only sees the new byte once the write
        // has actually landed. Reading the 16 source bytes NOW (read-at-cc) and
        // deferring their VRAM commits by `delay` dots reproduces the byte-0
        // boundary the hdma_start / hdma_late read tests probe: the SS offset is
        // 3 dots, the DS offset 5 (the `2 + 2*ds` ratio rescaled for the post-tick
        // read granularity). The OAM-DMA interleave still advances at fire time —
        // it is tuned independently and reads its own source, not the deferred
        // VRAM bytes.
        let delay: u32 = if self.is_double_speed_mode() { 5 } else { 3 };

        // OAM-DMA interleave (Gambatte `Memory::dma`): HDMA and GDMA share the SAME
        // `dma()` inner loop (memory.cpp:280 `intevent_dma` -> `dma(cc)`; only the
        // byte count differs). When an OAM-DMA is concurrently active each gated
        // HDMA byte writes the HDMA-read `data` into `OAM[src & 0xFF]`
        // (memory.cpp:357-372), NOT the OAM-DMA's own source byte. `execute_gdma`
        // already mirrors this; `run_hdma_block` previously advanced the OAM-DMA
        // with `dma_advance_one_mcycle` (its own source), dropping the conflict
        // overwrite the oamdma-transition tests probe. Use the same gated cadence.
        let ds = self.is_double_speed_mode();
        let per_byte_cc: i64 = if ds { 4 } else { 2 };
        // A block firing inside the STOP speed-switch halt window must NOT advance
        // the OAM-DMA: Gambatte's `Memory::dma` interleaves via `updateOamDma`,
        // whose `halted()` branch freezes `oamDmaPos_` while the CPU is
        // `intreq_.halt()`ed across the STOP. Without this gate the block's
        // interleave advanced oamDmaPos_ ~16 bytes, shifting the post-switch
        // in-flight conflict byte (hdma_transition_speedchange_oamdma: read 0x60
        // where the frozen position reads 0x71).
        let interleave = self.dma_active && !self.oam_dma_stop_freeze;
        // OAM-DMA catch-up. rustyboi resolves the FF55 write at the end of the
        // bus M-cycle; whether the current OAM-DMA byte for that M-cycle has
        // already been placed depends on the sub-cycle phase. When
        // `dma_subcycle == 0` an OAM-DMA M-cycle just completed, so rustyboi's
        // `dma_pos` lags Gambatte's `oamDmaPos_` by one and must catch up;
        // when a byte is mid-flight (`dma_subcycle != 0`) the gated loop below
        // already advances `dma_pos` on the same boundary Gambatte does, so an
        // extra catch-up over-advances by one (suppresses the final conflict).
        if interleave && self.dma_subcycle == 0 {
            self.dma_advance_one_mcycle();
        }
        let mut cc: i64 = 0;
        let mut loam: i64 = -(self.dma_subcycle as i64);

        for _ in 0..0x10 {
            let src = self.hdma_source;
            let (vaddr, byte, into_bank1) =
                self.resolve_dma_byte(self.hdma_source, self.hdma_dest);
            self.hdma_pending_writes.push((vaddr, byte, into_bank1));
            cc += per_byte_cc;
            if interleave && self.dma_active && cc - 3 > loam {
                loam += 4;
                self.dma_conflict_advance(src, byte);
            }
            self.hdma_source = self.hdma_source.wrapping_add(1);
            self.hdma_dest = self.hdma_dest.wrapping_add(1);
        }
        if interleave && self.dma_active {
            self.dma_subcycle = (cc - loam).rem_euclid(4) as u8;
        }
        // The OAM-DMA M-cycles for this 0x10-byte block were folded into the loop
        // above; suppress `step_dma` for Gambatte's true dma-event duration (the
        // 0x10-byte transfer plus the single trailing `cc += 4`) so the OAM-DMA is
        // not advanced twice (see `execute_gdma`).
        if interleave {
            self.oam_dma_stall_suppress += (0x10u32) * (per_byte_cc as u32) + 4;
        }
        self.hdma_write_delay = delay;

        if halted {
            // Gambatte `Memory::dma` `halted()` branch (memory.cpp:384-393): the
            // length is NOT recomputed — `ioamhram_[0x155]` keeps its written value
            // and only bit 7 is set; the subsequent `disableHdma` clears the enable.
            // `hdma_length` already holds the written `length_blocks_minus_1`, so
            // leaving it and clearing `hdma_enabled` makes FF55 read
            // `hdma_length | 0x80` (the written length with bit 7), not the 0xFF a
            // completing length-wrap would give.
            self.hdma_enabled = false;
        } else {
            self.hdma_length = self.hdma_length.wrapping_sub(1) & 0x7F;
            // After underflow from 0x00 -> 0xFF -> masked = 0x7F the transfer
            // is complete: FF55 reads 0xFF.
            if self.hdma_length == 0x7F {
                self.hdma_enabled = false;
            }
        }
        self.hdma_req_pending = false;

        // Stall: Gambatte `Memory::dma` advances `cc` by `(2 + 2*ds) * 16` per
        // byte (= 32 / 64) plus a trailing `cc += 4`. Gambatte runs the block as
        // an event preceded by `Interrupter::prefetch` (next opcode fetched
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
        let prefetch_fudge: u32 = if self.halt_wakeup_skew && self.hdma_enabled_at_halt {
            0
        } else {
            6
        };
        let base = if self.is_double_speed_mode() { 68 } else { 36 };
        base + prefetch_fudge
    }

    /// The byte the OAM-DMA engine copies into `OAM[pos]`. Mirrors Gambatte's
    /// `oamDmaSrcPtr()`:
    ///   - invalid / off source -> `rdisabledRam()` (filled with 0xFF).
    ///   - WRAM source -> `wramdata(src_high >> 4 & 1)` indexed by the 12-bit
    ///     offset (DMA source-high bit, NOT the CPU SVBK selection).
    ///   - rom/sram/vram -> normal read of `source_base + pos`.
    fn dma_source_byte(&self, pos: u8) -> u8 {
        match self.dma_src_kind() {
            4 => 0xFF,
            3 => self.dma_conflict_wram_read(self.dma_source_base.wrapping_add(pos as u16)),
            _ => self.read_during_dma(self.dma_source_base.wrapping_add(pos as u16)),
        }
    }

    /// Advance the OAM-DMA engine by one M-cycle (mirrors one iteration of
    /// Gambatte's `updateOamDma` loop). Advances `dma_pos`, (re)starts the
    /// transfer when it reaches `dma_start_pos`, copies the corresponding
    /// source byte into OAM, and ends the transfer at byte 160.
    fn dma_advance_one_mcycle(&mut self) {
        // Apply any deferred CGB VRAM-source conflict-read OAM zero before this
        // M-cycle places a new byte (Gambatte zeroes inside the read itself).
        let pending = self.pending_oam_zero.get();
        if pending >= 0 {
            self.oam.write(OAM_START + pending as u16, 0);
            self.pending_oam_zero.set(-1);
        }

        self.dma_pos = self.dma_pos.wrapping_add(1);

        if self.dma_pos == self.dma_start_pos {
            // startOamDma: transfer (re)starts from the top.
            self.dma_pos = 0;
            self.dma_start_pos = 0;
        }

        if self.dma_pos < 160 {
            let byte = self.dma_source_byte(self.dma_pos);
            self.oam.write(OAM_START + self.dma_pos as u16, byte);
        } else if self.dma_pos == 160 {
            // endOamDma: park the engine. Because no restart was requested
            // (`dma_start_pos == 0`), idle `dma_pos` at -2 and stop.
            if self.dma_start_pos == 0 {
                self.dma_pos = 0xFE;
                self.dma_active = false;
            }
        }
    }

    /// One OAM-DMA M-cycle that fires *inside* a concurrent GDMA/HDMA transfer.
    /// Unlike `dma_advance_one_mcycle` (which writes the OAM-DMA's own source
    /// byte), the conflict path writes the GDMA-read byte `data` into
    /// `OAM[src & 0xFF]` — the GDMA source low byte — mirroring Gambatte's
    /// `Memory::dma` inner loop (memory.cpp:357-372). Cells the GDMA bus index
    /// touches get overwritten with GDMA data; cells the OAM-DMA already wrote
    /// keep their values.
    fn dma_conflict_advance(&mut self, src: u16, data: u8) {
        self.dma_pos = self.dma_pos.wrapping_add(1);

        if self.dma_pos == self.dma_start_pos {
            self.dma_pos = 0;
            self.dma_start_pos = 0;
        }

        if (self.dma_pos as usize) < OAM_SIZE {
            let p = (src & 0xFF) as usize;
            if p < OAM_SIZE {
                self.oam.write(OAM_START + p as u16, data);
            } else if self.cgb_features_enabled {
                // p >= 160 writes the `ioamhram_` tail (0xFEA0-0xFEFF) masked
                // with 0xE7 (Gambatte memory.cpp:366, `!agbFlag_` branch).
                self.oam_high[(p & 0xE7) - 0xA0] = data;
            }
        } else if self.dma_pos as usize == OAM_SIZE {
            if self.dma_start_pos == 0 {
                self.dma_pos = 0xFE;
                self.dma_active = false;
            }
        }
    }

    /// Handle a CPU write to FF46. Arms the engine: the transfer of byte 0
    /// begins two M-cycles later (`dma_start_pos = dma_pos + 2`). A write while
    /// a transfer is already running schedules a restart at that point, leaving
    /// the in-flight transfer to continue until then (DMA-restart behavior).
    fn start_oam_dma(&mut self, value: u8) {
        self.dma_start_pos = self.dma_pos.wrapping_add(2);
        self.dma_subcycle = 0;
        self.dma_source_base = (value as u16) << 8;
        self.dma_active = true;
        self.io_registers.write(REG_DMA, value);
    }

    pub fn step_dma(&mut self) {
        // During the GDMA/HDMA stall the OAM-DMA was already advanced inside the
        // transfer loop (Gambatte folds it into `Memory::dma`); skip the dots that
        // re-tick the same transfer time so the OAM-DMA is not double-advanced.
        if self.oam_dma_stall_suppress > 0 {
            self.oam_dma_stall_suppress -= 1;
            return;
        }
        if !self.dma_active {
            return;
        }

        // One source byte is transferred per M-cycle (4 dots), not per dot.
        self.dma_subcycle += 1;
        if self.dma_subcycle < 4 {
            return;
        }
        self.dma_subcycle = 0;
        // STOP speed-switch unhalt window: the CPU is `intreq_.halt()`ed for the
        // 0x20000 cycles, so Gambatte's `updateOamDma` takes its `halted()` branch
        // and freezes `oamDmaPos_`. Mid-transfer OAM-DMA must stay put across the
        // window (oamdma_*_speedchange_* read the in-flight conflict byte after the
        // switch). The sub-M-cycle phase already advanced (reset above) but no byte
        // is placed.
        if self.oam_dma_stop_freeze {
            return;
        }
        // While the CPU is halted the OAM-DMA position is FROZEN: Gambatte's
        // `updateOamDma` halt branch consumes the elapsed M-cycles
        // (`lastOamDmaUpdate_ += 4*cycles`) WITHOUT advancing `oamDmaPos_`. Keep
        // the sub-M-cycle phase (reset above) but do not place a byte. Gambatte's
        // `Memory::halt` still advances ONE M-cycle at halt entry
        // (`updateOamDma(cc + 4)` runs before `intreq_.halt()`), i.e. the HALT
        // instruction's own M-cycle moves the OAM-DMA; only subsequent halt
        // M-cycles freeze. `halt_oam_grace` lets exactly that one through.
        if self.cpu_halted {
            if self.halt_oam_grace > 0 {
                self.halt_oam_grace -= 1;
            } else if self.dma_pos != 159 {
                // Freeze the OAM-DMA mid-transfer during HALT. EXCEPTION: the very
                // last byte (pos 159 -> 160 = endOamDma). Gambatte's `Memory::halt`
                // runs `updateOamDma(cc)` THEN `updateOamDma(cc + 4)` before
                // `intreq_.halt()`, so a transfer whose final byte's M-cycle lands
                // inside the halt-entry window completes BEFORE the freeze rather
                // than stalling to unhalt. rustyboi's per-dot `step_dma` catch-up
                // sits one M-cycle behind Gambatte's `updateOamDma(cc)` at the halt
                // instant (the FF46 two-M-cycle arm phase), so the grace M-cycle
                // only reaches pos 159; letting pos 159 -> 160 through here lands
                // endOamDma at the same point Gambatte does. A mid-transfer DMA
                // (pos << 159, e.g. oamdmasrc80_halt_*: pos 11) stays frozen.
                // Gating on the final byte keeps every existing freeze boundary
                // (the read8000 / hdma_transition_oamdma cases) byte-identical,
                // while letting oamdma_late_halt_stat_2 finish so LY=4's mode-2
                // scan sees the real OAM sprite (m0Time +11, STAT read mode 3).
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
    pub fn step_hdma(&mut self, period: Option<bool>) {
        if !self.cgb_features_enabled {
            return;
        }

        let lcdc = self.io_registers.read(ppu::LCD_CONTROL);
        let lcd_on = lcdc & (ppu::LCDCFlags::DisplayEnable as u8) != 0;

        // Cycle-exact HDMA-eligibility window from the PPU renderer (Gambatte
        // `isHdmaPeriod`). When the LCD is off, treat it as permanently in the
        // period (Gambatte fires HDMA immediately when armed). When the renderer
        // cannot supply a closed-form mode-0 dot (window/first line), fall back
        // to the STAT mode-3->0 edge below.
        let in_period = if !lcd_on { true } else { period.unwrap_or(false) };
        self.hdma_is_in_period_cached = in_period;

        // Reset the per-period "block already serviced" marker on the falling
        // edge so the next period's block is again owed.
        if self.hdma_prev_period && !in_period {
            self.hdma_block_done_this_period = false;
        }
        // A genuine period falling edge (closed-form `period` Some(true)->Some(false),
        // i.e. line-end past the HBlank window, not the Some->None renderer handoff)
        // means we are decisively out of the consumed-edge's period: drop the guard.
        if period == Some(false) {
            self.hdma_halt_edge_consumed = false;
        }

        // Gambatte's period-edge `flagHdmaReq` is suppressed while the CPU is
        // halted (video.h:41 `if (!intreq_.halted())`): during HALT — and equally
        // during the CGB STOP speed-switch window (`Memory::stop` also
        // `intreq_.halt()`s, see `in_stop_window`) — the block is governed by the
        // `haltHdmaState_` machine and re-flagged only on unhalt, so the edge must
        // NOT auto-arm here. Edge trackers are still advanced so the rising edge is
        // detected cleanly once the CPU unhalts.
        let arm_allowed = !self.cpu_halted && !self.in_stop_window;
        if lcd_on && period.is_some() {
            // Rising edge of the eligibility window arms a block.
            if arm_allowed && !self.hdma_prev_period && in_period && self.hdma_enabled {
                // High-at-halt unhalt: consume the first post-unhalt m0 edge (the
                // during-halt edge Gambatte already consumed, landing one dot past
                // our slightly-early unhalt cc). Suppress this arm and clear.
                if self.hdma_high_unhalt_consume {
                    self.hdma_high_unhalt_consume = false;
                } else if self.peraccess_consume_m0_arm() {
                    // Requested-unhalt sub-block-cc consume: this m0 edge fell inside
                    // block1's transfer span; Gambatte absorbs it and defers the next
                    // block one line. Suppress this arm.
                } else {
                    self.hdma_req_pending = true;
                }
            } else if !arm_allowed && !self.hdma_prev_period && in_period && self.hdma_enabled {
                // A period rising edge while HALTED. Gambatte suppresses (and
                // CONSUMES) the `flagHdmaReq` here. Whether this consumed edge must
                // STILL fire its block after unhalt depends on `haltHdmaState_`:
                //   - High (halt entered in-period, block already serviced this
                //     period): the unhalt does NOT reflag (memory.cpp:304 gate fails
                //     on High) and this period's block is gone — the NEXT line's m0
                //     edge fires the next block. Mark the edge consumed so rustyboi's
                //     STAT-mode-3->0 fallback (which can resurrect this same m0 edge
                //     the first dot after unhalt, once the closed-form `hdma_period`
                //     has handed off to None) skips it (hdma_m0halt_late_m3unhalt_*).
                //   - Low / Requested (out-of-period at halt, or armed-and-owed): the
                //     unhalt reflag path fires the block; the post-unhalt edge is the
                //     genuine first block and MUST NOT be skipped
                //     (late_hdma_vs_tima_*_halt). Leave the flag clear.
                if self.halt_hdma_state == HaltHdmaState::High
                    || self.hdma_block_done_this_period
                {
                    self.hdma_halt_edge_consumed = true;
                }
            }
            self.hdma_prev_period = in_period;
            // Keep the STAT-mode tracker current so a later fallback line edges
            // cleanly rather than firing on a stale mode value.
            self.hdma_prev_stat_mode = self.io_registers.read(ppu::LCD_STATUS) & 0x03;
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
            // was already serviced (`hdma_halt_edge_consumed`, set above for a
            // High-at-halt period re-entry), Gambatte has already consumed it; skip
            // the fallback arm so it is not re-fired post-unhalt (the spurious extra
            // block in hdma_m0halt_late_m3unhalt_*). A Low/Requested-at-halt period
            // entry leaves the flag clear, so its post-unhalt first block still fires
            // (late_hdma_vs_tima_*_halt). The genuine window / first-line fallback
            // paths never set the flag either.
            if arm_allowed
                && lcd_on
                && !self.hdma_halt_edge_consumed
                && self.hdma_prev_stat_mode == 3
                && mode == 0
                && self.hdma_enabled
            {
                // High-at-halt unhalt: consume the first post-unhalt m0 edge (see
                // the period-rising-edge branch). The lcdoffset m0halt tests fire
                // this edge through the STAT 3->0 fallback (period handed off to
                // None), one dot after the unhalt.
                if self.hdma_high_unhalt_consume {
                    self.hdma_high_unhalt_consume = false;
                } else if self.peraccess_consume_m0_arm() {
                    // Requested-unhalt sub-block-cc consume: see the
                    // period-rising-edge branch.
                } else {
                    self.hdma_req_pending = true;
                }
            }
            // The consumed-edge guard is single-use: it suppresses exactly the one
            // STAT 3->0 fallback that mirrors the consumed period edge, then clears
            // so subsequent lines arm normally.
            if self.hdma_prev_stat_mode == 3 && mode == 0 {
                self.hdma_halt_edge_consumed = false;
            }
            self.hdma_prev_stat_mode = mode;
            self.hdma_prev_period = in_period;
        }

        // C7-full event firing. Normally the block fires synchronously the dot the
        // request is latched (the byte-landing timing the hdma_start/late read
        // tests are calibrated to). The ONLY exception is the interrupt-vs-dma
        // precedence window: while an interrupt service is pushing PC
        // (`hdma_mcycle_fire_suppressed`), a block latched mid-service is HELD and
        // fired explicitly after the pushes (memory.cpp:312-320) so the pushed
        // return address is visible in the HDMA copy of that stack slot.
        if self.hdma_req_pending && self.hdma_enabled {
            if in_period {
                self.hdma_block_done_this_period = true;
            }
            if !self.hdma_mcycle_fire_suppressed {
                self.fire_pending_hdma_mcycle();
            }
        }
    }

    /// C7-full: fire any latched HDMA block at a CPU M-cycle boundary (the
    /// `intevent_dma` body). Called by the bus after each access M-cycle so the
    /// copy lands one M-cycle after the trigger — and, when an interrupt service
    /// pushed to the block's source region during this M-cycle, AFTER those
    /// pushes (memory.cpp:312-320 precedence). No-op when nothing is latched.
    pub fn fire_pending_hdma_mcycle(&mut self) {
        if !(self.hdma_req_pending && self.hdma_enabled) {
            return;
        }
        // Snapshot the pre-fire block pointers so the late-hdma-vs-interrupt
        // re-order (see `reorder_late_hdma_after_pushes`) can restore them and
        // re-run the block reading post-push memory when an interrupt won the
        // m0Time-vs-minIntTime race. Only meaningful while no OAM-DMA interleave
        // is active (the `late_hdma_vs_*` tests have none); a re-run with an
        // active OAM-DMA would double-advance its position, so the re-order is
        // gated on `!dma_active` at the service site.
        self.hdma_pre_fire_state =
            Some((self.hdma_source, self.hdma_dest, self.hdma_length, self.hdma_enabled));
        self.hdma_last_fire_cc = Some(self.master_cc());
        self.pending_dma_stall += self.run_hdma_block();
        // Gambatte intevent_dma (memory.cpp:280): after the block, a halt-time
        // `hdma_requested` collapses to `hdma_low` so a subsequent unhalt does
        // not re-fire it (the request has now been serviced).
        if self.halt_hdma_state == HaltHdmaState::Requested {
            self.halt_hdma_state = HaltHdmaState::Low;
        }
    }

    /// Fire the latched HDMA block whose `dma()` event lands inside the STOP
    /// speed-switch halt window. Same copy as `fire_pending_hdma_mcycle` but with
    /// the `halted()` FF55 semantics (no length decrement; see
    /// `run_hdma_block_stop_halt`).
    pub fn fire_pending_hdma_mcycle_stop_halt(&mut self) {
        if !(self.hdma_req_pending && self.hdma_enabled) {
            return;
        }
        self.hdma_pre_fire_state =
            Some((self.hdma_source, self.hdma_dest, self.hdma_length, self.hdma_enabled));
        self.hdma_last_fire_cc = Some(self.master_cc());
        self.pending_dma_stall += self.run_hdma_block_stop_halt();
        if self.halt_hdma_state == HaltHdmaState::Requested {
            self.halt_hdma_state = HaltHdmaState::Low;
        }
    }

    /// C7-full late-hdma-vs-interrupt re-order (memory.cpp:312-320 / the
    /// `intevent_dma` < `intevent_interrupts` event ordering). Gambatte resolves
    /// the race by event time: the m0-edge HDMA (`flagHdmaReq` at `m0Time`) wins
    /// over the interrupt only when `m0Time <= minIntTime_` (the interrupt's
    /// serviceable cc); otherwise the interrupt's PC pushes run first and the
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
    pub fn reorder_late_hdma_after_pushes(&mut self, service_access_cc: u64) {
        if self.dma_active {
            return;
        }
        let Some(fire_cc) = self.hdma_last_fire_cc else {
            return;
        };
        let Some((src, dst, len, en)) = self.hdma_pre_fire_state else {
            return;
        };
        // The interrupt won the race only when its dispatch boundary is within one
        // M-cycle (4cc) of the greedy fire on EITHER side: the failing vs tests
        // dispatch at fire+0 / fire+2, versus +46 for the genuine dma-wins races
        // (where the block fired a full instruction earlier and legitimately wins).
        // The two-sided window also rejects a stale `hdma_last_fire_cc` left by an
        // unrelated earlier block. `service_access_cc` is the pre-push boundary cc;
        // `fire_cc` is the post-tick master_cc of the fire.
        if fire_cc + 4 < service_access_cc || fire_cc > service_access_cc + 4 {
            return;
        }
        // Discard the stale (pre-push) deferred writes and re-run the block from
        // the restored pointers so its source reads see the just-pushed PC.
        self.hdma_pending_writes.clear();
        self.hdma_source = src;
        self.hdma_dest = dst;
        self.hdma_length = len;
        self.hdma_enabled = en;
        self.hdma_req_pending = true;
        self.run_hdma_block();
        self.hdma_last_fire_cc = None;
        self.hdma_pre_fire_state = None;
    }

    /// C7-full: whether the M-cycle-boundary HDMA fire is currently suppressed
    /// (an interrupt service is pushing PC; the block must fire after the pushes).
    pub fn hdma_mcycle_fire_suppressed(&self) -> bool {
        self.hdma_mcycle_fire_suppressed
    }

    /// C7-full: begin/end suppression of the M-cycle-boundary HDMA fire around an
    /// interrupt service's PC pushes.
    pub fn set_hdma_mcycle_fire_suppressed(&mut self, v: bool) {
        self.hdma_mcycle_fire_suppressed = v;
    }

    /// Late-hdma-vs-interrupt unhalt precedence: whether the just-unhalted HDMA
    /// block did NOT reflag at unhalt (its m0-edge falls within the following
    /// interrupt service and must fire AFTER the PC pushes).
    pub fn hdma_unhalt_noreflag_deferred(&self) -> bool {
        self.hdma_unhalt_noreflag_deferred
    }

    pub fn hdma_unhalt_reflag_deferred(&self) -> bool {
        self.hdma_unhalt_reflag_deferred
    }

    pub fn set_hdma_unhalt_reflag_deferred(&mut self, v: bool) {
        self.hdma_unhalt_reflag_deferred = v;
    }

    pub fn set_hdma_unhalt_noreflag_deferred(&mut self, v: bool) {
        self.hdma_unhalt_noreflag_deferred = v;
    }

    /// Read the pending DMA stall without consuming it or arming the post-DMA
    /// STAT-read bias (unlike `take_dma_stall`).
    pub fn peek_dma_stall(&self) -> u32 {
        self.pending_dma_stall
    }

    /// Drop `amount` cc from the pending DMA stall (saturating). Used to absorb a
    /// deferred stop_halt HDMA block's transfer span into the STOP unhalt window
    /// rather than charging it as a separate post-window stall.
    pub fn reduce_dma_stall(&mut self, amount: u32) {
        self.pending_dma_stall = self.pending_dma_stall.saturating_sub(amount);
    }

    /// Consume the CPU-cycle stall owed for completed HDMA/GDMA transfers.
    pub fn take_dma_stall(&mut self) -> u32 {
        let stall = std::mem::take(&mut self.pending_dma_stall);
        if stall > 0 {
            // C7: arm the post-DMA STAT-read bias (prefetch absorption) so the
            // first FF41 mode read after the stall resolves at Gambatte's read cc.
            self.dma_prefetch_stat_bias = true;
        }
        stall
    }

    /// C7: whether the next FF41 STAT-mode read should apply the post-DMA prefetch
    /// bias (resolve at `master_cc - 1`). Consumes the flag.
    pub fn take_dma_prefetch_stat_bias(&mut self) -> bool {
        std::mem::take(&mut self.dma_prefetch_stat_bias)
    }

    /// Whether the OAM-DMA engine is armed/running (mirrors
    /// `lastOamDmaUpdate_ != disabled_time`). Used by the bus to decide whether
    /// the DMA M-cycle must be advanced before resolving a CPU write.
    pub fn dma_active(&self) -> bool {
        self.dma_active
    }

    /// True while a transfer is actively placing bytes into OAM (the window in
    /// which the CPU bus conflicts with OAM DMA). Mirrors `oamDmaPos_ < 160`.
    fn dma_transfer_in_progress(&self) -> bool {
        self.dma_active && self.dma_pos < 160
    }

    /// Public view of the OAM-DMA "placing bytes" window (`startOamDma` ..
    /// `endOamDma`). The PPU's lazy sprite snapshot uses this to know when the
    /// OAM source reads as disabled RAM (0xFF), mirroring Gambatte pointing
    /// `oamReader_.oamram_` at `cart_.rdisabledRam()` for the DMA window.
    pub fn oam_dma_window_active(&self) -> bool {
        self.dma_transfer_in_progress()
    }

    /// Take (and clear) the pending-CPU-OAM-write flag. The PPU drains this each
    /// dot to fire the sprite-snapshot `change(cc)` (Gambatte `oamChange`).
    pub fn take_oam_write_pending(&mut self) -> bool {
        let p = self.oam_write_pending;
        self.oam_write_pending = false;
        p
    }

    /// Copy the 80 OAM position bytes (Y at even index, X at odd index, for each
    /// of the 40 sprites) into `out`. Reads the raw OAM buffer directly,
    /// bypassing the DMA-conflict bus logic — the PPU sprite snapshot wants the
    /// true post-write OAM contents (Gambatte `oamram_[2*i]`/`[2*i+1]`).
    pub fn peek_oam_pos(&self, out: &mut [u8; 80]) {
        for i in 0..40 {
            let base = OAM_START + (i as u16) * 4;
            out[2 * i] = self.oam.read(base);
            out[2 * i + 1] = self.oam.read(base + 1);
        }
    }

    /// Source-region classification of the active OAM DMA (mirrors
    /// `oamDmaInitSetup`/`cart_.oamDmaSrc()`): 0=rom 1=sram 2=vram 3=wram
    /// 4=invalid.
    fn dma_src_kind(&self) -> u8 {
        let cgb = self.cgb_features_enabled;
        let src_high = (self.dma_source_base >> 8) as u8;
        let wram_top: u16 = if cgb { 0xE0 } else { 0x100 };
        if src_high < 0xA0 {
            if src_high < 0x80 { 0 } else { 2 }
        } else if (src_high as u16) < wram_top {
            if src_high < 0xC0 { 1 } else { 3 }
        } else {
            4
        }
    }

    /// The WRAM "area" (bank slot) selected by the active OAM DMA during a CGB
    /// conflicting WRAM access. Mirrors Gambatte's
    /// `cart_.wramdata(ioamhram_[0x146] >> 4 & 1)`: bit 4 of the DMA source-high
    /// byte (NOT the CPU's SVBK selection) chooses between the fixed bank-0
    /// block (area 0) and the currently SVBK-banked block (area 1).
    fn dma_conflict_wram_area(&self) -> u8 {
        ((self.dma_source_base >> 8) >> 4 & 1) as u8
    }

    /// Read the WRAM byte seen on a CGB OAM-DMA conflicting access. The byte is
    /// taken from `wramdata(area)[p & 0xFFF]`, so the address's C/D range is
    /// ignored: only the 12-bit offset and the DMA-derived area matter.
    fn dma_conflict_wram_read(&self, addr: u16) -> u8 {
        let offset = addr & 0x0FFF;
        if self.dma_conflict_wram_area() == 0 {
            self.wram.read(WRAM_START + offset)
        } else {
            match self.wram_bank_select {
                2..=7 => self.wram_banks[(self.wram_bank_select - 2) as usize]
                    .read(WRAM_BANK_START + offset),
                _ => self.wram_bank.read(WRAM_BANK_START + offset),
            }
        }
    }

    /// Write the CPU byte into WRAM during a CGB OAM-DMA conflict, matching the
    /// `wramdata(area)[p & 0xFFF]` routing used by `dma_conflict_wram_read`.
    fn dma_conflict_wram_write(&mut self, addr: u16, value: u8) {
        let offset = addr & 0x0FFF;
        if self.dma_conflict_wram_area() == 0 {
            self.wram.write(WRAM_START + offset, value);
        } else {
            match self.wram_bank_select {
                2..=7 => self.wram_banks[(self.wram_bank_select - 2) as usize]
                    .write(WRAM_BANK_START + offset, value),
                _ => self.wram_bank.write(WRAM_BANK_START + offset, value),
            }
        }
    }

    /// Resolve a CPU write that lands in the OAM-DMA conflict area while a
    /// transfer is in progress. Mirrors the conflict branch of Gambatte's
    /// `nontrivial_write`: the write is redirected onto the shared bus, so the
    /// DMA copies the CPU-driven byte into `OAM[dma_pos]` instead of the
    /// original source byte. Returns true if the write was consumed here (and
    /// must not reach normal memory).
    fn dma_write_conflict(&mut self, addr: u16, value: u8) -> bool {
        if !self.dma_transfer_in_progress() || !self.dma_address_conflicts(addr) {
            return false;
        }
        let pos = self.dma_pos as u16;
        if self.cgb_features_enabled {
            if addr < WRAM_START {
                // rom/sram/vram source: OAM latches the CPU byte (0 for vram).
                let byte = if self.dma_src_kind() == 2 { 0 } else { value };
                self.oam.write(OAM_START + pos, byte);
            } else if self.dma_src_kind() != 3 {
                // WRAM region with a non-WRAM source: the write still reaches
                // WRAM, but on the bank slot chosen by the DMA source-high bit
                // (Gambatte `wramdata(ioamhram_[0x146] >> 4 & 1)`), not the
                // CPU's SVBK selection.
                self.dma_conflict_wram_write(addr, value);
            }
            // WRAM region with a WRAM source: write is swallowed (no effect).
        } else {
            // DMG: OAM latches the CPU byte; a WRAM source ANDs with the byte
            // the DMA already placed (bus conflict).
            let byte = if self.dma_src_kind() == 3 {
                self.oam.read(OAM_START + pos) & value
            } else {
                value
            };
            self.oam.write(OAM_START + pos, byte);
        }
        true
    }

    /// As `dma_transfer_in_progress`, but using the read-observed position.
    fn dma_read_conflict_active(&self) -> bool {
        self.dma_active && self.dma_pos < 160
    }

    /// Byte the CPU sees on a conflicting bus read while OAM DMA is mid-transfer.
    /// Mirrors the conflict branch of Gambatte's `nontrivial_read`: the read
    /// observes `OAM[dma_pos]`, the byte the DMA just placed this M-cycle (the
    /// bus tick already advanced the engine before this read resolves). On CGB,
    /// a read of the WRAM region with a non-WRAM source instead returns the live
    /// WRAM byte.
    fn dma_conflict_byte(&self, addr: u16) -> u8 {
        if self.cgb_features_enabled && self.dma_src_kind() != 3 && addr >= WRAM_START {
            return self.dma_conflict_wram_read(addr);
        }
        let byte = self.oam.read(OAM_START + self.dma_pos as u16);
        // CGB with a VRAM source: the conflict read returns OAM[pos] but then
        // zeroes that OAM byte (Gambatte `nontrivial_read`). Defer the zero to
        // the next DMA advance so the &self read path can record it.
        if self.cgb_features_enabled && self.dma_src_kind() == 2 {
            self.pending_oam_zero.set(self.dma_pos as i16);
        }
        byte
    }

    /// Whether a CPU access to `addr` conflicts with the in-progress OAM DMA.
    /// Faithful port of Gambatte's `isInOamDmaConflictArea`: classify the DMA
    /// source into rom/sram/vram/wram/invalid, then test a per-4KB-block
    /// conflict bitmask (which differs between DMG and CGB).
    fn dma_address_conflicts(&self, addr: u16) -> bool {
        if addr >= OAM_START {
            return false;
        }
        let cgb = self.cgb_features_enabled;
        let src = self.dma_src_kind();

        // Per-block conflict masks (bit n set => 4KB block n conflicts).
        let mask: u16 = match src {
            0 | 1 => 0xFCFF,
            2 => 0x0300,
            3 => if cgb { 0xF000 } else { 0xFCFF },
            _ => if cgb { 0xFCFF } else { 0x0000 },
        };
        (mask >> (addr >> 12)) & 1 != 0
    }

    pub fn set_input_state(&mut self, state: crate::input::ButtonState) {
        self.input.set_button_state(state);
    }

    // CGB Speed switching methods
    pub fn is_double_speed_mode(&self) -> bool {
        self.cgb_features_enabled && self.key1_current_speed
    }

    pub fn is_speed_switch_armed(&self) -> bool {
        self.cgb_features_enabled && self.key1_switch_armed
    }

    pub fn perform_speed_switch(&mut self) {
        if self.cgb_features_enabled && self.key1_switch_armed {
            // Gambatte evaluates `isDoubleSpeed()` for the PSG/timer speed-change
            // folds BEFORE toggling KEY1 (`ioamhram_[0x14D] ^= 0x81`), so capture
            // the speed being LEFT here.
            let old_ds = self.is_double_speed_mode();
            // Toggle the speed mode
            self.key1_current_speed = !self.key1_current_speed;
            // Clear the armed bit
            self.key1_switch_armed = false;
            // Gambatte's `Memory::stop` resets DIV and re-bases peripheral
            // timing on speed switch. We don't keep separately scaled internal
            // counters, so resetting DIV is the only resync we need; the
            // per-T-cycle stepping in gb.rs already produces the correct
            // half-rate PPU/audio cadence in double-speed.
            // Gambatte applies `Tima::speedChange` (a 4-cycle TIMA phase shift
            // for enabled fast timers) before the DIV reset; mirror that order.
            self.timer.speed_change();
            self.timer.stop_div_reset(old_ds);
            if self.timer.take_pending_irq() {
                self.request_interrupt(cpu::registers::InterruptFlag::Timer);
            }
            // Gambatte order (memory.cpp:466): after the DIV reset (which the APU
            // mirrors as a `PSG::divReset` fold on the next `sync_cc`), apply the
            // `PSG::speedChange` fold. Sync first so the divReset fold + flush to
            // the switch cc happen, then re-fold for the speed transition.
            //
            // Gambatte's `Memory::stop` runs both `psg_.divReset(isDoubleSpeed())`
            // and `psg_.speedChange(cc_, isDoubleSpeed())` with the OLD speed (the
            // KEY1 toggle is AFTER), and flushes the speedChange to
            // `cc_ = stopCc + 8 * !old_ds`, not to the current dot. KEY1 was already
            // toggled above, so `is_double_speed_mode` now reports the NEW speed;
            // sync with the captured `old_ds` so the divReset fold runs at the old
            // speed, then hand the stop cc to `psg_speed_change_at` for the faithful
            // `+8*!ds` flush.
            let stop_cc = self.timer.abs_cc();
            self.sync_apu_cc_with_ds(old_ds);
            self.audio.psg_speed_change_at(old_ds, stop_cc);
        }
    }

    pub fn is_dmg_compatibility_mode(&self) -> bool {
        self.cgb_features_enabled && self.key0_dmg_mode
    }

    // Private helper to read during DMA without triggering DMA conflicts
    fn read_during_dma(&self, addr: u16) -> u8 {
        match addr {
            BIOS_START..=BIOS_END => {
                match self.read(REG_BOOT_OFF) {
                    0 => {
                        match &self.bios {
                            Some(bios) => bios.read(addr),
                            None => EMPTY_BYTE,
                        }
                    },
                    _ => {
                        match &self.cartridge {
                            Some(cart) => cart.read(addr),
                            None => EMPTY_BYTE,
                        }
                    }
                }
            },
            CARTRIDGE_AFTER_BIOS_START..=CARTRIDGE_END => {
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
            // VRAM-source OAM DMA reads through the live VBK pointer
            // (Gambatte `vrambankptr()`), so a mid-DMA VBK write retargets
            // subsequent source bytes.
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
            WRAM_START..=WRAM_END => self.wram.read(addr),
            WRAM_BANK_START..=WRAM_BANK_END => self.wram_bank.read(addr),
            ECHO_RAM_START..=ECHO_RAM_END => {
                let addr = addr - 0x2000;
                match addr {
                    0..WRAM_START => panic!("This is literally never possible"),
                    WRAM_START..=WRAM_END => self.wram.read(addr),
                    WRAM_BANK_START..=ECHO_RAM_MIRROR_END => self.wram_bank.read(addr),
                    0xDE00..=0xFFFF => panic!("This is literally never possible"),
                }
            },
            IO_REGISTERS_START..=IO_REGISTERS_END => {
                match addr {
                    input::JOYP => self.input.read(addr),
                    timer::DIV..=timer::TAC => self.timer.read(addr),
                serial::SB..=serial::SC => self.serial.read(addr),
                cpu::registers::INTERRUPT_FLAG => self.io_registers.read(addr) | 0xE0,
                    REG_DMA => self.io_registers.read(addr),
                    _ => self.io_registers.read(addr),
                }
            }
            HRAM_START..=HRAM_END => self.hram.read(addr),
            _ => EMPTY_BYTE,
        }
    }

    fn write_lcd_status(&mut self, value: u8) {
        let current = self.io_registers.read(ppu::LCD_STATUS);
        self.io_registers
            .write(ppu::LCD_STATUS, (current & 0x07) | (value & 0x78));
        self.stat_register_write_pending = true;
        self.ff41_write_pending = true;
    }

    fn write_lcd_control(&mut self, value: u8) {
        let de = ppu::LCDCFlags::DisplayEnable as u8;
        let was_on = self.io_registers.read(ppu::LCD_CONTROL) & de != 0;
        let now_off = value & de == 0;
        self.io_registers.write(ppu::LCD_CONTROL, value);
        self.stat_register_write_pending = true;
        // Gambatte `lcdcChange` (memory.cpp:1144-1158): disabling the LCD while an
        // HDMA is armed flags an HDMA request directly (`if (hdmaEnabled) flagHdmaReq`).
        // With the LCD off `isHdmaPeriod` is permanently true, so the latched block
        // fires on the next `step_hdma` (the LCD-off arming paths there require
        // `lcd_on`, so without this edge the block would never arm — hdma_disable_display).
        if was_on && now_off && self.cgb_features_enabled && self.hdma_enabled {
            self.hdma_req_pending = true;
        }
    }

    pub fn write_lcd_status_from_ppu(&mut self, value: u8) {
        self.io_registers.write(ppu::LCD_STATUS, value);
    }

    /// CPU-side write to FF44 (LY). On real hardware this resets the line
    /// counter to 0 (the value written is ignored). The PPU will observe the
    /// pending flag on its next step and re-arm internal scanline state.
    fn write_ly_from_cpu(&mut self) {
        // FF44 (LY) is read-only on hardware; CPU writes are ignored.
    }

    /// PPU-side update of FF44 (LY). Bypasses the CPU-write reset semantics so
    /// the PPU can advance the line counter through normal scanline progression.
    pub fn write_ly_from_ppu(&mut self, value: u8) {
        self.io_registers.write(ppu::LY, value);
    }

    /// Consume the pending LY-write signal. Returns true if the CPU wrote to
    /// FF44 since the last call.
    pub fn take_ly_write_pending(&mut self) -> bool {
        let pending = self.ly_write_pending;
        self.ly_write_pending = false;
        pending
    }

    /// The persistent CPU T-cycle phase (survives instruction boundaries).
    pub fn cpu_t_phase(&self) -> u64 {
        self.cpu_t_phase
    }

    /// Advance the persistent CPU T-cycle phase by one.
    pub fn advance_cpu_t_phase(&mut self) {
        self.cpu_t_phase = self.cpu_t_phase.wrapping_add(1);
    }

    /// Consume the pending STAT-register-write signal. Returns true if the CPU
    /// wrote to FF40, FF41, or FF45 since the last call.
    pub fn take_stat_register_write_pending(&mut self) -> bool {
        let pending = self.stat_register_write_pending;
        self.stat_register_write_pending = false;
        pending
    }

    /// Consume the pending FF41 (STAT) write signal. True if FF41 was written
    /// since the last call, even if the value was unchanged.
    pub fn take_ff41_write_pending(&mut self) -> bool {
        let pending = self.ff41_write_pending;
        self.ff41_write_pending = false;
        pending
    }

    // --- libretro direct-memory accessors (appended) ---

    /// Mutable handle to the inserted cartridge, used by the libretro frontend
    /// to reach battery-backed save RAM and RTC bytes.
    pub fn get_cartridge_mut(&mut self) -> Option<&mut cartridge::Cartridge> {
        self.cartridge.as_mut()
    }

    /// Fixed work-RAM bank (0xC000-0xCFFF) as a mutable slice.
    pub fn wram_bank0_slice_mut(&mut self) -> &mut [u8] {
        self.wram.as_mut_slice()
    }

    /// Switchable work-RAM bank region (0xD000-0xDFFF) as a mutable slice. On
    /// CGB this is bank 1; banks 2-7 are not contiguous so only this slice is
    /// exposed as the canonical system-RAM bank window.
    pub fn wram_bank1_slice_mut(&mut self) -> &mut [u8] {
        self.wram_bank.as_mut_slice()
    }

    /// High RAM (0xFF80-0xFFFE) as a mutable slice.
    pub fn hram_slice_mut(&mut self) -> &mut [u8] {
        self.hram.as_mut_slice()
    }

    /// Video RAM bank 0 (0x8000-0x9FFF) as a mutable slice.
    pub fn vram_slice_mut(&mut self) -> &mut [u8] {
        self.vram.as_mut_slice()
    }

    /// Post-boot power-on contents of OAM (0xFE00-0xFE9F), the "unusable"
    /// 0xFEA0-0xFEFF shadow, and HRAM (0xFF80-0xFFFE). The boot ROM does not
    /// touch these (besides clearing OAM on CGB), so they retain the hardware
    /// power-on pattern. Bytes are Gambatte's `setInitial{Dmg,Cgb}Ioamhram`
    /// dumps (libgambatte/src/mem_dumps.h). Tests that read never-written OAM /
    /// unusable / HRAM (the fexx_* dumpers) depend on these.
    pub fn set_post_bios_ioamhram(&mut self, cgb: bool) {
        if cgb {
            // CGB: OAM cleared to 0x00. The 0xFEA0-0xFEFF shadow holds the feax
            // dump (the read path masks the index with 0xE7). These are the
            // cgb04c-revision power-on bytes, i.e. the exact values the Gambatte
            // `cgb04c` `.dump` oracles read back from an OAM tail untouched by DMA
            // (the gdmasrc0000 oamdumpers, whose GDMA source low byte never
            // reaches 0xA0). rustyboi previously seeded the generic-cgb
            // `setInitialCgbIoamhram` constants (0x08.../0x4A.../0x7F...) which
            // differ from cgb04c at a handful of bytes and left every
            // gdma-oamdumper failing on the tail even once the OAM body matched.
            // The cgb04c values regress no currently-passing test (the
            // generic-cgb fexx dumpers were already failing for other reasons).
            const CGB_FEAX: [u8; 0x60] = [
                0x18, 0x01, 0xEF, 0xDE, 0x06, 0x48, 0xCD, 0xBD,
                0x18, 0x01, 0xEF, 0xDE, 0x06, 0x48, 0xCD, 0xBD,
                0x18, 0x01, 0xEF, 0xDE, 0x06, 0x48, 0xCD, 0xBD,
                0x18, 0x01, 0xEF, 0xDE, 0x06, 0x48, 0xCD, 0xBD,
                0x00, 0x90, 0xF7, 0x5F, 0xC0, 0xF1, 0xB6, 0xFB,
                0x00, 0x90, 0xF7, 0x5F, 0xC0, 0xF1, 0xB6, 0xFB,
                0x00, 0x90, 0xF7, 0x5F, 0xC0, 0xF1, 0xB6, 0xFB,
                0x00, 0x90, 0xF7, 0x5F, 0xC0, 0xF1, 0xB6, 0xFB,
                0x24, 0x1B, 0xFD, 0x3A, 0x10, 0x12, 0xAC, 0x45,
                0x24, 0x1B, 0xFD, 0x3A, 0x10, 0x12, 0xAC, 0x45,
                0x24, 0x1B, 0xFD, 0x3A, 0x10, 0x12, 0xAC, 0x45,
                0x24, 0x1B, 0xFD, 0x3A, 0x10, 0x12, 0xAC, 0x45,
            ];
            const CGB_HRAM: [u8; 0x7F] = [
                0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B,
                0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00, 0x0D,
                0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E,
                0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99,
                0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC,
                0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E,
                0x45, 0xEC, 0x42, 0xFA, 0x08, 0xB7, 0x07, 0x5D,
                0x01, 0xF5, 0xC0, 0xFF, 0x08, 0xFC, 0x00, 0xE5,
                0x0B, 0xF8, 0xC2, 0xCA, 0xF4, 0xF9, 0x0D, 0x7F,
                0x44, 0x6D, 0x19, 0xFE, 0x46, 0x97, 0x33, 0x5E,
                0x08, 0xFF, 0xD1, 0xFF, 0xC6, 0x8B, 0x24, 0x74,
                0x12, 0xFC, 0x00, 0x9F, 0x94, 0xB7, 0x06, 0xD5,
                0x40, 0x7A, 0x20, 0x9E, 0x04, 0x5F, 0x41, 0x2F,
                0x3D, 0x77, 0x36, 0x75, 0x81, 0x8A, 0x70, 0x3A,
                0x98, 0xD1, 0x71, 0x02, 0x4D, 0x01, 0xC1, 0xFF,
                0x0D, 0x00, 0xD3, 0x05, 0xF9, 0x00, 0x0B,
            ];
            self.oam_high = CGB_FEAX;
            self.hram.as_mut_slice().copy_from_slice(&CGB_HRAM);
            // Power-on HDMA5 reads 0xFF (no transfer armed). With bit 7 set the
            // read is `hdma_length | 0x80`, so seed the length to 0x7F.
            self.hdma_length = 0x7F;
        } else {
            // DMG: OAM holds uninitialised garbage; 0xFEA0-0xFEFF reads 0x00.
            const DMG_OAM: [u8; 0xA0] = [
                0xBB, 0xD8, 0xC4, 0x04, 0xCD, 0xAC, 0xA1, 0xC7,
                0x7D, 0x85, 0x15, 0xF0, 0xAD, 0x19, 0x11, 0x6A,
                0xBA, 0xC7, 0x76, 0xF8, 0x5C, 0xA0, 0x67, 0x0A,
                0x7B, 0x75, 0x56, 0x3B, 0x65, 0x5C, 0x4D, 0xA3,
                0x00, 0x05, 0xD7, 0xC9, 0x1B, 0xCA, 0x11, 0x6D,
                0x38, 0xE7, 0x13, 0x2A, 0xB1, 0x10, 0x72, 0x4D,
                0xA7, 0x47, 0x13, 0x89, 0x7C, 0x62, 0x5F, 0x90,
                0x64, 0x2E, 0xD3, 0xEF, 0xAB, 0x01, 0x15, 0x85,
                0xE8, 0x2A, 0x6E, 0x4A, 0x1F, 0xBE, 0x49, 0xB1,
                0xE6, 0x0F, 0x93, 0xE2, 0xB6, 0x87, 0x5D, 0x35,
                0xD8, 0xD4, 0x4A, 0x45, 0xCA, 0xB3, 0x33, 0x74,
                0x18, 0xC1, 0x16, 0xFB, 0x8F, 0xA4, 0x8E, 0x70,
                0xCD, 0xB4, 0x4A, 0xDC, 0xE6, 0x34, 0x32, 0x41,
                0xF9, 0x84, 0x6A, 0x99, 0xEC, 0x92, 0xF1, 0x8B,
                0x5D, 0xA5, 0x09, 0xCF, 0x3A, 0x93, 0xBC, 0xE0,
                0x15, 0x19, 0xE4, 0xB6, 0x9A, 0x04, 0x3B, 0xC1,
                0x96, 0xB7, 0x56, 0x85, 0x6A, 0xAA, 0x1E, 0x2A,
                0x80, 0xEE, 0xE7, 0x46, 0x76, 0x8B, 0x0D, 0xBA,
                0x24, 0x40, 0x42, 0x05, 0x0E, 0x04, 0x20, 0xA6,
                0x5E, 0xC1, 0x97, 0x7E, 0x44, 0x05, 0x01, 0xA9,
            ];
            const DMG_HRAM: [u8; 0x7F] = [
                0x2B, 0x0B, 0x64, 0x2F, 0xAF, 0x15, 0x60, 0x6D,
                0x61, 0x4E, 0xAC, 0x45, 0x0F, 0xDA, 0x92, 0xF3,
                0x83, 0x38, 0xE4, 0x4E, 0xA7, 0x6C, 0x38, 0x58,
                0xBE, 0xEA, 0xE5, 0x81, 0xB4, 0xCB, 0xBF, 0x7B,
                0x59, 0xAD, 0x50, 0x13, 0x5E, 0xF6, 0xB3, 0xC1,
                0xDC, 0xDF, 0x9E, 0x68, 0xD7, 0x59, 0x26, 0xF3,
                0x62, 0x54, 0xF8, 0x36, 0xB7, 0x78, 0x6A, 0x22,
                0xA7, 0xDD, 0x88, 0x15, 0xCA, 0x96, 0x39, 0xD3,
                0xE6, 0x55, 0x6E, 0xEA, 0x90, 0x76, 0xB8, 0xFF,
                0x50, 0xCD, 0xB5, 0x1B, 0x1F, 0xA5, 0x4D, 0x2E,
                0xB4, 0x09, 0x47, 0x8A, 0xC4, 0x5A, 0x8C, 0x4E,
                0xE7, 0x29, 0x50, 0x88, 0xA8, 0x66, 0x85, 0x4B,
                0xAA, 0x38, 0xE7, 0x6B, 0x45, 0x3E, 0x30, 0x37,
                0xBA, 0xC5, 0x31, 0xF2, 0x71, 0xB4, 0xCF, 0x29,
                0xBC, 0x7F, 0x7E, 0xD0, 0xC7, 0xC3, 0xBD, 0xCF,
                0x59, 0xEA, 0x39, 0x01, 0x2E, 0x00, 0x69,
            ];
            for (i, b) in DMG_OAM.iter().enumerate() {
                self.oam.write(OAM_START + i as u16, *b);
            }
            self.hram.as_mut_slice().copy_from_slice(&DMG_HRAM);
        }
    }
}

impl memory::Addressable for Mmio {
    fn read(&self, addr: u16) -> u8 {
        // While an OAM DMA transfer is in progress, a CPU read of a memory
        // region that conflicts with the DMA source returns the byte the DMA
        // is currently moving into OAM, not the real memory. I/O and HRAM are
        // unaffected (Gambatte gates the conflict on `p < mm_hram_begin`).
        if self.dma_read_conflict_active() && self.dma_address_conflicts(addr) {
            return self.dma_conflict_byte(addr);
        }
        {
            // Normal memory access (the conflict above already handled).
            match addr {
                BIOS_START..=BIOS_END => {
                    match self.read(REG_BOOT_OFF) {
                        0 => {
                            match &self.bios {
                                Some(bios) => bios.read(addr),
                                None => EMPTY_BYTE,
                            }
                        },
                        _ => {
                            match &self.cartridge {
                                Some(cart) => cart.read(addr),
                                None => EMPTY_BYTE,
                            }
                        }
                    }
                },
                CARTRIDGE_AFTER_BIOS_START..=CARTRIDGE_END => {
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
                VRAM_START..=VRAM_END => {
                    if self.cgb_features_enabled && self.vram_bank == 1 {
                        self.vram_bank1.read(addr)
                    } else {
                        self.vram.read(addr) // Always use bank 0 on DMG or when bank 0 is selected
                    }
                },
                EXTERNAL_RAM_START..=EXTERNAL_RAM_END => {
                    match &self.cartridge {
                        Some(cart) => cart.read(addr),
                        None => EMPTY_BYTE,
                    }
                },
                WRAM_START..=WRAM_END => self.wram.read(addr),
                WRAM_BANK_START..=WRAM_BANK_END => {
                    if self.cgb_features_enabled {
                        match self.wram_bank_select {
                            0 | 1 => self.wram_bank.read(addr), // Bank 0 and 1 use the original wram_bank
                            2..=7 => {
                                let bank_index = (self.wram_bank_select - 2) as usize; 
                                self.wram_banks[bank_index].read(addr)
                            },
                            _ => self.wram_bank.read(addr), // Fallback to bank 1
                        }
                    } else {
                        self.wram_bank.read(addr) // DMG always uses bank 1
                    }
                },
                ECHO_RAM_START..=ECHO_RAM_END => {
                    let addr = addr - 0x2000;
                    match addr {
                        0..WRAM_START => panic!("This is literally never possible"),
                        WRAM_START..=WRAM_END => self.wram.read(addr),
                        WRAM_BANK_START..=ECHO_RAM_MIRROR_END => {
                            if self.cgb_features_enabled {
                                match self.wram_bank_select {
                                    0 | 1 => self.wram_bank.read(addr), // Bank 0 and 1 use the original wram_bank
                                    2..=7 => {
                                        let bank_index = (self.wram_bank_select - 2) as usize; 
                                        self.wram_banks[bank_index].read(addr)
                                    },
                                    _ => self.wram_bank.read(addr), // Fallback to bank 1
                                }
                            } else {
                                self.wram_bank.read(addr) // DMG always uses bank 1
                            }
                        },
                        0xDE00..=0xFFFF => panic!("This is literally never possible"),
                    }
                },
                // While a transfer is placing bytes into OAM the DMA owns the
                // OAM bus, so a CPU read returns 0xFF (Gambatte's
                // `oamDmaPos_ < oam_size` gate).
                OAM_START..=OAM_END => {
                    if self.dma_transfer_in_progress() {
                        0xFF
                    } else {
                        self.oam.read(addr)
                    }
                }
                // 0xFEA0-0xFEFF. While an OAM-DMA transfer owns the bus the read
                // returns 0xFF (Gambatte's `oamDmaPos_ < oam_size` gate). Otherwise
                // it returns the `oam_high` shadow: CGB mirrors into the OAM index
                // space masked with 0xE7; DMG indexes directly and the shadow is
                // initialised to 0x00 (Gambatte `ioamhram_[p - mm_oam_begin]`).
                UNUSED_START..=UNUSED_END => {
                    if self.dma_transfer_in_progress() {
                        EMPTY_BYTE
                    } else if self.cgb_features_enabled {
                        self.oam_high[((addr & 0xFF) & 0xE7) as usize - 0xA0]
                    } else {
                        self.oam_high[(addr & 0xFF) as usize - 0xA0]
                    }
                }
                IO_REGISTERS_START..=IO_REGISTERS_END => {
                    match addr {
                        input::JOYP => self.input.read(addr),
                        // TAC: only bits 0-2 are implemented; the unused upper
                        // bits always read 1 (Gambatte ORs 0xF8).
                        timer::TAC => self.timer.read(addr) | 0xF8,
                        timer::DIV..=timer::TAC => self.timer.read(addr),
                serial::SB..=serial::SC => self.serial.read(addr),
                cpu::registers::INTERRUPT_FLAG => self.io_registers.read(addr) | 0xE0,
                        audio::NR10..=audio::NR14 => self.audio.read(addr),
                        audio::NR21..=audio::NR24 => self.audio.read(addr),
                        audio::NR30..=audio::NR34 => self.audio.read(addr),
                        audio::NR41..=audio::NR52 => self.audio.read(addr),
                        audio::WAV_START..=audio::WAV_END => self.audio.read(addr),
                        // OAM-DMA source register (0xFF46). On CGB it reads
                        // back the written value; on DMG it always reads 0xFF
                        // (Gambatte's DMG ioamhram_[0x146] post-boot shadow).
                        REG_DMA => {
                            if self.cgb_features_enabled {
                                self.io_registers.read(addr)
                            } else {
                                0xFF
                            }
                        },

                        // KEY0 (0xFF4C, CGB DMG-compat select). Write-once and
                        // only meaningful while the boot ROM is mapped; once
                        // boot is disabled it reads 0xFF on both models
                        // (Gambatte `case 0x4C: if (!biosMode_) return 0xFF`).
                        REG_KEY0 => {
                            if self.io_registers.read(REG_BOOT_OFF) != 0 {
                                0xFF
                            } else if self.cgb_features_enabled {
                                (if self.key0_dmg_mode { 0x01 } else { 0x00 }) | 0xFE
                            } else {
                                0xFF
                            }
                        },
                        REG_KEY1 => {
                            if self.cgb_features_enabled {
                                // KEY1: Current speed (bit 7) | Switch armed (bit 0)
                                let speed_bit = if self.key1_current_speed { 0x80 } else { 0x00 };
                                let armed_bit = if self.key1_switch_armed { 0x01 } else { 0x00 };
                                speed_bit | armed_bit | 0x7E // Bits 1-6 = 1, bit 7 = current speed, bit 0 = switch armed
                            } else {
                                0xFF // DMG hardware returns 0xFF for CGB registers
                            }
                        },
                        REG_VBK => {
                            if self.cgb_features_enabled {
                                self.vram_bank | 0xFE // Bit 0 = bank, bits 1-7 = 1
                            } else {
                                0xFF // DMG hardware returns 0xFF for CGB registers
                            }
                        },
                        // HDMA1-4 (FF51-FF54) are write-only on real hardware;
                        // reads always return 0xFF. See Gambatte
                        // `nontrivial_ff_read` in memory.cpp, which falls
                        // through to the never-written ioamhram_ shadow.
                        REG_HDMA1 | REG_HDMA2 | REG_HDMA3 | REG_HDMA4 => 0xFF,
                        REG_HDMA5 => {
                            if self.cgb_features_enabled {
                                if self.hdma_enabled {
                                    // In-progress: bit 7 clear, low 7 bits =
                                    // blocks remaining minus 1.
                                    self.hdma_length & 0x7F
                                } else {
                                    // Done / cancelled / never-armed: bit 7
                                    // set. `hdma_length == 0x7F` after a
                                    // completed transfer encodes 0xFF.
                                    self.hdma_length | 0x80
                                }
                            } else {
                                0xFF
                            }
                        },
                        REG_SVBK => {
                            if self.cgb_features_enabled {
                                // Read back the RAW written low 3 bits, not the
                                // bank-0->1 remap (Gambatte stores the written
                                // value verbatim; the remap is access-time only).
                                (self.io_registers.read(REG_SVBK) & 0x07) | 0xF8
                            } else {
                                0xFF
                            }
                        },
                        REG_BCPS => {
                            if self.cgb_features_enabled {
                                // Bit 6 is unused and reads 1 (Gambatte stores
                                // the ffxxDump power-on 0x40 in that bit).
                                self.bg_palette_spec | 0x40
                            } else {
                                0xFF
                            }
                        },
                        REG_BCPD => {
                            if self.cgb_features_enabled {
                                let index = (self.bg_palette_spec & 0x3F) as usize; // Bits 0-5 = address
                                self.bg_palette_ram[index]
                            } else {
                                0xFF
                            }
                        },
                        REG_OCPS => {
                            if self.cgb_features_enabled {
                                // Bit 6 is unused and reads 1 (as BCPS).
                                self.obj_palette_spec | 0x40
                            } else {
                                0xFF
                            }
                        },
                        REG_OCPD => {
                            if self.cgb_features_enabled {
                                let index = (self.obj_palette_spec & 0x3F) as usize; // Bits 0-5 = address
                                self.obj_palette_ram[index]
                            } else {
                                0xFF
                            }
                        },
                        // Bit 7 of STAT is unused but always reads as 1 on real
                        // hardware. See Gambatte memory.cpp case 0x41.
                        ppu::LCD_STATUS => self.io_registers.read(addr) | 0x80,

                        // CGB-only registers with unused bits that read 1 (DMG
                        // returns 0xFF, handled by the FF51-77 catch-all below).
                        // RP/IR (0xFF56): bits 0,6,7 writable; bit 1 reads the IR
                        // input (no link -> 1) and the remaining bits read 1.
                        // Gambatte: `ioamhram_[0x156] | 0x02`, power-on 0x3E.
                        0xFF56 if self.cgb_features_enabled => {
                            self.io_registers.read(0xFF56) | 0x02
                        }
                        // OPRI (0xFF6C): only bit 0 implemented; bits 1-7 read 1.
                        0xFF6C if self.cgb_features_enabled => {
                            self.io_registers.read(0xFF6C) | 0xFE
                        }
                        // Undocumented FF75: only bits 4-6 are read/writable.
                        0xFF75 if self.cgb_features_enabled => {
                            self.io_registers.read(0xFF75) | 0x8F
                        }
                        // Unmapped CGB IO holes (no register) read open-bus
                        // 0xFF: FF57-FF67, FF6D-FF6F, FF71. (FF68/6A/6C/70 are
                        // handled above.)
                        0xFF57..=0xFF67 | 0xFF6D..=0xFF6F | 0xFF71
                            if self.cgb_features_enabled => 0xFF,

                        // 0xFF78-0xFF7F are unmapped on both DMG and CGB.
                        // Gambatte's nontrivial_ff_read falls through to a
                        // never-written 0xFF shadow; writes are dropped.
                        0xFF78..=0xFF7F => 0xFF,

                        // Genuinely unmapped IO holes on both models: no
                        // register backs them, so reads return open-bus 0xFF
                        // (Gambatte nontrivial_ff_read falls through to the
                        // never-written ioamhram_ shadow). 0xFF03 (between SC
                        // and DIV), 0xFF08-0xFF0E (between TAC and IF), 0xFF15
                        // (NR20), 0xFF1F (NR40), 0xFF27-0xFF2F (between NR52 and
                        // wave RAM), 0xFF4E.
                        0xFF03 | 0xFF08..=0xFF0E | 0xFF15 | 0xFF1F
                        | 0xFF27..=0xFF2F | 0xFF4E => 0xFF,

                        // BOOT-ROM-disable (0xFF50): write-once. Once disabled
                        // it reads as 0xFF on both models; while the boot ROM is
                        // mapped (stored 0) keep the raw value so the internal
                        // boot-mapping check still distinguishes the states.
                        REG_BOOT_OFF => {
                            if self.io_registers.read(REG_BOOT_OFF) != 0 { 0xFF } else { 0x00 }
                        },

                        // CGB-only registers (0xFF51-0xFF77, the ones not
                        // explicitly handled above) read 0xFF on DMG.
                        0xFF51..=0xFF77 if !self.cgb_features_enabled => 0xFF,

                        _ => self.io_registers.read(addr),
                    }
                }
                HRAM_START..=HRAM_END => self.hram.read(addr),
                IE_REGISTER => self.ie_register,
            }
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        // While an OAM DMA is running the CPU bus operates normally except for
        // (1) the source-region conflict, which redirects the write into OAM,
        // and (2) OAM itself, which the DMA owns. Everything else (non-conflict
        // ROM/VRAM/SRAM/WRAM/IO writes) proceeds as usual.
        if self.dma_active && self.dma_write_conflict(addr, value) {
            return;
        }
        {
            match addr {
                CARTRIDGE_START..=CARTRIDGE_END => {
                    if let Some(cart) = self.cartridge.as_mut() { cart.write(addr, value) }
                },
                CARTRIDGE_BANK_START..=CARTRIDGE_BANK_END => {
                    if let Some(cart) = self.cartridge.as_mut() { cart.write(addr, value) }
                },
                VRAM_START..=VRAM_END => {
                    if self.cgb_features_enabled && self.vram_bank == 1 {
                        self.vram_bank1.write(addr, value)
                    } else {
                        self.vram.write(addr, value) // Always use bank 0 on DMG or when bank 0 is selected
                    }
                },
                EXTERNAL_RAM_START..=EXTERNAL_RAM_END => {
                    if let Some(cart) = self.cartridge.as_mut() { cart.write(addr, value) }
                },
                WRAM_START..=WRAM_END => self.wram.write(addr, value),
                WRAM_BANK_START..=WRAM_BANK_END => {
                    if self.cgb_features_enabled {
                        match self.wram_bank_select {
                            0 | 1 => self.wram_bank.write(addr, value), // Bank 0 and 1 use the original wram_bank
                            2..=7 => {
                                let bank_index = (self.wram_bank_select - 2) as usize; 
                                self.wram_banks[bank_index].write(addr, value)
                            },
                            _ => self.wram_bank.write(addr, value), // Fallback to bank 1
                        }
                    } else {
                        self.wram_bank.write(addr, value) // DMG always uses bank 1
                    }
                },
                ECHO_RAM_START..=ECHO_RAM_END => {
                    let addr = addr - 0x2000;
                    match addr {
                        0..WRAM_START => panic!("This is literally never possible"),
                        WRAM_START..=WRAM_END => self.wram.write(addr, value),
                        WRAM_BANK_START..=ECHO_RAM_MIRROR_END => {
                            if self.cgb_features_enabled {
                                match self.wram_bank_select {
                                    0 | 1 => self.wram_bank.write(addr, value), // Bank 0 and 1 use the original wram_bank
                                    2..=7 => {
                                        let bank_index = (self.wram_bank_select - 2) as usize; 
                                        self.wram_banks[bank_index].write(addr, value)
                                    },
                                    _ => self.wram_bank.write(addr, value), // Fallback to bank 1
                                }
                            } else {
                                self.wram_bank.write(addr, value) // DMG always uses bank 1
                            }
                        },
                        0xDE00..=0xFFFF => panic!("This is literally never possible"),
                    }
                },
                // While a transfer is in progress the DMA owns the OAM bus, so a
                // CPU write to OAM is dropped; otherwise it lands normally.
                OAM_START..=OAM_END => {
                    if !self.dma_transfer_in_progress() {
                        self.oam.write(addr, value);
                        // Flag the position-buffer change for the PPU snapshot
                        // (Gambatte `lcd_.oamChange(cc)` on an OAM write). Only Y/X
                        // (bytes 0,1 of each entry) feed the snapshot, but Gambatte
                        // calls oamChange on any OAM write, so flag unconditionally.
                        self.oam_write_pending = true;
                    }
                }
                // CGB OAM mirror (0xFEA0-0xFEFF). Writable only when the OAM bus
                // is free (no in-progress OAM DMA); otherwise dropped. DMG
                // ignores writes here entirely.
                UNUSED_START..=UNUSED_END => {
                    if self.cgb_features_enabled && !self.dma_transfer_in_progress() {
                        self.oam_high[((addr & 0xFF) & 0xE7) as usize - 0xA0] = value;
                    }
                }
                IO_REGISTERS_START..=IO_REGISTERS_END => {
                    match addr {
                        input::JOYP => self.input.write(addr, value),
                        timer::DIV => {
                            // Gambatte 0x04: realign the pending serial event to
                            // the new divider phase before resetting DIV. Serial
                            // now shares the master cc, so feed the DIV write's
                            // canonical access cc (`access_cc()` = abs_cc + 5),
                            // the same cc the timer's own divReset resolves on
                            // (M8 serial merge).
                            let phase = self.timer.access_cc();
                            self.serial.realign_to_div(phase);
                            self.write_timer(addr, value);
                        }
                        timer::DIV..=timer::TAC => self.write_timer(addr, value),
                serial::SB => self.serial.write(addr, value),
                        serial::SC => self.write_serial_sc(value),
                        audio::NR10..=audio::NR52 | audio::WAV_START..=audio::WAV_END => {
                            self.write_apu(addr, value);
                        }
                        REG_DMA => self.start_oam_dma(value),
                        ppu::LCD_CONTROL => self.write_lcd_control(value),
                        ppu::LCD_STATUS => self.write_lcd_status(value),
                        ppu::LY => self.write_ly_from_cpu(),
                        ppu::LYC => {
                            self.io_registers.write(addr, value);
                            self.stat_register_write_pending = true;
                        }
                        ppu::SCY..=ppu::WX => self.io_registers.write(addr, value),
                        REG_BOOT_OFF => {
                            // Write-once: once the boot ROM has been unmapped
                            // (stored byte non-zero), further writes are ignored
                            // and the register stays latched (reads 0xFF). This
                            // matches hardware and Gambatte's sticky biosMode_.
                            if self.io_registers.read(REG_BOOT_OFF) == 0 {
                                // When boot ROM is disabled, lock the KEY0 register
                                if self.cgb_features_enabled && value != 0 {
                                    self.key0_locked = true;
                                }
                                self.io_registers.write(addr, value);
                            }
                        },
                        
                        // CGB registers - only writable when CGB features are enabled
                        REG_KEY0 => {
                            if self.cgb_features_enabled && !self.key0_locked {
                                // KEY0 can only be written before boot ROM finishes (when not locked)
                                self.key0_dmg_mode = (value & 0x01) != 0;
                            }
                            // Writes ignored if not CGB, or if KEY0 is locked
                        },
                        REG_KEY1 => {
                            if self.cgb_features_enabled {
                                // Only bit 0 (switch armed) is writable
                                self.key1_switch_armed = (value & 0x01) != 0;
                            }
                            // On DMG hardware, writes are ignored
                        },
                        REG_VBK => {
                            if self.cgb_features_enabled {
                                self.vram_bank = value & 0x01; // Only bit 0 is writable
                            }
                            // On DMG hardware, writes are ignored
                        },
                        REG_HDMA1 => {
                            if self.cgb_features_enabled {
                                self.hdma_source = (self.hdma_source & 0x00FF) | ((value as u16) << 8);
                            }
                        },
                        REG_HDMA2 => {
                            if self.cgb_features_enabled {
                                // Low nibble of source low byte is masked off on real hardware.
                                // See Gambatte memory.cpp case 0x52: `data & 0xF0`.
                                self.hdma_source = (self.hdma_source & 0xFF00) | ((value as u16) & 0x00F0);
                            }
                        },
                        REG_HDMA3 => {
                            if self.cgb_features_enabled {
                                self.hdma_dest = (self.hdma_dest & 0x00FF) | ((value as u16) << 8);
                            }
                        },
                        REG_HDMA4 => {
                            if self.cgb_features_enabled {
                                // Low nibble of dest low byte is masked off on real hardware.
                                // See Gambatte memory.cpp case 0x54: `data & 0xF0`.
                                self.hdma_dest = (self.hdma_dest & 0xFF00) | ((value as u16) & 0x00F0);
                            }
                        },
                        REG_HDMA5 => {
                            if self.cgb_features_enabled {
                                let length_blocks_minus_1 = value & 0x7F;
                                let new_mode = (value >> 7) & 0x01; // 0=GDMA, 1=HDMA
                                let lcd_on = (self.io_registers.read(ppu::LCD_CONTROL)
                                    & (ppu::LCDCFlags::DisplayEnable as u8)) != 0;

                                if self.hdma_enabled {
                                    // HDMA already armed: bit7=0 cancels,
                                    // bit7=1 restarts with new length / src
                                    // / dst (Gambatte memory.cpp ~line 1266).
                                    if new_mode == 0 {
                                        // FF55=00 disable-vs-m0-edge race
                                        // (Gambatte `disableHdma`): the disable
                                        // only clears the FUTURE m0-edge schedule.
                                        // A block whose m0 edge already fired
                                        // (`intevent_dma` latched) STILL runs. The
                                        // bus stashes that decision in
                                        // `hdma_disable_fires` by evaluating the
                                        // PPU m0Time at this write's access cc.
                                        if self.hdma_disable_fires == Some(true) {
                                            // m0 edge already passed: keep the
                                            // request latched so the block fires
                                            // this M-cycle (step_hdma), exactly as
                                            // Gambatte's `dma()` runs despite the
                                            // disable. The block-fire decrements
                                            // length and ends HDMA normally.
                                            self.hdma_req_pending = true;
                                            // Leave hdma_enabled = true so the
                                            // M-cycle fire gate passes; the
                                            // post-block length wrap clears it.
                                        } else {
                                            // Disable wins (edge not yet reached):
                                            // Gambatte preserves the remaining
                                            // length and flips bit 7 on read.
                                            self.hdma_enabled = false;
                                            self.hdma_req_pending = false;
                                        }
                                        self.hdma_disable_fires = None;
                                    } else {
                                        self.hdma_length = length_blocks_minus_1;
                                        if !lcd_on {
                                            // LCD off: Gambatte fires immediately
                                            // (no HDMA period concept).
                                            self.hdma_req_pending = true;
                                        } else {
                                            // LCD on: gate the immediate kick on the
                                            // LIVE isHdmaPeriod(cc+4), resolved by
                                            // the bus after this write (C7-full).
                                            self.hdma_kick_eval_pending = 2;
                                        }
                                    }
                                } else if new_mode == 0 {
                                    // GDMA kick (synchronous).
                                    let total_bytes = (length_blocks_minus_1 as usize + 1) * 16;
                                    self.execute_gdma(total_bytes);
                                    self.hdma_length = 0x7F; // FF55 reads 0xFF
                                } else {
                                    // Arm HDMA. Fire the first block now if
                                    // LCD off; otherwise gate the immediate kick
                                    // on the live isHdmaPeriod(cc+4) (resolved by
                                    // the bus), else the Mode 3->0 trigger arms it.
                                    self.hdma_enabled = true;
                                    self.hdma_length = length_blocks_minus_1;
                                    if !lcd_on {
                                        self.hdma_req_pending = true;
                                    } else {
                                        self.hdma_kick_eval_pending = 1;
                                    }
                                }
                                // Consume the per-write disable-race decision (only
                                // the disable branch above uses it).
                                self.hdma_disable_fires = None;
                            }
                        },
                        REG_SVBK => {
                            if self.cgb_features_enabled {
                                let bank = value & 0x07; // Bits 0-2 = bank select
                                self.wram_bank_select = if bank == 0 { 1 } else { bank }; // Bank 0 selects bank 1
                                // Keep the raw written value for read-back (the
                                // remapped bank above is access-time only).
                                self.io_registers.write(REG_SVBK, value);
                            }
                        },
                        REG_BCPS => {
                            if self.cgb_features_enabled {
                                self.bg_palette_spec = value;
                            }
                        },
                        REG_BCPD => {
                            if self.cgb_features_enabled {
                                let index = (self.bg_palette_spec & 0x3F) as usize; // Bits 0-5 = address
                                self.bg_palette_ram[index] = value;
                                
                                // Auto-increment if bit 7 is set
                                if (self.bg_palette_spec & 0x80) != 0 {
                                    let new_index = ((self.bg_palette_spec & 0x3F) + 1) & 0x3F;
                                    self.bg_palette_spec = (self.bg_palette_spec & 0x80) | new_index;
                                }
                            }
                        },
                        REG_OCPS => {
                            if self.cgb_features_enabled {
                                self.obj_palette_spec = value;
                            }
                        },
                        REG_OCPD => {
                            if self.cgb_features_enabled {
                                let index = (self.obj_palette_spec & 0x3F) as usize; // Bits 0-5 = address
                                self.obj_palette_ram[index] = value;
                                
                                // Auto-increment if bit 7 is set
                                if (self.obj_palette_spec & 0x80) != 0 {
                                    let new_index = ((self.obj_palette_spec & 0x3F) + 1) & 0x3F;
                                    self.obj_palette_spec = (self.obj_palette_spec & 0x80) | new_index;
                                }
                            }
                        },
                        
                        // 0xFF78-0xFF7F are unmapped: writes are dropped.
                        0xFF78..=0xFF7F => {}

                        // RP/IR (0xFF56): only bits 0,6,7 are writable; bits 1-5
                        // retain their (power-on) value. Gambatte:
                        // `(data & 0xC1) | (old & 0x3E)`.
                        0xFF56 if self.cgb_features_enabled => {
                            let old = self.io_registers.read(0xFF56);
                            self.io_registers.write(0xFF56, (value & 0xC1) | (old & 0x3E));
                        }

                        _ => self.io_registers.write(addr, value),
                    }
                }
                HRAM_START..=HRAM_END => self.hram.write(addr, value),
                IE_REGISTER => self.ie_register = value,
            }
        }
    }
}
