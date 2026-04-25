use crate::memory;
use crate::memory::mmio;
use serde::{Deserialize, Serialize};

use std::cell::Cell;
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

// Bankless ROM+RAM carts (Pan Docs "No MBC": "Optionally up to 8 KiB of RAM
// could be connected at $A000-BFFF"): the RAM chip is wired straight through,
// with no banking and no enable gate. $09 adds a battery. No licensed cart is
// known to use these type bytes, but homebrew, test ROMs and mis-headered
// dumps do.
const ROM_RAM: u8 = 0x08;
const ROM_RAM_BATTERY: u8 = 0x09;

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

// MBC7+SENSOR+RUMBLE+RAM+BATTERY (Kirby Tilt 'n' Tumble, Command Master).
// The "RAM" is a 93LC56 serial EEPROM (256 bytes) and the sensor is a 2-axis
// ADXL202E accelerometer; despite the official type name no MBC7 cart has a
// rumble motor. The Japan-only Command Master uses the larger 93LC66 EEPROM
// (512 bytes) - not modeled (remaining gap; would need header-checksum
// sniffing since the type byte is identical).
const MBC7_SENSOR_RUMBLE_RAM_BATTERY: u8 = 0x22;

// HuC-3: ROM/RAM banking + RTC + IR + piezo speaker (Robopon, Pocket Family).
// The type byte implies RAM+BATTERY+RTC.
const HUC3: u8 = 0xFE;

// HuC-1: ROM/RAM banking + IR link (Pokemon Card GB). The type byte implies
// RAM+BATTERY. Differs from MBC1 (Pan Docs HuC1): there is NO RAM-enable
// gate -- the 0x0000-0x1FFF register instead switches A000-BFFF between RAM
// mode and the IR transceiver ($0E selects IR, anything else RAM).
const HUC1_RAM_BATTERY: u8 = 0xFF;

// POCKET CAMERA (Game Boy Camera): MAC-GBD controller + M64282FP "retina"
// image sensor. MBC3-like banking, 128KB battery-backed RAM, and a 54-byte
// write-only sensor/dither register file mapped over A000-BFFF when the RAM
// bank select has bit 4 set (Pan Docs "Game Boy Camera", reverse-engineered
// by AntonioND: github.com/AntonioND/gbcam-rev-engineer). The type byte
// implies RAM+BATTERY.
const POCKET_CAMERA: u8 = 0xFC;

// Remaining unimplemented mapper families (fall through to NoMBC):
//   0xFD BANDAI TAMA5.

// ---------------------------------------------------------------------------
// Unlicensed / bootleg mappers. These boards spoof the header type byte
// ($00/$01, or use out-of-spec values like $97/$99/$EA), so they are detected
// from ROM content (logo checksums / publisher strings / title+size shapes),
// not from $0147. References: hhugboy (src/memory/CartDetection.cpp and
// src/memory/mbc/MbcUnl*.cpp, by taizou/NewRisingSun), mGBA (src/gb/mbc.c
// _detectUnlMBC + src/gb/mbc/unlicensed.c), Pan Docs "Other MBCs"
// (https://gbdev.io/pandocs/othermbc.html), and the gbdev forum thread
// "Cartridges with Rare Mappers" (https://gbdev.gg8.se/forums/viewtopic.php?id=948).
// ---------------------------------------------------------------------------

/// Byte sum of the 48-byte Nintendo logo at its usual $0104 location.
const LOGO_SUM_NINTENDO: u32 = 5446;
/// Byte sums of the two Sachen logo variants (hhugboy CartDetection.cpp).
const LOGO_SUM_SACHEN_A: u32 = 5542;
const LOGO_SUM_SACHEN_B: u32 = 7484;
/// Byte sum of the Rocket Games logo (hhugboy: 2756). Rocket carts never
/// contain the Nintendo logo in the dump; the mapper XOR-decodes it for the
/// boot ROM check (see ROCKET_LOGO_XOR).
const LOGO_SUM_ROCKET: u32 = 2756;

/// XOR pad that maps the Rocket Games logo bytes at $0104-$0133 to the
/// Nintendo logo. While the mapper is in its locked-CGB phase it presents the
/// XOR-decoded (Nintendo) logo so the boot ROM check passes; once unlocked
/// the game reads its own (Rocket) logo back. Verbatim from hhugboy
/// MbcUnlRocketGames.cpp (rocketXorNintendoLogo).
const ROCKET_LOGO_XOR: [u8; 48] = [
    0xdf, 0xce, 0x97, 0x78, 0xcd, 0x2f, 0xf0, 0x0b, 0x0b, 0xea, 0x78, 0x83,
    0x08, 0x1d, 0x9a, 0x45, 0x11, 0x2b, 0xe1, 0x11, 0xf8, 0x88, 0xf8, 0x8e,
    0xfe, 0x88, 0x2a, 0xc4, 0xff, 0xfc, 0xd9, 0x87, 0x22, 0xab, 0x67, 0x7d,
    0x77, 0x2c, 0xa8, 0xee, 0xff, 0x9b, 0x99, 0x91, 0xaa, 0x9b, 0x33, 0x3e,
];

// Lock-phase values shared by the Sachen and Rocket boot state machines
// (hhugboy MODE_LOCKED_DMG/MODE_LOCKED_CGB/MODE_UNLOCKED, mGBA GBSachenLockState).
const UNL_LOCKED_DMG: u8 = 0;
const UNL_LOCKED_CGB: u8 = 1;
const UNL_UNLOCKED: u8 = 2;

/// NT/Makon "old" bank-line swap tables for the $5003 bit-4 mode, applied to
/// the ROM bank number: output bit i = input bit table[i] (mGBA
/// _ntOld1Reorder/_ntOld2Reorder; identical to hhugboy's flippo1/flippo2).
const NT_OLD1_REORDER: [u8; 8] = [0, 2, 1, 4, 3, 5, 6, 7];
const NT_OLD2_REORDER: [u8; 8] = [1, 2, 0, 3, 4, 5, 6, 7];

/// Unlicensed mapper families detected from ROM content at load time. The
/// header type byte is unreliable on these boards, so this override wins over
/// `cartridge_type` in `get_cartridge_type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UnlMapper {
    #[default]
    None,
    /// Wisdom Tree one-latch board: a write anywhere in $0000-$3FFF selects a
    /// whole-$0000-$7FFF 32KB bank from the low 6 bits of the ADDRESS (data
    /// ignored). Pan Docs "Other MBCs"; mGBA _GBWisdomTree.
    WisdomTree,
    /// Rocket Games ($97 singles / $99 2-in-1s): 16KB inner bank at exactly
    /// $3F00 (0 maps to 1), 256KB outer bank at exactly $3FC0, plus the
    /// A15-transition unlock counter with the logo XOR window. hhugboy
    /// MbcUnlRocketGames.cpp; gbdev forum id=948; MiSTer unlicensed thread.
    Rocket,
    /// Sachen MMC1: base/mask outer banking + the $01xx address descramble +
    /// the DMG lock phase (RA7 forced high). hhugboy MbcUnlSachenMMC1.cpp;
    /// mGBA _GBSachen/_GBSachenMMC1Read.
    SachenMmc1,
    /// Sachen MMC2: MMC1 plus a DMG->CGB->unlocked 3-phase lock (the CGB
    /// phase presents the Nintendo logo copy at $0184). hhugboy
    /// MbcUnlSachenMMC2.cpp; mGBA _GBSachenMMC2Read.
    SachenMmc2,
    /// NT/Makon older board, MBC1-style 5-bit bank register. hhugboy
    /// MbcUnlNtOld1.cpp; mGBA _GBNTOld1.
    NtOld1,
    /// NT/Makon older board, MBC3-style 8-bit bank register (+ rumble on the
    /// multicarts). hhugboy MbcUnlNtOld2.cpp; mGBA _GBNTOld2.
    NtOld2,
    /// Header liars that are electrically plain MBC1 with no RAM: Sonic 3D
    /// Blast 5 (type $EA, "code in the header area" per hhugboy), Captain
    /// Knick-Knack (Sachen dump with a Tetris header), Pocket Monsters
    /// GO!GO!GO! 256KB dumps. hhugboy UNL_MBC1NOSAVE routing.
    ForceMbc1,
    /// M161 (Mani 4 in 1, DMG-601): a one-shot latch that maps one of eight
    /// whole-32KB banks. The header spoofs MBC3+RAM+BAT ($10), so it is
    /// content-detected (256KB + title "TETRIS SET"), exactly like gambatte's
    /// presumedM161. Ported from gambatte m161.cpp.
    M161,
}

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

// POCKET CAMERA geometry/constants (Pan Docs "Game Boy Camera" /
// AntonioND gbcam-rev-engineer doc v1.1.1).
// The CAM register file: A000 trigger/status, A001-A005 M64282FP sensor
// parameters, A006-A035 the 4x4x3 dither/contrast matrix. 54 bytes total,
// mirrored every $80 across A000-BFFF while selected.
const CAM_REG_COUNT: usize = 0x36;
// Visible capture output: 128x112 pixels, 2bpp GB tiles (16x14 tiles x 16
// bytes) written by the controller to RAM bank 0 at offset $0100.
const CAM_W: usize = 128;
const CAM_H: usize = 112;
const CAM_TILE_BYTES: usize = (CAM_W / 8) * (CAM_H / 8) * 16; // 3584
const CAM_RAM_IMAGE_OFFSET: usize = 0x0100;
// The sensor array is 128x123; the controller discards the corrupt top and
// bottom rows and uses the middle 112 of a 120-row window (GiiBiiAdvance
// GBCAM_SENSOR_EXTRA_LINES).
const CAM_SENSOR_EXTRA_LINES: usize = 8;
const CAM_SENSOR_H: usize = CAM_H + CAM_SENSOR_EXTRA_LINES; // 120

#[derive(Clone, Debug)]
pub enum CartridgeType {
    NoMBC { battery: bool },
    MBC1 { ram: bool, battery: bool },
    MBC2 { battery: bool },
    MBC3 { ram: bool, battery: bool, timer: bool },
    MBC5 { ram: bool, battery: bool, rumble: bool },
    MBC7,
    HuC1,
    HuC3,
    PocketCamera,
    // Unlicensed boards (selected via UnlMapper content detection, never via
    // the header type byte alone).
    WisdomTree,
    Rocket,
    Sachen { mmc2: bool },
    NtOld { v2: bool },
    /// Mani 4 in 1 one-shot 32KB bank-latch (gambatte m161.cpp).
    M161,
}

/// 93LC56 serial-EEPROM interface state for MBC7 (Pan Docs "MBC7"). The
/// EEPROM contents themselves live in `Cartridge::ram_data` (256 bytes =
/// 128 little-endian 16-bit words) so the existing battery-save plumbing
/// persists them; this struct only models the bit-banged serial link
/// exposed at the Ax8x register (bit0=DO, bit1=DI, bit6=CLK, bit7=CS).
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize, Default, Debug)]
enum Mbc7EepromState {
    /// CS low or waiting for the start bit (first 1 on DI while CS high).
    #[default]
    Idle,
    /// Collecting the 10 instruction bits (2-bit opcode + 8 payload bits).
    Command,
    /// Collecting the 16 data bits of a WRITE/WRAL instruction.
    Input,
    /// Shifting out the 16 data bits of a READ, MSB first.
    Output,
    /// Programming instruction fully received; the actual array write
    /// happens when CS falls (93LC56 datasheet: the internal programming
    /// cycle starts on the CS falling edge after the last data bit).
    Pending,
    /// Instruction finished; further clocks are ignored until CS falls.
    Done,
}

#[derive(Clone, Serialize, Deserialize, Default)]
struct Mbc7Eeprom {
    // Last-written pin levels (readable back through Ax8x).
    do_line: bool,
    di_line: bool,
    clk: bool,
    cs: bool,
    // Set by EWEN, cleared by EWDS. Programming ops are silently dropped
    // while disabled (the power-on state).
    write_enabled: bool,
    state: Mbc7EepromState,
    // Shared input shift register for the Command/Input phases.
    sr: u16,
    sr_n: u8,
    // Latched 10-bit instruction once the Command phase completes.
    command: u16,
    // Latched 16-bit data word once the Input phase completes.
    input: u16,
    // Output shift register for READ.
    out: u16,
    out_n: u8,
}

impl Mbc7Eeprom {
    /// Pin read-back for the Ax8x register: CS<<7 | CLK<<6 | DI<<1 | DO.
    /// Bits 2-5 are not wired to the EEPROM and read 0.
    fn pin_state(&self) -> u8 {
        ((self.cs as u8) << 7)
            | ((self.clk as u8) << 6)
            | ((self.di_line as u8) << 1)
            | (self.do_line as u8)
    }
}

fn serde_u16_8000() -> u16 {
    0x8000
}

fn serde_u8_one() -> u8 {
    1
}

fn serde_cam_regs() -> Vec<u8> {
    vec![0; CAM_REG_COUNT]
}

#[derive(Serialize, Deserialize)]
pub struct Cartridge {
    // ROM data - all banks. Read-only (never mutated after construction) and
    // potentially multi-MB, so it is kept OUT of savestates: serializing it into
    // every rewind-ring snapshot would be fatal. The frontend re-attaches it via
    // `attach_rom` after a state load from the already-resident ROM bytes; every
    // field that derives from it (`rom_banks`, `cartridge_type`, `mbc1_multicart`,
    // `unl_mapper`, `cgb_support`) DOES serialize, so bank math survives the load.
    #[serde(skip, default)]
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

    // MBC7 state. RAM-register access needs a TWO stage unlock: 0x0A to
    // 0x0000-0x1FFF (shared `ram_enabled`) AND exactly 0x40 to 0x4000-0x5FFF.
    #[serde(default)]
    mbc7_ram_enabled2: bool,
    // 8-bit ROM bank register; like MBC5, bank 0 is selectable at 0x4000-0x7FFF.
    #[serde(default = "serde_u8_one")]
    mbc7_rom_bank: u8,
    // Latched accelerometer sample, 16 bits per axis. Reads 0x8000 before the
    // first latch and after an 0x55 erase; a real sample is centered ~0x81D0.
    #[serde(default = "serde_u16_8000")]
    mbc7_accel_x: u16,
    #[serde(default = "serde_u16_8000")]
    mbc7_accel_y: u16,
    // A new 0xAA latch is only accepted after an 0x55 erase (Pan Docs: cannot
    // re-latch without erasing first).
    #[serde(default)]
    mbc7_accel_latched: bool,
    // Live sensor input in g, fed by the frontend via `set_accelerometer`.
    // Not persisted (transient hardware input, like buttons).
    #[serde(skip, default)]
    mbc7_sensor_x: f32,
    #[serde(skip, default)]
    mbc7_sensor_y: f32,
    #[serde(default)]
    mbc7_eeprom: Mbc7Eeprom,

    // HuC-3 state. The 0x0000-0x1FFF register selects what A000-BFFF accesses:
    // 0x0 RAM read-only, 0xA RAM read/write, 0xB RTC command mailbox (write),
    // 0xC RTC command/response (read), 0xD RTC semaphore, 0xE IR.
    #[serde(default)]
    huc3_mode: u8,
    #[serde(default = "serde_u8_one")]
    huc3_rom_bank: u8, // 7-bit; bank 0 selectable like MBC5
    #[serde(default)]
    huc3_ram_bank: u8,
    // RTC MCU mailbox: command (bits 6-4 of the 0xB write) + argument (3-0),
    // executed on a 0xD write with bit 0 clear; result readable through 0xC.
    #[serde(default)]
    huc3_rtc_command: u8,
    #[serde(default)]
    huc3_rtc_argument: u8,
    #[serde(default)]
    huc3_rtc_result: u8,
    // 256-nibble access pointer into the RTC MCU memory.
    #[serde(default)]
    huc3_rtc_address: u8,
    // The RTC MCU's 256-nibble internal memory (one nibble per byte). The live
    // clock is stored in-place: nibbles 0x10-0x12 = minute-of-day counter
    // (rolls at 1440), 0x13-0x15 = 12-bit day counter, little-endian nibbles
    // (Pan Docs "RTC Location Map"). Empty for non-HuC3 carts.
    #[serde(default)]
    huc3_rtc_mem: Vec<u8>,
    // Sub-minute cycle accumulator, master-clock derived like the MBC3 RTC.
    #[serde(default)]
    huc3_rtc_accum: u64,

    // HuC-1 state. RAM is always enabled; the 0x0000-0x1FFF register only
    // selects whether A000-BFFF accesses RAM (default) or the IR transceiver
    // (low nibble == 0xE, SameBoy-verified decode).
    #[serde(default)]
    huc1_ir_mode: bool,
    // 6-bit ROM bank register; bank 0 is selectable at 0x4000-0x7FFF (no
    // MBC1-style zero remap -- SameBoy-verified wiring; the largest HuC-1
    // cart is 1MB = 64 banks).
    #[serde(default = "serde_u8_one")]
    huc1_rom_bank: u8,
    // RAM bank register, "at least 2 bits" (Pan Docs); stored raw and
    // reduced modulo the cart's bank count like HuC-3.
    #[serde(default)]
    huc1_ram_bank: u8,
    // IR LED output latch (bit 0 of writes in IR mode). No IR transport is
    // modeled: reads always see "no light" (0xC0), the documented idle.
    #[serde(default)]
    huc1_ir_led: bool,

    // POCKET CAMERA (MAC-GBD + M64282FP) state.
    // 6-bit ROM bank register; bank 0 is selectable at 4000-7FFF (AntonioND:
    // "This area may contain any ROM bank (0 included)"). Initial bank 1.
    #[serde(default = "serde_u8_one")]
    cam_rom_bank: u8,
    // 4-bit RAM bank register (banks 0-$0F of the 128KB RAM).
    #[serde(default)]
    cam_ram_bank: u8,
    // Bit 4 of the 4000-5FFF write maps the CAM register file over A000-BFFF
    // instead of RAM (the ROM uses bank $10).
    #[serde(default)]
    cam_regs_selected: bool,
    // The 54-byte register file. Write-only except index 0 (trigger/status).
    #[serde(default = "serde_cam_regs")]
    cam_regs: Vec<u8>,
    // Remaining master-clock T-cycles of the in-flight (or stopped) capture.
    #[serde(default)]
    cam_clocks_left: u64,
    // Capture actively running (A000 bit 0 reads 1). Cleared by writing bit
    // 0 = 0 mid-capture (stop) and when the countdown expires.
    #[serde(default)]
    cam_running: bool,
    // Fully processed tile data of the in-flight capture, committed to RAM
    // bank 0 at $0100 when the countdown expires (the real controller
    // streams pixels into RAM during the sensor read period at the end of
    // the window; until then RAM keeps the previous image).
    #[serde(default)]
    cam_pending: Vec<u8>,
    // Live 128x112 8-bit grayscale sensor input, fed by the frontend via
    // `set_camera_image`. Empty => the built-in deterministic test pattern.
    // Not persisted (transient hardware input, like buttons).
    #[serde(skip, default)]
    cam_image: Vec<u8>,

    // Detected unlicensed mapper family (content heuristics; overrides the
    // header type byte in get_cartridge_type).
    #[serde(default)]
    unl_mapper: UnlMapper,

    // Wisdom Tree: 6-bit whole-32KB bank latch, loaded from the ADDRESS of
    // any $0000-$3FFF write.
    #[serde(default)]
    wt_bank: u8,

    // Rocket Games state. rocket_lock/rocket_unlock_count model the
    // A15-transition boot lock (hhugboy): the cart powers up locked and
    // presents the XOR-decoded Nintendo logo during the boot ROM check;
    // skip_bios unlocks immediately (no boot ROM ran). Cell: the counter
    // advances on ROM READS, and Addressable::read takes &self.
    #[serde(default = "serde_u8_one")]
    rocket_rom_bank: u8,
    #[serde(default)]
    rocket_outer: u8,
    #[serde(default)]
    rocket_lock: Cell<u8>,
    #[serde(default)]
    rocket_unlock_count: Cell<u8>,

    // Sachen MMC1/MMC2 state. sachen_bank is the raw inner-bank latch
    // ("unmasked bank"); base/mask writes only latch while
    // (sachen_bank & 0x30) == 0x30. Lock phases as for Rocket.
    #[serde(default)]
    sachen_base: u8,
    #[serde(default)]
    sachen_mask: u8,
    #[serde(default = "serde_u8_one")]
    sachen_bank: u8,
    #[serde(default)]
    sachen_lock: Cell<u8>,
    #[serde(default)]
    sachen_transition: Cell<u8>,

    // NT/Makon "old" board state. nt_bank holds the raw written bank; the
    // $5003 bit-swap is combinational on the bank lines, so it is applied at
    // read time (get_rom_bank), keeping swap-mode flips retroactive exactly
    // like the real wiring (mGBA/hhugboy re-switch on the mode write to
    // emulate the same thing in their push-model maps).
    #[serde(default = "serde_u8_one")]
    nt_bank: u8,
    #[serde(default)]
    nt_base: u8,
    #[serde(default)]
    nt_bank_mask: u8,
    #[serde(default)]
    nt_swapped: bool,

    // M161 (gambatte m161.cpp) one-shot 32KB latch. `m161_bank` is the even
    // 16KB half of the selected 32KB pair ((data & 7) << 1); the odd half is
    // `m161_bank | 1`. `m161_mapped` blocks any further latch until reset.
    #[serde(default)]
    m161_bank: u8,
    #[serde(default)]
    m161_mapped: bool,

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

    // Copy of the bytes last synced into `rtc_memory`, used to detect the
    // frontend writing externally-loaded RTC data into the RETRO_MEMORY_RTC
    // region (RetroArch memcpys its `.rtc` file straight into our buffer
    // after `retro_load_game`; there is no load callback).
    #[serde(skip, default)]
    rtc_memory_synced: Vec<u8>,

