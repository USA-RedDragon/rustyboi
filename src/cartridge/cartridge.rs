use crate::memory;
use crate::memory::mmio;
use serde::{Deserialize, Serialize};

use std::fs;
use std::io;

// Cartridge header offsets
const CARTRIDGE_TYPE_OFFSET: usize = 0x0147;
const ROM_SIZE_OFFSET: usize = 0x0148;
const RAM_SIZE_OFFSET: usize = 0x0149;

// Cartridge types for MBC1
const MBC1: u8 = 0x01;
const MBC1_RAM: u8 = 0x02;
const MBC1_RAM_BATTERY: u8 = 0x03;

// MBC1 register ranges
const RAM_ENABLE_START: u16 = 0x0000;
const RAM_ENABLE_END: u16 = 0x1FFF;
const ROM_BANK_SELECT_START: u16 = 0x2000;
const ROM_BANK_SELECT_END: u16 = 0x3FFF;
const RAM_BANK_ROM_BANK_HIGH_START: u16 = 0x4000;
const RAM_BANK_ROM_BANK_HIGH_END: u16 = 0x5FFF;
const BANKING_MODE_START: u16 = 0x6000;
const BANKING_MODE_END: u16 = 0x7FFF;

// External RAM area
const EXTERNAL_RAM_START: u16 = 0xA000;
const EXTERNAL_RAM_END: u16 = 0xBFFF;

#[derive(Clone, Debug)]
pub enum CartridgeType {
    NoMBC,
    MBC1 { ram: bool, battery: bool },
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Cartridge {
    // ROM data - all banks
    rom_data: Vec<u8>,
    // External RAM data - all banks
    ram_data: Vec<u8>,
    // Cartridge type
    cartridge_type: u8,
    // Number of ROM and RAM banks
    rom_banks: usize,
    ram_banks: usize,
    
    // MBC1 state
    ram_enabled: bool,
    rom_bank_low: u8,    // 5 bits (0x01-0x1F)
    ram_bank_or_rom_bank_high: u8, // 2 bits (0x00-0x03)
    banking_mode: u8,    // 0 = ROM banking mode, 1 = RAM banking mode
}

impl Cartridge {
    pub fn load(path: &str) -> Result<Self, io::Error> {
        let data = fs::read(path)?;
        
        if data.len() < 0x0150 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "ROM too small"));
        }
        
        // Read cartridge header information
        let cartridge_type = data[CARTRIDGE_TYPE_OFFSET];
        let rom_size_code = data[ROM_SIZE_OFFSET];
        let ram_size_code = data[RAM_SIZE_OFFSET];
        
        // Calculate number of ROM banks
        let rom_banks = match rom_size_code {
            0x00 => 2,   // 32KB = 2 banks of 16KB
            0x01 => 4,   // 64KB = 4 banks of 16KB
            0x02 => 8,   // 128KB = 8 banks of 16KB
            0x03 => 16,  // 256KB = 16 banks of 16KB
            0x04 => 32,  // 512KB = 32 banks of 16KB
            0x05 => 64,  // 1MB = 64 banks of 16KB
            0x06 => 128, // 2MB = 128 banks of 16KB
            0x07 => 256, // 4MB = 256 banks of 16KB (though MBC1 only supports up to 125)
            _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid ROM size")),
        };
        
        // Calculate number of RAM banks
        let ram_banks = match ram_size_code {
            0x00 => 0, // No RAM
            0x01 => 1, // 2KB (partial bank)
            0x02 => 1, // 8KB = 1 bank
            0x03 => 4, // 32KB = 4 banks of 8KB
            0x04 => 16, // 128KB = 16 banks of 8KB
            0x05 => 8,  // 64KB = 8 banks of 8KB
            _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid RAM size")),
        };
        
        // Copy ROM data
        let expected_rom_size = rom_banks * 0x4000; // 16KB per bank
        let rom_data = if data.len() >= expected_rom_size {
            data[..expected_rom_size].to_vec()
        } else {
            // Pad with 0xFF if ROM is smaller than expected
            let mut padded_rom = data.clone();
            padded_rom.resize(expected_rom_size, 0xFF);
            padded_rom
        };
        
        // Initialize RAM data
        let ram_data = vec![0xFF; ram_banks * 0x2000]; // 8KB per bank
        
        Ok(Cartridge {
            rom_data,
            ram_data,
            cartridge_type,
            rom_banks,
            ram_banks,
            ram_enabled: false,
            rom_bank_low: 1, // Bank 0 cannot be selected for 0x4000-0x7FFF area
            ram_bank_or_rom_bank_high: 0,
            banking_mode: 0,
        })
    }
    
    fn get_cartridge_type(&self) -> CartridgeType {
        match self.cartridge_type {
            MBC1 => CartridgeType::MBC1 { ram: false, battery: false },
            MBC1_RAM => CartridgeType::MBC1 { ram: true, battery: false },
            MBC1_RAM_BATTERY => CartridgeType::MBC1 { ram: true, battery: true },
            _ => CartridgeType::NoMBC,
        }
    }
    
    fn get_rom_bank(&self) -> usize {
        match self.get_cartridge_type() {
            CartridgeType::MBC1 { .. } => {
                let mut bank = self.rom_bank_low as usize;
                
                // In ROM banking mode, add upper 2 bits to ROM bank
                if self.banking_mode == 0 {
                    bank |= (self.ram_bank_or_rom_bank_high as usize) << 5;
                }
                
                // Bank 0 maps to bank 1 for the switchable area
                if bank == 0 {
                    bank = 1;
                }
                
                // Limit to available banks
                bank % self.rom_banks
            }
            CartridgeType::NoMBC => 1, // Simple cartridge always uses bank 1 for upper area
        }
    }
    
    fn get_ram_bank(&self) -> usize {
        match self.get_cartridge_type() {
            CartridgeType::MBC1 { .. } => {
                if self.banking_mode == 1 {
                    // RAM banking mode
                    (self.ram_bank_or_rom_bank_high as usize) % self.ram_banks.max(1)
                } else {
                    // ROM banking mode - always bank 0
                    0
                }
            }
            CartridgeType::NoMBC => 0,
        }
    }
}

