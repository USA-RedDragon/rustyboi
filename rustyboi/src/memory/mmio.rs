use crate::audio;
use crate::cartridge;
use crate::cpu;
use crate::input;
use crate::memory;
use crate::memory::Addressable;
use crate::ppu;
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
    io_registers: memory::Memory<IO_REGISTERS_START, IO_REGISTERS_SIZE>,
    hram: memory::Memory<HRAM_START, HRAM_SIZE>,
    ie_register: u8,
    audio: audio::Audio,
    // OAM DMA state
    dma_active: bool,
    dma_source_base: u16,
    dma_progress: u8, // 0-159, tracks which byte we're transferring
    
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
    hdma_source: u16,       // HDMA source address
    hdma_dest: u16,         // HDMA destination address
    hdma_length: u8,        // HDMA transfer length
    hdma_mode: u8,          // HDMA mode (0=general purpose, 1=H-blank)
    hdma_active: bool,      // HDMA transfer active
    
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
            io_registers: memory::Memory::new(),
            hram: memory::Memory::new(),
            ie_register: 0,
            audio: audio::Audio::new(),
            dma_active: false,
            dma_source_base: 0,
            dma_progress: 0,
            
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
            hdma_mode: 0,
            hdma_active: false,
            
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
        self.bios.clone_into(&mut new.bios);
        self.cartridge.clone_into(&mut new.cartridge);
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

    pub fn step_timer(&mut self, cpu: &mut cpu::SM83) {
        let mut timer = self.timer.clone();
        timer.step(cpu, self);
        self.timer = timer;
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

    pub fn step_dma(&mut self) {
        if !self.dma_active {
            return;
        }

        // Perform one byte transfer per cycle
        let source_addr = self.dma_source_base + self.dma_progress as u16;
        let dest_addr = OAM_START + self.dma_progress as u16;
        
        // Read from source address (bypassing DMA conflicts for now - the source read is always allowed)
        let byte = self.read_during_dma(source_addr);
        
        // Write directly to OAM memory
        self.oam.write(dest_addr, byte);
        
        self.dma_progress += 1;
        
        // DMA transfer is complete after 160 bytes (cycles)
        if self.dma_progress >= 160 {
            self.dma_active = false;
            self.dma_progress = 0;
        }
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
                    REG_DMA => self.io_registers.read(addr),
                    _ => self.io_registers.read(addr),
                }
            }
            HRAM_START..=HRAM_END => self.hram.read(addr),
            _ => EMPTY_BYTE,
        }
    }
}

impl memory::Addressable for Mmio {
    fn read(&self, addr: u16) -> u8 {
        // During DMA, CPU can only access HRAM and some IO registers
        if self.dma_active {
            match addr {
                HRAM_START..=HRAM_END => self.hram.read(addr),
                IE_REGISTER => self.ie_register,
                // Allow reading from some essential IO registers during DMA
                timer::DIV..=timer::TAC => self.timer.read(addr),
                input::JOYP => self.input.read(addr),
                REG_DMA => self.io_registers.read(addr),
                // Allow PPU registers during DMA since PPU continues to operate
                ppu::LCD_CONTROL..=ppu::WX => self.io_registers.read(addr),
                _ => 0xFF, // Return 0xFF for all other addresses during DMA
            }
        } else {
            // Normal memory access when DMA is not active
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
                OAM_START..=OAM_END => self.oam.read(addr),
                UNUSED_START..=UNUSED_END => EMPTY_BYTE,
                IO_REGISTERS_START..=IO_REGISTERS_END => {
                    match addr {
                        input::JOYP => self.input.read(addr),
                        timer::DIV..=timer::TAC => self.timer.read(addr),
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
                        REG_HDMA1 => {
                            if self.cgb_features_enabled {
                                (self.hdma_source >> 8) as u8
                            } else {
                                0xFF
                            }
                        },
                        REG_HDMA2 => {
                            if self.cgb_features_enabled {
                                self.hdma_source as u8
                            } else {
                                0xFF
                            }
                        },
                        REG_HDMA3 => {
                            if self.cgb_features_enabled {
                                (self.hdma_dest >> 8) as u8
                            } else {
                                0xFF
                            }
                        },
                        REG_HDMA4 => {
                            if self.cgb_features_enabled {
                                self.hdma_dest as u8
                            } else {
                                0xFF
                            }
                        },
                        REG_HDMA5 => {
                            if self.cgb_features_enabled {
                                if self.hdma_active {
                                    self.hdma_length // Transfer in progress, return remaining length
                                } else {
                                    0xFF // No transfer
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
                        
                        _ => self.io_registers.read(addr),
                    }
                }
                HRAM_START..=HRAM_END => self.hram.read(addr),
                IE_REGISTER => self.ie_register,
            }
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        // During DMA, CPU can only access HRAM and some IO registers
        if self.dma_active {
            match addr {
                HRAM_START..=HRAM_END => self.hram.write(addr, value),
                IE_REGISTER => self.ie_register = value,
                // Allow writing to some essential IO registers during DMA
                timer::DIV..=timer::TAC => self.timer.write(addr, value),
                input::JOYP => self.input.write(addr, value),
                REG_DMA => {
                    // Allow starting another DMA during current DMA (restarts)
                    self.dma_active = true;
                    self.dma_source_base = (value as u16) << 8;
                    self.dma_progress = 0;
                    self.io_registers.write(addr, value);
                },
                ppu::LCD_CONTROL..=ppu::WX => self.io_registers.write(addr, value),
                _ => (), // Ignore writes to other addresses during DMA
            }
        } else {
            // Normal memory access when DMA is not active
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
                OAM_START..=OAM_END => self.oam.write(addr, value),
                UNUSED_START..=UNUSED_END => (), // Writes to unused memory are ignored
                IO_REGISTERS_START..=IO_REGISTERS_END => {
                    match addr {
                        input::JOYP => self.input.write(addr, value),
                        timer::DIV..=timer::TAC => self.timer.write(addr, value),
                        audio::NR10..=audio::NR14 => self.audio.write(addr, value),
                        audio::NR21..=audio::NR24 => self.audio.write(addr, value),
                        audio::NR30..=audio::NR34 => self.audio.write(addr, value),
                        audio::NR41..=audio::NR52 => self.audio.write(addr, value),
                        audio::WAV_START..=audio::WAV_END => self.audio.write(addr, value),
                        REG_DMA => {
                            // Start OAM DMA transfer
                            // The high byte of the source address is written to DMA register
                            // The transfer copies 160 bytes from source to OAM
                            self.dma_active = true;
                            self.dma_source_base = (value as u16) << 8;
                            self.dma_progress = 0;
                            // Store the DMA register value for reads
                            self.io_registers.write(addr, value);
                        },
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
                                self.hdma_source = (self.hdma_source & 0xFF00) | (value as u16);
                            }
                        },
                        REG_HDMA3 => {
                            if self.cgb_features_enabled {
                                self.hdma_dest = (self.hdma_dest & 0x00FF) | ((value as u16) << 8);
                            }
                        },
                        REG_HDMA4 => {
                            if self.cgb_features_enabled {
                                self.hdma_dest = (self.hdma_dest & 0xFF00) | (value as u16);
                            }
                        },
                        REG_HDMA5 => {
                            if self.cgb_features_enabled {
                                // TODO: Implement HDMA transfer logic
                                self.hdma_length = value & 0x7F; // Bits 0-6 = length
                                self.hdma_mode = (value >> 7) & 0x01; // Bit 7 = mode
                                // For now, just store the values - full HDMA implementation would start transfer here
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