    // Open handle for the `.rtc` sidecar on RTC carts (MBC3 timer / HuC-3),
    // attached only on the disk-load path. None => RTC persistence disabled
    // (in-memory test-runner/WASM loads, host-managed frontends), which also
    // guarantees the cycle-derived RTC stays byte-deterministic: no sidecar
    // I/O and no host-clock reads ever happen without this handle.
    #[serde(skip)]
    rtc_file: Option<File>,

    // When true the cartridge will not open or write sidecar `.sav`/`.rtc`
    // files; the host (e.g. RetroArch) owns persistence of the in-memory RAM.
    #[serde(skip, default)]
    host_managed_saves: bool,
    // Physical SRAM chip-select decode of the emulated board for OAM-DMA
    // E000-FFFF sources (gb-ctr: the DMA asserts the external-RAM CS there and
    // "the resulting behaviour depends on the connected cartridge"). Strict
    // boards (default; Gambatte's hwtest fixture: srcE000_readFE00 cgb04c
    // capture reads 0xFF with RAMG on) exclude E000-FDFF, so the bus floats.
    // Lazy boards decode /CS & A13 only (AntonioND's gbc-hw-tests flashcart)
    // and drive SRAM[src & 0x1FFF] there. Set per test fixture via the
    // manifest `cart=lazy_sram_cs` token; not a savestate property.
    #[serde(skip, default)]
    sram_cs_lazy: bool,
}