impl memory::Addressable for Cartridge {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            // ROM Bank 0 (fixed)
            mmio::CARTRIDGE_START..=mmio::CARTRIDGE_END => {
                let offset = (addr - mmio::CARTRIDGE_START) as usize;
                if offset < self.rom_data.len() {
                    self.rom_data[offset]
                } else {
                    0xFF
                }
            }
            // ROM Bank 1-N (switchable)
            mmio::CARTRIDGE_BANK_START..=mmio::CARTRIDGE_BANK_END => {
                let rom_bank = self.get_rom_bank();
                let offset = (addr - mmio::CARTRIDGE_BANK_START) as usize + (rom_bank * 0x4000);
                if offset < self.rom_data.len() {
                    self.rom_data[offset]
                } else {
                    0xFF
                }
            }
            // External RAM
            EXTERNAL_RAM_START..=EXTERNAL_RAM_END => {
                match self.get_cartridge_type() {
                    CartridgeType::MBC1 { ram: true, .. } => {
                        if self.ram_enabled && !self.ram_data.is_empty() {
                            let ram_bank = self.get_ram_bank();
                            let offset = (addr - EXTERNAL_RAM_START) as usize + (ram_bank * 0x2000);
                            if offset < self.ram_data.len() {
                                self.ram_data[offset]
                            } else {
                                0xFF
                            }
                        } else {
                            0xFF
                        }
                    }
                    _ => 0xFF,
                }
            }
            _ => 0xFF,
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            // RAM Enable (0x0000-0x1FFF)
            RAM_ENABLE_START..=RAM_ENABLE_END => {
                match self.get_cartridge_type() {
                    CartridgeType::MBC1 { .. } => {
                        self.ram_enabled = (value & 0x0F) == 0x0A;
                    }
                    _ => {}
                }
            }
            // ROM Bank Number (0x2000-0x3FFF)
            ROM_BANK_SELECT_START..=ROM_BANK_SELECT_END => {
                match self.get_cartridge_type() {
                    CartridgeType::MBC1 { .. } => {
                        self.rom_bank_low = (value & 0x1F).max(1); // 5 bits, minimum value 1
                    }
                    _ => {}
                }
            }
            // RAM Bank Number / Upper ROM Bank Number (0x4000-0x5FFF)
            RAM_BANK_ROM_BANK_HIGH_START..=RAM_BANK_ROM_BANK_HIGH_END => {
                match self.get_cartridge_type() {
                    CartridgeType::MBC1 { .. } => {
                        self.ram_bank_or_rom_bank_high = value & 0x03; // 2 bits
                    }
                    _ => {}
                }
            }
            // Banking Mode Select (0x6000-0x7FFF)
            BANKING_MODE_START..=BANKING_MODE_END => {
                match self.get_cartridge_type() {
                    CartridgeType::MBC1 { .. } => {
                        self.banking_mode = value & 0x01; // 1 bit
                    }
                    _ => {}
                }
            }
            // External RAM
            EXTERNAL_RAM_START..=EXTERNAL_RAM_END => {
                match self.get_cartridge_type() {
                    CartridgeType::MBC1 { ram: true, .. } => {
                        if self.ram_enabled && !self.ram_data.is_empty() {
                            let ram_bank = self.get_ram_bank();
                            let offset = (addr - EXTERNAL_RAM_START) as usize + (ram_bank * 0x2000);
                            if offset < self.ram_data.len() {
                                self.ram_data[offset] = value;
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {
                // Ignore writes to other areas (ROM is read-only)
            }
        }
    }
}
