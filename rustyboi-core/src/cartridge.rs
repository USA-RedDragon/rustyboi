use crate::memory;
use crate::memory::mmio;
use serde::{Deserialize, Serialize};

use std::fs;
use std::fs::{File, OpenOptions};
use std::io;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use zip::ZipArchive;

// Cartridge header offsets
const CARTRIDGE_TYPE_OFFSET: usize = 0x0147;
const ROM_SIZE_OFFSET: usize = 0x0148;
const RAM_SIZE_OFFSET: usize = 0x0149;
const CGB_FLAG_OFFSET: usize = 0x0143;

// CGB support flags
const CGB_COMPATIBLE: u8 = 0x80; // Works on both DMG and CGB
const CGB_ONLY: u8 = 0xC0;       // CGB only

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CgbSupport {
    None,        // DMG only
    Compatible,  // Works on both DMG and CGB (0x80)
    Only,        // CGB only (0xC0)
}

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

// Cartridge types for MBC5
const MBC5: u8 = 0x19;
const MBC5_RAM: u8 = 0x1A;
const MBC5_RAM_BATTERY: u8 = 0x1B;
const MBC5_RUMBLE: u8 = 0x1C;
const MBC5_RUMBLE_RAM: u8 = 0x1D;
const MBC5_RUMBLE_RAM_BATTERY: u8 = 0x1E;

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

#[derive(Clone, Debug)]
pub enum CartridgeType {
    NoMBC,
    MBC1 { ram: bool, battery: bool },
    MBC2 { battery: bool },
    MBC3 { ram: bool, battery: bool, timer: bool },
    MBC5 { ram: bool, battery: bool, _rumble: bool },
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
    // MBC1 multicart: the BANK2 register supplies ROM-bank bits 4-5 and only the
    // low 4 bits of BANK1 are wired, so the combined bank is 6 bits. Detected
    // from the Nintendo-logo-per-segment header layout (see is_mbc1_multicart).
    #[serde(default)]
    mbc1_multicart: bool,

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

    // Sub-second cycle accumulator for the cycle-derived RTC. One RTC second is
    // 4_194_304 T-cycles (the 4.194304 MHz master/dot clock). The RTC crystal is
    // independent of CPU speed, so this is driven off the master `abs_cc` dot
    // clock (constant across single/double speed), NOT host wall-clock — keeping
    // RTC advancement fully deterministic and test-reproducible.
    #[serde(default)]
    rtc_cycle_accum: u64,

    // MBC5 state
    mbc5_rom_bank_low: u8,   // Lower 8 bits of ROM bank (0x2000-0x2FFF)
    mbc5_rom_bank_high: u8,  // Upper 1 bit of ROM bank (0x3000-0x3FFF) - only bit 0 used
    mbc5_ram_bank: u8,       // RAM bank select (0x4000-0x5FFF) - 4 bits used (0x00-0x0F)

    // CGB support information
    cgb_support: CgbSupport, // CGB compatibility from cartridge header

    // MBC5 rumble motor latch. Set from bit 3 of the RAM-bank register write on
    // rumble carts; read by the libretro frontend to drive the rumble motor.
    // Not persisted (transient hardware line).
    #[serde(skip, default)]
    rumble_motor: bool,

    // Scratch buffer backing the libretro `RETRO_MEMORY_RTC` view. Filled on
    // demand from the discrete RTC registers; not part of the save state.
    #[serde(skip, default)]
    rtc_memory: Vec<u8>,

    // When true the cartridge will not open or write sidecar `.sav`/`.rtc`
    // files; the host (e.g. RetroArch) owns persistence of the in-memory RAM.
    #[serde(skip, default)]
    host_managed_saves: bool,
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
            mbc1_multicart: self.mbc1_multicart,
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
            rtc_cycle_accum: self.rtc_cycle_accum,
            mbc5_rom_bank_low: self.mbc5_rom_bank_low,
            mbc5_rom_bank_high: self.mbc5_rom_bank_high,
            mbc5_ram_bank: self.mbc5_ram_bank,
            cgb_support: self.cgb_support.clone(),
            rumble_motor: self.rumble_motor,
            rtc_memory: self.rtc_memory.clone(),
            host_managed_saves: self.host_managed_saves,
        }
    }
}

impl Cartridge {
    /// Detect CGB support from cartridge header byte 0x0143
    fn detect_cgb_support(data: &[u8]) -> CgbSupport {
        if data.len() <= CGB_FLAG_OFFSET {
            return CgbSupport::None;
        }

        match data[CGB_FLAG_OFFSET] {
            CGB_COMPATIBLE => CgbSupport::Compatible,
            CGB_ONLY => CgbSupport::Only,
            _ => CgbSupport::None,
        }
    }

