use crate::audio;
use crate::cartridge;
use crate::cpu;
use crate::input;
use crate::memory;
use crate::memory::Addressable;
use crate::ppu::ppu;
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
                VRAM_START..=VRAM_END => self.vram.write(addr, value),
                EXTERNAL_RAM_START..=EXTERNAL_RAM_END => {
                    if let Some(cart) = self.cartridge.as_mut() { cart.write(addr, value) }
                },
                WRAM_START..=WRAM_END => self.wram.write(addr, value),
                WRAM_BANK_START..=WRAM_BANK_END => self.wram_bank.write(addr, value),
                ECHO_RAM_START..=ECHO_RAM_END => {
                    let addr = addr - 0x2000;
                    match addr {
                        0..WRAM_START => panic!("This is literally never possible"),
                        WRAM_START..=WRAM_END => self.wram.write(addr, value),
                        WRAM_BANK_START..=ECHO_RAM_MIRROR_END => self.wram_bank.write(addr, value),
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
                        _ => self.io_registers.write(addr, value),
                    }
                }
                HRAM_START..=HRAM_END => self.hram.write(addr, value),
                IE_REGISTER => self.ie_register = value,
            }
        }
    }
}
