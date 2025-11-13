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

    // Whether the HDMA block owed for the *current* eligibility period has
    // already been serviced. rustyboi fires the period block immediately at the
    // rising edge, whereas Gambatte defers it to the `intevent_dma` event; this
    // flag lets `on_cpu_halt` recover Gambatte's distinction between "in period,
    // block already done" (hdma_high) and "in period, block still owed"
    // (hdma_requested -> fires on the deferred/unhalt path). Reset on the period
    // falling edge.
    #[serde(skip, default)]
    hdma_block_done_this_period: bool,

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
            hdma_req_pending: false,
            halt_hdma_state: HaltHdmaState::Low,
            hdma_is_in_period_cached: false,
            hdma_prev_stat_mode: 0,
            hdma_prev_period: false,
            cpu_halted: false,
            hdma_block_done_this_period: false,

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
        let abs_cc = self.timer.abs_cc();
        let div_resets = self.timer.div_reset_count();
        let div_anchor = self.timer.div_anchor();
        let ds = self.is_double_speed_mode();
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

    /// Execute a CGB General-Purpose DMA (GDMA) transfer synchronously.
    /// Copies `length` bytes from `self.hdma_source` into VRAM starting at
    /// `self.hdma_dest`. Mirrors Gambatte's `Memory::dma`:
    ///   - If the LCD is off, GDMA does not run.
    ///   - Destination clamped if it would overflow the 16-bit address
    ///     space (memory.cpp:335-337).
    fn execute_gdma(&mut self, length: usize) {
        let lcdc = self.io_registers.read(ppu::LCD_CONTROL);
        if lcdc & (ppu::LCDCFlags::DisplayEnable as u8) == 0 {
            return;
        }

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
        let interleave = self.dma_active;
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
        // overlap; +6 lands the next STAT-mode read on the exact mode-0 dot for
        // the gdma_cycles boundary pairs.
        let (per_byte, setup) = if self.is_double_speed_mode() { (4, 5) } else { (2, 4) };
        self.pending_dma_stall += (effective_length as u32) * per_byte + setup + 6;
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

    /// CPU has left HALT. Clears the `intreq_.halted()` mirror so the
    /// period-edge `flagHdmaReq` resumes (video.h:41).
    pub fn clear_cpu_halt(&mut self) {
        self.cpu_halted = false;
    }

    pub fn update_hdma_period_cache(&mut self, in_period: bool) {
        self.hdma_is_in_period_cached = in_period;
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
    pub fn on_cpu_halt(&mut self) {
        self.cpu_halted = true;
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
        self.halt_hdma_state = if self.hdma_req_pending {
            HaltHdmaState::Requested
        } else if self.hdma_enabled && self.hdma_is_in_period_cached {
            if self.hdma_block_done_this_period {
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

    /// Execute one 0x10-byte HDMA block. Caller must have verified
    /// `hdma_req_pending && hdma_enabled`. Bytes are copied synchronously;
    /// callers charge the returned CPU-cycle stall via the outer per-cycle
    /// loop so PPU/timer/audio continue to tick during the transfer.
    pub fn run_hdma_block(&mut self) -> u32 {
        for _ in 0..0x10 {
            self.copy_dma_byte(self.hdma_source, self.hdma_dest);
            self.hdma_source = self.hdma_source.wrapping_add(1);
            self.hdma_dest = self.hdma_dest.wrapping_add(1);

            // Interleave one OAM-DMA M-cycle per HDMA byte. Mirrors Gambatte's
            // `Memory::dma` which advances `oamDmaPos_` inside the HDMA loop.
            if self.dma_active {
                self.dma_advance_one_mcycle();
            }
        }

        self.hdma_length = self.hdma_length.wrapping_sub(1) & 0x7F;
        // After underflow from 0x00 -> 0xFF -> masked = 0x7F the transfer
        // is complete: FF55 reads 0xFF.
        if self.hdma_length == 0x7F {
            self.hdma_enabled = false;
        }
        self.hdma_req_pending = false;

        // Stall: Gambatte `Memory::dma` advances `cc` by `(2 + 2*ds) * 16` per
        // byte (= 32 / 64) plus a trailing `cc += 4`. Gambatte runs the block as
        // an event preceded by `Interrupter::prefetch` (next opcode fetched
        // before the transfer's cc advance); synchronous HDMA here absorbs that
        // prefetch/setup overlap with +6 so the post-block stall return lands
        // the next STAT-mode read on the exact mode-0 dot (36+6 / 68+6).
        if self.is_double_speed_mode() { 74 } else { 42 }
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
        if !self.dma_active {
            return;
        }

        // One source byte is transferred per M-cycle (4 dots), not per dot.
        self.dma_subcycle += 1;
        if self.dma_subcycle < 4 {
            return;
        }
        self.dma_subcycle = 0;
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

        // Gambatte's period-edge `flagHdmaReq` is suppressed while the CPU is
        // halted (video.h:41 `if (!intreq_.halted())`): during HALT the block is
        // governed by the `haltHdmaState_` machine and re-flagged only on unhalt,
        // so the edge must NOT auto-arm here. Edge trackers are still advanced so
        // the rising edge is detected cleanly once the CPU unhalts.
        let arm_allowed = !self.cpu_halted;
        if lcd_on && period.is_some() {
            // Rising edge of the eligibility window arms a block.
            if arm_allowed && !self.hdma_prev_period && in_period && self.hdma_enabled {
                self.hdma_req_pending = true;
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
            if arm_allowed && lcd_on && self.hdma_prev_stat_mode == 3 && mode == 0 && self.hdma_enabled
            {
                self.hdma_req_pending = true;
            }
            self.hdma_prev_stat_mode = mode;
            self.hdma_prev_period = in_period;
        }

        if self.hdma_req_pending && self.hdma_enabled {
            self.pending_dma_stall += self.run_hdma_block();
            if in_period {
                self.hdma_block_done_this_period = true;
            }
            // Gambatte intevent_dma (memory.cpp:280): after the block, a halt-time
            // `hdma_requested` collapses to `hdma_low` so a subsequent unhalt does
            // not re-fire it (the request has now been serviced).
            if self.halt_hdma_state == HaltHdmaState::Requested {
                self.halt_hdma_state = HaltHdmaState::Low;
            }
        }
    }

    /// Consume the CPU-cycle stall owed for completed HDMA/GDMA transfers.
    pub fn take_dma_stall(&mut self) -> u32 {
        std::mem::take(&mut self.pending_dma_stall)
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
            self.timer.stop_div_reset();
            if self.timer.take_pending_irq() {
                self.request_interrupt(cpu::registers::InterruptFlag::Timer);
            }
            // Gambatte order (memory.cpp:466): after the DIV reset (which the APU
            // mirrors as a `PSG::divReset` fold on the next `sync_cc`), apply the
            // `PSG::speedChange` fold. Sync first so the divReset fold + flush to
            // the switch cc happen, then re-fold for the speed transition.
            self.sync_apu_cc();
            self.audio.psg_speed_change(old_ds);
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
        self.io_registers.write(ppu::LCD_CONTROL, value);
        self.stat_register_write_pending = true;
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
            // CGB: OAM cleared to 0x00. The 0xFEA0-0xFEFF shadow holds the
            // cgb feax dump (the read path masks the index with 0xE7).
            const CGB_FEAX: [u8; 0x60] = [
                0x08, 0x01, 0xEF, 0xDE, 0x06, 0x4A, 0xCD, 0xBD,
                0x08, 0x01, 0xEF, 0xDE, 0x06, 0x4A, 0xCD, 0xBD,
                0x08, 0x01, 0xEF, 0xDE, 0x06, 0x4A, 0xCD, 0xBD,
                0x08, 0x01, 0xEF, 0xDE, 0x06, 0x4A, 0xCD, 0xBD,
                0x00, 0x90, 0xF7, 0x7F, 0xC0, 0xB1, 0xBC, 0xFB,
                0x00, 0x90, 0xF7, 0x7F, 0xC0, 0xB1, 0xBC, 0xFB,
                0x00, 0x90, 0xF7, 0x7F, 0xC0, 0xB1, 0xBC, 0xFB,
                0x00, 0x90, 0xF7, 0x7F, 0xC0, 0xB1, 0xBC, 0xFB,
                0x24, 0x13, 0xFD, 0x3A, 0x10, 0x10, 0xAD, 0x45,
                0x24, 0x13, 0xFD, 0x3A, 0x10, 0x10, 0xAD, 0x45,
                0x24, 0x13, 0xFD, 0x3A, 0x10, 0x10, 0xAD, 0x45,
                0x24, 0x13, 0xFD, 0x3A, 0x10, 0x10, 0xAD, 0x45,
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
                                        // Cancel only: Gambatte preserves
                                        // the existing remaining length and
                                        // just flips bit 7 on read by
                                        // disabling HDMA.
                                        self.hdma_enabled = false;
                                        self.hdma_req_pending = false;
                                    } else {
                                        self.hdma_length = length_blocks_minus_1;
                                        if !lcd_on || self.hdma_is_in_period_cached {
                                            self.hdma_req_pending = true;
                                        }
                                    }
                                } else if new_mode == 0 {
                                    // GDMA kick (synchronous).
                                    let total_bytes = (length_blocks_minus_1 as usize + 1) * 16;
                                    self.execute_gdma(total_bytes);
                                    self.hdma_length = 0x7F; // FF55 reads 0xFF
                                } else {
                                    // Arm HDMA. Fire the first block now if
                                    // LCD off or already in the HDMA period;
                                    // otherwise the PPU's Mode 3->0 trigger
                                    // will set the req on the next H-blank.
                                    self.hdma_enabled = true;
                                    self.hdma_length = length_blocks_minus_1;
                                    if !lcd_on || self.hdma_is_in_period_cached {
                                        self.hdma_req_pending = true;
                                    }
                                }
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