    /// Detect an MBC1 multicart. These are 8Mbit (1MB) MBC1 carts whose ROM is
    /// divided into four 256KB games, each carrying its own Nintendo logo at
    /// 0x104. The accepted heuristic (used by mooneye / hardware reference
    /// emulators) is: cartridge type is MBC1, ROM is exactly 64 banks, and the
    /// Nintendo logo appears at the start of two or more of the four 256KB
    /// segments. On a multicart BANK2 supplies bank bits 4-5 (not 5-6) and only
    /// the low 4 bits of BANK1 are wired.
    fn detect_mbc1_multicart(cartridge_type: u8, data: &[u8]) -> bool {
        if !matches!(cartridge_type, MBC1 | MBC1_RAM | MBC1_RAM_BATTERY) {
            return false;
        }
        if data.len() != 64 * 0x4000 {
            return false; // multicarts are exactly 8Mbit / 1MB
        }
        let logo = &data[0x0104..0x0134];
        let mut copies = 0;
        for seg in 0..4 {
            let base = seg * 0x40000;
            if data[base + 0x0104..base + 0x0134] == *logo {
                copies += 1;
            }
        }
        copies >= 2
    }

    /// Determine the number of 16KB ROM banks. The cartridge header byte at
    /// 0x0148 is the nominal size, but it is only metadata: the physical ROM
    /// chip determines how many banks the MBC can actually address. Some test
    /// ROMs (e.g. gbmicrotest) ship a deliberately wrong header (claims 32KB
    /// but is 2MB), so when the real file is larger we trust the file size,
    /// rounding up to the next power-of-two bank count (banking masks are
    /// bit-based: bank index is taken modulo this count).
    fn compute_rom_banks(rom_size_code: u8, data_len: usize) -> Result<usize, io::Error> {
        let header_banks = match rom_size_code {
            0x00 => 2,   // 32KB = 2 banks of 16KB
            0x01 => 4,   // 64KB = 4 banks of 16KB
            0x02 => 8,   // 128KB = 8 banks of 16KB
            0x03 => 16,  // 256KB = 16 banks of 16KB
            0x04 => 32,  // 512KB = 32 banks of 16KB
            0x05 => 64,  // 1MB = 64 banks of 16KB
            0x06 => 128, // 2MB = 128 banks of 16KB
            0x07 => 256, // 4MB = 256 banks of 16KB
            0x08 => 512, // 8MB = 512 banks of 16KB (MBC5 64Mbit)
            _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid ROM size")),
        };
        // Number of whole 16KB banks present in the actual file, rounded up to a
        // power of two so the bank-number modulo mask matches the wired address
        // lines.
        let file_banks = data_len.div_ceil(0x4000).next_power_of_two().max(2);
        Ok(header_banks.max(file_banks))
    }

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

        // Calculate number of ROM banks (header size, widened to the real file).
        let rom_banks = Self::compute_rom_banks(rom_size_code, data.len())?;

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

        // Detect CGB support
        let cgb_support = Self::detect_cgb_support(&data);