/// Which per-dot RTC advance a cartridge needs. Cached by the MMIO so the hot
/// `tick_rtc` path avoids recomputing `get_cartridge_type()` every dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RtcTickKind {
    #[default]
    None,
    Mbc3,
    HuC3,
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
            sram_cs_lazy: self.sram_cs_lazy,
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
            mbc7_ram_enabled2: self.mbc7_ram_enabled2,
            mbc7_rom_bank: self.mbc7_rom_bank,
            mbc7_accel_x: self.mbc7_accel_x,
            mbc7_accel_y: self.mbc7_accel_y,
            mbc7_accel_latched: self.mbc7_accel_latched,
            mbc7_sensor_x: self.mbc7_sensor_x,
            mbc7_sensor_y: self.mbc7_sensor_y,
            mbc7_eeprom: self.mbc7_eeprom.clone(),
            huc3_mode: self.huc3_mode,
            huc3_rom_bank: self.huc3_rom_bank,
            huc3_ram_bank: self.huc3_ram_bank,
            huc3_rtc_command: self.huc3_rtc_command,
            huc3_rtc_argument: self.huc3_rtc_argument,
            huc3_rtc_result: self.huc3_rtc_result,
            huc3_rtc_address: self.huc3_rtc_address,
            huc3_rtc_mem: self.huc3_rtc_mem.clone(),
            huc3_rtc_accum: self.huc3_rtc_accum,
            huc1_ir_mode: self.huc1_ir_mode,
            huc1_rom_bank: self.huc1_rom_bank,
            huc1_ram_bank: self.huc1_ram_bank,
            huc1_ir_led: self.huc1_ir_led,
            cam_rom_bank: self.cam_rom_bank,
            cam_ram_bank: self.cam_ram_bank,
            cam_regs_selected: self.cam_regs_selected,
            cam_regs: self.cam_regs.clone(),
            cam_clocks_left: self.cam_clocks_left,
            cam_running: self.cam_running,
            cam_pending: self.cam_pending.clone(),
            cam_image: self.cam_image.clone(),
            unl_mapper: self.unl_mapper,
            wt_bank: self.wt_bank,
            rocket_rom_bank: self.rocket_rom_bank,
            rocket_outer: self.rocket_outer,
            rocket_lock: self.rocket_lock.clone(),
            rocket_unlock_count: self.rocket_unlock_count.clone(),
            sachen_base: self.sachen_base,
            sachen_mask: self.sachen_mask,
            sachen_bank: self.sachen_bank,
            sachen_lock: self.sachen_lock.clone(),
            sachen_transition: self.sachen_transition.clone(),
            nt_bank: self.nt_bank,
            nt_base: self.nt_base,
            nt_bank_mask: self.nt_bank_mask,
            nt_swapped: self.nt_swapped,
            m161_bank: self.m161_bank,
            m161_mapped: self.m161_mapped,
            cgb_support: self.cgb_support.clone(),
            rumble_motor: self.rumble_motor,
            rtc_memory: self.rtc_memory.clone(),
            rtc_memory_synced: self.rtc_memory_synced.clone(),
            rtc_file: None, // Don't clone file handles
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
            // Out-of-spec size byte: the physical chip is what matters, so
            // size purely from the file. Unlicensed carts routinely have
            // garbage here (raw Sachen dumps keep the whole header scrambled;
            // Makon games overlap code with the header), and hhugboy likewise
            // falls back to the file size (detectGbRomSize).
            _ => 0,
        };
        // Number of whole 16KB banks present in the actual file, rounded up to a
        // power of two so the bank-number modulo mask matches the wired address
        // lines.
        let file_banks = data_len.div_ceil(0x4000).next_power_of_two().max(2);
        Ok(header_banks.max(file_banks))
    }

    /// Number of 8KB RAM banks from the header RAM-size byte. Out-of-spec
    /// values are treated as "no RAM" rather than a load failure: unlicensed
    /// carts routinely carry garbage here (Sonic 3D Blast 5 has $20 because
    /// game code overlaps the header), and hhugboy does the same (RAMsize
    /// stays 0 for values > 5).
    fn compute_ram_banks(ram_size_code: u8) -> usize {
        match ram_size_code {
            0x00 => 0,  // No RAM
            0x01 => 1,  // 2KB (partial bank)
            0x02 => 1,  // 8KB = 1 bank
            0x03 => 4,  // 32KB = 4 banks of 8KB
            0x04 => 16, // 128KB = 16 banks of 8KB
            0x05 => 8,  // 64KB = 8 banks of 8KB
            _ => 0,
        }
    }

    /// Physical external-RAM byte size. Header RAM-size $01 is a 2KB chip (a
    /// partial 8KB bank): it decodes only A0-A10, so the chip mirrors 4x across
    /// the $A000-$BFFF window (Pan Docs "No MBC" / the RAM-size table). Sizing
    /// the buffer to the true 2KB makes the existing `offset % ram_data.len()`
    /// in every RAM read/write reproduce that mirror. All other codes are a
    /// whole number of 8KB banks.
    fn compute_ram_len(ram_size_code: u8, ram_banks: usize) -> usize {
        if ram_banks > 0 && ram_size_code == 0x01 {
            0x800
        } else {
            ram_banks * 0x2000
        }
    }

    /// The Sachen MMC address descramble for CPU reads in $0100-$01FF (A8
    /// high, A15..A9 low): RA0<=A6, RA1<=A4, RA4<=A1, RA6<=A0 (bit swaps, so
    /// the mapping is an involution). hhugboy MbcUnlSachenMMC1.cpp; mGBA
    /// _unscrambleSachen.
    fn sachen_unscramble(addr: u16) -> u16 {
        (addr & 0xFFAC)
            | ((addr >> 6) & 0x01)
            | ((addr >> 3) & 0x02)
            | ((addr << 3) & 0x10)
            | ((addr << 6) & 0x40)
    }

    /// Bank-line bit swap used by the NT/Makon and related boards: output bit
    /// i = input bit table[i] (mGBA _reorderBits).
    fn reorder_bits(input: u8, table: &[u8; 8]) -> u8 {
        let mut out = 0;
        for (newbit, &oldbit) in table.iter().enumerate() {
            out |= ((input >> oldbit) & 1) << newbit;
        }
        out
    }

    /// Detect unlicensed mapper families from ROM content. The heuristics are
    /// lifted from the two reference emulators that support these boards
    /// (hhugboy CartDetection::detectUnlCompatMode, mGBA _detectUnlMBC) and
    /// are deliberately narrow so no licensed cart can ever match:
    /// - Sachen/Rocket require the plain Nintendo logo to be ABSENT at $0104
    ///   (every licensed cart has it, or it would not boot on hardware).
    /// - Wisdom Tree requires header type $00 with a >32KB file plus the
    ///   publisher string (a licensed $00 cart is 32KB by definition), or the
    ///   Pan Docs $C0/$D1 header magic.
    /// - The NT/Makon and ForceMbc1 title rules replicate hhugboy's exact
    ///   title/licensee/size shapes.
    fn detect_unl_mapper(data: &[u8]) -> UnlMapper {
        if data.len() < 0x8000 {
            // Smaller than one full 32KB image: nothing here needs (or can
            // safely take) an unlicensed mapper.
            return UnlMapper::None;
        }

        // M161 (Mani 4 in 1): the header spoofs MBC3+RAM+BATTERY ($10), so
        // gambatte (presumedM161) gates on the exact shape of the one known
        // cart -- a 256KB image whose title is "TETRIS SET". The title check
        // is specific enough that no real MBC3 cart can match.
        if data.len() == 16 * 0x4000
            && data[CARTRIDGE_TYPE_OFFSET] == 0x10
            && &data[0x134..0x13E] == b"TETRIS SET"
        {
            return UnlMapper::M161;
        }

        let logo_sum = |base: usize, scrambled: bool| -> u32 {
            (0..0x30)
                .map(|i| {
                    let a = base + i;
                    let a = if scrambled {
                        Self::sachen_unscramble(a as u16) as usize
                    } else {
                        a
                    };
                    data.get(a).copied().unwrap_or(0) as u32
                })
                .sum()
        };
        let plain_0104 = logo_sum(0x104, false);
        let scrambled_0104 = logo_sum(0x104, true);
        let scrambled_0184 = logo_sum(0x184, true);

        if plain_0104 != LOGO_SUM_NINTENDO {
            // Sachen MMC raw dumps: the Nintendo logo only exists at the
            // scrambled addresses (MMC1 at $01xx, MMC2 at the |0x80 copy),
            // with the Sachen logo at the other bank. Match on either logo
            // (hhugboy uses the Sachen sums, mGBA the Nintendo bytes).
            let sachen_a = |s: u32| s == LOGO_SUM_SACHEN_A || s == LOGO_SUM_SACHEN_B;
            if scrambled_0104 == LOGO_SUM_NINTENDO || sachen_a(scrambled_0184) {
                return UnlMapper::SachenMmc1;
            }
            if scrambled_0184 == LOGO_SUM_NINTENDO || sachen_a(scrambled_0104) {
                return UnlMapper::SachenMmc2;
            }
            // Rocket Games logo (hhugboy: checksum 2756; all $97/$99 carts).
            if plain_0104 == LOGO_SUM_ROCKET {
                return UnlMapper::Rocket;
            }
        }

        // hhugboy strcmp semantics on the 15-byte title at $0134-$0142.
        let title = &data[0x134..0x143];
        let title_eq = |s: &[u8]| -> bool {
            s.len() <= title.len()
                && &title[..s.len()] == s
                && title[s.len()..].first().is_none_or(|&b| b == 0)
        };
        let title_contains =
            |s: &[u8]| -> bool { title.windows(s.len()).any(|w| w == s) };
        let newlic_mk = &data[0x144..0x146] == b"MK";
        let rom_size_code = data[ROM_SIZE_OFFSET];

        // NT/Makon older boards (hhugboy detectUnlCompatMode):
        // multicarts with the Pocket Bomberman / Trump Boy / Q Billion menus,
        // the NT Rockman 99 single, and the early Makon GBC singles (Makon
        // "MK" licensee + known title + untouched 256KB header).
        if title_eq(b"POKEBOM USA") && data.len() > 512 * 1024 {
            if data[0x102] == 0xE0 {
                return UnlMapper::NtOld2; // 23-in-1 with Mario
            }
            if data[0x102] == 0xC0 {
                return UnlMapper::NtOld1; // 25-in-1 with Rockman
            }
        }
        if (title_eq(b" - TRUMP  BOY -") || title_eq(b"QBILLION")) && data.len() > 512 * 1024 {
            return UnlMapper::NtOld2;
        }
        if title_eq(b"ROCKMAN 99")
            && !newlic_mk
            && data.get(0x8001).is_some_and(|&b| b != 0xB7)
        {
            return UnlMapper::NtOld1;
        }
        if newlic_mk
            && (title_eq(b"SONIC 7")
                || title_eq(b"SUPER MARIO 3")
                || title_eq(b"DONKEY\tKONG 5")
                || title_eq(b"ROCKMAN 99"))
            && rom_size_code == 0x03
        {
            return UnlMapper::NtOld2;
        }

        // Electrically-plain-MBC1 header liars (hhugboy UNL_MBC1NOSAVE):
        // Sonic 3D Blast 5 / Super Donkey Kong 3 (type $EA is header-overlap
        // garbage), Captain Knick-Knack (Sachen dump wearing a Tetris header;
        // real Tetris is exactly 32KB so the size gate excludes it), and the
        // 256KB Pocket Monsters GO!GO!GO! dumps.
        if title_contains(b"SONIC5") {
            return UnlMapper::ForceMbc1;
        }
        if title_eq(b"TETRIS") && data.len() > 0x8000 && rom_size_code == 0 {
            return UnlMapper::ForceMbc1;
        }
        if title_eq(b"POCKET MONSTER") && rom_size_code == 0x03 {
            return UnlMapper::ForceMbc1;
        }

        // Wisdom Tree: the Pan Docs $C0-type/$D1 magic, or (type $00 with a
        // banked-size file) the publisher string in the ROM. Both variants
        // from hhugboy; mGBA additionally requires the blank-header shape,
        // which the string+type+size gate already implies in practice.
        if data[0x147] == 0xC0 && data[0x14A] == 0xD1 {
            return UnlMapper::WisdomTree;
        }
        if data[0x147] == 0x00
            && (data.windows(11).any(|w| w == b"WISDOM TREE")
                || data.windows(11).any(|w| w == b"WISDOM\0TREE"))
        {
            return UnlMapper::WisdomTree;
        }

        UnlMapper::None
    }

    /// The detected unlicensed mapper family (None for licensed carts).
    pub fn unl_mapper(&self) -> UnlMapper {
        self.unl_mapper
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
        let ram_banks = Self::compute_ram_banks(ram_size_code);

        // Detect unlicensed mapper families (header-spoofing boards) from ROM
        // content. Must run on the raw file image, before padding.
        let unl_mapper = Self::detect_unl_mapper(&data);

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

        // Initialize RAM data. MBC7 carts declare RAM size 0x00 in the header;
        // their "save RAM" is the 93LC56 EEPROM: 256 bytes = 128 little-endian
        // 16-bit words, erased state 0xFF. Routing it through ram_data reuses
        // the whole battery-save path (LE word order matches SameBoy/mGBA .sav
        // files). ForceMbc1 header-liars carry garbage RAM-size bytes; hhugboy
        // (UNL_MBC1NOSAVE) forces RAM off for them.
        let ram_banks = if unl_mapper == UnlMapper::ForceMbc1 { 0 } else { ram_banks };
        let ram_data = if cartridge_type == MBC7_SENSOR_RUMBLE_RAM_BATTERY {
            vec![0xFF; 256]
        } else {
            vec![0xFF; Self::compute_ram_len(ram_size_code, ram_banks)]
        };

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
            sram_cs_lazy: false,
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
            mbc7_ram_enabled2: false,
            mbc7_rom_bank: 1,
            mbc7_accel_x: 0x8000,
            mbc7_accel_y: 0x8000,
            mbc7_accel_latched: false,
            mbc7_sensor_x: 0.0,
            mbc7_sensor_y: 0.0,
            mbc7_eeprom: Mbc7Eeprom::default(),
            huc3_mode: 0,
            huc3_rom_bank: 1,
            huc3_ram_bank: 0,
            huc3_rtc_command: 0,
            huc3_rtc_argument: 0,
            huc3_rtc_result: 0,
            huc3_rtc_address: 0,
            huc3_rtc_mem: if cartridge_type == HUC3 { vec![0; 256] } else { Vec::new() },
            huc3_rtc_accum: 0,
            huc1_ir_mode: false,
            huc1_rom_bank: 1,
            huc1_ram_bank: 0,
            huc1_ir_led: false,
            cam_rom_bank: 1,
            cam_ram_bank: 0,
            cam_regs_selected: false,
            cam_regs: serde_cam_regs(),
            cam_clocks_left: 0,
            cam_running: false,
            cam_pending: Vec::new(),
            cam_image: Vec::new(),
            unl_mapper,
            wt_bank: 0,
            rocket_rom_bank: 1,
            rocket_outer: 0,
            rocket_lock: Cell::new(UNL_LOCKED_DMG),
            rocket_unlock_count: Cell::new(0),
            sachen_base: 0,
            sachen_mask: 0,
            sachen_bank: 1,
            sachen_lock: Cell::new(UNL_LOCKED_DMG),
            sachen_transition: Cell::new(0),
            nt_bank: 1,
            nt_base: 0,
            nt_bank_mask: 0,
            nt_swapped: false,
            m161_bank: 0,
            m161_mapped: false,
            cgb_support,
            rumble_motor: false,
            rtc_memory: Vec::new(),
            rtc_memory_synced: Vec::new(),
            rtc_file: None,
            host_managed_saves: false,
        };

        // Try to load existing save file or create new one (only for battery-backed RAM)
        cartridge.load_or_create_save_file()?;
        // Restore the persisted RTC (with wall-clock catch-up) and attach the
        // `.rtc` sidecar. Disk-load path only; in-memory loads skip this.
        cartridge.attach_rtc_sidecar()?;

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
        let ram_banks = Self::compute_ram_banks(ram_size_code);

        // Detect unlicensed mapper families (header-spoofing boards).
        let unl_mapper = Self::detect_unl_mapper(&actual_data);

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

        // Initialize RAM data (MBC7: 256-byte 93LC56 EEPROM, see `load`;
        // ForceMbc1 header-liars get no RAM, see `load`).
        let ram_banks = if unl_mapper == UnlMapper::ForceMbc1 { 0 } else { ram_banks };
        let ram_data = if cartridge_type == MBC7_SENSOR_RUMBLE_RAM_BATTERY {
            vec![0xFF; 256]
        } else {
            vec![0xFF; Self::compute_ram_len(ram_size_code, ram_banks)]
        };

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
            sram_cs_lazy: false,
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
            mbc7_ram_enabled2: false,
            mbc7_rom_bank: 1,
            mbc7_accel_x: 0x8000,
            mbc7_accel_y: 0x8000,
            mbc7_accel_latched: false,
            mbc7_sensor_x: 0.0,
            mbc7_sensor_y: 0.0,
            mbc7_eeprom: Mbc7Eeprom::default(),
            huc3_mode: 0,
            huc3_rom_bank: 1,
            huc3_ram_bank: 0,
            huc3_rtc_command: 0,
            huc3_rtc_argument: 0,
            huc3_rtc_result: 0,
            huc3_rtc_address: 0,
            huc3_rtc_mem: if cartridge_type == HUC3 { vec![0; 256] } else { Vec::new() },
            huc3_rtc_accum: 0,
            huc1_ir_mode: false,
            huc1_rom_bank: 1,
            huc1_ram_bank: 0,
            huc1_ir_led: false,
            cam_rom_bank: 1,
            cam_ram_bank: 0,
            cam_regs_selected: false,
            cam_regs: serde_cam_regs(),
            cam_clocks_left: 0,
            cam_running: false,
            cam_pending: Vec::new(),
            cam_image: Vec::new(),
            unl_mapper,
            wt_bank: 0,
            rocket_rom_bank: 1,
            rocket_outer: 0,
            rocket_lock: Cell::new(UNL_LOCKED_DMG),
            rocket_unlock_count: Cell::new(0),
            sachen_base: 0,
            sachen_mask: 0,
            sachen_bank: 1,
            sachen_lock: Cell::new(UNL_LOCKED_DMG),
            sachen_transition: Cell::new(0),
            nt_bank: 1,
            nt_base: 0,
            nt_bank_mask: 0,
            nt_swapped: false,
            m161_bank: 0,
            m161_mapped: false,
            cgb_support,
            rumble_motor: false,
            rtc_memory: Vec::new(),
            rtc_memory_synced: Vec::new(),
            rtc_file: None,
            host_managed_saves: false,
        };

        // In-memory loading intentionally skips save files so test runners and
        // WASM callers do not create sidecar files. This also skips the `.rtc`
        // sidecar + wall-clock catch-up: the RTC starts at zero and advances
        // only on the deterministic cycle-derived tick.

        Ok(cartridge)
    }

    /// Clone the raw ROM image (all banks, already padded to `rom_banks`) out of
    /// a live cartridge so it can be re-attached to a savestate-restored one. The
    /// ROM is `#[serde(skip)]`, so this is how a load path carries the ROM across.
    pub fn detach_rom(&self) -> Vec<u8> {
        self.rom_data.clone()
    }

    /// Re-attach the ROM image after a savestate load (where `rom_data` was
    /// skipped). Pads/truncates to the serialized `rom_banks * 0x4000` exactly as
    /// the constructors do, so the already-restored bank registers index the same
    /// bytes. All other runtime state (RAM, bank regs, RTC) is already present
    /// from deserialize; this only refills the read-only ROM.
    pub fn attach_rom(&mut self, rom: Vec<u8>) {
        let expected = self.rom_banks * 0x4000;
        self.rom_data = if rom.len() >= expected {
            rom[..expected].to_vec()
        } else {
            let mut padded = rom;
            padded.resize(expected, 0xFF);
            padded
        };
    }

    /// Whether the ROM image is currently attached (present after construction or
    /// `attach_rom`; empty right after a savestate deserialize).
    pub fn has_rom(&self) -> bool {
        !self.rom_data.is_empty()
    }

    fn get_cartridge_type(&self) -> CartridgeType {
        // Content-detected unlicensed boards override the (spoofed) header
        // type byte.
        match self.unl_mapper {
            UnlMapper::None => {}
            UnlMapper::WisdomTree => return CartridgeType::WisdomTree,
            UnlMapper::Rocket => return CartridgeType::Rocket,
            UnlMapper::SachenMmc1 => return CartridgeType::Sachen { mmc2: false },
            UnlMapper::SachenMmc2 => return CartridgeType::Sachen { mmc2: true },
            UnlMapper::NtOld1 => return CartridgeType::NtOld { v2: false },
            UnlMapper::NtOld2 => return CartridgeType::NtOld { v2: true },
            UnlMapper::ForceMbc1 => {
                return CartridgeType::MBC1 { ram: false, battery: false }
            }
            UnlMapper::M161 => return CartridgeType::M161,
        }
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
            MBC5 => CartridgeType::MBC5 { ram: false, battery: false, rumble: false },
            MBC5_RAM => CartridgeType::MBC5 { ram: true, battery: false, rumble: false },
            MBC5_RAM_BATTERY => CartridgeType::MBC5 { ram: true, battery: true, rumble: false },
            MBC5_RUMBLE => CartridgeType::MBC5 { ram: false, battery: false, rumble: true },
            MBC5_RUMBLE_RAM => CartridgeType::MBC5 { ram: true, battery: false, rumble: true },
            MBC5_RUMBLE_RAM_BATTERY => CartridgeType::MBC5 { ram: true, battery: true, rumble: true },
            MBC7_SENSOR_RUMBLE_RAM_BATTERY => CartridgeType::MBC7,
            HUC1_RAM_BATTERY => CartridgeType::HuC1,
            HUC3 => CartridgeType::HuC3,
            POCKET_CAMERA => CartridgeType::PocketCamera,
            // Bankless carts: RAM presence comes from the header RAM-size
            // byte, so $00 ROM ONLY and $08 ROM+RAM decode identically; $09
            // adds the battery. Unknown/unimplemented types fall through to
            // NoMBC too.
            ROM_RAM => CartridgeType::NoMBC { battery: false },
            ROM_RAM_BATTERY => CartridgeType::NoMBC { battery: true },
            _ => CartridgeType::NoMBC { battery: false },
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
            CartridgeType::MBC7 => {
                // 8-bit register; like MBC5 bank 0 is selectable here.
                (self.mbc7_rom_bank as usize) % self.rom_banks
            }
            CartridgeType::HuC1 => {
                // 6-bit register; bank 0 is selectable here (no zero remap).
                (self.huc1_rom_bank as usize) % self.rom_banks
            }
            CartridgeType::HuC3 => {
                // 7-bit register; like MBC5 bank 0 is selectable here.
                (self.huc3_rom_bank as usize) % self.rom_banks
            }
            CartridgeType::PocketCamera => {
                // 6-bit register; bank 0 is selectable here (AntonioND: "may
                // contain any ROM bank (0 included)"; mGBA _GBPocketCam).
                (self.cam_rom_bank as usize) % self.rom_banks
            }
            CartridgeType::WisdomTree => {
                // Whole-$0000-$7FFF 32KB banking: this half is the odd 16KB
                // bank of the selected 32KB pair.
                (self.wt_bank as usize * 2 + 1) % self.rom_banks
            }
            CartridgeType::Rocket => {
                // Outer 256KB bank (high nibble) | 16KB inner bank (hhugboy:
                // (outerBank | rom_bank) << 14).
                (((self.rocket_outer as usize & 0x0F) << 4)
                    | (self.rocket_rom_bank as usize))
                    % self.rom_banks
            }
            CartridgeType::Sachen { .. } => {
                // Masked outer/inner combination: mask bits come from the
                // base register, the rest from the inner bank register
                // (hhugboy: outerBank&outerMask | rom_bank&~outerMask).
                (((self.sachen_bank & !self.sachen_mask)
                    | (self.sachen_base & self.sachen_mask)) as usize)
                    % self.rom_banks
            }
            CartridgeType::NtOld { v2 } => {
                // The $5003 bit-swap is combinational on the bank lines; the
                // $5002 bank-count mask and the $5001 multicart base (32KB
                // units = 2 x 16KB banks) apply after it (mGBA _GBNTOld1/2).
                let mut bank = self.nt_bank;
                if self.nt_swapped {
                    bank = Self::reorder_bits(
                        bank,
                        if v2 { &NT_OLD2_REORDER } else { &NT_OLD1_REORDER },
                    );
                }
                if self.nt_bank_mask != 0 {
                    bank &= self.nt_bank_mask;
                }
                (bank as usize + self.nt_base as usize * 2) % self.rom_banks
            }
            CartridgeType::M161 => {
                // Odd 16KB half of the latched 32KB pair (gambatte
                // setRombank: (rombank_ | 1) & (rombanks - 1)).
                ((self.m161_bank as usize) | 1) & (self.rom_banks - 1)
            }
            CartridgeType::NoMBC { .. } => 1, // Simple cartridge always uses bank 1 for upper area
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
            CartridgeType::MBC7 => 0, // no banked RAM (EEPROM is serial)
            CartridgeType::M161 => 0, // no external RAM (disabledRam)
            CartridgeType::HuC1 => {
                // "At least 2 bits" per Pan Docs; the real cart has 4 banks.
                (self.huc1_ram_bank as usize) % self.ram_banks.max(1)
            }
            CartridgeType::HuC3 => {
                // "At least 2 bits" per Pan Docs; real carts have 4 banks.
                (self.huc3_ram_bank as usize) % self.ram_banks.max(1)
            }
            CartridgeType::PocketCamera => {
                // 4-bit register, 16 banks of the 128KB RAM.
                (self.cam_ram_bank as usize) % self.ram_banks.max(1)
            }
            // None of the unlicensed boards bank their (optional) RAM.
            CartridgeType::WisdomTree
            | CartridgeType::Rocket
            | CartridgeType::Sachen { .. }
            | CartridgeType::NtOld { .. } => 0,
            CartridgeType::NoMBC { .. } => 0,
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
            // Even 16KB half of the selected whole-32KB bank.
            CartridgeType::WisdomTree => (self.wt_bank as usize * 2) % self.rom_banks,
            // Outer bank alone (hhugboy: (outerBank | 0) << 14).
            CartridgeType::Rocket => {
                ((self.rocket_outer as usize & 0x0F) << 4) % self.rom_banks
            }
            // Masked base bank (hhugboy: outerBank & outerMask).
            CartridgeType::Sachen { .. } => {
                ((self.sachen_base & self.sachen_mask) as usize) % self.rom_banks
            }
            // Multicart base (32KB units).
            CartridgeType::NtOld { .. } => (self.nt_base as usize * 2) % self.rom_banks,
            // Even 16KB half of the latched 32KB pair (gambatte setRombank:
            // rombank_ & (rombanks - 2)).
            CartridgeType::M161 => (self.m161_bank as usize) & (self.rom_banks - 2),
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
                    // A file larger than the cart RAM is normal: mGBA / VBA-M /
                    // BGB append an RTC footer to the `.sav` (read separately by
                    // `read_sav_rtc_footer`). Load the RAM-sized prefix.
                    let n = loaded_data.len().min(self.ram_data.len());
                    self.ram_data[..n].copy_from_slice(&loaded_data[..n]);
                    println!("Loaded save file: {}", save_path.display());
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
            // MBC7's EEPROM is inherently non-volatile; HuC-3 ($FE) implies
            // RAM+BATTERY+RTC, HuC-1 ($FF) implies RAM+BATTERY, and POCKET
            // CAMERA ($FC) implies RAM+BATTERY (the photo album).
            CartridgeType::MBC7
            | CartridgeType::HuC1
            | CartridgeType::HuC3
            | CartridgeType::PocketCamera => true,
            // No known cart on these unlicensed boards has battery-backed RAM.
            CartridgeType::WisdomTree
            | CartridgeType::Rocket
            | CartridgeType::Sachen { .. }
            | CartridgeType::NtOld { .. }
            // M161's RAM line is permanently disabled (gambatte disabledRam);
            // gambatte also zeroes its header type so it never saves.
            | CartridgeType::M161 => false,
            // $09 ROM+RAM+BATTERY; plain $00/$08 (and unknown fallthroughs)
            // have none.
            CartridgeType::NoMBC { battery } => battery,
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

    /// True when the header declares Super Game Boy support: SGB flag
    /// ($0146) == $03 AND old licensee ($014B) == $33 (Pan Docs "SGB
    /// Unlocking"). The SGB system software only honors command packets from
    /// such carts.
    pub fn supports_sgb(&self) -> bool {
        self.rom_data.get(0x0146).copied() == Some(0x03)
            && self.rom_data.get(0x014B).copied() == Some(0x33)
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
        // Persist software clock-sets / HALT toggles immediately.
        self.flush_rtc_file();
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
        // Keep the persisted latched shadows fresh: mGBA reconstructs the
        // clock from the blob's LATCHED fields + timestamp, so they matter
        // for cross-emulator reads. No-op without a sidecar.
        self.flush_rtc_file();
    }

    /// Advance the cycle-derived RTC by `cycles` master (dot) clock T-cycles.
    /// Driven from the bus tick loop (`master_cc` advances at 4.194304 MHz
    /// regardless of CPU speed), so the clock is fully deterministic. No-op
    /// unless this cart actually has an RTC (MBC3 timer or HuC-3). For MBC3
    /// the HALT bit (bit 6 of days_high) freezes advancement but the
    /// sub-second accumulator keeps running so the halt/resume boundary lands
    /// on an exact second, matching hardware.
    pub fn rtc_tick(&mut self, cycles: u64, kind: RtcTickKind) {
        if cycles == 0 {
            return;
        }
        match kind {
            RtcTickKind::Mbc3 => {
                // HALT bit frozen: the crystal still oscillates but the counters
                // do not advance. Do not accumulate while halted so no seconds
                // are "banked".
                if self.rtc_days_high & 0x40 != 0 {
                    return;
                }
                self.rtc_cycle_accum = self.rtc_cycle_accum.wrapping_add(cycles);
                const CYCLES_PER_SECOND: u64 = 4_194_304;
                let mut advanced = false;
                while self.rtc_cycle_accum >= CYCLES_PER_SECOND {
                    self.rtc_cycle_accum -= CYCLES_PER_SECOND;
                    self.advance_rtc_second();
                    advanced = true;
                }
                if advanced {
                    // Stream the advanced clock to the `.rtc` sidecar (no-op
                    // without one, keeping the test path I/O- and
                    // wall-clock-free).
                    self.flush_rtc_file();
                }
            }
            RtcTickKind::HuC3 => {
                // The HuC-3 clock counts whole minutes: minute-of-day rolls at
                // 1440 into a 12-bit day counter (Pan Docs RTC location map).
                self.huc3_rtc_accum = self.huc3_rtc_accum.wrapping_add(cycles);
                const CYCLES_PER_MINUTE: u64 = 60 * 4_194_304;
                let mut advanced = false;
                while self.huc3_rtc_accum >= CYCLES_PER_MINUTE {
                    self.huc3_rtc_accum -= CYCLES_PER_MINUTE;
                    let (mut minutes, mut days) = self.huc3_clock();
                    minutes += 1;
                    if minutes >= 1440 {
                        minutes = 0;
                        days = (days + 1) & 0x0FFF;
                    }
                    self.huc3_set_clock(minutes, days);
                    advanced = true;
                }
                if advanced {
                    self.flush_rtc_file();
                }
            }
            RtcTickKind::None => {}
        }
    }

    /// Live HuC-3 clock (minute-of-day, day counter) read from its nibble
    /// locations 0x10-0x12 / 0x13-0x15 in the RTC MCU memory.
    fn huc3_clock(&self) -> (u16, u16) {
        if self.huc3_rtc_mem.len() < 0x16 {
            return (0, 0);
        }
        let m = &self.huc3_rtc_mem;
        let minutes = (m[0x10] as u16 & 0xF) | ((m[0x11] as u16 & 0xF) << 4) | ((m[0x12] as u16 & 0xF) << 8);
        let days = (m[0x13] as u16 & 0xF) | ((m[0x14] as u16 & 0xF) << 4) | ((m[0x15] as u16 & 0xF) << 8);
        (minutes, days)
    }

    fn huc3_set_clock(&mut self, minutes: u16, days: u16) {
        if self.huc3_rtc_mem.len() < 0x16 {
            return;
        }
        let m = &mut self.huc3_rtc_mem;
        m[0x10] = (minutes & 0xF) as u8;
        m[0x11] = ((minutes >> 4) & 0xF) as u8;
        m[0x12] = ((minutes >> 8) & 0xF) as u8;
        m[0x13] = (days & 0xF) as u8;
        m[0x14] = ((days >> 4) & 0xF) as u8;
        m[0x15] = ((days >> 8) & 0xF) as u8;
    }

    /// Event ("alarm") time as total minutes, from nibbles 0x58-0x5A (minutes)
    /// and 0x5B-0x5D (days).
    fn huc3_event_total_minutes(&self) -> i64 {
        let m = &self.huc3_rtc_mem;
        let minutes =
            (m[0x58] as i64 & 0xF) | ((m[0x59] as i64 & 0xF) << 4) | ((m[0x5A] as i64 & 0xF) << 8);
        let days =
            (m[0x5B] as i64 & 0xF) | ((m[0x5C] as i64 & 0xF) << 4) | ((m[0x5D] as i64 & 0xF) << 8);
        days * 1440 + minutes
    }

    fn huc3_set_event_total_minutes(&mut self, total: i64) {
        // 12-bit day counter x 1440 minutes wraps the representable range.
        let total = total.rem_euclid(4096 * 1440);
        let minutes = (total % 1440) as u16;
        let days = (total / 1440) as u16;
        let m = &mut self.huc3_rtc_mem;
        m[0x58] = (minutes & 0xF) as u8;
        m[0x59] = ((minutes >> 4) & 0xF) as u8;
        m[0x5A] = ((minutes >> 8) & 0xF) as u8;
        m[0x5B] = (days & 0xF) as u8;
        m[0x5C] = ((days >> 4) & 0xF) as u8;
        m[0x5D] = ((days >> 8) & 0xF) as u8;
    }

    /// Execute the pending HuC-3 RTC MCU command (mailbox command+argument,
    /// triggered by a semaphore write with bit 0 clear). The MCU is modeled as
    /// always-ready / instant execution; the semaphore therefore always reads
    /// "ready". Command set per Pan Docs "RTC Communication Protocol".
    fn huc3_execute_command(&mut self) {
        if self.huc3_rtc_mem.len() < 0x100 {
            return;
        }
        let addr = self.huc3_rtc_address as usize;
        match self.huc3_rtc_command {
            0x1 => {
                // Read value and increment access address.
                self.huc3_rtc_result = self.huc3_rtc_mem[addr] & 0x0F;
                self.huc3_rtc_address = self.huc3_rtc_address.wrapping_add(1);
            }
            0x3 => {
                // Write value and increment access address.
                self.huc3_rtc_mem[addr] = self.huc3_rtc_argument & 0x0F;
                self.huc3_rtc_address = self.huc3_rtc_address.wrapping_add(1);
            }
            0x4 => {
                // Set access address least significant nibble.
                self.huc3_rtc_address = (self.huc3_rtc_address & 0xF0) | self.huc3_rtc_argument;
            }
            0x5 => {
                // Set access address most significant nibble.
                self.huc3_rtc_address =
                    (self.huc3_rtc_address & 0x0F) | (self.huc3_rtc_argument << 4);
            }
            0x6 => {
                // Extended command in the argument nibble.
                match self.huc3_rtc_argument {
                    0x0 => {
                        // Copy current time (0x10-0x16) to I/O space 0x00-0x06.
                        // Pan Docs specifies "locations $00-06": 7 nibbles.
                        for i in 0..7 {
                            self.huc3_rtc_mem[i] = self.huc3_rtc_mem[0x10 + i] & 0x0F;
                        }
                    }
                    0x1 => {
                        // Copy I/O space 0x00-0x06 to current time, and shift
                        // the event time by the same delta so the remaining
                        // duration until the event is preserved (Pan Docs).
                        let (old_min, old_day) = self.huc3_clock();
                        for i in 0..7 {
                            self.huc3_rtc_mem[0x10 + i] = self.huc3_rtc_mem[i] & 0x0F;
                        }
                        let (new_min, new_day) = self.huc3_clock();
                        let delta = (new_day as i64 * 1440 + new_min as i64)
                            - (old_day as i64 * 1440 + old_min as i64);
                        let event = self.huc3_event_total_minutes();
                        self.huc3_set_event_total_minutes(event + delta);
                        // Setting the time restarts the current minute.
                        self.huc3_rtc_accum = 0;
                    }
                    0x2 => {
                        // Status request issued by games on boot; they refuse
                        // to start unless the response is 1 (Pan Docs).
                    }
                    0xE => {
                        // Tone generator trigger. The piezo speaker is not
                        // modeled; accept and ignore.
                    }
                    _ => {}
                }
                // Hardware-observed: extended commands leave 1 in the response
                // nibble (this is what boot-time $62 status checks rely on).
                self.huc3_rtc_result = 0x1;
            }
            // Commands $0, $2 and $7 are unobserved/unknown on hardware
            // (Pan Docs); treat as no-ops.
            _ => {}
        }
        // Commands can rewrite the clock/event nibbles; persist immediately.
        self.flush_rtc_file();
    }

    /// Feed the MBC7 accelerometer with a live tilt sample, in units of g
    /// (Earth gravity). Neutral (flat) is (0, 0); positive x tilts left,
    /// positive y tilts up, matching Pan Docs' "lower values are towards the
    /// right / bottom". The value is only observed by software when it latches
    /// a sample via the Ax0x/Ax1x erase+latch protocol. No-op storage for
    /// non-MBC7 carts.
    ///
    /// This is the sole input hook for MBC7 tilt (parallel to `set_camera_image`
    /// for the GB Camera); it is the intended path for a frontend to drive the
    /// accelerometer and is awaiting frontend wiring, so it is currently unused.
    #[allow(dead_code)]
    pub fn set_accelerometer(&mut self, x_g: f32, y_g: f32) {
        self.mbc7_sensor_x = x_g;
        self.mbc7_sensor_y = y_g;
    }

    /// Convert a sensor reading in g to the latched 16-bit accelerometer
    /// value: centered at 0x81D0, 1 g ~ 0x70 counts (Pan Docs).
    fn mbc7_accel_counts(g: f32) -> u16 {
        let v = 0x81D0_i32 + (g * 0x70 as f32) as i32;
        v.clamp(0, 0xFFFF) as u16
    }

    /// One 16-bit word of the MBC7 EEPROM (128 little-endian words backed by
    /// `ram_data`).
    fn mbc7_eeprom_word(&self, addr: usize) -> u16 {
        let i = (addr & 0x7F) * 2;
        (self.ram_data[i] as u16) | ((self.ram_data[i + 1] as u16) << 8)
    }

    fn mbc7_eeprom_set_word(&mut self, addr: usize, word: u16) {
        let i = (addr & 0x7F) * 2;
        // write_ram_byte streams to the battery save file as well.
        let _ = self.write_ram_byte(i, (word & 0xFF) as u8);
        let _ = self.write_ram_byte(i + 1, (word >> 8) as u8);
    }

    /// Bit-banged 93LC56 write via the Ax8x register: bit 0 = DO (ignored on
    /// write), bit 1 = DI, bit 6 = CLK, bit 7 = CS. Commands are 1 start bit
    /// followed by 10 instruction bits, shifted MSB-first on rising CLK edges
    /// while CS is high (leading 0 bits before the start bit are ignored):
    ///
    /// ```text
    /// READ  10xAAAAAAA (then 16 bits out)   EWEN 0011xxxxxx
    /// WRITE 01xAAAAAAA (then 16 bits in)    EWDS 0000xxxxxx
    /// ERASE 11xAAAAAAA                      ERAL 0010xxxxxx
    /// WRAL  0001xxxxxx (then 16 bits in)
    /// ```
    ///
    /// Programming ops (WRITE/ERASE/WRAL/ERAL) execute on the CS falling edge
    /// that follows the last bit, require a prior EWEN, and are modeled as
    /// completing instantly: DO then reads 1 (RDY) for the software
    /// busy-poll.
    fn mbc7_eeprom_write(&mut self, value: u8) {
        let di = value & 0x02 != 0;
        let clk = value & 0x40 != 0;
        let cs = value & 0x80 != 0;
        let rising_clk = clk && !self.mbc7_eeprom.clk;
        let falling_cs = !cs && self.mbc7_eeprom.cs;

        if rising_clk && cs {
            match self.mbc7_eeprom.state {
                Mbc7EepromState::Idle => {
                    if di {
                        // Start bit.
                        self.mbc7_eeprom.state = Mbc7EepromState::Command;
                        self.mbc7_eeprom.sr = 0;
                        self.mbc7_eeprom.sr_n = 0;
                    }
                }
                Mbc7EepromState::Command => {
                    self.mbc7_eeprom.sr = (self.mbc7_eeprom.sr << 1) | di as u16;
                    self.mbc7_eeprom.sr_n += 1;
                    if self.mbc7_eeprom.sr_n == 10 {
                        self.mbc7_eeprom_decode();
                    }
                }
                Mbc7EepromState::Input => {
                    self.mbc7_eeprom.sr = (self.mbc7_eeprom.sr << 1) | di as u16;
                    self.mbc7_eeprom.sr_n += 1;
                    if self.mbc7_eeprom.sr_n == 16 {
                        self.mbc7_eeprom.input = self.mbc7_eeprom.sr;
                        self.mbc7_eeprom.state = Mbc7EepromState::Pending;
                    }
                }
                Mbc7EepromState::Output => {
                    self.mbc7_eeprom.do_line = self.mbc7_eeprom.out & 0x8000 != 0;
                    self.mbc7_eeprom.out <<= 1;
                    self.mbc7_eeprom.out_n += 1;
                    if self.mbc7_eeprom.out_n == 16 {
                        self.mbc7_eeprom.state = Mbc7EepromState::Done;
                    }
                }
                Mbc7EepromState::Pending | Mbc7EepromState::Done => {}
            }
        }

        if falling_cs {
            if self.mbc7_eeprom.state == Mbc7EepromState::Pending {
                self.mbc7_eeprom_program();
            }
            // Any in-flight instruction is aborted by deselecting the chip.
            self.mbc7_eeprom.state = Mbc7EepromState::Idle;
        }

        self.mbc7_eeprom.di_line = di;
        self.mbc7_eeprom.clk = clk;
        self.mbc7_eeprom.cs = cs;
    }

    /// Decode a completed 10-bit instruction. The top 4 bits identify the
    /// operation; the low 7 bits are the word address for READ/WRITE/ERASE.
    fn mbc7_eeprom_decode(&mut self) {
        let cmd = self.mbc7_eeprom.sr & 0x03FF;
        self.mbc7_eeprom.command = cmd;
        match (cmd >> 6) & 0xF {
            0b1000..=0b1011 => {
                // READ: present the word MSB-first on subsequent rising edges.
                // DO drops to 0 immediately (the datasheet's dummy zero bit,
                // which does not consume a clock).
                self.mbc7_eeprom.out = self.mbc7_eeprom_word((cmd & 0x7F) as usize);
                self.mbc7_eeprom.out_n = 0;
                self.mbc7_eeprom.do_line = false;
                self.mbc7_eeprom.state = Mbc7EepromState::Output;
            }
            0b0100..=0b0111 | 0b0001 => {
                // WRITE / WRAL: 16 data bits follow.
                self.mbc7_eeprom.sr = 0;
                self.mbc7_eeprom.sr_n = 0;
                self.mbc7_eeprom.state = Mbc7EepromState::Input;
            }
            0b1100..=0b1111 | 0b0010 => {
                // ERASE / ERAL: programs on CS fall.
                self.mbc7_eeprom.state = Mbc7EepromState::Pending;
            }
            0b0011 => {
                self.mbc7_eeprom.write_enabled = true;
                self.mbc7_eeprom.state = Mbc7EepromState::Done;
            }
            0b0000 => {
                self.mbc7_eeprom.write_enabled = false;
                self.mbc7_eeprom.state = Mbc7EepromState::Done;
            }
            _ => unreachable!(),
        }
    }

    /// Execute a pending programming instruction at the CS falling edge. If
    /// erase/write is not enabled (no EWEN) the operation is silently dropped
    /// and DO keeps its previous level (no programming cycle ever starts).
    fn mbc7_eeprom_program(&mut self) {
        if !self.mbc7_eeprom.write_enabled {
            return;
        }
        let cmd = self.mbc7_eeprom.command;
        let addr = (cmd & 0x7F) as usize;
        let input = self.mbc7_eeprom.input;
        match (cmd >> 6) & 0xF {
            0b0100..=0b0111 => self.mbc7_eeprom_set_word(addr, input),
            0b1100..=0b1111 => self.mbc7_eeprom_set_word(addr, 0xFFFF),
            0b0001 => {
                for a in 0..128 {
                    self.mbc7_eeprom_set_word(a, input);
                }
            }
            0b0010 => {
                for a in 0..128 {
                    self.mbc7_eeprom_set_word(a, 0xFFFF);
                }
            }
            _ => {}
        }
        // Programming modeled as instant: DO = RDY as soon as CS re-rises.
        self.mbc7_eeprom.do_line = true;
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

    // --- RTC persistence -------------------------------------------------
    //
    // MBC3 blob: the de-facto VBA-M "RTC data" layout, 48 bytes, all fields
    // little-endian. VBA-M, BGB and mGBA write this same block as a footer
    // appended to the `.sav`; mGBA's libretro core exposes it verbatim as
    // RETRO_MEMORY_RTC, so RetroArch `.rtc` files use it too. We store it in
    // a `.rtc` sidecar next to the `.sav` (the RetroArch convention) and
    // additionally READ it from a `.sav` footer for imported saves.
    //
    //   offset size field
    //   0x00   4    seconds       (live counter)
    //   0x04   4    minutes       (live)
    //   0x08   4    hours         (live)
    //   0x0C   4    days low      (live)
    //   0x10   4    days high     (live; bit0=day bit8, bit6=HALT, bit7=carry)
    //   0x14   4    latched seconds
    //   0x18   4    latched minutes
    //   0x1C   4    latched hours
    //   0x20   4    latched days low
    //   0x24   4    latched days high
    //   0x28   8    u64 UNIX time the state was saved at (the legacy 44-byte
    //               variant stores a u32 here; accepted on read)
    //
    // Provenance: VBA-M src/core/gb/gbMemory.h `struct mapperMBC3`
    // (mapperSeconds..mapperControl, the latched mapperL* copies, then a
    // union{time_t,u64}) written raw with a -4 read leeway for the u32 form;
    // mGBA include/mgba/internal/gb/mbc.h `struct GBMBCRTCSaveBuffer` +
    // src/gb/mbc.c GBMBCRTCRead/GBMBCRTCWrite (STORE/LOAD_32LE + 64LE,
    // read accepts sizeof-4).
    //
    // HuC-3 blob: mGBA's `struct GBMBCHuC3SaveBuffer`, 136 bytes: the RTC
    // MCU's 256 nibbles packed two per byte (nibble N -> byte N/2, even N in
    // the low half) followed by a u64 LE UNIX timestamp. This carries the
    // architected minute-of-day/day-counter nibbles (0x10-0x15) plus the
    // whole MCU memory (event time, tone, scratch I/O).

    const MBC3_RTC_BLOB_LEN: usize = 48;
    const MBC3_RTC_BLOB_LEN_LEGACY: usize = 44;
    const HUC3_RTC_BLOB_LEN: usize = 136;

    fn mbc3_rtc_serialize(&self, unix_time: u64) -> [u8; Self::MBC3_RTC_BLOB_LEN] {
        let regs = [
            self.rtc_seconds,
            self.rtc_minutes,
            self.rtc_hours,
            self.rtc_days_low,
            self.rtc_days_high,
            self.rtc_seconds_latched,
            self.rtc_minutes_latched,
            self.rtc_hours_latched,
            self.rtc_days_low_latched,
            self.rtc_days_high_latched,
        ];
        let mut out = [0u8; Self::MBC3_RTC_BLOB_LEN];
        for (i, r) in regs.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&(*r as u32).to_le_bytes());
        }
        out[40..48].copy_from_slice(&unix_time.to_le_bytes());
        out
    }

    /// Restore the MBC3 RTC registers from a 44/48-byte blob; returns the
    /// stored save-time timestamp. Registers are masked to their physical
    /// widths (as in `write_rtc_register`); out-of-range 6-bit values a game
    /// wrote (e.g. seconds 60-63) survive the round trip.
    fn mbc3_rtc_deserialize(&mut self, data: &[u8]) -> Option<u64> {
        if data.len() < Self::MBC3_RTC_BLOB_LEN_LEGACY {
            return None;
        }
        let reg = |i: usize| u32::from_le_bytes(data[i * 4..i * 4 + 4].try_into().unwrap()) as u8;
        self.rtc_seconds = reg(0) & 0x3F;
        self.rtc_minutes = reg(1) & 0x3F;
        self.rtc_hours = reg(2) & 0x1F;
        self.rtc_days_low = reg(3);
        self.rtc_days_high = reg(4) & 0xC1;
        self.rtc_seconds_latched = reg(5) & 0x3F;
        self.rtc_minutes_latched = reg(6) & 0x3F;
        self.rtc_hours_latched = reg(7) & 0x1F;
        self.rtc_days_low_latched = reg(8);
        self.rtc_days_high_latched = reg(9) & 0xC1;
        // The restored state begins a fresh second.
        self.rtc_cycle_accum = 0;
        Some(if data.len() >= Self::MBC3_RTC_BLOB_LEN {
            u64::from_le_bytes(data[40..48].try_into().unwrap())
        } else {
            u32::from_le_bytes(data[40..44].try_into().unwrap()) as u64
        })
    }

    fn huc3_rtc_serialize(&self, unix_time: u64) -> [u8; Self::HUC3_RTC_BLOB_LEN] {
        let mut out = [0u8; Self::HUC3_RTC_BLOB_LEN];
        for (i, chunk) in self.huc3_rtc_mem.chunks(2).take(0x80).enumerate() {
            out[i] = (chunk[0] & 0x0F) | (chunk.get(1).copied().unwrap_or(0) << 4);
        }
        out[128..136].copy_from_slice(&unix_time.to_le_bytes());
        out
    }

    fn huc3_rtc_deserialize(&mut self, data: &[u8]) -> Option<u64> {
        if data.len() < Self::HUC3_RTC_BLOB_LEN || self.huc3_rtc_mem.len() < 0x100 {
            return None;
        }
        for i in 0..0x80 {
            self.huc3_rtc_mem[i * 2] = data[i] & 0x0F;
            self.huc3_rtc_mem[i * 2 + 1] = data[i] >> 4;
        }
        // The restored state begins a fresh minute.
        self.huc3_rtc_accum = 0;
        Some(u64::from_le_bytes(data[128..136].try_into().unwrap()))
    }

    /// Serialize the RTC state to its persistence blob (see the format notes
    /// above); None for carts without an RTC.
    fn rtc_serialize(&self, unix_time: u64) -> Option<Vec<u8>> {
        match self.get_cartridge_type() {
            CartridgeType::MBC3 { timer: true, .. } => {
                Some(self.mbc3_rtc_serialize(unix_time).to_vec())
            }
            CartridgeType::HuC3 => Some(self.huc3_rtc_serialize(unix_time).to_vec()),
            _ => None,
        }
    }

    /// Restore the RTC state from a persistence blob; returns the stored
    /// save-time timestamp on success.
    fn rtc_deserialize(&mut self, data: &[u8]) -> Option<u64> {
        match self.get_cartridge_type() {
            CartridgeType::MBC3 { timer: true, .. } => self.mbc3_rtc_deserialize(data),
            CartridgeType::HuC3 => self.huc3_rtc_deserialize(data),
            _ => None,
        }
    }

    /// Closed-form advance of one MBC3 cascade stage. `width` is the physical
    /// register modulus (64 for the 6-bit seconds/minutes, 32 for the 5-bit
    /// hours), `period` the natural roll-over (60/60/24). Returns the final
    /// value and the carries produced into the next stage. Out-of-range
    /// values (e.g. seconds 60-63) keep counting up and wrap to 0 at `width`
    /// WITHOUT a carry -- exactly the `advance_rtc_second` behaviour.
    fn counter_advance(value: u8, width: u64, period: u64, n: u64) -> (u8, u64) {
        let v = value as u64;
        if v < period {
            (((v + n) % period) as u8, (v + n) / period)
        } else if n < width - v {
            ((v + n) as u8, 0)
        } else {
            let m = n - (width - v);
            ((m % period) as u8, m / period)
        }
    }

    /// Advance the live MBC3 RTC by `n` seconds in closed form; equivalent to
    /// `n` calls of `advance_rtc_second` (unit-tested) but O(1), so
    /// multi-year wall-clock catch-up is instant. Latched shadows are not
    /// touched (they only move on an explicit latch), matching VBA-M's
    /// catch-up which advances only the live mapper counters.
    fn mbc3_rtc_advance_seconds(&mut self, n: u64) {
        if n == 0 {
            return;
        }
        let (s, carries) = Self::counter_advance(self.rtc_seconds & 0x3F, 64, 60, n);
        self.rtc_seconds = s;
        if carries == 0 {
            return;
        }
        let (m, carries) = Self::counter_advance(self.rtc_minutes & 0x3F, 64, 60, carries);
        self.rtc_minutes = m;
        if carries == 0 {
            return;
        }
        let (h, carries) = Self::counter_advance(self.rtc_hours & 0x1F, 32, 24, carries);
        self.rtc_hours = h;
        if carries == 0 {
            return;
        }
        let day = (self.rtc_days_low as u64) | (((self.rtc_days_high & 0x01) as u64) << 8);
        let total = day + carries;
        self.rtc_days_low = (total & 0xFF) as u8;
        let mut high = self.rtc_days_high & 0xC0;
        high |= ((total >> 8) & 0x01) as u8;
        if total > 0x1FF {
            high |= 0x80; // day-counter overflow latches until software clears it
        }
        self.rtc_days_high = high;
    }

    /// Advance the HuC-3 minute-of-day/day counters by `n` minutes in closed
    /// form; equivalent to `n` iterations of the per-minute tick.
    fn huc3_rtc_advance_minutes(&mut self, mut n: u64) {
        if n == 0 || self.huc3_rtc_mem.len() < 0x16 {
            return;
        }
        let (mut minutes, mut days) = self.huc3_clock();
        // An out-of-range minute-of-day (>= 1440, only reachable via a raw
        // nibble write) normalises to 0 with a day carry on its first tick,
        // same as the incremental path.
        if minutes >= 1440 {
            minutes = 0;
            days = (days + 1) & 0x0FFF;
            n -= 1;
        }
        let total = minutes as u64 + n;
        let final_minutes = (total % 1440) as u16;
        let final_days = ((days as u64 + total / 1440) & 0x0FFF) as u16;
        self.huc3_set_clock(final_minutes, final_days);
    }

    /// Wall-clock catch-up applied when RTC state is restored from
    /// persistence: advance the clock by the real seconds elapsed since the
    /// state was saved (Pan Docs MBC3: the coin cell keeps the oscillator
    /// running while the console is off). MBC3 honours the HALT bit (a halted
    /// clock stays put across sessions); the HuC-3 clock has no halt. Never
    /// reached on the deterministic in-memory path (nothing is restored
    /// there).
    fn rtc_catch_up(&mut self, elapsed_seconds: u64) {
        if elapsed_seconds == 0 {
            return;
        }
        match self.get_cartridge_type() {
            CartridgeType::MBC3 { timer: true, .. } => {
                if self.rtc_days_high & 0x40 != 0 {
                    return; // halted
                }
                self.mbc3_rtc_advance_seconds(elapsed_seconds);
            }
            CartridgeType::HuC3 => {
                self.huc3_rtc_advance_minutes(elapsed_seconds / 60);
                // Sub-minute remainder feeds the cycle accumulator so the
                // next in-session minute fires early by the carried amount.
                self.huc3_rtc_accum = self
                    .huc3_rtc_accum
                    .saturating_add((elapsed_seconds % 60) * 4_194_304);
            }
            _ => {}
        }
    }

    /// Restore RTC state from a blob and apply wall-clock catch-up. A zero
    /// timestamp (writer had no wall clock, e.g. an older rustyboi
    /// RETRO_MEMORY_RTC dump) or one from the future (host clock skew)
    /// restores the registers without catch-up.
    fn rtc_restore_with_catch_up(&mut self, data: &[u8]) -> bool {
        let Some(saved_at) = self.rtc_deserialize(data) else {
            return false;
        };
        let now = Self::unix_now();
        if saved_at != 0 && saved_at < now {
            self.rtc_catch_up(now - saved_at);
        }
        true
    }

    /// Current wall clock as UNIX seconds. Only ever called on persistence
    /// paths (sidecar attach/flush, libretro RTC memory), never on the
    /// deterministic cycle-derived path.
    fn unix_now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// `.rtc` sidecar path: the ROM path with its extension replaced (same
    /// derivation as the `.sav`, so the two land side by side).
    fn get_rtc_file_path(&self) -> Option<String> {
        self.rom_path.as_ref().map(|path| {
            let mut rtc_path = path.clone();
            if let Some(dot_pos) = rtc_path.rfind('.') {
                rtc_path.truncate(dot_pos);
            }
            rtc_path.push_str(".rtc");
            rtc_path
        })
    }

    /// A VBA-M/BGB/mGBA RTC blob appended to the `.sav`, if the file is
    /// exactly RAM+blob sized. Read-only interop: the `.rtc` sidecar is
    /// canonical for us and the footer is never (re)written, but a save
    /// imported from those emulators restores its clock on first load.
    fn read_sav_rtc_footer(&self) -> Option<Vec<u8>> {
        let expected: &[usize] = match self.get_cartridge_type() {
            CartridgeType::MBC3 { timer: true, .. } => {
                &[Self::MBC3_RTC_BLOB_LEN, Self::MBC3_RTC_BLOB_LEN_LEGACY]
            }
            CartridgeType::HuC3 => &[Self::HUC3_RTC_BLOB_LEN],
            _ => return None,
        };
        let sav_path = self.get_save_file_path()?;
        let data = fs::read(Path::new(&sav_path)).ok()?;
        let footer_len = data.len().checked_sub(self.ram_data.len())?;
        if expected.contains(&footer_len) {
            Some(data[self.ram_data.len()..].to_vec())
        } else {
            None
        }
    }

    /// Attach the `.rtc` sidecar (disk-load path only): restore persisted RTC
    /// state with wall-clock catch-up and keep the file open for streaming
    /// rewrites as the clock advances. When no sidecar exists, fall back to a
    /// `.sav` RTC footer, then create the sidecar. No-op without an RTC, for
    /// host-managed carts, and for in-memory carts (no `rom_path`).
    fn attach_rtc_sidecar(&mut self) -> Result<(), io::Error> {
        if !self.has_rtc() || self.host_managed_saves {
            return Ok(());
        }
        let Some(rtc_path) = self.get_rtc_file_path() else {
            return Ok(());
        };
        let rtc_path = Path::new(&rtc_path);
        if rtc_path.exists() {
            let data = fs::read(rtc_path)?;
            if self.rtc_restore_with_catch_up(&data) {
                println!("Loaded RTC file: {}", rtc_path.display());
            }
        } else if let Some(footer) = self.read_sav_rtc_footer()
            && self.rtc_restore_with_catch_up(&footer)
        {
            println!("Loaded RTC footer from existing save file");
        }
        self.rtc_file = Some(
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(rtc_path)?,
        );
        // Seed/refresh the sidecar so it is valid from the first second.
        self.flush_rtc_file();
        Ok(())
    }

    /// Rewrite the `.rtc` sidecar with the current state stamped with the
    /// current wall clock. No-op unless a sidecar is attached, so the
    /// deterministic test path performs no I/O and never reads the host
    /// clock. I/O errors are swallowed like the `.sav` streaming writes.
    fn flush_rtc_file(&mut self) {
        if self.rtc_file.is_none() {
            return;
        }
        let Some(blob) = self.rtc_serialize(Self::unix_now()) else {
            return;
        };
        if let Some(file) = self.rtc_file.as_mut() {
            let _ = file.seek(SeekFrom::Start(0));
            let _ = file.write_all(&blob);
            let _ = file.flush();
        }
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

    /// Import a battery save image into the cart's RAM (File → Import Battery
    /// Save). Copies `min(src, dst)` bytes so a footer-carrying `.sav` (mGBA RTC
    /// footer) or a short file loads its RAM-sized prefix; a wildly-oversized
    /// file (more than double the RAM) is rejected so a mis-picked file can't be
    /// silently accepted. If a sidecar `.sav` is attached (desktop) the freshly
    /// loaded image is flushed straight through it, so the import survives a
    /// reload with no extra host plumbing. No-op for non-battery carts.
    pub fn import_save_ram(&mut self, bytes: &[u8]) -> Result<usize, String> {
        if !self.has_battery() {
            return Err("cartridge has no battery-backed save RAM".into());
        }
        let ram_len = self.save_ram().len();
        if ram_len == 0 {
            return Err("cartridge has no save RAM".into());
        }
        if bytes.len() > ram_len.saturating_mul(2) {
            return Err(format!(
                "save file is {} bytes but this cart's RAM is {ram_len} bytes",
                bytes.len()
            ));
        }
        self.load_sram_bytes(bytes).map_err(|e| e.to_string())
    }

    /// Byte the cartridge RAM chip drives when the OAM-DMA controller asserts
    /// the external-RAM chip select (gb-ctr "OAM DMA address decoding": all
    /// A000-FFFF sources are external-RAM accesses). Bypasses the CPU read
    /// front-end (unlicensed boot locks / descramblers watch CPU ROM fetches,
    /// not the RAM chip select) and models the plain RAMG-gated array: enabled
    /// banked RAM drives its byte, anything else leaves the bus floating
    /// (0xFF, matching Gambatte's RAM-less srcE000 cgb04c captures).
    pub fn dma_sram_bus_read(&self, addr: u16) -> u8 {
        if self.sram_cs_lazy && self.ram_enabled && !self.ram_data.is_empty() {
            let offset =
                ((addr as usize & 0x1FFF) + self.get_ram_bank() * 0x2000) % self.ram_data.len();
            self.ram_data[offset]
        } else {
            0xFF
        }
    }

    /// Select the board's SRAM chip-select decode (see `dma_sram_bus_read`).
    pub fn set_sram_cs_lazy(&mut self, lazy: bool) {
        self.sram_cs_lazy = lazy;
    }

    /// True if this cartridge has a real-time clock (MBC3 timer or HuC-3).
    /// Gates the bus-driven `rtc_tick` path.
    pub fn has_rtc(&self) -> bool {
        matches!(
            self.get_cartridge_type(),
            CartridgeType::MBC3 { timer: true, .. } | CartridgeType::HuC3
        )
    }

    /// True for POCKET CAMERA carts (MAC-GBD + M64282FP sensor). Frontends
    /// use this to know when `set_camera_image` is meaningful; the bus uses
    /// it to gate the capture-countdown tick.
    pub fn has_camera(&self) -> bool {
        matches!(self.get_cartridge_type(), CartridgeType::PocketCamera)
    }

    /// Classify the per-dot RTC advance once, so the hot path can cache it.
    pub fn rtc_kind(&self) -> RtcTickKind {
        match self.get_cartridge_type() {
            CartridgeType::MBC3 { timer: true, .. } => RtcTickKind::Mbc3,
            CartridgeType::HuC3 => RtcTickKind::HuC3,
            _ => RtcTickKind::None,
        }
    }

    /// True if the cartridge needs the per-dot peripheral clock tick (an RTC
    /// crystal or the camera capture countdown).
    pub fn needs_clock_tick(&self) -> bool {
        self.has_rtc() || self.has_camera()
    }

    // -----------------------------------------------------------------------
    // POCKET CAMERA (MAC-GBD controller + Mitsubishi M64282FP image sensor)
    //
    // References: Pan Docs "Game Boy Camera" (the section AntonioND compiled
    // from his reverse engineering), AntonioND/gbcam-rev-engineer doc
    // v1.1.1 (register semantics, capture timings, GiiBiiAdvance reference
    // pipeline), mGBA src/gb/mbc/pocket-cam.c and SameBoy Core/camera.c for
    // mapper cross-checks.
    //
    // Register file (A000-A035 while a bank with bit 4 set is selected,
    // mirrored every $80):
    //   A000     trigger/status: bit0 start capture / busy flag, bits 1-2
    //            select the M64282FP 1-D filtering set (P/M/X registers).
    //   A001     -> sensor reg 1: N (bit7), VH (bits 5-6), gain (bits 0-4).
    //   A002/03  -> sensor regs 2/3: 16-bit exposure, MSB first.
    //   A004     -> sensor reg 7: E3+edge ratio (bits 4-7), invert (bit 3),
    //            output node bias V (bits 0-2, analog only).
    //   A005     -> sensor reg 0: zero-point calibration (bits 6-7), output
    //            reference voltage O (bits 0-5, analog only).
    //   A006-35  4x4 dither/contrast matrix, 3 threshold bytes per cell.
    // -----------------------------------------------------------------------

    /// Feed the live sensor image: 128x112 8-bit grayscale, row-major
    /// (`pixels[y * 128 + x]`), 0 = black. This is the frontend integration
    /// point for a webcam/still source; without it captures use a built-in
    /// deterministic test pattern. No-op outside camera carts' effect (the
    /// buffer is simply never consumed).
    pub fn set_camera_image(&mut self, pixels: &[u8; CAM_W * CAM_H]) {
        if self.cam_image.len() != CAM_W * CAM_H {
            self.cam_image = vec![0; CAM_W * CAM_H];
        }
        self.cam_image.copy_from_slice(pixels);
    }

    /// Built-in deterministic sensor input: a diagonal luminance gradient
    /// with a dark disc, a bright disc and a mid-gray border frame, spanning
    /// the full 0-255 range so all four GB shades appear after dithering.
    fn cam_builtin_pattern() -> Vec<u8> {
        let mut img = vec![0u8; CAM_W * CAM_H];
        for y in 0..CAM_H {
            for x in 0..CAM_W {
                let mut v = ((x * 255) / (CAM_W - 1) + (y * 255) / (CAM_H - 1)) / 2;
                // Dark disc, upper-left quadrant.
                let (dx, dy) = (x as i32 - 40, y as i32 - 40);
                if dx * dx + dy * dy < 24 * 24 {
                    v = 24;
                }
                // Bright disc, lower-right quadrant.
                let (dx, dy) = (x as i32 - 92, y as i32 - 76);
                if dx * dx + dy * dy < 20 * 20 {
                    v = 232;
                }
                // Mid-gray frame border.
                if !(4..CAM_W - 4).contains(&x) || !(4..CAM_H - 4).contains(&y) {
                    v = 128;
                }
                img[y * CAM_W + x] = v as u8;
            }
        }
        img
    }

    /// Write to the CAM register file (index = addr & 0x7F).
    fn cam_reg_write(&mut self, idx: u16, value: u8) {
        if idx == 0 {
            // Only the low 3 bits are wired.
            self.cam_regs[0] = value & 0x07;
            if value & 0x01 != 0 {
                if !self.cam_running {
                    if self.cam_clocks_left > 0 {
                        // Restart after a mid-capture stop: "it will continue
                        // the previous capture process with the old capture
                        // parameters, even if the registers are changed in
                        // between" -- cam_pending was already processed with
                        // the trigger-time parameters.
                        self.cam_running = true;
                    } else {
                        self.cam_start_capture();
                    }
                }
            } else if self.cam_running {
                // Stop the capture; RAM is readable again. The countdown
                // freezes so a later '1' write resumes it.
                self.cam_running = false;
            }
        } else if (idx as usize) < CAM_REG_COUNT {
            self.cam_regs[idx as usize] = value;
        }
        // A036-A07F: unmapped, writes ignored (SameBoy warns and drops).
    }

    /// Start a capture: compute the busy window and process the sensor
    /// image. The result is committed to RAM when the countdown expires (the
    /// real controller streams pixels into RAM during the sensor read period
    /// at the END of the window; committing at expiry keeps the previous
    /// image visible if the capture is stopped early, as documented).
    fn cam_start_capture(&mut self) {
        let n_bit = self.cam_regs[1] & 0x80 != 0;
        let exposure = ((self.cam_regs[2] as u64) << 8) | self.cam_regs[3] as u64;
        // Pan Docs: M-cycles(1MiHz) = 32446 + (N ? 0 : 512) + 16 * exposure.
        // Stored in master-clock T-cycles (x4); cam_tick halves the window
        // in CGB double-speed mode where PHI runs twice as fast.
        self.cam_clocks_left = 4 * (32446 + if n_bit { 0 } else { 512 } + 16 * exposure);
        self.cam_running = true;
        self.cam_pending = self.cam_process_image();
    }

    /// Advance the capture countdown by `phi_quarters` PHI/4 units (master
    /// dots at single speed; the caller doubles the span in CGB double-speed
    /// mode, where the PHI cartridge clock runs at 2.097152 MHz). No-op
    /// unless a capture is actively running.
    pub fn cam_tick(&mut self, phi_quarters: u64) {
        if !self.cam_running || phi_quarters == 0 {
            return;
        }
        if self.cam_clocks_left > phi_quarters {
            self.cam_clocks_left -= phi_quarters;
            return;
        }
        // Capture finished: the controller has streamed the processed tile
        // data into RAM bank 0 at $0100 and the busy flag clears.
        self.cam_clocks_left = 0;
        self.cam_running = false;
        if self.ram_data.len() >= CAM_RAM_IMAGE_OFFSET + CAM_TILE_BYTES
            && self.cam_pending.len() == CAM_TILE_BYTES
        {
            let pending = std::mem::take(&mut self.cam_pending);
            self.ram_data[CAM_RAM_IMAGE_OFFSET..CAM_RAM_IMAGE_OFFSET + CAM_TILE_BYTES]
                .copy_from_slice(&pending);
            // Stream the block to the battery .sav (single bulk write, not
            // 3584 per-byte writes).
            if let Some(file) = &mut self.save_file {
                let _ = file
                    .seek(SeekFrom::Start(CAM_RAM_IMAGE_OFFSET as u64))
                    .and_then(|_| file.write_all(&pending))
                    .and_then(|_| file.flush());
            }
        }
    }

    /// The M64282FP sensor + MAC-GBD controller pipeline, following the
    /// AntonioND/GiiBiiAdvance reference model reproduced in Pan Docs
    /// ("Sample code for emulators"), in exact-integer form: exposure
    /// scaling, optional inversion, the documented 3x3 edge kernels / 1-D
    /// filtering selected by N/VH/E3 and the A000 P/M bits, then the 4x4x3
    /// dither/contrast matrix, packed as GB 2bpp tiles (16x14 tiles, the
    /// layout the ROM expects at RAM bank 0 offset $0100).
    fn cam_process_image(&self) -> Vec<u8> {
        // --- Sensor input: 128x120 window (112 visible + 4 padding rows
        // top/bottom standing in for the sensor's discarded edge rows).
        let builtin;
        let input: &[u8] = if self.cam_image.len() == CAM_W * CAM_H {
            &self.cam_image
        } else {
            builtin = Self::cam_builtin_pattern();
            &builtin
        };
        let src_row = |k: usize| {
            let y = (k as i32 - (CAM_SENSOR_EXTRA_LINES / 2) as i32)
                .clamp(0, CAM_H as i32 - 1) as usize;
            &input[y * CAM_W..(y + 1) * CAM_W]
        };

        // --- Configuration (registers latched at trigger time).
        // A000 bits 1-2 select the 1-D filter P/M sets (doc v1.1.1 §3.1.3).
        let (p_bits, m_bits) = match (self.cam_regs[0] >> 1) & 3 {
            0 => (0x00u32, 0x01u32),
            1 => (0x01, 0x00),
            _ => (0x01, 0x02),
        };
        let n_bit = (self.cam_regs[1] >> 7) as u32;
        let vh_bits = ((self.cam_regs[1] >> 5) & 3) as u32;
        let exposure = ((self.cam_regs[2] as i32) << 8) | self.cam_regs[3] as i32;
        let e3_bit = (self.cam_regs[4] >> 7) as u32;
        let i_bit = self.cam_regs[4] & 0x08 != 0;
        // Edge enhancement ratio in quarters: 0.50,0.75,1.00,1.25,2,3,4,5.
        let alpha4 = [2i32, 3, 4, 5, 8, 12, 16, 20][((self.cam_regs[4] >> 4) & 7) as usize];
        // alpha-scaled add with GiiBiiAdvance's exact float->int semantics:
        // trunc(px + diff*alpha) == trunc((4*px + diff*alpha4) / 4).
        let edge = |px: i32, diff: i32| (px * 4 + diff * alpha4) / 4;

        // --- Analog stage: exposure scaling + level squash (GiiBiiAdvance's
        // measured approximation of the sensor's gain/level control against
        // the ROM's ~$80-centered dither thresholds), optional inversion,
        // then signed representation for the edge kernels. Column-major
        // (x * CAM_SENSOR_H + y) like the reference buf[i][j].
        let h = CAM_SENSOR_H;
        let w = CAM_W;
        let at = |i: usize, j: usize| i * h + j;
        let mut buf = vec![0i32; w * h];
        for i in 0..w {
            for j in 0..h {
                let mut v = src_row(j)[i] as i32;
                v = v * exposure / 0x0300;
                v = 128 + (v - 128) / 8;
                v = v.clamp(0, 255);
                if i_bit {
                    v = 255 - v;
                }
                buf[at(i, j)] = v - 128;
            }
        }

        // 1-D filtering: vout = P/M-selected sum of the pixel and its south
        // neighbor (the sensor streams line pairs through the 1-D kernel).
        let one_d = |src: &[i32], dst: &mut [i32]| {
            for i in 0..w {
                for j in 0..h {
                    let px = src[at(i, j)];
                    let ms = src[at(i, (j + 1).min(h - 1))];
                    let mut value = 0;
                    if p_bits & 1 != 0 {
                        value += px;
                    }
                    if p_bits & 2 != 0 {
                        value += ms;
                    }
                    if m_bits & 1 != 0 {
                        value -= px;
                    }
                    if m_bits & 2 != 0 {
                        value -= ms;
                    }
                    dst[at(i, j)] = value.clamp(-128, 127);
                }
            }
        };

        let filtering_mode = (n_bit << 3) | (vh_bits << 1) | e3_bit;
        match filtering_mode {
            0x0 => {
                // Positive/negative image: plain 1-D filtering.
                let src = buf.clone();
                one_d(&src, &mut buf);
            }
            0x2 => {
                // Horizontal enhancement (P + {2P-(MW+ME)}*alpha), then 1-D.
                let mut temp = vec![0i32; w * h];
                for i in 0..w {
                    for j in 0..h {
                        let mw = buf[at(i.saturating_sub(1), j)];
                        let me = buf[at((i + 1).min(w - 1), j)];
                        let px = buf[at(i, j)];
                        temp[at(i, j)] = edge(px, 2 * px - mw - me).clamp(0, 255);
                    }
                }
                one_d(&temp, &mut buf);
            }
            0xE => {
                // 2D enhancement (P + {4P-(MN+MS+ME+MW)}*alpha). This is the
                // mode the GB Camera ROM shoots with (A001 = $E0|gain).
                let mut temp = vec![0i32; w * h];
                for i in 0..w {
                    for j in 0..h {
                        let ms = buf[at(i, (j + 1).min(h - 1))];
                        let mn = buf[at(i, j.saturating_sub(1))];
                        let mw = buf[at(i.saturating_sub(1), j)];
                        let me = buf[at((i + 1).min(w - 1), j)];
                        let px = buf[at(i, j)];
                        temp[at(i, j)] = edge(px, 4 * px - mw - me - mn - ms).clamp(-128, 127);
                    }
                }
                buf = temp;
            }
            0x1 => {
                // AntonioND: real cartridges output a constant color in this
                // configuration (likely a sensor bug); model as flat 0.
                buf.fill(0);
            }
            0x3 => {
                // Horizontal extraction ({2P-(MW+ME)}*alpha), then 1-D
                // (doc v1.1.1 Table 1; unused by the GB Camera ROM).
                let mut temp = vec![0i32; w * h];
                for i in 0..w {
                    for j in 0..h {
                        let mw = buf[at(i.saturating_sub(1), j)];
                        let me = buf[at((i + 1).min(w - 1), j)];
                        let px = buf[at(i, j)];
                        temp[at(i, j)] = edge(0, 2 * px - mw - me).clamp(0, 255);
                    }
                }
                one_d(&temp, &mut buf);
            }
            0xC | 0xD => {
                // Vertical enhancement / extraction (Table 1, no 1-D).
                let mut temp = vec![0i32; w * h];
                for i in 0..w {
                    for j in 0..h {
                        let ms = buf[at(i, (j + 1).min(h - 1))];
                        let mn = buf[at(i, j.saturating_sub(1))];
                        let px = buf[at(i, j)];
                        let base = if filtering_mode == 0xC { px } else { 0 };
                        temp[at(i, j)] = edge(base, 2 * px - mn - ms).clamp(-128, 127);
                    }
                }
                buf = temp;
            }
            0xF => {
                // 2D extraction ({4P-(MN+MS+ME+MW)}*alpha, Table 1).
                let mut temp = vec![0i32; w * h];
                for i in 0..w {
                    for j in 0..h {
                        let ms = buf[at(i, (j + 1).min(h - 1))];
                        let mn = buf[at(i, j.saturating_sub(1))];
                        let mw = buf[at(i.saturating_sub(1), j)];
                        let me = buf[at((i + 1).min(w - 1), j)];
                        let px = buf[at(i, j)];
                        temp[at(i, j)] = edge(0, 4 * px - mw - me - mn - ms).clamp(-128, 127);
                    }
                }
                buf = temp;
            }
            _ => {
                // Undefined combination: no filtering.
            }
        }

        // --- Controller stage: back to unsigned, 4x4x3 threshold matrix
        // (contrast + dithering), then GB 2bpp tile packing.
        let mut tiles = vec![0u8; CAM_TILE_BYTES];
        for j in 0..CAM_H {
            for i in 0..CAM_W {
                let value = (buf[at(i, j + CAM_SENSOR_EXTRA_LINES / 2)] + 128).clamp(0, 255);
                let base = 6 + ((j & 3) * 4 + (i & 3)) * 3;
                // sensor < DxyL -> black; < DxyM -> dark gray; < DxyH ->
                // light gray; else white (shades as 2bpp color numbers).
                let color: u8 = if value < self.cam_regs[base] as i32 {
                    3
                } else if value < self.cam_regs[base + 1] as i32 {
                    2
                } else if value < self.cam_regs[base + 2] as i32 {
                    1
                } else {
                    0
                };
                // 16 tiles per row, 16 bytes per tile, MSB = leftmost pixel.
                let tile_base = ((j >> 3) * 16 + (i >> 3)) * 16 + (j & 7) * 2;
                let bit = 7 - (i & 7);
                tiles[tile_base] |= (color & 1) << bit;
                tiles[tile_base + 1] |= ((color >> 1) & 1) << bit;
            }
        }
        tiles
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

    /// Mutable view of the RTC bytes for `RETRO_MEMORY_RTC`, in the exact
    /// `.rtc` persistence format (MBC3: the 48-byte VBA-M/mGBA block; HuC-3:
    /// the 136-byte mGBA block) stamped with the current wall clock, so the
    /// frontend's `.rtc` files are byte-compatible with mGBA's. The buffer
    /// allocation stays stable across calls (the frontend caches the raw
    /// pointer). Empty for carts without an RTC.
    pub fn rtc_memory_mut(&mut self) -> &mut [u8] {
        self.rtc_memory_refresh();
        &mut self.rtc_memory
    }

    /// Read-only mirror of [`rtc_memory_mut`](Self::rtc_memory_mut): the
    /// serialized RTC region. Empty for carts without an RTC. Takes `&mut self`
    /// only because it must refresh the region from live state first (the
    /// pointer stays stable), but performs no external mutation.
    pub fn rtc_memory(&mut self) -> &[u8] {
        self.rtc_memory_refresh();
        &self.rtc_memory
    }

    /// The current RTC state serialized to the mGBA/BGB `.rtc` sidecar format
    /// (File → Export RTC). `None` for carts without an RTC.
    pub fn export_rtc(&self) -> Option<Vec<u8>> {
        if !self.has_rtc() {
            return None;
        }
        self.rtc_serialize(Self::unix_now())
    }

    /// Import a `.rtc` sidecar blob (File → Import RTC): restore the persisted
    /// clock with wall-clock catch-up, then flush the attached sidecar (desktop)
    /// so the import survives a reload. Errors on a blob that doesn't match this
    /// cart's RTC layout. No-op-error for non-RTC carts.
    pub fn import_rtc(&mut self, bytes: &[u8]) -> Result<(), String> {
        if !self.has_rtc() {
            return Err("cartridge has no real-time clock".into());
        }
        if !self.rtc_restore_with_catch_up(bytes) {
            return Err("RTC data does not match this cartridge".into());
        }
        self.flush_rtc_file();
        Ok(())
    }

    /// Re-sync the RETRO_MEMORY_RTC buffer from the live state (+ a fresh
    /// timestamp) and remember what we wrote, so an external write into the
    /// region by the frontend is detectable.
    fn rtc_memory_refresh(&mut self) {
        let Some(blob) = self.rtc_serialize(Self::unix_now()) else {
            self.rtc_memory.clear();
            self.rtc_memory_synced.clear();
            return;
        };
        if self.rtc_memory.len() == blob.len() {
            self.rtc_memory.copy_from_slice(&blob); // in place: pointer stays valid
        } else {
            self.rtc_memory = blob.clone();
        }
        if self.rtc_memory_synced.len() == blob.len() {
            self.rtc_memory_synced.copy_from_slice(&blob);
        } else {
            self.rtc_memory_synced = blob;
        }
    }

    /// Once-per-frame RTC sync for the libretro frontend. RetroArch loads an
    /// existing `.rtc` file by memcpying it straight into the
    /// RETRO_MEMORY_RTC region after `retro_load_game` (there is no load
    /// callback), so: if the buffer no longer matches what we last synced,
    /// adopt the externally-written state with wall-clock catch-up; then
    /// refresh the buffer so frontend (auto)saves always read current state.
    /// No-op until the frontend has requested the region.
    pub fn rtc_memory_frame_sync(&mut self) {
        if self.rtc_memory.is_empty() || !self.has_rtc() {
            return;
        }
        if self.rtc_memory != self.rtc_memory_synced {
            let external = std::mem::take(&mut self.rtc_memory);
            self.rtc_restore_with_catch_up(&external);
            self.rtc_memory = external; // hand the allocation back (cached ptr)
        }
        self.rtc_memory_refresh();
    }

    /// True for MBC5 rumble cartridges.
    pub fn has_rumble(&self) -> bool {
        matches!(self.get_cartridge_type(), CartridgeType::MBC5 { rumble: true, .. })
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

    /// Boot-ROM handoff for skip_bios: the Sachen and Rocket boot locks model
    /// the cart's power-on state as seen BY a real boot ROM; when the boot is
    /// skipped they must start unlocked (hhugboy resetVars without
    /// bootstrap). No-op for every other mapper.
    pub fn skip_boot_handoff(&mut self) {
        match self.get_cartridge_type() {
            CartridgeType::Sachen { .. } => self.sachen_lock.set(UNL_UNLOCKED),
            CartridgeType::Rocket => self.rocket_lock.set(UNL_UNLOCKED),
            _ => {}
        }
    }

    /// The 48 header-logo bytes a DMG boot ROM would have read through the
    /// LOCKED mapper, when they differ from a plain $0104 read. Sachen MMC1
    /// games check the boot-decompressed VRAM tiles for the SACHEN logo as
    /// copy protection, so skip_bios must seed those tiles instead of the
    /// Nintendo ones (hhugboy pokes the same expansion into $8010 when no
    /// bootstrap is emulated). Locked MMC1 reads force RA7 high and pass
    /// through the $01xx descramble, so the bytes come from
    /// unscramble($0184+i) — bit 7 survives the bit-swap.
    pub fn boot_logo_override(&self) -> Option<[u8; 48]> {
        if !matches!(self.get_cartridge_type(), CartridgeType::Sachen { mmc2: false }) {
            return None;
        }
        let mut out = [0u8; 48];
        for (i, b) in out.iter_mut().enumerate() {
            let a = Self::sachen_unscramble((0x184 + i) as u16) as usize;
            *b = self.rom_data.get(a).copied().unwrap_or(0xFF);
        }
        Some(out)
    }

    /// The 48 header-logo bytes the boot ROM would decompress into the VRAM
    /// tiles at $8010: normally the cart's own $0104-$0133, or the locked-mapper
    /// substitution for Sachen MMC1 (`boot_logo_override`). Read straight from
    /// `rom_data` (no bus side effects) so skip_bios never perturbs mapper state.
    pub fn boot_logo_bytes(&self) -> [u8; 48] {
        if let Some(logo) = self.boot_logo_override() {
            return logo;
        }
        let mut out = [0u8; 48];
        for (i, b) in out.iter_mut().enumerate() {
            *b = self.rom_data.get(0x104 + i).copied().unwrap_or(0xFF);
        }
        out
    }

    /// Sachen MMC read-side address transform: boot-lock phase counting plus
    /// the $01xx descramble. Interior mutability (Cell) because the lock
    /// transitions are driven by CPU READS (the A15-transition counter on the
    /// real board). mGBA _GBSachenMMC1Read/_GBSachenMMC2Read.
    fn sachen_read_addr(&self, mut addr: u16, mmc2: bool) -> u16 {
        let lock = self.sachen_lock.get();
        if mmc2 {
            // MMC2: DMG -> CGB -> unlocked, 0x31 transitions each. (The
            // DMG->CGB shortcut on WRAM traffic is not visible from the
            // cart bus here; the counter path below matches mGBA's.)
            if lock != UNL_UNLOCKED && (addr & 0x8700) == 0x0100 {
                let t = self.sachen_transition.get() + 1;
                if t == 0x31 {
                    self.sachen_lock.set(lock + 1);
                    self.sachen_transition.set(0);
                } else {
                    self.sachen_transition.set(t);
                }
            }
            if (addr & 0xFF00) == 0x0100 {
                if self.sachen_lock.get() == UNL_LOCKED_CGB {
                    // Locked: RA7 forced high (presents the second header
                    // copy).
                    addr |= 0x80;
                }
                addr = Self::sachen_unscramble(addr);
            }
        } else {
            // MMC1: single locked phase; the 0x31st $01xx read unlocks.
            if lock != UNL_UNLOCKED && (addr & 0xFF00) == 0x0100 {
                let t = self.sachen_transition.get() + 1;
                self.sachen_transition.set(t);
                if t == 0x31 {
                    self.sachen_lock.set(UNL_UNLOCKED);
                } else {
                    addr |= 0x80;
                }
            }
            if (addr & 0xFF00) == 0x0100 {
                addr = Self::sachen_unscramble(addr);
            }
        }
        addr
    }

    /// Rocket Games read-side lock counter. Returns the XOR pad byte to apply
    /// (locked-CGB phase presents the XOR-decoded Nintendo logo at
    /// $0104-$0133); 0 otherwise. hhugboy MbcUnlRocketGames::readMemory.
    fn rocket_read_xor(&self, addr: u16) -> u8 {
        let mode = self.rocket_lock.get();
        if mode != UNL_UNLOCKED {
            let count = self.rocket_unlock_count.get();
            if count == 0x30 {
                if mode == UNL_LOCKED_DMG {
                    self.rocket_lock.set(UNL_LOCKED_CGB);
                    self.rocket_unlock_count.set(0);
                } else {
                    self.rocket_lock.set(UNL_UNLOCKED);
                }
            } else {
                self.rocket_unlock_count.set(count + 1);
            }
        }
        if self.rocket_lock.get() == UNL_LOCKED_CGB && (0x0104..0x0134).contains(&addr) {
            ROCKET_LOGO_XOR[(addr - 0x0104) as usize]
        } else {
            0
        }
    }
}

impl memory::Addressable for Cartridge {
    fn read(&self, addr: u16) -> u8 {
        // Unlicensed-board read front-end: Sachen boot lock + $01xx address
        // descramble, Rocket boot lock + logo XOR window. Licensed carts
        // (UnlMapper::None) skip this entirely.
        let mut addr = addr;
        let mut xor = 0u8;
        match self.unl_mapper {
            UnlMapper::SachenMmc1 if addr < 0x8000 => {
                addr = self.sachen_read_addr(addr, false);
            }
            UnlMapper::SachenMmc2 if addr < 0x8000 => {
                addr = self.sachen_read_addr(addr, true);
            }
            UnlMapper::Rocket if addr < 0x8000 => {
                xor = self.rocket_read_xor(addr);
            }
            _ => {}
        }
        xor ^ match addr {
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
                    CartridgeType::MBC7 => {
                        // MBC7 exposes registers, not RAM. They only respond
                        // when BOTH enable stages are unlocked, and only in
                        // A000-AFFF (B000-BFFF just reads 0xFF). The register
                        // is selected by address bits 4-7; bits 0-3 and 8-11
                        // are ignored.
                        if self.ram_enabled && self.mbc7_ram_enabled2 && addr < 0xB000 {
                            match (addr >> 4) & 0x0F {
                                0x2 => (self.mbc7_accel_x & 0xFF) as u8,
                                0x3 => (self.mbc7_accel_x >> 8) as u8,
                                0x4 => (self.mbc7_accel_y & 0xFF) as u8,
                                0x5 => (self.mbc7_accel_y >> 8) as u8,
                                // Ax6x always reads 0x00 (possibly a reserved
                                // Z axis); Ax7x always 0xFF.
                                0x6 => 0x00,
                                0x8 => self.mbc7_eeprom.pin_state(),
                                // Ax0x/Ax1x are write-only (latch control),
                                // Ax7x and Ax9x-AxFx read 0xFF.
                                _ => 0xFF,
                            }
                        } else {
                            0xFF
                        }
                    }
                    CartridgeType::HuC1 => {
                        if self.huc1_ir_mode {
                            // IR receiver: 0xC1 = light seen, 0xC0 = no light
                            // (Pan Docs HuC1). No IR transport is modeled, so
                            // this always reads the documented idle 0xC0.
                            0xC0
                        } else if !self.ram_data.is_empty() {
                            // RAM is always enabled (no MBC1-style gate).
                            let ram_bank = self.get_ram_bank();
                            let offset = ((addr - EXTERNAL_RAM_START) as usize
                                + (ram_bank * 0x2000))
                                % self.ram_data.len();
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    CartridgeType::PocketCamera => {
                        if self.cam_regs_selected {
                            // Register file, mirrored every $80. Only A000 is
                            // readable: bits 1-2 are the stored 1-D filter
                            // set, bit 0 is the live capture-busy flag; bits
                            // 3-7 read '0'. All other registers read $00.
                            if (addr & 0x7F) == 0 {
                                (self.cam_regs[0] & 0x06) | (self.cam_running as u8)
                            } else {
                                0x00
                            }
                        } else if self.cam_running {
                            // "When the capture process is active all RAM
                            // banks will return 00h when read."
                            0x00
                        } else if !self.ram_data.is_empty() {
                            // No read gate: RAM reads are always enabled.
                            let ram_bank = self.get_ram_bank();
                            let offset = ((addr - EXTERNAL_RAM_START) as usize
                                + (ram_bank * 0x2000))
                                % self.ram_data.len();
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    CartridgeType::HuC3 => {
                        match self.huc3_mode {
                            // 0x0 = RAM read-only, 0xA = RAM read/write; both
                            // read the banked external RAM.
                            0x0 | 0xA => {
                                if !self.ram_data.is_empty() {
                                    let ram_bank = self.get_ram_bank();
                                    let offset = ((addr - EXTERNAL_RAM_START) as usize
                                        + (ram_bank * 0x2000))
                                        % self.ram_data.len();
                                    self.ram_data[offset]
                                } else {
                                    0xFF
                                }
                            }
                            // RTC command/response: bits 6-4 echo the last
                            // command written to the mailbox, bits 3-0 hold
                            // the result of the last executed command. D7 is
                            // not driven by the chip (open bus, usually
                            // high).
                            0xC => 0x80 | (self.huc3_rtc_command << 4) | self.huc3_rtc_result,
                            // RTC semaphore: bit 0 high = MCU ready. Modeled
                            // as always ready (instant execution). Bits 7-1
                            // are not specified; 0 matches observed software
                            // expectations.
                            0xD => 0x01,
                            // IR receiver stub: 0xC0 = no light seen (same
                            // idle value as HuC-1's IR register). Full IR
                            // link emulation is out of scope.
                            0xE => 0xC0,
                            // 0xB is the write-only command mailbox; other
                            // select values are unmapped. Reads are open bus.
                            _ => 0xFF,
                        }
                    }
                    CartridgeType::NoMBC { .. } => {
                        // Pan Docs "No MBC": optional RAM (up to 8KB) is wired
                        // straight through at A000-BFFF -- no banking, no
                        // enable gate. A smaller chip mirrors across the
                        // window (address modulo its size).
                        if !self.ram_data.is_empty() {
                            let offset =
                                (addr - EXTERNAL_RAM_START) as usize % self.ram_data.len();
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    // Rocket/Sachen boards wire any RAM straight through with
                    // no enable gate (hhugboy maps it unconditionally).
                    CartridgeType::Rocket | CartridgeType::Sachen { .. } => {
                        if !self.ram_data.is_empty() {
                            let offset =
                                (addr - EXTERNAL_RAM_START) as usize % self.ram_data.len();
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    // NT/Makon old boards gate RAM MBC3-style ($0A to
                    // $0000-$1FFF), unbanked.
                    CartridgeType::NtOld { .. } => {
                        if self.ram_enabled && !self.ram_data.is_empty() {
                            let offset =
                                (addr - EXTERNAL_RAM_START) as usize % self.ram_data.len();
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
            // Wisdom Tree: a single '377 latch loaded from the ADDRESS lines
            // on any $0000-$3FFF write; the data byte is ignored. The low 6
            // bits select a whole-$0000-$7FFF 32KB bank (Pan Docs "Other
            // MBCs"; mGBA _GBWisdomTree: bank = address & 0x3F).
            RAM_ENABLE_START..=ROM_BANK_SELECT_END
                if matches!(self.get_cartridge_type(), CartridgeType::WisdomTree) =>
            {
                self.wt_bank = (addr & 0x3F) as u8;
            }
            // M161 (gambatte m161.cpp romWrite): the FIRST write anywhere in
            // the whole $0000-$7FFF ROM area latches the 32KB bank from data
            // bits 0-2; every later write is ignored until reset.
            RAM_ENABLE_START..=BANKING_MODE_END
                if matches!(self.get_cartridge_type(), CartridgeType::M161) =>
            {
                if !self.m161_mapped {
                    self.m161_bank = (value & 7) << 1;
                    self.m161_mapped = true;
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
                    CartridgeType::MBC7 => {
                        // Stage 1 of the two-stage RAM-register unlock; stage
                        // 2 is 0x40 to 0x4000-0x5FFF.
                        self.ram_enabled = (value & 0x0F) == 0x0A;
                    }
                    CartridgeType::HuC1 => {
                        // IR select: only 0xE in the low nibble maps the IR
                        // transceiver at A000-BFFF; anything else selects RAM.
                        // There is no RAM-disable state.
                        self.huc1_ir_mode = (value & 0x0F) == 0x0E;
                    }
                    CartridgeType::HuC3 => {
                        // RAM/RTC/IR select: maps what A000-BFFF accesses.
                        // Only the low 4 bits are significant.
                        self.huc3_mode = value & 0x0F;
                    }
                    CartridgeType::PocketCamera => {
                        // Gates RAM WRITES only: "Reading from RAM or
                        // registers is always enabled. Writing to registers
                        // is always enabled." (Pan Docs Game Boy Camera).
                        self.ram_enabled = (value & 0x0F) == 0x0A;
                    }
                    CartridgeType::Sachen { .. } => {
                        // Base ROM bank register, latched only while the
                        // inner bank register has bits 4-5 both set (hhugboy
                        // MbcUnlSachenMMC1/2 case 0).
                        if (self.sachen_bank & 0x30) == 0x30 {
                            self.sachen_base = value;
                        }
                    }
                    CartridgeType::NtOld { .. } => {
                        // MBC3-style RAM enable (mGBA routes this to _GBMBC3).
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
                    CartridgeType::MBC7 => {
                        self.mbc7_rom_bank = value; // like MBC5, bank 0 allowed
                    }
                    CartridgeType::HuC1 => {
                        self.huc1_rom_bank = value & 0x3F; // 6-bit, bank 0 allowed
                    }
                    CartridgeType::HuC3 => {
                        self.huc3_rom_bank = value & 0x7F; // 7-bit, bank 0 allowed
                    }
                    CartridgeType::PocketCamera => {
                        self.cam_rom_bank = value & 0x3F; // 6-bit, bank 0 allowed
                    }
                    CartridgeType::Rocket => {
                        // Two EXACT register addresses (hhugboy
                        // MbcUnlRocketGames::writeMemory); everything else in
                        // the region is ignored.
                        match addr {
                            // Inner 16KB bank, 0 maps to 1.
                            0x3F00 => self.rocket_rom_bank = value.max(1),
                            // Outer 256KB bank (effective bank bits 4-7; the
                            // $99 2-in-1s use it to pick the sub-game).
                            0x3FC0 => self.rocket_outer = value,
                            _ => {}
                        }
                    }
                    CartridgeType::Sachen { .. } => {
                        // Inner ("unmasked") bank register, 0 maps to 1.
                        self.sachen_bank = value.max(1);
                    }
                    CartridgeType::NtOld { v2 } => {
                        // v1 is MBC1-style 5-bit, v2 MBC3-style 8-bit; both
                        // remap 0 to 1. The raw value is stored; the $5003
                        // bit-swap applies combinationally in get_rom_bank.
                        let bank = if v2 { value } else { value & 0x1F };
                        self.nt_bank = bank.max(1);
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
                    CartridgeType::MBC5 { rumble, .. } => {
                        if rumble {
                            // On rumble carts bit 3 drives the motor; only the
                            // low 3 bits select the RAM bank.
                            self.rumble_motor = (value & 0x08) != 0;
                        }
                        self.mbc5_ram_bank = value; // 4 bits used (0x00-0x0F)
                    }
                    CartridgeType::MBC7 => {
                        // Stage 2 of the RAM-register unlock: exactly 0x40
                        // enables; any other value disables.
                        self.mbc7_ram_enabled2 = value == 0x40;
                    }
                    CartridgeType::HuC1 => {
                        self.huc1_ram_bank = value;
                    }
                    CartridgeType::HuC3 => {
                        self.huc3_ram_bank = value;
                    }
                    CartridgeType::PocketCamera => {
                        // Bit 4 set maps the CAM register file over
                        // A000-BFFF; otherwise the low 4 bits select a RAM
                        // bank (the bank latch is untouched while registers
                        // are selected, matching mGBA _GBPocketCam).
                        if value & 0x10 != 0 {
                            self.cam_regs_selected = true;
                        } else {
                            self.cam_regs_selected = false;
                            self.cam_ram_bank = value & 0x0F;
                        }
                    }
                    CartridgeType::Sachen { .. } => {
                        // ROM bank mask register, latched only while the
                        // inner bank register has bits 4-5 both set.
                        if (self.sachen_bank & 0x30) == 0x30 {
                            self.sachen_mask = value;
                        }
                    }
                    CartridgeType::NtOld { .. } => {
                        // Mode registers live in $5000-$5FFF, decoded by
                        // A0-A1 (mGBA _ntOldMulticart; hhugboy
                        // handleOldMakonCartModeSet). $4000-$4FFF is ignored
                        // (v2 rumble data bits are not wired to a motor
                        // here).
                        if (addr & 0xF000) == 0x5000 {
                            match addr & 0x03 {
                                0x01 => {
                                    // Multicart base, 32KB units.
                                    self.nt_base = value & 0x3F;
                                }
                                0x02 => {
                                    // High nibble $Ex declares 8KB cart RAM
                                    // (the header on these boards says none).
                                    if (value & 0xF0) == 0xE0 && self.ram_data.is_empty() {
                                        self.ram_data = vec![0xFF; 0x2000];
                                        self.ram_banks = 1;
                                    }
                                    // Low nibble selects the sub-game bank
                                    // window (bank-count mask).
                                    self.nt_bank_mask = match value & 0x0F {
                                        0x00 => 31, // 512KB
                                        0x08 => 15, // 256KB
                                        0x0C => 7,  // 128KB
                                        0x0E => 3,  // 64KB
                                        0x0F => 1,  // 32KB
                                        _ => 31,
                                    };
                                }
                                0x03 => {
                                    // Bank-line bit-swap mode (bit 4).
                                    self.nt_swapped = (value & 0x10) != 0;
                                }
                                _ => {}
                            }
                        }
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
                    CartridgeType::MBC7 => {
                        // Registers respond only with both enable stages
                        // unlocked, and only in A000-AFFF (see the read path).
                        if self.ram_enabled && self.mbc7_ram_enabled2 && addr < 0xB000 {
                            match (addr >> 4) & 0x0F {
                                0x0 => {
                                    // Erase the accelerometer latch: values
                                    // reset to 0x8000 and re-latching is
                                    // re-armed.
                                    if value == 0x55 {
                                        self.mbc7_accel_x = 0x8000;
                                        self.mbc7_accel_y = 0x8000;
                                        self.mbc7_accel_latched = false;
                                    }
                                }
                                0x1 => {
                                    // Latch the current sensor sample. Only
                                    // accepted after an erase (cannot
                                    // re-latch without erasing first).
                                    if value == 0xAA && !self.mbc7_accel_latched {
                                        self.mbc7_accel_x =
                                            Self::mbc7_accel_counts(self.mbc7_sensor_x);
                                        self.mbc7_accel_y =
                                            Self::mbc7_accel_counts(self.mbc7_sensor_y);
                                        self.mbc7_accel_latched = true;
                                    }
                                }
                                0x8 => self.mbc7_eeprom_write(value),
                                _ => {}
                            }
                        }
                    }
                    CartridgeType::HuC1 => {
                        if self.huc1_ir_mode {
                            // IR transmitter: bit 0 drives the LED ($01 on,
                            // $00 off). Latched for a future IR transport;
                            // nothing observes it yet.
                            self.huc1_ir_led = value & 0x01 != 0;
                        } else if !self.ram_data.is_empty() {
                            // RAM is always enabled (no MBC1-style gate).
                            let ram_bank = self.get_ram_bank();
                            let offset = ((addr - EXTERNAL_RAM_START) as usize
                                + (ram_bank * 0x2000))
                                % self.ram_data.len();
                            let _ = self.write_ram_byte(offset, value);
                        }
                    }
                    CartridgeType::PocketCamera => {
                        if self.cam_regs_selected {
                            // Register writes are always enabled (no RAMG
                            // gate) and mirror every $80.
                            self.cam_reg_write(addr & 0x7F, value);
                        } else if self.ram_enabled
                            && !self.cam_running
                            && !self.ram_data.is_empty()
                        {
                            // RAM writes need the $0A gate and are ignored
                            // while the capture unit is working.
                            let ram_bank = self.get_ram_bank();
                            let offset = ((addr - EXTERNAL_RAM_START) as usize
                                + (ram_bank * 0x2000))
                                % self.ram_data.len();
                            let _ = self.write_ram_byte(offset, value);
                        }
                    }
                    CartridgeType::HuC3 => {
                        match self.huc3_mode {
                            // RAM read/write. Mode 0x0 (read-only) ignores
                            // writes.
                            0xA => {
                                if !self.ram_data.is_empty() {
                                    let ram_bank = self.get_ram_bank();
                                    let offset = ((addr - EXTERNAL_RAM_START) as usize
                                        + (ram_bank * 0x2000))
                                        % self.ram_data.len();
                                    let _ = self.write_ram_byte(offset, value);
                                }
                            }
                            // RTC command/argument mailbox: command in bits
                            // 6-4, argument in bits 3-0. Writing only stores
                            // the mailbox; execution happens via the
                            // semaphore. D7 is not connected and is ignored.
                            0xB => {
                                self.huc3_rtc_command = (value >> 4) & 0x07;
                                self.huc3_rtc_argument = value & 0x0F;
                            }
                            // RTC semaphore: writing with bit 0 clear requests
                            // that the MCU execute the pending command.
                            0xD => {
                                if value & 0x01 == 0 {
                                    self.huc3_execute_command();
                                }
                            }
                            // 0xC is read-only; 0xE is the IR transmitter
                            // (stubbed: no receiver on the other end); other
                            // select values are unmapped.
                            _ => {}
                        }
                    }
                    CartridgeType::NoMBC { .. } => {
                        // Straight-through RAM, no enable gate (see the read
                        // path). Battery variants ($09) stream to the .sav.
                        if !self.ram_data.is_empty() {
                            let offset =
                                (addr - EXTERNAL_RAM_START) as usize % self.ram_data.len();
                            let _ = self.write_ram_byte(offset, value);
                        }
                    }
                    // Straight-through, ungated (see the read path).
                    CartridgeType::Rocket | CartridgeType::Sachen { .. } => {
                        if !self.ram_data.is_empty() {
                            let offset =
                                (addr - EXTERNAL_RAM_START) as usize % self.ram_data.len();
                            let _ = self.write_ram_byte(offset, value);
                        }
                    }
                    // MBC3-style enable gate, unbanked.
                    CartridgeType::NtOld { .. } => {
                        if self.ram_enabled && !self.ram_data.is_empty() {
                            let offset =
                                (addr - EXTERNAL_RAM_START) as usize % self.ram_data.len();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Addressable;

    /// Minimal in-memory ROM image with the given type/RAM-size header bytes.
    fn make_rom(cartridge_type: u8, ram_size_code: u8) -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[CARTRIDGE_TYPE_OFFSET] = cartridge_type;
        rom[ROM_SIZE_OFFSET] = 0x00;
        rom[RAM_SIZE_OFFSET] = ram_size_code;
        rom
    }

    const NINTENDO_LOGO: [u8; 48] = [
        0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83,
        0x00, 0x0C, 0x00, 0x0D, 0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E,
        0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99, 0xBB, 0xBB, 0x67, 0x63,
        0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E,
    ];

    /// Sized ROM with a bank-index marker at offset 0x1000 of every 16KB bank.
    fn make_sized_rom(cartridge_type: u8, rom_size_code: u8, size: usize) -> Vec<u8> {
        let mut rom = vec![0u8; size];
        rom[CARTRIDGE_TYPE_OFFSET] = cartridge_type;
        rom[ROM_SIZE_OFFSET] = rom_size_code;
        for bank in 0..(size / 0x4000) {
            rom[bank * 0x4000 + 0x1000] = bank as u8;
        }
        rom
    }

    #[test]
    fn licensed_shapes_are_not_misdetected() {
        // Plain 32KB ROM-only cart with the Nintendo logo (e.g. Tetris).
        let mut rom = make_rom(0x00, 0x00);
        rom[0x104..0x134].copy_from_slice(&NINTENDO_LOGO);
        rom[0x134..0x13A].copy_from_slice(b"TETRIS");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);

        // 128KB MBC1 cart titled GAME (the shape of the owner's descrambled
        // Sachen singles): must stay plain MBC1.
        let mut rom = make_sized_rom(0x01, 0x02, 0x20000);
        rom[0x104..0x134].copy_from_slice(&NINTENDO_LOGO);
        rom[0x134..0x138].copy_from_slice(b"GAME");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);
        let cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(cart.get_cartridge_type(), CartridgeType::MBC1 { .. }));

        // Header claims 32KB but the file is 2MB with a normal logo
        // (gbmicrotest shape, type $03): still MBC1, never Wisdom Tree.
        let mut rom = make_sized_rom(0x03, 0x00, 0x200000);
        rom[0x104..0x134].copy_from_slice(&NINTENDO_LOGO);
        rom[0x134..0x13D].copy_from_slice(b"microtest");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);

        // A real 256KB MBC3+RAM+BATTERY ($10) cart NOT titled "TETRIS SET"
        // must stay MBC3 -- M161 detection is gated on the exact title.
        let mut rom = make_sized_rom(0x10, 0x03, 0x40000);
        rom[0x104..0x134].copy_from_slice(&NINTENDO_LOGO);
        rom[0x134..0x13B].copy_from_slice(b"POKEMON");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);
        assert!(matches!(
            Cartridge::from_bytes(&rom).unwrap().get_cartridge_type(),
            CartridgeType::MBC3 { .. }
        ));
    }

    #[test]
    fn m161_latches_a_32kb_bank_once() {
        // Mani 4 in 1 shape: 256KB, header spoofs MBC3+RAM+BAT ($10), title
        // "TETRIS SET" (gambatte presumedM161).
        let mut rom = make_sized_rom(0x10, 0x03, 0x40000);
        rom[0x134..0x13E].copy_from_slice(b"TETRIS SET");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::M161);

        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(cart.get_cartridge_type(), CartridgeType::M161));
        assert!(!cart.has_battery()); // gambatte disabledRam + zeroed header

        // Power-on (unmapped): the first 32KB pair -> 16KB banks 0 and 1.
        assert_eq!(cart.read(0x1000), 0);
        assert_eq!(cart.read(0x5000), 1);
        // External RAM line is permanently disabled.
        assert_eq!(cart.read(0xA000), 0xFF);

        // First ROM write anywhere in $0000-$7FFF latches the 32KB bank from
        // data bits 0-2: value 3 -> even/odd 16KB banks 6/7.
        cart.write(0x2000, 0x03);
        assert_eq!(cart.read(0x1000), 6);
        assert_eq!(cart.read(0x5000), 7);

        // Every later write is ignored until reset (one-shot latch).
        cart.write(0x6000, 0x01);
        assert_eq!(cart.read(0x1000), 6);
        assert_eq!(cart.read(0x5000), 7);

        // Bank 7 (data & 7) selects the top 32KB pair (banks 14/15).
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        cart.write(0x0000, 0xFF); // low 3 bits = 7; upper bits ignored
        assert_eq!(cart.read(0x1000), 14);
        assert_eq!(cart.read(0x5000), 15);
    }

    #[test]
    fn wisdom_tree_detects_and_switches_whole_window() {
        // Exodus shape: type $00, header claims 32KB, 128KB file, publisher
        // string in the ROM.
        let mut rom = make_sized_rom(0x00, 0x00, 0x20000);
        rom[0x104..0x134].copy_from_slice(&NINTENDO_LOGO);
        rom[0x300..0x30B].copy_from_slice(b"WISDOM TREE");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::WisdomTree);

        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        // Power-on: 32KB bank 0 across the whole window.
        assert_eq!(cart.read(0x1000), 0);
        assert_eq!(cart.read(0x5000), 1);
        // Bank select = ADDRESS low bits of any $0000-$3FFF write; the data
        // byte is ignored.
        cart.write(0x0003, 0xA5);
        assert_eq!(cart.read(0x1000), 6); // 16KB banks 6/7 = 32KB bank 3
        assert_eq!(cart.read(0x5000), 7);
        // Out-of-range bank wraps on the wired lines (128KB = 4 x 32KB).
        cart.write(0x0005, 0x00);
        assert_eq!(cart.read(0x1000), 2); // bank 5 % 4 = 1 -> 16KB banks 2/3
        assert_eq!(cart.read(0x5000), 3);

        // The Pan Docs $C0/$D1 header magic alone also detects.
        let mut rom = make_sized_rom(0x00, 0x00, 0x10000);
        rom[0x147] = 0xC0;
        rom[0x14A] = 0xD1;
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::WisdomTree);
    }

    #[test]
    fn rocket_games_registers_and_boot_lock() {
        // Real Rocket carts store their own logo = Nintendo ^ pad (sums to
        // 2756), which is what the detection keys on.
        let mut rom = make_sized_rom(0x97, 0x04, 0x80000);
        for i in 0..48 {
            rom[0x104 + i] = NINTENDO_LOGO[i] ^ ROCKET_LOGO_XOR[i];
        }
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::Rocket);

        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        cart.skip_boot_handoff(); // no boot ROM: start unlocked
        // Unlocked reads return the raw (Rocket) logo.
        assert_eq!(cart.read(0x0104), NINTENDO_LOGO[0] ^ ROCKET_LOGO_XOR[0]);
        // Inner bank at exactly $3F00 (0 -> 1), outer 256KB bank at $3FC0.
        assert_eq!(cart.read(0x5000), 1);
        cart.write(0x3F00, 0x05);
        assert_eq!(cart.read(0x5000), 5);
        cart.write(0x3F00, 0x00);
        assert_eq!(cart.read(0x5000), 1);
        cart.write(0x3FC0, 0x01);
        assert_eq!(cart.read(0x1000), 16); // outer bank alone at $0000
        assert_eq!(cart.read(0x5000), 17); // outer | inner at $4000
        // Writes elsewhere in the region are ignored.
        cart.write(0x2000, 0x07);
        assert_eq!(cart.read(0x5000), 17);

        // Boot lock: a fresh cart is locked; after 0x30 ROM reads it enters
        // the CGB phase where $0104-$0133 reads are XOR-decoded to the
        // Nintendo logo (the boot ROM check), and after 0x30 more it unlocks.
        let cart = Cartridge::from_bytes(&rom).unwrap();
        for _ in 0..0x31 {
            cart.read(0x0000);
        }
        assert_eq!(cart.read(0x0104), NINTENDO_LOGO[0]);
        assert_eq!(cart.read(0x0105), NINTENDO_LOGO[1]);
        for _ in 0..0x31 {
            cart.read(0x0000);
        }
        assert_eq!(cart.read(0x0104), NINTENDO_LOGO[0] ^ ROCKET_LOGO_XOR[0]);
    }

    #[test]
    fn sachen_mmc1_descramble_lock_and_masked_banking() {
        // Raw-dump shape: the Nintendo logo lives at the DESCRAMBLED
        // positions of $0104 (CPU reads through the $01xx address swizzle),
        // and the Sachen logo (here: marker bytes) at the |0x80 copy.
        let mut rom = make_sized_rom(0x00, 0x00, 0x20000);
        for i in 0..48u16 {
            rom[Cartridge::sachen_unscramble(0x104 + i) as usize] = NINTENDO_LOGO[i as usize];
            rom[Cartridge::sachen_unscramble(0x184 + i) as usize] = 0xB0 | (i as u8 & 0x0F);
        }
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::SachenMmc1);

        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        // The boot-logo override presents the locked view (Sachen logo).
        let logo = cart.boot_logo_override().unwrap();
        assert_eq!(logo[0], 0xB0);
        assert_eq!(logo[47], 0xB0 | (47 & 0x0F));

        // Locked (power-on): $01xx reads are forced to the |0x80 copy. The
        // 0x31st such read unlocks.
        for i in 0..0x30u16 {
            assert_eq!(cart.read(0x0104 + i), 0xB0 | (i as u8 & 0x0F));
        }
        // Unlock transition read, then the descrambled Nintendo logo is
        // visible at $0104.
        cart.read(0x0104);
        assert_eq!(cart.read(0x0104), NINTENDO_LOGO[0]);
        assert_eq!(cart.read(0x0105), NINTENDO_LOGO[1]);
        assert_eq!(cart.read(0x0133), NINTENDO_LOGO[47]);

        // Masked outer banking (hhugboy/mGBA): base/mask only latch while
        // the inner bank has bits 4-5 set; effective switchable bank =
        // base&mask | bank&~mask, base window = base&mask.
        cart.write(0x2000, 0x33); // open the latch gate
        cart.write(0x0000, 0x04); // base
        cart.write(0x4000, 0x04); // mask
        cart.write(0x2000, 0x03); // inner bank (gate now closed)
        cart.write(0x0000, 0x00); // ignored: gate closed
        assert_eq!(cart.read(0x1000), 4); // base & mask
        assert_eq!(cart.read(0x5000), 7); // 4 | 3
        // skip_boot_handoff unlocks immediately (no boot ROM).
        let mut fresh = Cartridge::from_bytes(&rom).unwrap();
        fresh.skip_boot_handoff();
        assert_eq!(fresh.read(0x0104), NINTENDO_LOGO[0]);
    }

    #[test]
    fn nt_old2_swap_multicart_and_ram_declare() {
        // Super Mario Special 3 shape: MBC1-spoofing header, Makon "MK"
        // licensee, 256KB.
        let mut rom = make_sized_rom(0x01, 0x03, 0x40000);
        rom[0x104..0x134].copy_from_slice(&NINTENDO_LOGO);
        rom[0x134..0x141].copy_from_slice(b"SUPER MARIO 3");
        rom[0x144] = b'M';
        rom[0x145] = b'K';
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::NtOld2);

        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        // MBC3-style 8-bit bank, 0 -> 1.
        cart.write(0x2000, 0x05);
        assert_eq!(cart.read(0x5000), 5);
        // $5003 bit-swap mode: bank lines reorder combinationally
        // (mGBA _ntOld2Reorder: out0=in1, out1=in2, out2=in0).
        cart.write(0x5003, 0x10);
        assert_eq!(cart.read(0x5000), 6); // reorder(5) = 0b110
        cart.write(0x5003, 0x00);
        assert_eq!(cart.read(0x5000), 5);
        // $5001 multicart base (32KB units) offsets both windows; $5002 low
        // nibble masks the bank window.
        cart.write(0x5001, 0x02);
        cart.write(0x5002, 0x0C); // 128KB window -> mask 7
        cart.write(0x2000, 0x09);
        assert_eq!(cart.read(0x1000), 4); // base bank
        assert_eq!(cart.read(0x5000), 4 + 1); // (9 & 7) + base
        // $5002 high-nibble $Ex declares 8KB RAM on a header that lists none.
        assert!(cart.ram_data.is_empty());
        cart.write(0x5002, 0xE8);
        assert_eq!(cart.ram_data.len(), 0x2000);
        cart.write(0x0000, 0x0A); // MBC3-style enable
        cart.write(0xA123, 0x77);
        assert_eq!(cart.read(0xA123), 0x77);
        cart.write(0x0000, 0x00);
        assert_eq!(cart.read(0xA123), 0xFF);
    }

    #[test]
    fn force_mbc1_header_liars_load_and_bank() {
        // Sonic 3D Blast 5 shape: type $EA and garbage RAM size $20 (game
        // code overlaps the header), 256KB file with a 32KB size byte. Must
        // LOAD (no invalid-RAM error) and bank as plain MBC1 sized from the
        // file.
        let mut rom = make_sized_rom(0xEA, 0x00, 0x40000);
        rom[RAM_SIZE_OFFSET] = 0x20;
        rom[0x104..0x134].copy_from_slice(&NINTENDO_LOGO);
        rom[0x134..0x13A].copy_from_slice(b"SONIC5");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::ForceMbc1);
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(cart.get_cartridge_type(), CartridgeType::MBC1 { ram: false, battery: false }));
        assert!(cart.ram_data.is_empty());
        cart.write(0x2000, 0x0B);
        assert_eq!(cart.read(0x5000), 11);

        // Captain Knick-Knack: type $00 with a Tetris header on a 128KB file.
        let mut rom = make_sized_rom(0x00, 0x00, 0x20000);
        rom[0x104..0x134].copy_from_slice(&NINTENDO_LOGO);
        rom[0x134..0x13A].copy_from_slice(b"TETRIS");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::ForceMbc1);
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        cart.write(0x2000, 0x07);
        assert_eq!(cart.read(0x5000), 7);
    }

    fn mbc3_rtc_cart() -> Cartridge {
        Cartridge::from_bytes(&make_rom(MBC3_TIMER_RAM_BATTERY, 0x03)).unwrap()
    }

    /// Pan Docs MBC5: on rumble carts bit 3 of the $4000-$5FFF RAM-bank write
    /// drives the motor. `rumble_active()` is what the libretro frontend polls
    /// each frame; plain MBC5 carts must never report a motor.
    #[test]
    fn mbc5_rumble_latch_via_bus() {
        let mut cart = Cartridge::from_bytes(&make_rom(MBC5_RUMBLE_RAM, 0x03)).unwrap();
        assert!(cart.has_rumble());
        assert!(!cart.rumble_active());
        cart.write(0x4000, 0x08);
        assert!(cart.rumble_active());
        cart.write(0x5FFF, 0x07); // bank bits only, motor off
        assert!(!cart.rumble_active());

        let mut plain = Cartridge::from_bytes(&make_rom(MBC5_RAM, 0x03)).unwrap();
        assert!(!plain.has_rumble());
        plain.write(0x4000, 0x08);
        assert!(!plain.rumble_active());
    }

    fn huc3_cart() -> Cartridge {
        Cartridge::from_bytes(&make_rom(HUC3, 0x03)).unwrap()
    }

    fn set_mbc3_rtc(cart: &mut Cartridge, regs: (u8, u8, u8, u8, u8)) {
        cart.rtc_seconds = regs.0;
        cart.rtc_minutes = regs.1;
        cart.rtc_hours = regs.2;
        cart.rtc_days_low = regs.3;
        cart.rtc_days_high = regs.4;
    }

    fn mbc3_rtc(cart: &Cartridge) -> (u8, u8, u8, u8, u8) {
        (
            cart.rtc_seconds,
            cart.rtc_minutes,
            cart.rtc_hours,
            cart.rtc_days_low,
            cart.rtc_days_high,
        )
    }

    /// The closed-form catch-up must be bit-exact with iterating the
    /// per-second cascade, including the out-of-range 6/5-bit register bands
    /// (values 60-63 / 24-31 wrap to 0 without a carry) and the day-counter
    /// overflow latch.
    #[test]
    fn mbc3_catch_up_matches_iterative_cascade() {
        let states = [
            (0u8, 0u8, 0u8, 0u8, 0u8),
            (59, 59, 23, 0xFF, 0x01),
            (59, 59, 23, 0xFF, 0x41), // halted flag preserved (advance ignores it)
            (60, 0, 0, 0, 0),         // out-of-range seconds
            (63, 63, 31, 0xFE, 0x01), // everything out-of-range near wrap
            (30, 61, 25, 0x80, 0x80), // carry already latched stays latched
            (1, 2, 3, 4, 0xC1),
        ];
        let ns = [
            0u64, 1, 2, 59, 60, 61, 119, 3599, 3600, 3661, 86399, 86400, 86401, 2 * 86400 + 123,
            1_000_000,
        ];
        for &state in &states {
            for &n in &ns {
                let mut iter_cart = mbc3_rtc_cart();
                set_mbc3_rtc(&mut iter_cart, state);
                for _ in 0..n {
                    iter_cart.advance_rtc_second();
                }

                let mut closed_cart = mbc3_rtc_cart();
                set_mbc3_rtc(&mut closed_cart, state);
                closed_cart.mbc3_rtc_advance_seconds(n);

                assert_eq!(
                    mbc3_rtc(&iter_cart),
                    mbc3_rtc(&closed_cart),
                    "state {state:?} + {n}s"
                );
            }
        }
    }

    #[test]
    fn mbc3_rtc_blob_round_trips() {
        let mut cart = mbc3_rtc_cart();
        set_mbc3_rtc(&mut cart, (61, 5, 17, 0xAB, 0xC1)); // incl. out-of-range seconds
        cart.rtc_seconds_latched = 33;
        cart.rtc_minutes_latched = 44;
        cart.rtc_hours_latched = 12;
        cart.rtc_days_low_latched = 0x12;
        cart.rtc_days_high_latched = 0x81;

        let blob = cart.mbc3_rtc_serialize(0x0102_0304_0506_0708);
        assert_eq!(blob.len(), 48);
        // Spot-check the documented layout: LE u32 fields in VBA-M order.
        assert_eq!(&blob[0..4], &[61, 0, 0, 0]);
        assert_eq!(&blob[16..20], &[0xC1, 0, 0, 0]);
        assert_eq!(&blob[20..24], &[33, 0, 0, 0]);
        assert_eq!(&blob[40..48], &0x0102_0304_0506_0708u64.to_le_bytes());

        let mut restored = mbc3_rtc_cart();
        let ts = restored.mbc3_rtc_deserialize(&blob).unwrap();
        assert_eq!(ts, 0x0102_0304_0506_0708);
        assert_eq!(mbc3_rtc(&restored), (61, 5, 17, 0xAB, 0xC1));
        assert_eq!(restored.rtc_seconds_latched, 33);
        assert_eq!(restored.rtc_days_high_latched, 0x81);
    }

    /// The legacy 44-byte variant (32-bit timestamp, old VBA builds) must be
    /// accepted, mirroring VBA-M's -4 read leeway / mGBA's sizeof-4 check.
    #[test]
    fn mbc3_rtc_blob_accepts_legacy_44_bytes() {
        let mut cart = mbc3_rtc_cart();
        set_mbc3_rtc(&mut cart, (10, 20, 15, 0x55, 0x00));
        let mut blob = cart.mbc3_rtc_serialize(0).to_vec();
        blob.truncate(44);
        blob[40..44].copy_from_slice(&0xCAFE_F00Du32.to_le_bytes());

        let mut restored = mbc3_rtc_cart();
        let ts = restored.mbc3_rtc_deserialize(&blob).unwrap();
        assert_eq!(ts, 0xCAFE_F00D);
        assert_eq!(mbc3_rtc(&restored), (10, 20, 15, 0x55, 0x00));
    }

    #[test]
    fn mbc3_catch_up_respects_halt() {
        let mut cart = mbc3_rtc_cart();
        set_mbc3_rtc(&mut cart, (5, 6, 7, 8, 0x40));
        cart.rtc_catch_up(86_400);
        assert_eq!(mbc3_rtc(&cart), (5, 6, 7, 8, 0x40));
    }

    #[test]
    fn huc3_rtc_blob_round_trips_with_nibble_packing() {
        let mut cart = huc3_cart();
        cart.huc3_set_clock(0x2A5, 0x123);
        cart.huc3_rtc_mem[0x58] = 0x7; // event-time nibble
        let blob = cart.huc3_rtc_serialize(0xDEAD_BEEF);
        assert_eq!(blob.len(), 136);
        // mGBA packing: nibble N -> byte N/2, even N in the low half. Minutes
        // 0x2A5 -> nibbles 0x10=0x5, 0x11=0xA, 0x12=0x2; days 0x123 ->
        // 0x13=0x3. Byte 8 = nib 0x10|0x11<<4, byte 9 = nib 0x12|0x13<<4.
        assert_eq!(blob[0x08], 0xA5);
        assert_eq!(blob[0x09], 0x32);
        let mut restored = huc3_cart();
        let ts = restored.huc3_rtc_deserialize(&blob).unwrap();
        assert_eq!(ts, 0xDEAD_BEEF);
        assert_eq!(restored.huc3_clock(), (0x2A5, 0x123));
        assert_eq!(restored.huc3_rtc_mem[0x58], 0x7);
    }

    /// Closed-form HuC-3 minute catch-up == iterating the per-minute tick,
    /// across midnight and 12-bit day-counter wraps.
    #[test]
    fn huc3_catch_up_matches_iterative_tick() {
        let states = [(0u16, 0u16), (1439, 0), (1438, 0xFFF), (720, 0x7FF), (1500, 5)];
        let ns = [0u64, 1, 2, 1439, 1440, 1441, 3 * 1440 + 7, 100_000];
        for &(minutes, days) in &states {
            for &n in &ns {
                let mut iter_cart = huc3_cart();
                iter_cart.huc3_set_clock(minutes, days);
                for _ in 0..n {
                    let (mut m, mut d) = iter_cart.huc3_clock();
                    m += 1;
                    if m >= 1440 {
                        m = 0;
                        d = (d + 1) & 0x0FFF;
                    }
                    iter_cart.huc3_set_clock(m, d);
                }

                let mut closed_cart = huc3_cart();
                closed_cart.huc3_set_clock(minutes, days);
                closed_cart.huc3_rtc_advance_minutes(n);

                assert_eq!(
                    iter_cart.huc3_clock(),
                    closed_cart.huc3_clock(),
                    "clock ({minutes},{days}) + {n}min"
                );
            }
        }
    }

    /// End-to-end sidecar flow on the disk-load path: a fresh load creates
    /// the `.rtc`; a reload after back-dating its timestamp catches the clock
    /// up by the elapsed wall time; a halted clock stays put.
    #[test]
    fn rtc_sidecar_round_trip_with_wall_clock_catch_up() {
        let dir = std::env::temp_dir().join(format!(
            "rustyboi-rtc-test-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir_all(&dir).unwrap();
        let rom_path = dir.join("game.gb");
        fs::write(&rom_path, make_rom(MBC3_TIMER_RAM_BATTERY, 0x03)).unwrap();
        let rom_path_str = rom_path.to_str().unwrap();
        let rtc_path = dir.join("game.rtc");

        {
            let cart = Cartridge::load(rom_path_str).unwrap();
            assert_eq!(mbc3_rtc(&cart), (0, 0, 0, 0, 0));
        }
        assert_eq!(fs::read(&rtc_path).unwrap().len(), 48);

        // Back-date: registers (5,0,0), saved one hour ago.
        let mut cart = mbc3_rtc_cart();
        set_mbc3_rtc(&mut cart, (5, 0, 0, 0, 0));
        let before = Cartridge::unix_now();
        fs::write(&rtc_path, cart.mbc3_rtc_serialize(before - 3600)).unwrap();

        let cart = Cartridge::load(rom_path_str).unwrap();
        let (s, m, h, dl, dh) = mbc3_rtc(&cart);
        let total = s as u64 + m as u64 * 60 + h as u64 * 3600;
        let elapsed_max = 3600 + (Cartridge::unix_now() - before) + 1;
        assert!(
            (3605..=5 + elapsed_max).contains(&total),
            "expected ~1h subsequent catch-up, got {}s ({s} {m} {h})",
            total
        );
        assert_eq!((dl, dh), (0, 0));

        // Halted clock: no catch-up applied.
        let mut cart = mbc3_rtc_cart();
        set_mbc3_rtc(&mut cart, (7, 8, 9, 1, 0x40));
        fs::write(&rtc_path, cart.mbc3_rtc_serialize(Cartridge::unix_now() - 86_400)).unwrap();
        let cart = Cartridge::load(rom_path_str).unwrap();
        assert_eq!(mbc3_rtc(&cart), (7, 8, 9, 1, 0x40));

        fs::remove_dir_all(&dir).unwrap();
    }

    /// A `.sav` with a VBA-M/mGBA RTC footer (RAM + 48 bytes) restores both
    /// the RAM prefix and the clock when no `.rtc` sidecar exists yet.
    #[test]
    fn sav_rtc_footer_import() {
        let dir = std::env::temp_dir().join(format!(
            "rustyboi-footer-test-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir_all(&dir).unwrap();
        let rom_path = dir.join("game.gb");
        fs::write(&rom_path, make_rom(MBC3_TIMER_RAM_BATTERY, 0x03)).unwrap();

        let mut donor = mbc3_rtc_cart();
        set_mbc3_rtc(&mut donor, (11, 22, 13, 0x44, 0x01));
        let mut sav = vec![0x5A; 32 * 1024];
        sav.extend_from_slice(&donor.mbc3_rtc_serialize(Cartridge::unix_now()));
        fs::write(dir.join("game.sav"), &sav).unwrap();

        let mut cart = Cartridge::load(rom_path.to_str().unwrap()).unwrap();
        // RAM prefix loaded (footer not spilled into RAM).
        assert_eq!(cart.ram_data[0], 0x5A);
        assert_eq!(cart.ram_data.len(), 32 * 1024);
        // Clock restored (catch-up window: allow a couple of live seconds).
        let (s, m, h, dl, dh) = mbc3_rtc(&cart);
        assert!(s >= 11 && s <= 13, "seconds {s}");
        assert_eq!((m, h, dl, dh), (22, 13, 0x44, 0x01));
        // Sidecar was created and now wins over the footer.
        assert!(dir.join("game.rtc").exists());

        // The live RAM write path still streams to the .sav without
        // clobbering the (read-only) footer.
        cart.write(0x0000, 0x0A);
        cart.write(0x4000, 0x00);
        cart.write(0xA000, 0x77);
        let sav_after = fs::read(dir.join("game.sav")).unwrap();
        assert_eq!(sav_after.len(), sav.len());
        assert_eq!(sav_after[0], 0x77);
        assert_eq!(&sav_after[32 * 1024..], &sav[32 * 1024..]);

        fs::remove_dir_all(&dir).unwrap();
    }

    /// The libretro RETRO_MEMORY_RTC region: stable pointer, mGBA-format
    /// content, and external writes are adopted with catch-up on the next
    /// frame sync.
    #[test]
    fn libretro_rtc_memory_sync_adopts_external_writes() {
        let mut cart = mbc3_rtc_cart();
        let ptr_before = cart.rtc_memory_mut().as_ptr();
        assert_eq!(cart.rtc_memory_mut().len(), 48);

        // Simulate RetroArch memcpying a `.rtc` file into the region:
        // registers (9,0,0), saved two minutes ago.
        let mut donor = mbc3_rtc_cart();
        set_mbc3_rtc(&mut donor, (9, 0, 0, 0, 0));
        let before = Cartridge::unix_now();
        let blob = donor.mbc3_rtc_serialize(before - 120);
        // The frontend writes through its cached raw pointer; poking the
        // buffer directly models that (bypassing the refresh in
        // rtc_memory_mut).
        cart.rtc_memory.copy_from_slice(&blob);

        cart.rtc_memory_frame_sync();
        let (s, m, h, _, _) = mbc3_rtc(&cart);
        let total = s as u64 + m as u64 * 60 + h as u64 * 3600;
        let elapsed_max = 120 + (Cartridge::unix_now() - before) + 1;
        assert!(
            (129..=9 + elapsed_max).contains(&total),
            "expected ~2min catch-up, got {total}s"
        );
        assert_eq!(cart.rtc_memory_mut().as_ptr(), ptr_before);

        // Idle frames (no external write) leave the clock alone.
        let regs = mbc3_rtc(&cart);
        cart.rtc_memory_frame_sync();
        assert_eq!(mbc3_rtc(&cart), regs);
    }

    /// HuC-3 carts expose the 136-byte mGBA blob through the libretro view.
    #[test]
    fn libretro_rtc_memory_huc3_shape() {
        let mut cart = huc3_cart();
        cart.huc3_set_clock(100, 2);
        let mem = cart.rtc_memory_mut();
        assert_eq!(mem.len(), 136);
        // Non-RTC carts expose nothing.
        let mut plain = Cartridge::from_bytes(&make_rom(MBC1, 0x02)).unwrap();
        assert!(plain.rtc_memory_mut().is_empty());
    }

    /// HuC-1 image shaped like Pokemon Card GB: 1MB ROM (64 banks) with a
    /// marker byte per bank, 32KB RAM (4 banks).
    fn huc1_cart() -> Cartridge {
        let mut rom = vec![0u8; 64 * 0x4000];
        rom[CARTRIDGE_TYPE_OFFSET] = HUC1_RAM_BATTERY;
        rom[ROM_SIZE_OFFSET] = 0x05;
        rom[RAM_SIZE_OFFSET] = 0x03;
        for bank in 0..64 {
            rom[bank * 0x4000 + 0x100] = bank as u8;
        }
        Cartridge::from_bytes(&rom).unwrap()
    }

    #[test]
    fn huc1_rom_banking_is_6_bit_with_bank_0_selectable() {
        let mut cart = huc1_cart();
        assert_eq!(cart.read(0x4100), 1); // power-on default bank 1
        cart.write(0x2000, 0x05);
        assert_eq!(cart.read(0x4100), 5);
        // Bank 0 has no zero->one remap on HuC-1.
        cart.write(0x2000, 0x00);
        assert_eq!(cart.read(0x4100), 0);
        // Only 6 bits are wired: 0x7F decodes as bank 0x3F.
        cart.write(0x2000, 0x7F);
        assert_eq!(cart.read(0x4100), 0x3F);
        // Fixed bank 0 at 0000-3FFF regardless.
        assert_eq!(cart.read(0x0100), 0);
    }

    #[test]
    fn huc1_ram_is_always_enabled_and_banked() {
        let mut cart = huc1_cart();
        // No 0x0A enable write anywhere: RAM must respond immediately.
        cart.write(0xA000, 0x42);
        assert_eq!(cart.read(0xA000), 0x42);
        // Bank switch via 4000-5FFF.
        cart.write(0x4000, 0x01);
        assert_eq!(cart.read(0xA000), 0xFF); // untouched cell in bank 1
        cart.write(0xA000, 0x77);
        assert_eq!(cart.read(0xA000), 0x77);
        cart.write(0x4000, 0x00);
        assert_eq!(cart.read(0xA000), 0x42); // bank 0 cell intact
        assert!(cart.has_battery());
    }

    #[test]
    fn huc1_ir_mode_switches_a000_region() {
        let mut cart = huc1_cart();
        cart.write(0xA000, 0x42);
        // Low nibble 0xE selects IR mode; reads see "no light" and writes
        // drive the LED instead of RAM.
        cart.write(0x0000, 0x0E);
        assert_eq!(cart.read(0xA000), 0xC0);
        cart.write(0xA000, 0x01);
        assert!(cart.huc1_ir_led);
        cart.write(0xA000, 0x00);
        assert!(!cart.huc1_ir_led);
        // Anything else selects RAM mode again; RAM was not disturbed.
        cart.write(0x0000, 0x00);
        assert_eq!(cart.read(0xA000), 0x42);
        // 0x0A (a plain MBC RAM-enable value) is RAM mode too, not IR.
        cart.write(0x0000, 0x0A);
        assert_eq!(cart.read(0xA000), 0x42);
    }

    #[test]
    fn nombc_ram_is_wired_straight_through() {
        // $08 ROM+RAM, 8KB: reads/writes hit RAM directly, no enable gate.
        let mut cart = Cartridge::from_bytes(&make_rom(ROM_RAM, 0x02)).unwrap();
        assert!(!cart.has_battery());
        cart.write(0xA000, 0x77);
        assert_eq!(cart.read(0xA000), 0x77);
        cart.write(0xBFFF, 0x12);
        assert_eq!(cart.read(0xBFFF), 0x12);

        // $09 adds the battery.
        let cart = Cartridge::from_bytes(&make_rom(ROM_RAM_BATTERY, 0x02)).unwrap();
        assert!(cart.has_battery());

        // $00 ROM ONLY with no header RAM keeps floating reads.
        let mut cart = Cartridge::from_bytes(&make_rom(0x00, 0x00)).unwrap();
        cart.write(0xA000, 0x77);
        assert_eq!(cart.read(0xA000), 0xFF);
    }

    #[test]
    fn nombc_2kb_ram_mirrors_across_the_window() {
        // $08 ROM+RAM with RAM-size $01 = a 2KB chip: it decodes only A0-A10,
        // so the 2KB repeats 4x across $A000-$BFFF (Pan Docs "No MBC").
        let mut cart = Cartridge::from_bytes(&make_rom(ROM_RAM, 0x01)).unwrap();
        cart.write(0xA000, 0x11);
        cart.write(0xA123, 0x22);
        // Every 2KB-offset alias of $A000 / $A123 reads the same cell.
        for base in [0xA000u16, 0xA800, 0xB000, 0xB800] {
            assert_eq!(cart.read(base), 0x11, "mirror of A000 at {base:04X}");
            assert_eq!(cart.read(base + 0x123), 0x22, "mirror of A123 at {base:04X}");
        }
        // Writing through a high alias lands in the same physical cell.
        cart.write(0xB800, 0x33);
        assert_eq!(cart.read(0xA000), 0x33);

        // Contrast: an 8KB chip ($02) does NOT mirror -- A800 is its own cell.
        let mut cart = Cartridge::from_bytes(&make_rom(ROM_RAM, 0x02)).unwrap();
        cart.write(0xA000, 0x11);
        assert_eq!(cart.read(0xA800), 0xFF);
    }

    /// The completeness-audit repro, end to end through the CPU/bus: a type
    /// $08 micro-ROM stores $77 to $A000, reads it back and parks it in HRAM
    /// ($FF80). Previously the NoMBC arm returned $FF.
    #[test]
    fn nombc_ram_micro_rom_via_cpu() {
        let mut rom = make_rom(ROM_RAM, 0x02);
        // 0x100: nop; jp 0x0150
        rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
        rom[0x150..0x15E].copy_from_slice(&[
            0x3E, 0x77, // ld a, $77
            0xEA, 0x00, 0xA0, // ld ($A000), a
            0x3E, 0x00, // ld a, $00
            0xFA, 0x00, 0xA0, // ld a, ($A000)
            0xE0, 0x80, // ldh ($80), a
            0x18, 0xFE, // jr -2 (spin)
        ]);
        let cart = Cartridge::from_bytes(&rom).unwrap();
        let mut gb = crate::gb::GB::new(crate::gb::Hardware::DMG);
        gb.insert(cart);
        gb.skip_bios();
        // Two frames: skip_bios hands off near the end of a frame, so the
        // first frame_ready() fires after only a handful of instructions.
        gb.run_until_frame(false);
        gb.run_until_frame(false);
        assert_eq!(gb.read_memory(0xFF80), 0x77);
    }

    /// POCKET CAMERA image shaped like the real cart: 1MB ROM (64 banks)
    /// with a marker byte per bank, 128KB RAM (16 banks).
    fn camera_cart() -> Cartridge {
        let mut rom = vec![0u8; 64 * 0x4000];
        rom[CARTRIDGE_TYPE_OFFSET] = POCKET_CAMERA;
        rom[ROM_SIZE_OFFSET] = 0x05;
        rom[RAM_SIZE_OFFSET] = 0x04;
        for bank in 0..64 {
            rom[bank * 0x4000 + 0x100] = bank as u8;
        }
        Cartridge::from_bytes(&rom).unwrap()
    }

    /// Program a usable capture configuration: 2D enhancement (the ROM's
    /// shooting mode), mid exposure, and a flat $80 threshold matrix.
    fn camera_configure(cart: &mut Cartridge) {
        cart.write(0x4000, 0x10); // select CAM registers
        cart.write(0xA001, 0xE8); // N=1 VH=3 gain
        cart.write(0xA002, 0x08); // exposure MSB
        cart.write(0xA003, 0x00); // exposure LSB
        cart.write(0xA004, 0x24); // E3=0 alpha=1.00 I=0 V=4
        cart.write(0xA005, 0x3F); // zero point / Vref (analog only)
        for i in 0..48u16 {
            cart.write(0xA006 + i, 0x80);
        }
    }

    #[test]
    fn pocket_camera_rom_banking_is_6_bit_with_bank_0_selectable() {
        let mut cart = camera_cart();
        assert!(cart.has_camera() && cart.has_battery() && !cart.has_rtc());
        assert_eq!(cart.read(0x4100), 1); // power-on default bank 1
        cart.write(0x2000, 0x3F);
        assert_eq!(cart.read(0x4100), 0x3F);
        // Bank 0 is selectable (no zero->one remap), and only 6 bits wired.
        cart.write(0x2000, 0x40);
        assert_eq!(cart.read(0x4100), 0);
        assert_eq!(cart.read(0x0100), 0); // fixed bank 0 at 0000-3FFF
    }

    #[test]
    fn pocket_camera_ram_banking_and_register_select() {
        let mut cart = camera_cart();
        // RAM WRITES need the $0A gate; reads are always enabled.
        cart.write(0xA000, 0x42);
        assert_eq!(cart.read(0xA000), 0xFF); // write dropped (gate closed)
        cart.write(0x0000, 0x0A);
        cart.write(0xA000, 0x42);
        assert_eq!(cart.read(0xA000), 0x42);
        // 16 RAM banks.
        cart.write(0x4000, 0x0F);
        cart.write(0xA000, 0x77);
        cart.write(0x4000, 0x00);
        assert_eq!(cart.read(0xA000), 0x42);
        cart.write(0x0000, 0x00); // close the gate again
        assert_eq!(cart.read(0xA000), 0x42); // reads still enabled
        // Bit 4 maps the register file; all registers but A000 read $00,
        // and the file mirrors every $80.
        cart.write(0x4000, 0x10);
        assert_eq!(cart.read(0xA000), 0x00); // idle: busy clear
        assert_eq!(cart.read(0xA001), 0x00);
        cart.write(0xA004, 0x55); // write-only, sticks despite closed gate
        assert_eq!(cart.read(0xA004), 0x00);
        cart.write(0xA000 + 0x80, 0x06); // mirror of A000: bits 1-2 stored
        assert_eq!(cart.read(0xA000), 0x06);
        // Back to RAM: bank latch survived the register window.
        cart.write(0x4000, 0x00);
        assert_eq!(cart.read(0xA000), 0x42);
    }

    #[test]
    fn pocket_camera_capture_timing_busy_gate_and_commit() {
        let mut cart = camera_cart();
        cart.write(0x0000, 0x0A);
        camera_configure(&mut cart);
        cart.write(0xA000, 0x03); // trigger, positive 1-D set
        assert_eq!(cart.read(0xA000), 0x03); // busy | stored bits 1-2
        // RAM is unreadable (returns $00) and write-locked while capturing.
        cart.write(0x4000, 0x00);
        assert_eq!(cart.read(0xA000), 0x00);
        assert_eq!(cart.read(0xA100), 0x00);
        cart.write(0xA000, 0x99); // ignored
        // Pan Docs: M-cycles = 32446 + (N?0:512) + 16*exposure; N=1 here.
        let total = 4 * (32446 + 16 * 0x0800u64);
        cart.cam_tick(total - 1);
        cart.write(0x4000, 0x10);
        assert_eq!(cart.read(0xA000), 0x03); // still busy on the last cycle
        cart.cam_tick(1);
        assert_eq!(cart.read(0xA000), 0x02); // busy cleared, bits 1-2 kept
        // RAM readable again; the A000 write during capture was dropped and
        // the processed tile data landed at bank 0 offset $0100.
        cart.write(0x4000, 0x00);
        assert_eq!(cart.read(0xA000), 0xFF); // untouched cell (not 0x99)
        let tiles: Vec<u8> = (0..CAM_TILE_BYTES)
            .map(|i| cart.read(0xA100 + i as u16))
            .collect();
        assert!(tiles.iter().any(|&b| b != tiles[0]), "flat capture output");
    }

    #[test]
    fn pocket_camera_capture_stop_and_resume() {
        let mut cart = camera_cart();
        cart.write(0x0000, 0x0A);
        camera_configure(&mut cart);
        cart.write(0x4000, 0x00);
        cart.write(0xA100, 0xAB); // pre-capture RAM content
        cart.write(0x4000, 0x10);
        cart.write(0xA000, 0x03);
        cart.cam_tick(1000);
        // Stop mid-capture: busy clears, RAM shows the OLD contents again.
        cart.write(0xA000, 0x02);
        assert_eq!(cart.read(0xA000), 0x02);
        cart.write(0x4000, 0x00);
        assert_eq!(cart.read(0xA100), 0xAB);
        cart.cam_tick(1 << 40); // stopped: countdown frozen
        cart.write(0x4000, 0x10);
        assert_eq!(cart.read(0xA000), 0x02);
        // Resume: finishes with the ORIGINAL parameters/image.
        cart.write(0xA000, 0x03);
        assert_eq!(cart.read(0xA000), 0x03);
        cart.cam_tick(4 * (32446 + 16 * 0x0800u64)); // > remaining
        assert_eq!(cart.read(0xA000), 0x02);
        cart.write(0x4000, 0x00);
        assert_ne!(cart.read(0xA100), 0xAB); // committed over the old byte
    }

    #[test]
    fn pocket_camera_sensor_image_feeds_capture() {
        let run_capture = |image: Option<[u8; CAM_W * CAM_H]>| -> Vec<u8> {
            let mut cart = camera_cart();
            cart.write(0x0000, 0x0A);
            if let Some(img) = image {
                cart.set_camera_image(&img);
            }
            camera_configure(&mut cart);
            cart.write(0xA000, 0x01);
            cart.cam_tick(u64::MAX / 2);
            cart.write(0x4000, 0x00);
            (0..CAM_TILE_BYTES)
                .map(|i| cart.read(0xA100 + i as u16))
                .collect()
        };
        let builtin = run_capture(None);
        let dark = run_capture(Some([0u8; CAM_W * CAM_H]));
        let bright = run_capture(Some([255u8; CAM_W * CAM_H]));
        // A flat black input dithers to solid black tiles (both bitplanes
        // set), flat white to solid white; the built-in pattern differs from
        // both.
        assert!(dark.iter().all(|&b| b == 0xFF));
        assert!(bright.iter().all(|&b| b == 0x00));
        assert_ne!(builtin, dark);
        assert_ne!(builtin, bright);
    }

    #[test]
    fn pocket_camera_photo_persists_to_sav() {
        let dir = std::env::temp_dir().join(format!(
            "rustyboi-cam-test-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir_all(&dir).unwrap();
        let sav = dir.join("camera.sav");

        let mut cart = camera_cart();
        cart.attach_save_file(&sav).unwrap();
        cart.write(0x0000, 0x0A);
        camera_configure(&mut cart);
        cart.write(0xA000, 0x01);
        cart.cam_tick(u64::MAX / 2);
        cart.write(0x4000, 0x00);
        let expected: Vec<u8> = (0..CAM_TILE_BYTES)
            .map(|i| cart.read(0xA100 + i as u16))
            .collect();
        drop(cart);

        let bytes = fs::read(&sav).unwrap();
        assert_eq!(bytes.len(), 16 * 0x2000); // full 128KB album RAM
        assert_eq!(&bytes[0x100..0x100 + CAM_TILE_BYTES], &expected[..]);
        fs::remove_dir_all(&dir).ok();
    }

    /// Unique-ish suffix for temp dirs (tests may run in parallel).
    fn unique_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }
}
