use crate::memory;
use crate::memory::mmio;
use serde::{Deserialize, Serialize};

use std::fs;
use std::fs::{File, OpenOptions};
use std::io;
use std::io::{Read, Seek, SeekFrom, Write};
use zip::ZipArchive;

// Cartridge header offsets
const CARTRIDGE_TYPE_OFFSET: usize = 0x0147;
const ROM_SIZE_OFFSET: usize = 0x0148;
const RAM_SIZE_OFFSET: usize = 0x0149;

// Cartridge types for MBC1
const MBC1: u8 = 0x01;
const MBC1_RAM: u8 = 0x02;
const MBC1_RAM_BATTERY: u8 = 0x03;

// Cartridge types for MBC2
const MBC2: u8 = 0x05;
const MBC2_BATTERY: u8 = 0x06;

// Cartridge types for MBC3
const MBC3_TIMER_BATTERY: u8 = 0x0F;
const MBC3_TIMER_RAM_BATTERY: u8 = 0x10;
const MBC3: u8 = 0x11;
const MBC3_RAM: u8 = 0x12;
const MBC3_RAM_BATTERY: u8 = 0x13;

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

// MBC2 specific ranges
const MBC2_RAM_SIZE: usize = 512; // 512 x 4 bits
const MBC2_RAM_START: u16 = 0xA000;
const MBC2_RAM_END: u16 = 0xA1FF;

#[derive(Clone, Debug)]
pub enum CartridgeType {
    NoMBC,
    MBC1 { ram: bool, battery: bool },
    MBC2 { battery: bool },
    MBC3 { ram: bool, battery: bool, timer: bool },
}

#[derive(Serialize, Deserialize)]
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
    // ROM file path (for determining .sav file location)
    #[serde(skip)]
    rom_path: Option<String>,
    // Open file handle for save file (for battery-backed cartridges)
    #[serde(skip)]
    save_file: Option<File>,
    
    // MBC1 state
    ram_enabled: bool,
    rom_bank_low: u8,    // 5 bits (0x01-0x1F)
    ram_bank_or_rom_bank_high: u8, // 2 bits (0x00-0x03)
    banking_mode: u8,    // 0 = ROM banking mode, 1 = RAM banking mode
    
    // MBC2 state (MBC2 has built-in 512x4 RAM)
    mbc2_ram: Vec<u8>, // MBC2 built-in RAM (512 x 4 bits, stored as full bytes)
    
    // MBC3 state
    mbc3_ram_bank: u8,   // 0x00-0x03 for RAM, 0x08-0x0C for RTC
    mbc3_rtc_latch: u8,  // RTC latch register
    mbc3_rtc_latched: bool, // Whether RTC registers are latched
    
    // MBC3 RTC registers
    rtc_seconds: u8,     // 0-59
    rtc_minutes: u8,     // 0-59  
    rtc_hours: u8,       // 0-23
    rtc_days_low: u8,    // Lower 8 bits of day counter
    rtc_days_high: u8,   // Upper 1 bit of day counter + halt flag + day carry
    
    // MBC3 RTC latched values
    rtc_seconds_latched: u8,
    rtc_minutes_latched: u8,
    rtc_hours_latched: u8,
    rtc_days_low_latched: u8,
    rtc_days_high_latched: u8,
}

impl Clone for Cartridge {
    fn clone(&self) -> Self {
        Cartridge {
            rom_data: self.rom_data.clone(),
            ram_data: self.ram_data.clone(),
            cartridge_type: self.cartridge_type,
            rom_banks: self.rom_banks,
            ram_banks: self.ram_banks,
            rom_path: self.rom_path.clone(),
            save_file: None, // Don't clone file handles
            ram_enabled: self.ram_enabled,
            rom_bank_low: self.rom_bank_low,
            ram_bank_or_rom_bank_high: self.ram_bank_or_rom_bank_high,
            banking_mode: self.banking_mode,
            mbc2_ram: self.mbc2_ram.clone(),
            mbc3_ram_bank: self.mbc3_ram_bank,
            mbc3_rtc_latch: self.mbc3_rtc_latch,
            mbc3_rtc_latched: self.mbc3_rtc_latched,
            rtc_seconds: self.rtc_seconds,
            rtc_minutes: self.rtc_minutes,
            rtc_hours: self.rtc_hours,
            rtc_days_low: self.rtc_days_low,
            rtc_days_high: self.rtc_days_high,
            rtc_seconds_latched: self.rtc_seconds_latched,
            rtc_minutes_latched: self.rtc_minutes_latched,
            rtc_hours_latched: self.rtc_hours_latched,
            rtc_days_low_latched: self.rtc_days_low_latched,
            rtc_days_high_latched: self.rtc_days_high_latched,
        }
    }
}