        // Detect MBC1 multicart wiring from the per-segment logo layout.
        let mbc1_multicart = Self::detect_mbc1_multicart(cartridge_type, &data);

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
            mbc1_multicart,
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
            rtc_cycle_accum: 0,
            mbc5_rom_bank_low: 1,
            mbc5_rom_bank_high: 0,
            mbc5_ram_bank: 0,
            cgb_support,
            rumble_motor: false,
            rtc_memory: Vec::new(),
            host_managed_saves: false,
        };

        // Try to load existing save file or create new one (only for battery-backed RAM)
        cartridge.load_or_create_save_file()?;

        Ok(cartridge)
    }

    /// Extract ROM data from zip bytes.
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

        // Calculate number of ROM banks (header size, widened to the real file).
        let rom_banks = Self::compute_rom_banks(rom_size_code, actual_data.len())?;

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

        // Detect CGB support
        let cgb_support = Self::detect_cgb_support(&actual_data);

        // Detect MBC1 multicart wiring from the per-segment logo layout.
        let mbc1_multicart = Self::detect_mbc1_multicart(cartridge_type, &actual_data);

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
            mbc1_multicart,
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
            rtc_cycle_accum: 0,
            mbc5_rom_bank_low: 1,
            mbc5_rom_bank_high: 0,
            mbc5_ram_bank: 0,
            cgb_support,
            rumble_motor: false,
            rtc_memory: Vec::new(),
            host_managed_saves: false,
        };

        // In-memory loading intentionally skips save files so test runners and
        // WASM callers do not create sidecar files.

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
            MBC5 => CartridgeType::MBC5 { ram: false, battery: false, _rumble: false },
            MBC5_RAM => CartridgeType::MBC5 { ram: true, battery: false, _rumble: false },
            MBC5_RAM_BATTERY => CartridgeType::MBC5 { ram: true, battery: true, _rumble: false },
            MBC5_RUMBLE => CartridgeType::MBC5 { ram: false, battery: false, _rumble: true },
            MBC5_RUMBLE_RAM => CartridgeType::MBC5 { ram: true, battery: false, _rumble: true },
            MBC5_RUMBLE_RAM_BATTERY => CartridgeType::MBC5 { ram: true, battery: true, _rumble: true },
            _ => CartridgeType::NoMBC,
        }
    }

    fn get_rom_bank(&self) -> usize {
        match self.get_cartridge_type() {
            CartridgeType::MBC1 { .. } => {
                // The 0x4000-0x7FFF ROM bank is always (BANK2 << shift) | BANK1,
                // regardless of banking mode. BANK1's zero->one remap is applied
                // at write time, so banks 0x20/0x40/0x60 (BANK1==0 with BANK2 set)
                // remain inaccessible exactly as on hardware.
                let bank = if self.mbc1_multicart {
                    // Multicart: BANK2 -> bits 4-5, only low 4 bits of BANK1 wired.
                    ((self.ram_bank_or_rom_bank_high as usize) << 4)
                        | (self.rom_bank_low as usize & 0x0F)
                } else {
                    ((self.ram_bank_or_rom_bank_high as usize) << 5)
                        | (self.rom_bank_low as usize)
                };

                // Limit to available banks
                bank % self.rom_banks
            }
            CartridgeType::MBC2 { .. } => {
                // MBC2 uses only the lower 4 bits, bank 0 maps to bank 1
                let bank = (self.rom_bank_low & 0x0F) as usize;
                if bank == 0 { 1 } else { bank % self.rom_banks }
            }
            CartridgeType::MBC3 { .. } => {
                // MBC3 uses 7 bits for ROM bank selection; the MBC30 variant
                // (>2MB ROM / >32KB RAM carts) wires all 8. Bank 0 maps to 1.
                let mask = if self.is_mbc30() { 0xFF } else { 0x7F };
                let bank = (self.rom_bank_low & mask) as usize;
                if bank == 0 { 1 } else { bank % self.rom_banks }
            }
            CartridgeType::MBC5 { .. } => {
                // MBC5 uses 9 bits for ROM bank selection (8 bits low + 1 bit high)
                // Bank 0 can be selected for the switchable area in MBC5
                let bank = (self.mbc5_rom_bank_low as usize) | ((self.mbc5_rom_bank_high as usize & 0x01) << 8);
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
            CartridgeType::MBC2 { .. } => 0, // MBC2 has built-in RAM, no banking
            CartridgeType::MBC3 { .. } => {
                // MBC3 uses mbc3_ram_bank for both RAM and RTC. MBC30 has 8 RAM
                // banks (64KB) so a third select bit is wired.
                let mask = if self.is_mbc30() { 0x07 } else { 0x03 };
                (self.mbc3_ram_bank & mask) as usize % self.ram_banks.max(1)
            }
            CartridgeType::MBC5 { .. } => {
                // MBC5 uses 4 bits for RAM bank selection (0x00-0x0F)
                (self.mbc5_ram_bank & 0x0F) as usize % self.ram_banks.max(1)
            }
            CartridgeType::NoMBC => 0,
        }
    }

    /// ROM bank mapped at the 0x0000-0x3FFF region. Normally bank 0, but on
    /// MBC1 in banking mode 1 the BANK2 register is also applied here, so a
    /// large cart sees bank 0x20/0x40/0x60 (or 0x10/0x20/0x30 on a multicart).
    fn get_rom_bank0(&self) -> usize {
        match self.get_cartridge_type() {
            CartridgeType::MBC1 { .. } if self.banking_mode == 1 => {
                let bank = if self.mbc1_multicart {
                    (self.ram_bank_or_rom_bank_high as usize) << 4
                } else {
                    (self.ram_bank_or_rom_bank_high as usize) << 5
                };
                bank % self.rom_banks
            }
            _ => 0,
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
        if let Some(save_path) = self.get_save_file_path() {
            self.attach_save_file_at(Path::new(&save_path))
        } else {
            Ok(())
        }
    }

    /// Attach a battery-backed save file at an explicit path. Used by
    /// callers (e.g. the Android entry point) that loaded the ROM via
    /// `from_bytes` and therefore have no `rom_path` from which to derive
    /// the default sidecar `.sav` location. Behaviour mirrors
    /// `load_or_create_save_file`: if the file exists its contents are
    /// copied into the cart's RAM, otherwise the current RAM contents
    /// are written out. Either way the file is kept open for streaming
    /// per-byte writes from `write_ram_byte` / `write_mbc2_ram_byte`.
    ///
    /// No-op for cartridges without battery-backed RAM.
    pub fn attach_save_file(&mut self, path: impl AsRef<Path>) -> Result<(), io::Error> {
        self.attach_save_file_at(path.as_ref())
    }

    /// Overwrite the cartridge's battery-backed RAM with the supplied
    /// bytes. Intended for the Android sibling-`.sav` path: SAF hands us
    /// the user's existing save bytes from /sdcard, and we copy them
    /// into the live cart RAM *after* `attach_save_file` has prepared
    /// the internal sidecar so subsequent writes still persist. If a
    /// save file is currently attached, the whole RAM image is flushed
    /// to disk so the internal sidecar matches the loaded state.
    ///
    /// Returns the number of bytes actually copied (truncated to the
    /// cart's RAM size). No-op for non-battery carts.
    pub fn load_sram_bytes(&mut self, bytes: &[u8]) -> Result<usize, io::Error> {
        if !self.has_battery() {
            return Ok(0);
        }
        let copied = match self.get_cartridge_type() {
            CartridgeType::MBC2 { .. } => {
                let n = bytes.len().min(self.mbc2_ram.len());
                self.mbc2_ram[..n].copy_from_slice(&bytes[..n]);
                // MBC2 stores 4-bit nibbles; mask to be safe.
                for b in &mut self.mbc2_ram[..n] {
                    *b &= 0x0F;
                }
                n
            }
            _ => {
                if self.ram_data.is_empty() {
                    return Ok(0);
                }
                let n = bytes.len().min(self.ram_data.len());
                self.ram_data[..n].copy_from_slice(&bytes[..n]);
                n
            }
        };
        // If a save file is attached, flush the current RAM image so the
        // internal sidecar mirrors the freshly-loaded state.
        let is_mbc2 = matches!(self.get_cartridge_type(), CartridgeType::MBC2 { .. });
        if let Some(ref mut file) = self.save_file {
            file.seek(SeekFrom::Start(0))?;
            let buf: &[u8] = if is_mbc2 { &self.mbc2_ram } else { &self.ram_data };
            file.write_all(buf)?;
            file.flush()?;
        }
        Ok(copied)
    }

    fn attach_save_file_at(&mut self, save_path: &Path) -> Result<(), io::Error> {
        // Only process save files for cartridges with battery-backed RAM
        if !self.has_battery() || self.host_managed_saves {
            return Ok(());
        }

        // For MBC2, we need to save the built-in RAM instead of external RAM
        let save_data_is_empty = match self.get_cartridge_type() {
            CartridgeType::MBC2 { .. } => self.mbc2_ram.is_empty(),
            _ => self.ram_data.is_empty(),
        };
        if save_data_is_empty {
            return Ok(());
        }

        // Ensure the parent directory exists; on Android the save
        // directory is created by `android::save_dir()` but callers may
        // hand us nested paths.
        if let Some(parent) = save_path.parent()
            && !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }

        if save_path.exists() {
            // Load existing save file
            let loaded_data = fs::read(save_path)?;
            match self.get_cartridge_type() {
                CartridgeType::MBC2 { .. } => {
                    if loaded_data.len() <= self.mbc2_ram.len() {
                        self.mbc2_ram[..loaded_data.len()].copy_from_slice(&loaded_data);
                        println!("Loaded MBC2 save file: {}", save_path.display());
                    }
                }
                _ => {
                    if loaded_data.len() <= self.ram_data.len() {
                        self.ram_data[..loaded_data.len()].copy_from_slice(&loaded_data);
                        println!("Loaded save file: {}", save_path.display());
                    }
                }
            }
        } else {
            // Create new save file with current RAM data
            match self.get_cartridge_type() {
                CartridgeType::MBC2 { .. } => {
                    fs::write(save_path, &self.mbc2_ram)?;
                    println!("Created new MBC2 save file: {}", save_path.display());
                }
                _ => {
                    fs::write(save_path, &self.ram_data)?;
                    println!("Created new save file: {}", save_path.display());
                }
            }
        }

        // Open file handle for efficient streaming writes
        self.save_file = Some(OpenOptions::new().write(true).open(save_path)?);
        Ok(())
    }

    /// Attach a battery-backed save file via an already-open `File`
    /// handle. Used by callers that can't represent the save location
    /// as a filesystem `Path` (e.g. Android SAF, which gives us a file
    /// descriptor pointing at a `content://` document). The file must
    /// be opened read+write and positioned arbitrarily; this function
    /// will `seek` as needed.
    ///
    /// Behaviour mirrors [`attach_save_file_at`]: if the file is
    /// non-empty its contents are copied into the cart's RAM, otherwise
    /// the current RAM contents are written out. The file is retained
    /// for streaming per-byte writes from `write_ram_byte` /
    /// `write_mbc2_ram_byte`.
    ///
    /// No-op for cartridges without battery-backed RAM (the file is
    /// dropped, closing its underlying descriptor).
    pub fn attach_save_file_from(&mut self, mut file: File) -> Result<(), io::Error> {
        if !self.has_battery() {
            return Ok(());
        }
        let save_data_is_empty = match self.get_cartridge_type() {
            CartridgeType::MBC2 { .. } => self.mbc2_ram.is_empty(),
            _ => self.ram_data.is_empty(),
        };
        if save_data_is_empty {
            return Ok(());
        }
        let len = file.seek(SeekFrom::End(0))?;
        file.seek(SeekFrom::Start(0))?;
        if len > 0 {
            let mut loaded_data = Vec::with_capacity(len as usize);
            file.read_to_end(&mut loaded_data)?;
            match self.get_cartridge_type() {
                CartridgeType::MBC2 { .. } => {
                    let n = loaded_data.len().min(self.mbc2_ram.len());
                    self.mbc2_ram[..n].copy_from_slice(&loaded_data[..n]);
                    for b in &mut self.mbc2_ram[..n] {
                        *b &= 0x0F;
                    }
                    println!("Loaded MBC2 save file from fd ({n} bytes)");
                }
                _ => {
                    let n = loaded_data.len().min(self.ram_data.len());
                    self.ram_data[..n].copy_from_slice(&loaded_data[..n]);
                    println!("Loaded save file from fd ({n} bytes)");
                }
            }
        } else {
            // Empty/new save file: seed it with the current RAM image so
            // subsequent per-byte streaming writes have a well-defined
            // backing region.
            file.seek(SeekFrom::Start(0))?;
            let buf: &[u8] = match self.get_cartridge_type() {
                CartridgeType::MBC2 { .. } => &self.mbc2_ram,
                _ => &self.ram_data,
            };
            file.write_all(buf)?;
            file.flush()?;
            println!("Initialised new save file via fd ({} bytes)", buf.len());
        }
        self.save_file = Some(file);
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
            CartridgeType::MBC5 { battery, .. } => battery,
            CartridgeType::NoMBC => false,
        }
    }

    /// Get CGB support information from cartridge header
    pub fn get_cgb_support(&self) -> CgbSupport {
        self.cgb_support.clone()
    }

    /// Check if this cartridge supports CGB features
    pub fn supports_cgb(&self) -> bool {
        matches!(self.cgb_support, CgbSupport::Compatible | CgbSupport::Only)
    }

    /// Check if this cartridge requires CGB hardware
    pub fn requires_cgb(&self) -> bool {
        matches!(self.cgb_support, CgbSupport::Only)
    }

    /// Read from MBC3 RTC registers
    fn read_rtc_register(&self) -> u8 {
        // Reads always return the CPU-visible (latched) shadow register. On real
        // MBC3 the internal free-running counters (`rtc_seconds`..) are never read
        // directly — a latch (any write to 0x6000-0x7FFF) copies them into these
        // shadow registers, and software reads the shadows. Register writes go to
        // the internal counters only (see `write_rtc_register`), so a freshly
        // written value is not visible until the next latch.
        match self.mbc3_ram_bank {
            0x08 => self.rtc_seconds_latched,
            0x09 => self.rtc_minutes_latched,
            0x0A => self.rtc_hours_latched,
            0x0B => self.rtc_days_low_latched,
            0x0C => self.rtc_days_high_latched,
            _ => 0xFF,
        }
    }

    /// Write to MBC3 RTC registers. A write updates the INTERNAL free-running
    /// counter (`rtc_*`, advanced by the cycle-derived tick) only — it does NOT
    /// touch the CPU-visible latched shadow (`rtc_*_latched`, the read path).
    /// The written value only becomes visible on the next latch, exactly like
    /// gambatte's `Rtc::setS`/`setM`/... (which write `data*`, never `latch*`).
    /// Register widths are the documented MBC3 masks (seconds/minutes 6-bit,
    /// hours 5-bit, days_high = day bit 8 + HALT + carry).
    fn write_rtc_register(&mut self, value: u8) {
        match self.mbc3_ram_bank {
            0x08 => {
                self.rtc_seconds = value & 0x3F;
                // Writing seconds resets the internal sub-second divider (gambatte
                // `setS` clears dataC_), so the next tick is a full second away.
                self.rtc_cycle_accum = 0;
            }
            0x09 => self.rtc_minutes = value & 0x3F,
            0x0A => self.rtc_hours = value & 0x1F,
            0x0B => self.rtc_days_low = value,
            0x0C => self.rtc_days_high = value & 0xC1,
            _ => {}
        }
    }

    /// Copy the live internal RTC counters into the CPU-visible latch registers.
    /// On real MBC3 this happens on ANY write to the 0x6000-0x7FFF region (no
    /// 0x00->0x01 edge is required — gambatte `Mbc3::romWrite` case 3 calls
    /// `rtc_->latch(cc)` unconditionally). The read path returns these shadows,
    /// so software must latch to observe the advancing clock.
    fn latch_rtc(&mut self) {
        self.rtc_seconds_latched = self.rtc_seconds;
        self.rtc_minutes_latched = self.rtc_minutes;
        self.rtc_hours_latched = self.rtc_hours;
        self.rtc_days_low_latched = self.rtc_days_low;
        self.rtc_days_high_latched = self.rtc_days_high;
        self.mbc3_rtc_latched = true;
    }

    /// Advance the cycle-derived RTC by `cycles` master (dot) clock T-cycles.
    /// Driven from the bus tick loop (`master_cc` advances at 4.194304 MHz
    /// regardless of CPU speed), so the clock is fully deterministic. No-op
    /// unless this cart actually has an RTC. The HALT bit (bit 6 of days_high)
    /// freezes advancement but the sub-second accumulator keeps running so the
    /// halt/resume boundary lands on an exact second, matching hardware.
    pub fn rtc_tick(&mut self, cycles: u64) {
        if cycles == 0 || !self.has_rtc() {
            return;
        }
        // HALT bit frozen: the crystal still oscillates but the counters do not
        // advance. Do not accumulate while halted so no seconds are "banked".
        if self.rtc_days_high & 0x40 != 0 {
            return;
        }
        self.rtc_cycle_accum = self.rtc_cycle_accum.wrapping_add(cycles);
        const CYCLES_PER_SECOND: u64 = 4_194_304;
        while self.rtc_cycle_accum >= CYCLES_PER_SECOND {
            self.rtc_cycle_accum -= CYCLES_PER_SECOND;
            self.advance_rtc_second();
        }
    }

    /// Increment the live RTC by one second with the full MBC3 cascade:
    /// seconds 0->59, minutes 0->59, hours 0->23, then the 9-bit day counter
    /// (days_low + bit 0 of days_high). Overflow of the day counter sets the
    /// day-carry flag (bit 7 of days_high), which latches until software clears
    /// it. Mirrors real MBC3: the 6-bit seconds/minutes registers can hold
    /// out-of-range values written by software; on the natural tick the seconds
    /// counter counts 0..59 and wraps, and an out-of-range value simply keeps
    /// counting up (it does NOT force-normalise), so a value like 60 advances to
    /// 61.. up to 63 then wraps to 0 with a minute carry — the documented
    /// hardware quirk the RTC test ROMs check.
    fn advance_rtc_second(&mut self) {
        // Seconds: 6-bit counter. 59 -> 0 carries to minutes; any other value
        // (including out-of-range 60-62) just increments, and 63 -> 0 without a
        // carry (the 6-bit register simply overflows) — matching hardware where
        // only the 59->0 transition produces the minute carry.
        let sec = self.rtc_seconds & 0x3F;
        if sec == 59 {
            self.rtc_seconds = 0;
        } else {
            self.rtc_seconds = (sec + 1) & 0x3F;
            return;
        }

        let min = self.rtc_minutes & 0x3F;
        if min == 59 {
            self.rtc_minutes = 0;
        } else {
            self.rtc_minutes = (min + 1) & 0x3F;
            return;
        }

        let hour = self.rtc_hours & 0x1F;
        if hour == 23 {
            self.rtc_hours = 0;
        } else {
            self.rtc_hours = (hour + 1) & 0x1F;
            return;
        }

        // Day counter: 9 bits = days_low (8) + bit 0 of days_high. On overflow
        // past 0x1FF the counter wraps to 0 and the carry flag (bit 7) latches.
        let day = (self.rtc_days_low as u16) | (((self.rtc_days_high & 0x01) as u16) << 8);
        let next = day + 1;
        self.rtc_days_low = (next & 0xFF) as u8;
        // Preserve HALT (bit 6) and the already-latched carry (bit 7); set bit 0
        // from the new day counter, and set carry on the 0x1FF -> 0x200 wrap.
        let mut high = self.rtc_days_high & 0xC0;
        if next & 0x100 != 0 {
            high |= 0x01;
        }
        if next > 0x1FF {
            self.rtc_days_low = 0;
            high &= !0x01;
            high |= 0x80; // day-carry latches until software clears it
        }
        self.rtc_days_high = high;
    }

    // --- libretro accessors ---

    /// Mark this cartridge as host-managed: it will not open or write any
    /// sidecar `.sav` file. Persistence of the in-memory RAM is the frontend's
    /// responsibility (e.g. RetroArch via `RETRO_MEMORY_SAVE_RAM`).
    pub fn set_host_managed_saves(&mut self, enabled: bool) {
        self.host_managed_saves = enabled;
    }

    /// Mutable view of the battery/save RAM the frontend should persist. For
    /// MBC2 this is the built-in 512x4 RAM; otherwise the external RAM banks.
    /// Returns an empty slice when there is no save RAM.
    pub fn save_ram_mut(&mut self) -> &mut [u8] {
        match self.get_cartridge_type() {
            CartridgeType::MBC2 { .. } => &mut self.mbc2_ram,
            _ => &mut self.ram_data,
        }
    }

    /// Read-only view of the battery/save RAM.
    pub fn save_ram(&self) -> &[u8] {
        match self.get_cartridge_type() {
            CartridgeType::MBC2 { .. } => &self.mbc2_ram,
            _ => &self.ram_data,
        }
    }

    /// True if this cartridge has an MBC3 real-time clock.
    pub fn has_rtc(&self) -> bool {
        matches!(self.get_cartridge_type(), CartridgeType::MBC3 { timer: true, .. })
    }

    /// MBC30: the large-capacity MBC3 variant (used by e.g. Japanese Pokémon
    /// Crystal) that wires 8 ROM-bank bits (256 banks / 4MB, vs MBC3's 7 bits /
    /// 2MB) and 3 RAM-bank bits (8 banks / 64KB, vs 2 bits / 32KB). There is no
    /// header flag for it; a cart wired for MBC3 addressing cannot exceed 2MB
    /// ROM / 32KB RAM, so exceeding either limit identifies the MBC30 per
    /// Pan Docs.
    fn is_mbc30(&self) -> bool {
        matches!(self.get_cartridge_type(), CartridgeType::MBC3 { .. })
            && (self.rom_banks > 128 || self.ram_banks > 4)
    }

    /// Mutable view of the RTC register bytes for `RETRO_MEMORY_RTC`. Layout is
    /// the 10 register values (live then latched) as little-endian u32 plus an
    /// 8-byte latch timestamp, matching the common BGB/libretro `.rtc` format.
    /// The scratch buffer is synced from the live registers on each call.
    pub fn rtc_memory_mut(&mut self) -> &mut [u8] {
        if !self.has_rtc() {
            self.rtc_memory.clear();
            return &mut self.rtc_memory;
        }
        let regs = [
            self.rtc_seconds as u32,
            self.rtc_minutes as u32,
            self.rtc_hours as u32,
            self.rtc_days_low as u32,
            self.rtc_days_high as u32,
            self.rtc_seconds_latched as u32,
            self.rtc_minutes_latched as u32,
            self.rtc_hours_latched as u32,
            self.rtc_days_low_latched as u32,
            self.rtc_days_high_latched as u32,
        ];
        self.rtc_memory.resize(48, 0);
        for (i, r) in regs.iter().enumerate() {
            self.rtc_memory[i * 4..i * 4 + 4].copy_from_slice(&r.to_le_bytes());
        }
        &mut self.rtc_memory
    }

    /// True for MBC5 rumble cartridges.
    pub fn has_rumble(&self) -> bool {
        matches!(self.get_cartridge_type(), CartridgeType::MBC5 { _rumble: true, .. })
    }

    /// Current state of the rumble motor (bit 3 of the last RAM-bank write on
    /// a rumble cart). Always false for non-rumble carts.
    pub fn rumble_active(&self) -> bool {
        self.rumble_motor
    }

    /// Patch a ROM byte (Game Genie). `addr` is a 0x0000-0x7FFF CPU address;
    /// the patch is applied to ROM bank 0 for 0x0000-0x3FFF and to the bank
    /// currently mapped at 0x4000-0x7FFF otherwise. When `compare` is given the
    /// patch only applies if the existing byte matches. Game Genie codes target
    /// bank 0 / early ROM in practice, where the mapping is fixed.
    pub fn apply_rom_patch(&mut self, addr: u16, new: u8, compare: Option<u8>) {
        let offset = if addr < 0x4000 {
            addr as usize
        } else if addr < 0x8000 {
            let bank = self.get_rom_bank();
            (addr as usize - 0x4000) + bank * 0x4000
        } else {
            return;
        };
        if offset >= self.rom_data.len() {
            return;
        }
        if let Some(c) = compare
            && self.rom_data[offset] != c
        {
            return;
        }
        self.rom_data[offset] = new;
    }
}

