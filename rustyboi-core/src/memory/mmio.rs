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
    // Previous STAT mode observed by `step_dma`, used to detect the Mode 3->0
    // (HBlank) edge that arms an HDMA block. Not part of save state.
    #[serde(skip, default)]
    hdma_prev_stat_mode: u8,

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
            ly_write_pending: false,
            stat_register_write_pending: false,
            
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

    pub fn step_serial(&mut self) {
        let divider = self.timer.internal_counter();
        let mut serial = self.serial.clone();
        serial.step(divider, self);
        self.serial = serial;
    }

    pub fn set_serial_cgb(&mut self, cgb: bool) {
        self.serial.set_cgb(cgb);
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

    pub fn step_audio(&mut self) {
        let mut audio = self.audio.clone();
        audio.step(self);
        self.audio = audio;
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
    fn copy_dma_byte(&mut self, src: u16, dst: u16) {
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

        for _ in 0..effective_length {
            self.copy_dma_byte(src, dst);
            src = src.wrapping_add(1);
            dst = dst.wrapping_add(1);
        }

        self.hdma_source = src;
        self.hdma_dest = dst;

        // Same per-block stall as run_hdma_block (36 SS / 68 DS), charged for
        // every 0x10-byte block of the immediate transfer.
        let blocks = (effective_length / 0x10) as u32;
        let per_block = if self.is_double_speed_mode() { 68 } else { 36 };
        self.pending_dma_stall += blocks * per_block;
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

    pub fn update_hdma_period_cache(&mut self, in_period: bool) {
        self.hdma_is_in_period_cached = in_period;
    }

    pub fn hdma_is_in_period_cached(&self) -> bool {
        self.hdma_is_in_period_cached
    }

    /// CPU has just entered HALT. Mirrors Gambatte's `Memory::halt`
    /// (memory.cpp:407): records the halt-HDMA state and acks any
    /// currently flagged req so it does not double-fire on unhalt.
    pub fn on_cpu_halt(&mut self) {
        if !self.cgb_features_enabled {
            self.halt_hdma_state = HaltHdmaState::Low;
            return;
        }
        self.halt_hdma_state = if self.hdma_req_pending {
            HaltHdmaState::Requested
        } else if self.hdma_enabled && self.hdma_is_in_period_cached {
            HaltHdmaState::High
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

        // Stall: Gambatte `Memory::dma` advances `cc` by `(2 + 2*ds) * 16`
        // per byte plus a trailing `cc += 4` setup overhead = 36 / 68.
        // Gambatte's `cc` and rustyboi's `cycles` use the same unit (NOP
        // returns 4 in both, frame budget = 70224 SS / 140448 DS), so use
        // these values verbatim.
        if self.is_double_speed_mode() { 68 } else { 36 }
    }

    /// Advance the OAM-DMA engine by one M-cycle (mirrors one iteration of
    /// Gambatte's `updateOamDma` loop). Advances `dma_pos`, (re)starts the
    /// transfer when it reaches `dma_start_pos`, copies the corresponding
    /// source byte into OAM, and ends the transfer at byte 160.
    fn dma_advance_one_mcycle(&mut self) {
        self.dma_pos = self.dma_pos.wrapping_add(1);

        if self.dma_pos == self.dma_start_pos {
            // startOamDma: transfer (re)starts from the top.
            self.dma_pos = 0;
            self.dma_start_pos = 0;
        }

        if self.dma_pos < 160 {
            let source_addr = self.dma_source_base + self.dma_pos as u16;
            let byte = self.read_during_dma(source_addr);
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
        self.step_hdma();

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
    fn step_hdma(&mut self) {
        if !self.cgb_features_enabled {
            return;
        }

        let lcdc = self.io_registers.read(ppu::LCD_CONTROL);
        let lcd_on = lcdc & (ppu::LCDCFlags::DisplayEnable as u8) != 0;
        let mode = if lcd_on {
            self.io_registers.read(ppu::LCD_STATUS) & 0x03
        } else {
            // LCD off: treat as a permanent HBlank-like period (Gambatte fires
            // HDMA immediately when armed with the LCD disabled).
            0
        };

        // Mode 3 -> Mode 0 edge: entering HBlank. Arm one block if HDMA is on.
        if lcd_on && self.hdma_prev_stat_mode == 3 && mode == 0 && self.hdma_enabled {
            self.hdma_req_pending = true;
        }
        self.hdma_prev_stat_mode = mode;

        if self.hdma_req_pending && self.hdma_enabled {
            self.pending_dma_stall += self.run_hdma_block();
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

    /// Write into the WRAM/echo region honoring CGB bank selection. Used by the
    /// OAM-DMA conflict path (CGB, non-WRAM source) where the write still has to
    /// land in WRAM while the normal during-DMA routing would drop it.
    fn write_wram_region(&mut self, addr: u16, value: u8) {
        let addr = if addr >= ECHO_RAM_START { addr - 0x2000 } else { addr };
        match addr {
            WRAM_START..=WRAM_END => self.wram.write(addr, value),
            WRAM_BANK_START..=WRAM_BANK_END => {
                if self.cgb_features_enabled {
                    match self.wram_bank_select {
                        0 | 1 => self.wram_bank.write(addr, value),
                        2..=7 => {
                            let bank_index = (self.wram_bank_select - 2) as usize;
                            self.wram_banks[bank_index].write(addr, value)
                        }
                        _ => self.wram_bank.write(addr, value),
                    }
                } else {
                    self.wram_bank.write(addr, value)
                }
            }
            _ => (),
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
                // WRAM region with a non-WRAM source: the write still reaches the
                // CPU-selected WRAM bank (it does not disturb OAM).
                self.write_wram_region(addr, value);
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
            return self.read_during_dma(addr);
        }
        self.oam.read(OAM_START + self.dma_pos as u16)
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
            // Toggle the speed mode
            self.key1_current_speed = !self.key1_current_speed;
            // Clear the armed bit
            self.key1_switch_armed = false;
            // Gambatte's `Memory::stop` resets DIV and re-bases peripheral
            // timing on speed switch. We don't keep separately scaled internal
            // counters, so resetting DIV is the only resync we need; the
            // per-T-cycle stepping in gb.rs already produces the correct
            // half-rate PPU/audio cadence in double-speed.
            self.timer.write(timer::DIV, 0);
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
            VRAM_START..=VRAM_END => self.vram.read(addr),
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

    /// Consume the pending STAT-register-write signal. Returns true if the CPU
    /// wrote to FF40, FF41, or FF45 since the last call.
    pub fn take_stat_register_write_pending(&mut self) -> bool {
        let pending = self.stat_register_write_pending;
        self.stat_register_write_pending = false;
        pending
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
                UNUSED_START..=UNUSED_END => EMPTY_BYTE,
                IO_REGISTERS_START..=IO_REGISTERS_END => {
                    match addr {
                        input::JOYP => self.input.read(addr),
                        timer::DIV..=timer::TAC => self.timer.read(addr),
                serial::SB..=serial::SC => self.serial.read(addr),
                cpu::registers::INTERRUPT_FLAG => self.io_registers.read(addr) | 0xE0,
                        audio::NR10..=audio::NR14 => self.audio.read(addr),
                        audio::NR21..=audio::NR24 => self.audio.read(addr),
                        audio::NR30..=audio::NR34 => self.audio.read(addr),
                        audio::NR41..=audio::NR52 => self.audio.read(addr),
                        audio::WAV_START..=audio::WAV_END => self.audio.read(addr),
                        REG_DMA => self.io_registers.read(addr),
                        
                        // CGB registers - only accessible when CGB features are enabled
                        REG_KEY0 => {
                            if self.cgb_features_enabled {
                                // KEY0: DMG compatibility mode bit
                                (if self.key0_dmg_mode { 0x01 } else { 0x00 }) | 0xFE // Bit 0 = DMG mode, bits 1-7 = 1
                            } else {
                                0xFF // DMG hardware returns 0xFF for CGB registers
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
                                self.wram_bank_select | 0xF8 // Bits 0-2 = bank, bits 3-7 = 1
                            } else {
                                0xFF
                            }
                        },
                        REG_BCPS => {
                            if self.cgb_features_enabled {
                                self.bg_palette_spec
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
                                self.obj_palette_spec
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
                UNUSED_START..=UNUSED_END => (), // Writes to unused memory are ignored
                IO_REGISTERS_START..=IO_REGISTERS_END => {
                    match addr {
                        input::JOYP => self.input.write(addr, value),
                        timer::DIV..=timer::TAC => self.timer.write(addr, value),
                serial::SB..=serial::SC => self.serial.write(addr, value),
                        audio::NR10..=audio::NR14 => self.audio.write(addr, value),
                        audio::NR21..=audio::NR24 => self.audio.write(addr, value),
                        audio::NR30..=audio::NR34 => self.audio.write(addr, value),
                        audio::NR41..=audio::NR52 => self.audio.write(addr, value),
                        audio::WAV_START..=audio::WAV_END => self.audio.write(addr, value),
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
                            // When boot ROM is disabled, lock the KEY0 register
                            if self.cgb_features_enabled && value != 0 {
                                self.key0_locked = true;
                            }
                            self.io_registers.write(addr, value);
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
                        
                        _ => self.io_registers.write(addr, value),
                    }
                }
                HRAM_START..=HRAM_END => self.hram.write(addr, value),
                IE_REGISTER => self.ie_register = value,
            }
        }
    }
}