impl Cartridge {
    /// Extract ROM data from a zip file, looking for common ROM file extensions
    #[cfg(not(target_arch = "wasm32"))]
    fn extract_rom_from_zip(path: &str) -> Result<Vec<u8>, io::Error> {
        let file = File::open(path)?;
        let mut archive = ZipArchive::new(file)?;
        
        // Common Game Boy ROM extensions
        let rom_extensions = [".gb", ".gbc", ".sgb"];
        
        // First, try to find a file with a ROM extension
        for i in 0..archive.len() {
            let mut file = archive.by_index(i)?;
            let name = file.name().to_lowercase();
            
            if rom_extensions.iter().any(|ext| name.ends_with(ext)) {
                let mut data = Vec::with_capacity(file.size() as usize);
                file.read_to_end(&mut data)?;
                println!("Found ROM in zip: {}", file.name());
                return Ok(data);
            }
        }
        
        // If no ROM extension found, look for the largest file (common case)
        let mut largest_file_index = 0;
        let mut largest_size = 0;
        
        for i in 0..archive.len() {
            let file = archive.by_index(i)?;
            if !file.is_dir() && file.size() > largest_size {
                largest_size = file.size();
                largest_file_index = i;
            }
        }
        
        if largest_size > 0 {
            let mut file = archive.by_index(largest_file_index)?;
            let mut data = Vec::with_capacity(file.size() as usize);
            file.read_to_end(&mut data)?;
            println!("Using largest file in zip as ROM: {} ({} bytes)", file.name(), data.len());
            return Ok(data);
        }
        
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "No suitable ROM file found in zip archive"
        ))
    }

    pub fn load(path: &str) -> Result<Self, io::Error> {
        let data = if path.to_lowercase().ends_with(".zip") {
            #[cfg(not(target_arch = "wasm32"))]
            {
                Self::extract_rom_from_zip(path)?
            }
            #[cfg(target_arch = "wasm32")]
            {
                // For WASM, read the zip file and extract from bytes
                let zip_data = fs::read(path)?;
                Self::extract_rom_from_zip_bytes(&zip_data)?
            }
        } else {
            fs::read(path)?
        };
        
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
        
        let mut cartridge = Cartridge {
            rom_data,
            ram_data,
            cartridge_type,
            rom_banks,
            ram_banks,
            rom_path: Some(path.to_string()),
            save_file: None,
            ram_enabled: false,
            rom_bank_low: 1, // Bank 0 cannot be selected for 0x4000-0x7FFF area
            ram_bank_or_rom_bank_high: 0,
            banking_mode: 0,
            mbc2_ram: vec![0xFF; MBC2_RAM_SIZE],
            mbc3_ram_bank: 0,
            mbc3_rtc_latch: 0,
            mbc3_rtc_latched: false,
            rtc_seconds: 0,
            rtc_minutes: 0,
            rtc_hours: 0,
            rtc_days_low: 0,
            rtc_days_high: 0,
            rtc_seconds_latched: 0,
            rtc_minutes_latched: 0,
            rtc_hours_latched: 0,
            rtc_days_low_latched: 0,
            rtc_days_high_latched: 0,
        };
        
        // Try to load existing save file or create new one (only for battery-backed RAM)
        cartridge.load_or_create_save_file()?;
        
        Ok(cartridge)
    }

    #[cfg(target_arch = "wasm32")]
    /// Extract ROM data from zip bytes for WASM
    fn extract_rom_from_zip_bytes(data: &[u8]) -> Result<Vec<u8>, io::Error> {
        use std::io::Cursor;
        
        let cursor = Cursor::new(data);
        let mut archive = ZipArchive::new(cursor)?;
        
        // Common Game Boy ROM extensions
        let rom_extensions = [".gb", ".gbc", ".sgb"];
        
        // First, try to find a file with a ROM extension
        for i in 0..archive.len() {
            let mut file = archive.by_index(i)?;
            let name = file.name().to_lowercase();
            
            if rom_extensions.iter().any(|ext| name.ends_with(ext)) {
                let mut rom_data = Vec::with_capacity(file.size() as usize);
                file.read_to_end(&mut rom_data)?;
                return Ok(rom_data);
            }
        }
        
        // If no ROM extension found, look for the largest file
        let mut largest_file_index = 0;
        let mut largest_size = 0;
        
        for i in 0..archive.len() {
            let file = archive.by_index(i)?;
            if !file.is_dir() && file.size() > largest_size {
                largest_size = file.size();
                largest_file_index = i;
            }
        }
        
        if largest_size > 0 {
            let mut file = archive.by_index(largest_file_index)?;
            let mut rom_data = Vec::with_capacity(file.size() as usize);
            file.read_to_end(&mut rom_data)?;
            return Ok(rom_data);
        }
        
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "No suitable ROM file found in zip archive"
        ))
    }

    #[cfg(target_arch = "wasm32")]
    pub fn from_bytes(data: &[u8]) -> Result<Self, io::Error> {
        // Try to detect if this is a zip file by checking the magic bytes
        let actual_data = if data.len() >= 4 && &data[0..4] == b"PK\x03\x04" {
            // This looks like a ZIP file
            Self::extract_rom_from_zip_bytes(data)?
        } else {
            data.to_vec()
        };
        if actual_data.len() < 0x0150 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "ROM too small"));
        }
        
        // Read cartridge header information
        let cartridge_type = actual_data[CARTRIDGE_TYPE_OFFSET];
        let rom_size_code = actual_data[ROM_SIZE_OFFSET];
        let ram_size_code = actual_data[RAM_SIZE_OFFSET];
        
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
        let rom_data = if actual_data.len() >= expected_rom_size {
            actual_data[..expected_rom_size].to_vec()
        } else {
            // Pad with 0xFF if ROM is smaller than expected
            let mut padded_rom = actual_data.clone();
            padded_rom.resize(expected_rom_size, 0xFF);
            padded_rom
        };
        
        // Initialize RAM data
        let ram_data = vec![0xFF; ram_banks * 0x2000]; // 8KB per bank
        
        let cartridge = Cartridge {
            rom_data,
            ram_data,
            cartridge_type,
            rom_banks,
            ram_banks,
            rom_path: None, // No path for in-memory data
            save_file: None,
            ram_enabled: false,
            rom_bank_low: 1, // Bank 0 cannot be selected for 0x4000-0x7FFF area
            ram_bank_or_rom_bank_high: 0,
            banking_mode: 0,
            mbc2_ram: vec![0xFF; MBC2_RAM_SIZE],
            mbc3_ram_bank: 0,
            mbc3_rtc_latch: 0,
            mbc3_rtc_latched: false,
            rtc_seconds: 0,
            rtc_minutes: 0,
            rtc_hours: 0,
            rtc_days_low: 0,
            rtc_days_high: 0,
            rtc_seconds_latched: 0,
            rtc_minutes_latched: 0,
            rtc_hours_latched: 0,
            rtc_days_low_latched: 0,
            rtc_days_high_latched: 0,
        };
        
        // Note: For WASM/in-memory loading, we skip save file loading
        // since there's no persistent filesystem
        
        Ok(cartridge)
    }
    
    fn get_cartridge_type(&self) -> CartridgeType {
        match self.cartridge_type {
            MBC1 => CartridgeType::MBC1 { ram: false, battery: false },
            MBC1_RAM => CartridgeType::MBC1 { ram: true, battery: false },
            MBC1_RAM_BATTERY => CartridgeType::MBC1 { ram: true, battery: true },
            MBC2 => CartridgeType::MBC2 { battery: false },
            MBC2_BATTERY => CartridgeType::MBC2 { battery: true },
            MBC3_TIMER_BATTERY => CartridgeType::MBC3 { ram: false, battery: true, timer: true },
            MBC3_TIMER_RAM_BATTERY => CartridgeType::MBC3 { ram: true, battery: true, timer: true },
            MBC3 => CartridgeType::MBC3 { ram: false, battery: false, timer: false },
            MBC3_RAM => CartridgeType::MBC3 { ram: true, battery: false, timer: false },
            MBC3_RAM_BATTERY => CartridgeType::MBC3 { ram: true, battery: true, timer: false },
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
            CartridgeType::MBC2 { .. } => {
                // MBC2 uses only the lower 4 bits, bank 0 maps to bank 1
                let bank = (self.rom_bank_low & 0x0F) as usize;
                if bank == 0 { 1 } else { bank % self.rom_banks }
            }
            CartridgeType::MBC3 { .. } => {
                // MBC3 uses 7 bits for ROM bank selection, bank 0 maps to bank 1
                let bank = (self.rom_bank_low & 0x7F) as usize;
                if bank == 0 { 1 } else { bank % self.rom_banks }
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
            CartridgeType::MBC2 { .. } => 0, // MBC2 has built-in RAM, no banking
            CartridgeType::MBC3 { .. } => {
                // MBC3 uses mbc3_ram_bank for both RAM and RTC
                (self.mbc3_ram_bank & 0x03) as usize % self.ram_banks.max(1)
            }
            CartridgeType::NoMBC => 0,
        }
    }
    
    /// Get the save file path for this cartridge
    fn get_save_file_path(&self) -> Option<String> {
        self.rom_path.as_ref().map(|path| {
            // Replace the extension with .sav
            let mut save_path = path.clone();
            if let Some(dot_pos) = save_path.rfind('.') {
                save_path.truncate(dot_pos);
            }
            save_path.push_str(".sav");
            save_path
        })
    }
    
    /// Load save file data into RAM if it exists, or create empty save file (only for battery-backed RAM)
    fn load_or_create_save_file(&mut self) -> Result<(), io::Error> {
        // Only process save files for cartridges with battery-backed RAM
        if !self.has_battery() {
            return Ok(());
        }
        
        // For MBC2, we need to save the built-in RAM instead of external RAM
        let save_data = match self.get_cartridge_type() {
            CartridgeType::MBC2 { .. } => &self.mbc2_ram,
            _ => &self.ram_data,
        };
        
        if save_data.is_empty() {
            return Ok(());
        }
        
        if let Some(save_path) = self.get_save_file_path() {
            if std::path::Path::new(&save_path).exists() {
                // Load existing save file
                let loaded_data = fs::read(&save_path)?;
                match self.get_cartridge_type() {
                    CartridgeType::MBC2 { .. } => {
                        if loaded_data.len() <= self.mbc2_ram.len() {
                            self.mbc2_ram[..loaded_data.len()].copy_from_slice(&loaded_data);
                            println!("Loaded MBC2 save file: {}", save_path);
                        }
                    }
                    _ => {
                        if loaded_data.len() <= self.ram_data.len() {
                            self.ram_data[..loaded_data.len()].copy_from_slice(&loaded_data);
                            println!("Loaded save file: {}", save_path);
                        }
                    }
                }
            } else {
                // Create new save file with current RAM data
                match self.get_cartridge_type() {
                    CartridgeType::MBC2 { .. } => {
                        fs::write(&save_path, &self.mbc2_ram)?;
                        println!("Created new MBC2 save file: {}", save_path);
                    }
                    _ => {
                        fs::write(&save_path, &self.ram_data)?;
                        println!("Created new save file: {}", save_path);
                    }
                }
            }
            
            // Open file handle for efficient writing
            self.save_file = Some(OpenOptions::new()
                .write(true)
                .open(&save_path)?);
        }
        Ok(())
    }
    
    /// Write a byte to both RAM and save file simultaneously (if battery-backed)
    fn write_ram_byte(&mut self, offset: usize, value: u8) -> Result<(), io::Error> {
        if !self.ram_data.is_empty() {
            // Write to RAM buffer (offset is already wrapped by caller)
            self.ram_data[offset] = value;
            
            // Also write to save file if we have one open
            if let Some(ref mut file) = self.save_file {
                file.seek(SeekFrom::Start(offset as u64))?;
                file.write_all(&[value])?;
                file.flush()?; // Ensure immediate write
            }
        }
        Ok(())
    }
    
    /// Write a byte to MBC2 RAM and save file simultaneously (if battery-backed)
    fn write_mbc2_ram_byte(&mut self, offset: usize, value: u8) -> Result<(), io::Error> {
        if !self.mbc2_ram.is_empty() {
            // Write to MBC2 RAM buffer (offset is already wrapped by caller)
            self.mbc2_ram[offset] = value & 0x0F; // Only 4 bits valid
            
            // Also write to save file if we have one open
            if let Some(ref mut file) = self.save_file {
                file.seek(SeekFrom::Start(offset as u64))?;
                file.write_all(&[self.mbc2_ram[offset]])?;
                file.flush()?; // Ensure immediate write
            }
        }
        Ok(())
    }

    /// Check if this cartridge has battery-backed RAM
    pub fn has_battery(&self) -> bool {
        match self.get_cartridge_type() {
            CartridgeType::MBC1 { battery, .. } => battery,
            CartridgeType::MBC2 { battery } => battery,
            CartridgeType::MBC3 { battery, .. } => battery,
            CartridgeType::NoMBC => false,
        }
    }
    
    /// Read from MBC3 RTC registers
    fn read_rtc_register(&self) -> u8 {
        match self.mbc3_ram_bank {
            0x08 => if self.mbc3_rtc_latched { self.rtc_seconds_latched } else { self.rtc_seconds },
            0x09 => if self.mbc3_rtc_latched { self.rtc_minutes_latched } else { self.rtc_minutes },
            0x0A => if self.mbc3_rtc_latched { self.rtc_hours_latched } else { self.rtc_hours },
            0x0B => if self.mbc3_rtc_latched { self.rtc_days_low_latched } else { self.rtc_days_low },
            0x0C => if self.mbc3_rtc_latched { self.rtc_days_high_latched } else { self.rtc_days_high },
            _ => 0xFF,
        }
    }
    
    /// Write to MBC3 RTC registers
    fn write_rtc_register(&mut self, value: u8) {
        match self.mbc3_ram_bank {
            0x08 => self.rtc_seconds = value & 0x3F, // 0-59
            0x09 => self.rtc_minutes = value & 0x3F, // 0-59
            0x0A => self.rtc_hours = value & 0x1F,   // 0-23
            0x0B => self.rtc_days_low = value,       // Lower 8 bits of day counter
            0x0C => self.rtc_days_high = value & 0xC1, // Upper bit + halt flag + day carry
            _ => {}
        }
    }
    
    /// Latch current RTC values for consistent reading
    fn latch_rtc(&mut self) {
        self.rtc_seconds_latched = self.rtc_seconds;
        self.rtc_minutes_latched = self.rtc_minutes;
        self.rtc_hours_latched = self.rtc_hours;
        self.rtc_days_low_latched = self.rtc_days_low;
        self.rtc_days_high_latched = self.rtc_days_high;
        self.mbc3_rtc_latched = true;
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
                            let offset = ((addr - EXTERNAL_RAM_START) as usize + (ram_bank * 0x2000)) % self.ram_data.len();
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    CartridgeType::MBC2 { .. } => {
                        // MBC2 has built-in 512x4 RAM at 0xA000-0xA1FF
                        if self.ram_enabled && addr <= MBC2_RAM_END {
                            let offset = (addr - MBC2_RAM_START) as usize % self.mbc2_ram.len();
                            self.mbc2_ram[offset] & 0x0F // Only lower 4 bits are valid
                        } else {
                            0xFF
                        }
                    }
                    CartridgeType::MBC3 { ram: true, .. } => {
                        if self.ram_enabled {
                            match self.mbc3_ram_bank {
                                0x00..=0x03 => {
                                    // RAM bank access
                                    if !self.ram_data.is_empty() {
                                        let ram_bank = self.get_ram_bank();
                                        let offset = ((addr - EXTERNAL_RAM_START) as usize + (ram_bank * 0x2000)) % self.ram_data.len();
                                        self.ram_data[offset]
                                    } else {
                                        0xFF
                                    }
                                }
                                0x08..=0x0C => {
                                    // RTC register access
                                    self.read_rtc_register()
                                }
                                _ => 0xFF,
                            }
                        } else {
                            0xFF
                        }
                    }
                    CartridgeType::MBC3 { ram: false, timer: true, .. } => {
                        // Timer-only MBC3 (no RAM)
                        if self.ram_enabled && (0x08..=0x0C).contains(&self.mbc3_ram_bank) {
                            self.read_rtc_register()
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
                    CartridgeType::MBC2 { .. } => {
                        // MBC2 uses bit 8 of the address to differentiate from ROM bank select
                        if (addr & 0x0100) == 0 {
                            self.ram_enabled = (value & 0x0F) == 0x0A;
                        }
                    }
                    CartridgeType::MBC3 { .. } => {
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
                    CartridgeType::MBC2 { .. } => {
                        // MBC2 uses bit 8 of the address to differentiate from RAM enable
                        if (addr & 0x0100) != 0 {
                            self.rom_bank_low = (value & 0x0F).max(1); // 4 bits, minimum value 1
                        }
                    }
                    CartridgeType::MBC3 { .. } => {
                        self.rom_bank_low = (value & 0x7F).max(1); // 7 bits, minimum value 1
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
                    CartridgeType::MBC2 { .. } => {
                        // MBC2 doesn't use this register
                    }
                    CartridgeType::MBC3 { .. } => {
                        self.mbc3_ram_bank = value; // Can be 0x00-0x03 for RAM or 0x08-0x0C for RTC
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
                    CartridgeType::MBC2 { .. } => {
                        // MBC2 doesn't use this register
                    }
                    CartridgeType::MBC3 { timer: true, .. } => {
                        // MBC3 RTC latch register
                        if self.mbc3_rtc_latch == 0x00 && value == 0x01 {
                            self.latch_rtc();
                        }
                        self.mbc3_rtc_latch = value;
                    }
                    CartridgeType::MBC3 { .. } => {
                        // Non-timer MBC3 ignores this register
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
                            let offset = ((addr - EXTERNAL_RAM_START) as usize + (ram_bank * 0x2000)) % self.ram_data.len();
                            // Use our dual-write method that writes to both RAM and save file
                            let _ = self.write_ram_byte(offset, value); // Ignore errors for now
                        }
                    }
                    CartridgeType::MBC2 { .. } => {
                        // MBC2 has built-in 512x4 RAM at 0xA000-0xA1FF
                        if self.ram_enabled && addr <= MBC2_RAM_END {
                            let offset = (addr - MBC2_RAM_START) as usize % self.mbc2_ram.len();
                            let _ = self.write_mbc2_ram_byte(offset, value); // Ignore errors for now
                        }
                    }
                    CartridgeType::MBC3 { ram: true, .. } => {
                        if self.ram_enabled {
                            match self.mbc3_ram_bank {
                                0x00..=0x03 => {
                                    // RAM bank access
                                    if !self.ram_data.is_empty() {
                                        let ram_bank = self.get_ram_bank();
                                        let offset = ((addr - EXTERNAL_RAM_START) as usize + (ram_bank * 0x2000)) % self.ram_data.len();
                                        let _ = self.write_ram_byte(offset, value);
                                    }
                                }
                                0x08..=0x0C => {
                                    // RTC register access
                                    self.write_rtc_register(value);
                                }
                                _ => {}
                            }
                        }
                    }
                    CartridgeType::MBC3 { ram: false, timer: true, .. } => {
                        // Timer-only MBC3 (no RAM)
                        if self.ram_enabled && (0x08..=0x0C).contains(&self.mbc3_ram_bank) {
                            self.write_rtc_register(value);
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