impl memory::Addressable for Cartridge {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            // ROM Bank 0 (0x0000-0x3FFF). Fixed to bank 0 except on MBC1 in
            // banking mode 1, where BANK2 also selects this region.
            mmio::CARTRIDGE_START..=mmio::CARTRIDGE_END => {
                let rom_bank0 = self.get_rom_bank0();
                let offset = (addr - mmio::CARTRIDGE_START) as usize + (rom_bank0 * 0x4000);
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
                        // MBC2 has built-in 512x4 RAM. The 512 nibbles echo every
                        // 0x200 bytes across the whole 0xA000-0xBFFF window. Only
                        // the low 4 data bits are stored; the upper 4 read back as
                        // 1s (open data lines), so reads return 0xF0 | nibble.
                        if self.ram_enabled {
                            let offset = (addr - MBC2_RAM_START) as usize % self.mbc2_ram.len();
                            0xF0 | (self.mbc2_ram[offset] & 0x0F)
                        } else {
                            0xFF
                        }
                    }
                    CartridgeType::MBC3 { ram: true, .. } => {
                        if self.ram_enabled {
                            // MBC30 wires a third RAM-bank bit: selects 0x00-0x07
                            // are RAM there, 0x00-0x03 on plain MBC3. 0x08-0x0C
                            // are the RTC registers on both.
                            let ram_select_max = if self.is_mbc30() { 0x07 } else { 0x03 };
                            if self.mbc3_ram_bank <= ram_select_max {
                                // RAM bank access
                                if !self.ram_data.is_empty() {
                                    let ram_bank = self.get_ram_bank();
                                    let offset = ((addr - EXTERNAL_RAM_START) as usize + (ram_bank * 0x2000)) % self.ram_data.len();
                                    self.ram_data[offset]
                                } else {
                                    0xFF
                                }
                            } else if (0x08..=0x0C).contains(&self.mbc3_ram_bank) {
                                // RTC register access
                                self.read_rtc_register()
                            } else {
                                0xFF
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
                    CartridgeType::MBC5 { ram: true, .. } => {
                        if self.ram_enabled && !self.ram_data.is_empty() {
                            let ram_bank = self.get_ram_bank();
                            let offset = ((addr - EXTERNAL_RAM_START) as usize + (ram_bank * 0x2000)) % self.ram_data.len();
                            self.ram_data[offset]
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
            // MBC2 register block (0x0000-0x3FFF). MBC2 has a SINGLE register
            // region here, selected by address bit 8: bit8==0 => RAMG (RAM
            // enable), bit8==1 => ROMB (ROM bank, low 4 bits). The 0x2000
            // boundary is irrelevant on MBC2 — only bit 8 matters — so handle
            // the whole range here before the generic per-quarter arms.
            RAM_ENABLE_START..=ROM_BANK_SELECT_END
                if matches!(self.get_cartridge_type(), CartridgeType::MBC2 { .. }) =>
            {
                if (addr & 0x0100) == 0 {
                    // RAMG: RAM enable
                    self.ram_enabled = (value & 0x0F) == 0x0A;
                } else {
                    // ROMB: 4-bit ROM bank, value 0 maps to bank 1
                    self.rom_bank_low = (value & 0x0F).max(1);
                }
            }
            // RAM Enable (0x0000-0x1FFF)
            RAM_ENABLE_START..=RAM_ENABLE_END => {
                match self.get_cartridge_type() {
                    CartridgeType::MBC1 { .. } => {
                        self.ram_enabled = (value & 0x0F) == 0x0A;
                    }
                    CartridgeType::MBC3 { .. } => {
                        self.ram_enabled = (value & 0x0F) == 0x0A;
                    }
                    CartridgeType::MBC5 { .. } => {
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
                    CartridgeType::MBC3 { .. } => {
                        // 7 bits (8 on MBC30), minimum value 1. The full write
                        // is stored; get_rom_bank applies the wired width, so
                        // e.g. 0x80 on plain MBC3 decodes as bank 0 -> 1.
                        self.rom_bank_low = value.max(1);
                    }
                    CartridgeType::MBC5 { .. } => {
                        // MBC5 ROM bank select depends on address range
                        if addr <= 0x2FFF {
                            // 0x2000-0x2FFF: Lower 8 bits of ROM bank
                            self.mbc5_rom_bank_low = value; // MBC5 allows bank 0
                        } else {
                            // 0x3000-0x3FFF: Upper 1 bit of ROM bank
                            self.mbc5_rom_bank_high = value & 0x01; // Only bit 0 is used
                        }
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
                        // The MBC3 RAM-bank / RTC-select register is 4 bits wide:
                        // only the low nibble is latched. Values 0x00-0x03 select
                        // a RAM bank, 0x08-0x0C select an RTC register, and the
                        // rest (0x04-0x07, 0x0D-0x0F) read back 0xFF. Because it is
                        // a 4-bit register, a write of e.g. 0x18 behaves exactly as
                        // 0x08 (rtc-invalid-banks-test relies on this masking).
                        self.mbc3_ram_bank = value & 0x0F;
                    }
                    CartridgeType::MBC5 { _rumble, .. } => {
                        if _rumble {
                            // On rumble carts bit 3 drives the motor; only the
                            // low 3 bits select the RAM bank.
                            self.rumble_motor = (value & 0x08) != 0;
                        }
                        self.mbc5_ram_bank = value; // 4 bits used (0x00-0x0F)
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
                        // RTC latch: ANY write to 0x6000-0x7FFF copies the live
                        // clock into the visible latch registers. Real MBC3 does
                        // not require a 0x00->0x01 edge (gambatte latches on every
                        // write); latch-rtc-test writes random values here and
                        // expects each to re-latch.
                        self.latch_rtc();
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
                        // MBC2 has built-in 512x4 RAM that echoes every 0x200
                        // bytes across the whole 0xA000-0xBFFF window.
                        if self.ram_enabled {
                            let offset = (addr - MBC2_RAM_START) as usize % self.mbc2_ram.len();
                            let _ = self.write_mbc2_ram_byte(offset, value); // Ignore errors for now
                        }
                    }
                    CartridgeType::MBC3 { ram: true, .. } => {
                        if self.ram_enabled {
                            // MBC30 RAM selects reach 0x07 (see the read path).
                            let ram_select_max = if self.is_mbc30() { 0x07 } else { 0x03 };
                            if self.mbc3_ram_bank <= ram_select_max {
                                // RAM bank access
                                if !self.ram_data.is_empty() {
                                    let ram_bank = self.get_ram_bank();
                                    let offset = ((addr - EXTERNAL_RAM_START) as usize + (ram_bank * 0x2000)) % self.ram_data.len();
                                    let _ = self.write_ram_byte(offset, value);
                                }
                            } else if (0x08..=0x0C).contains(&self.mbc3_ram_bank) {
                                // RTC register access
                                self.write_rtc_register(value);
                            }
                        }
                    }
                    CartridgeType::MBC3 { ram: false, timer: true, .. } => {
                        // Timer-only MBC3 (no RAM)
                        if self.ram_enabled && (0x08..=0x0C).contains(&self.mbc3_ram_bank) {
                            self.write_rtc_register(value);
                        }
                    }
                    CartridgeType::MBC5 { ram: true, .. } => {
                        if self.ram_enabled && !self.ram_data.is_empty() {
                            let ram_bank = self.get_ram_bank();
                            let offset = ((addr - EXTERNAL_RAM_START) as usize + (ram_bank * 0x2000)) % self.ram_data.len();
                            let _ = self.write_ram_byte(offset, value);
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
