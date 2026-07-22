//! Cartridge subsystem: the `Cartridge` container (ROM/RAM buffers, battery
//! persistence, header decode, RTC) plus the per-board mappers alongside it.
//!
//! Header decode lives in [`header`]. The mapper behavior is being migrated
//! out into per-board modules behind a `Mapper` enum (enum-dispatched, no
//! `dyn`, so serde savestates and the hot read/write path are preserved).

use crate::memory;
use crate::memory::mmio;
use serde::{Deserialize, Serialize};

use std::cell::Cell;
use std::fs;
use std::fs::{File, OpenOptions};
use std::io;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use zip::ZipArchive;

mod header;
mod mapper;
pub use self::header::{find_logo_in_boot_rom, CgbSupport, Destination};
use self::header::*;
mod rtc;
mod mbc7;
mod huc3;
mod camera;
mod unlicensed;
mod mbc1;
mod mbc2;
mod mbc3;
mod mbc5;
mod huc1;
mod nombc;
use self::mapper::*;
use self::rtc::{HuC3Rtc, Mbc3Rtc};
// ---------------------------------------------------------------------------
// Unlicensed / bootleg mappers. These boards spoof the header type byte
// ($00/$01, or use out-of-spec values like $97/$99/$EA), so they are detected
// from ROM content (logo checksums / publisher strings / title+size shapes),
// not from $0147. References: the community reverse-engineering of these
// boards, Pan Docs "Other MBCs"
// (https://gbdev.io/pandocs/othermbc.html), and the gbdev forum thread
// "Cartridges with Rare Mappers" (https://gbdev.gg8.se/forums/viewtopic.php?id=948).
// ---------------------------------------------------------------------------

/// Byte sums of the two Sachen logo variants.
const LOGO_SUM_SACHEN_A: u32 = 5542;
const LOGO_SUM_SACHEN_B: u32 = 7484;
/// Byte sum of the Rocket Games logo (2756). Rocket carts never
/// contain the Nintendo logo in the dump; while a boot ROM runs, the mapper
/// presents the logo (sourced from the boot ROM) during its locked-CGB phase so
/// the boot ROM's logo check passes.
const LOGO_SUM_ROCKET: u32 = 2756;
/// Byte sum of the secondary Vast Fame logo at $0184 on the VF001-class
/// Legend of Heroes board. Not one of hhugboy's known VF001 sums
/// (4844/6127/4406) — this cart speaks a different, earlier register-file
/// protocol (see `UnlMapper::Vf001`), so it gets its own detection.
const LOGO_SUM_VF001_LOH: u32 = 4593;
/// File offset and first bytes of the Legend of Heroes boot protection stub
/// (`ld de,$7080; ld a,$9a; ld (de),a; ...`). Required together with the
/// $0184 logo sum so a licensed cart whose header area happens to sum to
/// 4593 can never match.
const VF001_STUB_OFFSET: usize = 0x32FC;
const VF001_STUB: [u8; 6] = [0x11, 0x80, 0x70, 0x3E, 0x9A, 0x12];

// Lock-phase values shared by the Sachen and Rocket boot state machines
// (the board powers up locked and unlocks in DMG -> CGB -> unlocked phases).
const UNL_LOCKED_DMG: u8 = 0;
const UNL_LOCKED_CGB: u8 = 1;
const UNL_UNLOCKED: u8 = 2;

/// NT/Makon "old" bank-line swap tables for the $5003 bit-4 mode, applied to
/// the ROM bank number: output bit i = input bit table[i].
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
    /// ignored). Pan Docs "Other MBCs".
    WisdomTree,
    /// Rocket Games ($97 singles / $99 2-in-1s): 16KB inner bank at exactly
    /// $3F00 (0 maps to 1), 256KB outer bank at exactly $3FC0, plus the
    /// A15-transition unlock counter with the logo XOR window. gbdev forum
    /// id=948; MiSTer unlicensed thread.
    Rocket,
    /// Sachen MMC1: base/mask outer banking + the $01xx address descramble +
    /// the DMG lock phase (RA7 forced high).
    SachenMmc1,
    /// Sachen MMC2: MMC1 plus a DMG->CGB->unlocked 3-phase lock (the CGB
    /// phase presents the Nintendo logo copy at $0184).
    SachenMmc2,
    /// NT/Makon older board, MBC1-style 5-bit bank register.
    NtOld1,
    /// NT/Makon older board, MBC3-style 8-bit bank register (+ rumble on the
    /// multicarts).
    NtOld2,
    /// Header liars that are electrically plain MBC1 with no RAM: Sonic 3D
    /// Blast 5 (type $EA, code overlapping the header area), Captain
    /// Knick-Knack (Sachen dump with a Tetris header), Pocket Monsters
    /// GO!GO!GO! 256KB dumps. Routed as MBC1 with no RAM.
    ForceMbc1,
    /// M161 (Mani 4 in 1, DMG-601): a one-shot latch that maps one of eight
    /// whole-32KB banks. The header spoofs MBC3+RAM+BAT ($10), so it is
    /// content-detected (256KB + title "TETRIS SET").
    M161,
    /// Vast Fame VF001-class protection board (Legend of Heroes). Electrically
    /// a normal MBC5+RAM+BATTERY plus a 4-port protection register file
    /// decoded from A10-A11: writes at $7080/$7480/$7880/$7C80, value
    /// readback through the cart-RAM window at $A000/$A400/$A800/$AC00.
    /// Port 0 is a command port (last three bytes form the command); writes
    /// to ports 1-3 select which derived value the next protection read
    /// returns. Reverse-engineered protocol of the one known cart (all four
    /// sequences in the ROM; static RE of the required `jp (hl)` targets):
    ///
    ///   cmd $9A,$B8,$B9 (boot gate, $32FC): reads of port 2 ($A800) return
    ///       $C1 after select $B9 and $F8 after select $83; the stub decodes
    ///       hl = ($0C, $AE) via swap/offset and `jp (hl)` -> $0CAE (init).
    ///   cmd $7E,$29,$79 (gate at $0D16): side effect — the device drives the
    ///       MBC5 ROM-bank register to 6 (the following `jp $60d0` needs the
    ///       bank-6 continuation; bank 1 holds a decoy that decompresses
    ///       garbage). The $AFFF read that follows is a decoy (discarded).
    ///   cmd $37,$52,$CD (gate at $0D36): reads of port 2 return $82 after
    ///       select $BA and $8F after select $A9 -> `jp (hl)` -> $08E9
    ///       (title/graphics setup).
    ///   cmd ...,$B9,$81 ($1015): read of port 0 ($A000) supplies the TMA
    ///       seed (timer IRQs are never taken; value is not branched on).
    ///
    /// A trailing command write of $31 closes each sequence. Reads that match
    /// no armed command fall through to normal cart RAM, so saves work.
    Vf001(Vf001State),
}

/// Protection register-file state for `UnlMapper::Vf001`. Carried inside the
/// enum variant (not as a `Cartridge` field) so the bincode savestate layout
/// of every other cart stays byte-identical.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Vf001State {
    /// Last three bytes written to the command port (port 0), oldest first.
    cmd: [u8; 3],
    /// Most recent byte written to any select port (ports 1-3).
    select: u8,
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
/// One external-RAM bank as seen through the $A000-$BFFF window.
const RAM_BANK_SIZE: usize = 0x2000;

// MBC2 specific ranges
const MBC2_RAM_SIZE: usize = 512; // 512 x 4 bits
const MBC2_RAM_START: u16 = 0xA000;

 // 3584
 // 120

#[derive(Clone, Copy, Debug)]
pub(crate) enum CartridgeType {
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
    /// Mani 4 in 1 one-shot 32KB bank-latch (M161 board).
    M161,
}
























#[derive(Serialize, Deserialize)]
pub struct Cartridge {
    // ROM data - all banks. Read-only (never mutated after construction) and
    // potentially multi-MB, so it is kept OUT of savestates: serializing it into
    // every rewind-ring snapshot would be fatal. The frontend re-attaches it via
    // `attach_rom` after a state load from the already-resident ROM bytes; every
    // field that derives from it (`rom_banks`, `cartridge_type`, `mbc1_multicart`,
    // `unl_mapper`, `cgb_support`) DOES serialize, so bank math survives the load.
    // Held behind an `Arc` so `Cartridge::clone` (and thus `GB::clone`, used by
    // the offloaded rewind capture every few frames) shares this multi-MB buffer
    // by refcount instead of deep-copying it. The sole mutation — a Game Genie
    // patch in `apply_rom_patch` — uses `Arc::make_mut` for copy-on-write, so a
    // live clone is never disturbed.
    #[serde(skip, default)]
    rom_data: Arc<[u8]>,
    // Cached (bank0_base, bankN_base) ROM byte offsets for the licensed-mapper
    // read fast path, so a ROM read is an add + bounds check instead of the
    // full mapper-type + bank-register derivation per access. Invalidated by
    // every `write` (the only mutation path for licensed bank registers);
    // never used for unlicensed boards (their lock state can advance on
    // reads). `serde(skip)` deserializes to None = recompute.
    #[serde(skip, default)]
    rom_bank_cache: Cell<Option<(usize, usize)>>,
    // Cached decode of (`unl_mapper`, `cartridge_type`) -> CartridgeType, which
    // every external-RAM access derived two or three times over (the mapper
    // match, then again inside `get_ram_bank`, then `is_mbc30` on MBC3). Both
    // inputs are fixed at construction: nothing assigns `cartridge_type`, and
    // the only runtime write to `unl_mapper` (`vf001_write`) mutates the
    // Vf001 PAYLOAD, never the variant — and the decode ignores that payload.
    // So unlike `rom_bank_cache` this never needs invalidating. `serde(skip)`
    // deserializes to None = recompute, so it is correct even if consulted
    // before `attach_rom`.
    #[serde(skip, default)]
    cartridge_type_cache: Cell<Option<CartridgeType>>,
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

    // The live mapper: each board's volatile registers, enum-dispatched (see
    // cartridge/mapper.rs). The battery/persistent domain (RAM, RTC) and the
    // peripheral engines stay on Cartridge below.
    mapper: Mapper,
    // MBC1 multicart: the BANK2 register supplies ROM-bank bits 4-5 and only the
    // low 4 bits of BANK1 are wired, so the combined bank is 6 bits. Detected
    // from the Nintendo-logo-per-segment header layout (see is_mbc1_multicart).
    #[serde(default)]
    mbc1_multicart: bool,

    // MBC2 state (MBC2 has built-in 512x4 RAM)
    mbc2_ram: Vec<u8>, // MBC2 built-in RAM (512 x 4 bits, stored as full bytes)

    // Live MBC3 RTC counters, and the CPU-visible shadows a $6000-$7FFF write
    // latches them into. Same register shape, so they share a type.
    rtc: Mbc3Rtc,
    rtc_latched: Mbc3Rtc,

    // Sub-second cycle accumulator for the cycle-derived RTC. One RTC second is
    // 4_194_304 T-cycles (the 4.194304 MHz master/dot clock). The RTC crystal is
    // independent of CPU speed, so this is driven off the master `abs_cc` dot
    // clock (constant across single/double speed), NOT host wall-clock — keeping
    // RTC advancement fully deterministic and test-reproducible.
    #[serde(default)]
    rtc_cycle_accum: u64,

    // Live sensor input in g, fed by the frontend via `set_accelerometer`.
    // Not persisted (transient hardware input, like buttons), and survives
    // `reset` -- power-cycling the console does not null gravity -- so it sits
    // outside `Mbc7State`, which resets wholesale.
    #[serde(skip, default)]
    mbc7_sensor_x: f32,
    #[serde(skip, default)]
    mbc7_sensor_y: f32,

    // Battery-fed HuC-3 clock: survives `reset` while the mailbox registers
    // above do not, so it is a separate struct.
    #[serde(default)]
    huc3_rtc: HuC3Rtc,


    // Live 128x112 8-bit grayscale sensor input, fed by the frontend via
    // `set_camera_image`. Empty => the built-in deterministic test pattern.
    // Not persisted (transient hardware input, like buttons).
    #[serde(skip, default)]
    cam_image: Vec<u8>,

    // Detected unlicensed mapper family (content heuristics; overrides the
    // header type byte in get_cartridge_type).
    #[serde(default)]
    unl_mapper: UnlMapper,

    // Nintendo logo bytes the Rocket mapper presents at $0104-$0133 during its
    // locked-CGB phase, sourced at RUNTIME from the loaded boot ROM (which
    // contains the logo it checks against) so no logo data is embedded here.
    // None unless a boot ROM is present; only ever observed by a running boot
    // ROM. Not persisted (re-seeded from the boot ROM on load).
    #[serde(skip, default)]
    rocket_boot_logo: Option<[u8; 48]>,

    // CGB support information
    cgb_support: CgbSupport, // CGB compatibility from cartridge header

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
    // boards (default; the srcE000_readFE00 cgb04c hwtest capture
    // reads 0xFF with RAMG on) exclude E000-FDFF, so the bus floats.
    // Lazy boards decode /CS & A13 only (AntonioND's gbc-hw-tests flashcart)
    // and drive SRAM[src & 0x1FFF] there. Set per test fixture via the
    // manifest `cart=lazy_sram_cs` token; not a savestate property.
    #[serde(skip, default)]
    sram_cs_lazy: bool,
}

/// The ROM-derived identity of a cartridge: the expanded/padded image plus
/// every field computed from it at load time (header decode + content
/// heuristics). Immutable after construction, so `reset` carries it from the
/// live cart instead of re-running the detection predicates (which were
/// designed for the original file bytes, not the padded image). Consumed by
/// `power_on`, the single construction site for a fresh cart.
struct RomIdentity {
    rom_data: Arc<[u8]>,
    cartridge_type: u8,
    rom_banks: usize,
    ram_banks: usize,
    unl_mapper: UnlMapper,
    cgb_support: CgbSupport,
    mbc1_multicart: bool,
}

/// Which per-dot RTC advance a cartridge needs. Cached by the MMIO so the hot
/// `tick_rtc` path avoids recomputing `get_cartridge_type()` every dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum RtcTickKind {
    #[default]
    None,
    Mbc3,
    HuC3,
}

impl Clone for Cartridge {
    fn clone(&self) -> Self {
        Cartridge {
            rom_data: self.rom_data.clone(),
            rom_bank_cache: self.rom_bank_cache.clone(),
            cartridge_type_cache: self.cartridge_type_cache.clone(),
            ram_data: self.ram_data.clone(),
            cartridge_type: self.cartridge_type,
            rom_banks: self.rom_banks,
            ram_banks: self.ram_banks,
            rom_path: self.rom_path.clone(),
            save_file: None, // Don't clone file handles
            mapper: self.mapper.clone(),
            mbc1_multicart: self.mbc1_multicart,
            sram_cs_lazy: self.sram_cs_lazy,
            mbc2_ram: self.mbc2_ram.clone(),
            rtc: self.rtc,
            rtc_latched: self.rtc_latched,
            rtc_cycle_accum: self.rtc_cycle_accum,
            mbc7_sensor_x: self.mbc7_sensor_x,
            mbc7_sensor_y: self.mbc7_sensor_y,
            huc3_rtc: self.huc3_rtc.clone(),
            cam_image: self.cam_image.clone(),
            unl_mapper: self.unl_mapper,
            rocket_boot_logo: self.rocket_boot_logo,
            cgb_support: self.cgb_support.clone(),
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
            // Makon games overlap code with the header), so the loader likewise
            // falls back to the file size.
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
    /// game code overlaps the header), matching reference decoders (RAM size
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




    /// The detected unlicensed mapper family (None for licensed carts).
    pub fn unl_mapper(&self) -> UnlMapper {
        self.unl_mapper
    }

    pub fn load(path: &str) -> Result<Self, io::Error> {
        let data = if path.to_lowercase().ends_with(".zip") {
            Self::extract_rom_from_zip_bytes(&fs::read(path)?)?
        } else {
            fs::read(path)?
        };

        let mut cartridge = Self::from_rom_image(data)?;
        cartridge.rom_path = Some(path.to_string());

        // Try to load existing save file or create new one (only for battery-backed RAM)
        cartridge.load_or_create_save_file()?;
        // Restore the persisted RTC (with wall-clock catch-up) and attach the
        // `.rtc` sidecar. Disk-load path only; in-memory loads skip this.
        cartridge.attach_rtc_sidecar()?;

        Ok(cartridge)
    }

    /// Shared constructor core: derive everything from an already-unzipped ROM
    /// file image and hand it to `power_on`. `load` and `from_bytes` differ
    /// only in how they obtain the bytes and in sidecar/save-file attachment.
    fn from_rom_image(data: Vec<u8>) -> Result<Self, io::Error> {
        if data.len() < 0x0150 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "ROM too small"));
        }

        // Re-expand trimmed MBC1 multicart dumps before any derived fields.
        let data = Self::reconstruct_trimmed_mbc1m(&data).unwrap_or(data);

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

        // Detect CGB support
        let cgb_support = Self::detect_cgb_support(&data);

        // Detect MBC1 multicart wiring from the per-segment logo layout.
        let mbc1_multicart = Self::detect_mbc1_multicart(cartridge_type, &data);

        // Initialize RAM data. MBC7 carts declare RAM size 0x00 in the header;
        // their "save RAM" is the 93LC56 EEPROM: 256 bytes = 128 little-endian
        // 16-bit words, erased state 0xFF. Routing it through ram_data reuses
        // the whole battery-save path (LE word order matches the de-facto
        // `.sav` files). ForceMbc1 header-liars carry garbage RAM-size bytes;
        // RAM is forced off for them.
        let ram_banks = if unl_mapper == UnlMapper::ForceMbc1 { 0 } else { ram_banks };
        let ram_data = if cartridge_type == MBC7_SENSOR_RUMBLE_RAM_BATTERY {
            vec![0xFF; 256]
        } else {
            vec![0xFF; Self::compute_ram_len(ram_size_code, ram_banks)]
        };

        // Copy ROM data. `Arc::from(&slice)` copies exactly once — going
        // through an intermediate `Vec` and then `.into()` would copy twice
        // and leave a ROM-sized transient for the allocator to retain.
        let expected_rom_size = rom_banks * 0x4000; // 16KB per bank
        let rom_data: Arc<[u8]> = if data.len() >= expected_rom_size {
            Arc::from(&data[..expected_rom_size])
        } else {
            // Pad with 0xFF if ROM is smaller than expected
            let mut padded_rom = data;
            padded_rom.resize(expected_rom_size, 0xFF);
            padded_rom.into()
        };

        let identity = RomIdentity {
            rom_data,
            cartridge_type,
            rom_banks,
            ram_banks,
            unl_mapper,
            cgb_support,
            mbc1_multicart,
        };
        Ok(Self::power_on(identity, ram_data))
    }

    /// Build a cartridge in its power-on state: every volatile mapper latch at
    /// its documented initial value (bank registers homed, enable gates
    /// closed, boot locks armed, no in-flight peripheral activity), RAM/RTC as
    /// given/empty. This is the ONLY full `Cartridge` construction site, the
    /// single source of truth for power-on values: `from_rom_image` builds new
    /// carts through it and `reset` rebuilds the volatile domain from it, so a
    /// new field added here is automatically volatile across `reset` unless
    /// explicitly carried in reset's persist list.
    fn power_on(identity: RomIdentity, ram_data: Vec<u8>) -> Self {
        let RomIdentity {
            rom_data,
            cartridge_type,
            rom_banks,
            ram_banks,
            unl_mapper,
            cgb_support,
            mbc1_multicart,
        } = identity;
        // VF001's protection register file is volatile logic; normalize it to
        // its power-on state so reset() (which carries the possibly-mutated
        // identity in) always powers up clean, exactly like a fresh load.
        let unl_mapper = match unl_mapper {
            UnlMapper::Vf001(_) => UnlMapper::Vf001(Vf001State::default()),
            other => other,
        };
        Cartridge {
            rom_data,
            rom_bank_cache: Cell::new(None),
            cartridge_type_cache: Cell::new(None),
            ram_data,
            cartridge_type,
            rom_banks,
            ram_banks,
            rom_path: None,
            save_file: None,
            mapper: Mapper::from_header(unl_mapper, cartridge_type, mbc1_multicart),
            mbc1_multicart,
            sram_cs_lazy: false,
            mbc2_ram: vec![0xFF; MBC2_RAM_SIZE],
            rtc: Mbc3Rtc::default(),
            rtc_latched: Mbc3Rtc::default(),
            rtc_cycle_accum: 0,
            mbc7_sensor_x: 0.0,
            mbc7_sensor_y: 0.0,
            huc3_rtc: HuC3Rtc {
                mem: if cartridge_type == HUC3 { vec![0; 256] } else { Vec::new() },
                accum: 0,
            },
            cam_image: Vec::new(),
            unl_mapper,
            rocket_boot_logo: None,
            cgb_support,
            rtc_memory: Vec::new(),
            rtc_memory_synced: Vec::new(),
            rtc_file: None,
            host_managed_saves: false,
        }
    }

    /// Extract the ROM image from an in-memory zip container: prefer a member
    /// with a Game Boy extension, else the largest non-directory member.
    /// `load` reads the file in and comes through here too, so the path and
    /// byte entry points cannot drift apart.
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

    /// Decompress `data` to the raw ROM bytes: unzips a `PK\x03\x04` container
    /// (the same extraction `from_bytes` does), else returns the bytes as-is.
    /// Useful when a caller needs the actual ROM image — e.g. to hash it for a
    /// No-Intro CRC32 lookup rather than hashing the zip container.
    pub fn extract_rom_bytes(data: &[u8]) -> Result<Vec<u8>, io::Error> {
        if data.len() >= 4 && &data[0..4] == b"PK\x03\x04" {
            Self::extract_rom_from_zip_bytes(data)
        } else {
            Ok(data.to_vec())
        }
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, io::Error> {
        // Try to detect if this is a zip file by checking the magic bytes
        let actual_data = Self::extract_rom_bytes(data)?;

        // In-memory loading intentionally skips save files so test runners and
        // WASM callers do not create sidecar files. This also skips the `.rtc`
        // sidecar + wall-clock catch-up: the RTC starts at zero and advances
        // only on the deterministic cycle-derived tick.
        Self::from_rom_image(actual_data)
    }

    /// Clone the raw ROM image (all banks, already padded to `rom_banks`) out of
    /// a live cartridge so it can be re-attached to a savestate-restored one. The
    /// ROM is `#[serde(skip)]`, so this is how a load path carries the ROM across.
    pub fn detach_rom(&self) -> Vec<u8> {
        self.rom_data.to_vec()
    }

    /// Re-attach the ROM image after a savestate load (where `rom_data` was
    /// skipped). Pads/truncates to the serialized `rom_banks * 0x4000` exactly as
    /// the constructors do, so the already-restored bank registers index the same
    /// bytes. All other runtime state (RAM, bank regs, RTC) is already present
    /// from deserialize; this only refills the read-only ROM.
    pub(crate) fn attach_rom(&mut self, rom: Vec<u8>) {
        // A caller may re-attach the ORIGINAL file bytes (not a `detach_rom`
        // image), so apply the same trimmed-MBC1M expansion as the
        // constructors; the serialized bank registers assume the physical
        // layout. Already-expanded images never match the predicate.
        let rom = Self::reconstruct_trimmed_mbc1m(&rom).unwrap_or(rom);
        let expected = self.rom_banks * 0x4000;
        self.rom_data = if rom.len() >= expected {
            Arc::from(&rom[..expected])
        } else {
            let mut padded = rom;
            padded.resize(expected, 0xFF);
            padded.into()
        };
    }

    /// Whether the ROM image is currently attached (present after construction or
    /// `attach_rom`; empty right after a savestate deserialize).
    pub fn has_rom(&self) -> bool {
        !self.rom_data.is_empty()
    }

    /// Decoded mapper for this board. Hot: the external-RAM read/write arms
    /// hit it two to three times per access, so the pure
    /// (`unl_mapper`, `cartridge_type`) -> `CartridgeType` decode below is
    /// memoized (see `cartridge_type_cache` for why it never goes stale).
    #[inline]
    fn get_cartridge_type(&self) -> CartridgeType {
        if let Some(ty) = self.cartridge_type_cache.get() {
            return ty;
        }
        let ty = self.decode_cartridge_type();
        self.cartridge_type_cache.set(Some(ty));
        ty
    }

    fn decode_cartridge_type(&self) -> CartridgeType {
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
            // VF001 is electrically a normal MBC5+RAM+BATTERY (header $1B is
            // truthful); only the $6000-$7FFF write / $A000-$BFFF read
            // intercepts differ, so fall through to the header type.
            UnlMapper::Vf001(_) => {}
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

    /// Whether external RAM is currently enabled, from whichever board carries
    /// the RAMG gate. Boards without one (HuC1 always-on, HuC3 mode-select,
    /// Sachen/Rocket ungated, bankless) report false, matching the old shared
    /// `ram_enabled` field which those paths never set.
    fn ram_enabled(&self) -> bool {
        match &self.mapper {
            Mapper::Mbc1(m) => m.ram_enabled,
            Mapper::Mbc2(m) => m.ram_enabled,
            Mapper::Mbc3(m) => m.ram_enabled,
            Mapper::Mbc5(m) => m.ram_enabled,
            Mapper::Mbc7(m) => m.ram_enabled,
            Mapper::Camera(m) => m.ram_enabled,
            Mapper::NtOld(m) => m.ram_enabled,
            Mapper::Vf001(m) => m.ram_enabled,
            _ => false,
        }
    }

    /// ROM/RAM geometry the mapper's bank math needs.
    fn geom(&self) -> Geom {
        Geom { rom_banks: self.rom_banks, ram_banks: self.ram_banks }
    }

    fn get_rom_bank(&self) -> usize {
        self.mapper.rom_bankn(self.geom())
    }

    fn get_ram_bank(&self) -> usize {
        self.mapper.ram_bank(self.geom())
    }

    /// ROM bank mapped at the 0x0000-0x3FFF region. Normally bank 0, but on
    /// MBC1 in banking mode 1 the BANK2 register is also applied here, so a
    /// large cart sees bank 0x20/0x40/0x60 (or 0x10/0x20/0x30 on a multicart).
    fn get_rom_bank0(&self) -> usize {
        self.mapper.rom_bank0(self.geom())
    }

    /// Cached (bank0, bankN) ROM byte-offset bases for the read fast path.
    /// Whether a content-detected unlicensed mapper is active (their lock
    /// state can advance on reads, so flat-map caches must exclude them).
    #[inline]
    pub fn is_unlicensed(&self) -> bool {
        self.unl_mapper != UnlMapper::None
    }

    /// Public view of the cached (bank0, bankN) ROM byte-offset bases for the
    /// passive-read page table.
    #[inline]
    pub fn rom_bases(&self) -> (usize, usize) {
        self.rom_bank_bases()
    }

    /// Bounds-checked raw ROM byte (open-bus 0xFF past the image), mirroring
    /// the banked read arms.
    #[inline]
    pub(crate) fn rom_byte(&self, offset: usize) -> u8 {
        self.rom_data.get(offset).copied().unwrap_or(0xFF)
    }

    /// Cached (bank0, bankN) ROM byte-offset bases for the read fast path.
    /// Licensed mappers only mutate bank registers through `write`, which
    /// invalidates the cache; unlicensed boards can advance lock state during
    /// reads, so they always recompute (identical to the pre-cache behavior).
    #[inline]
    fn rom_bank_bases(&self) -> (usize, usize) {
        if self.unl_mapper != UnlMapper::None {
            return (self.get_rom_bank0() * 0x4000, self.get_rom_bank() * 0x4000);
        }
        if let Some(bases) = self.rom_bank_cache.get() {
            return bases;
        }
        let bases = (self.get_rom_bank0() * 0x4000, self.get_rom_bank() * 0x4000);
        self.rom_bank_cache.set(Some(bases));
        bases
    }

    /// Byte index into `ram_data` for a banked external-RAM access at `addr`
    /// (which must be inside the $A000-$BFFF window). `None` when the board
    /// carries no RAM array, so callers keep their open-bus/no-op branch. A
    /// chip smaller than the selected window mirrors, hence the modulo.
    #[inline]
    fn banked_ram_offset(&self, addr: u16) -> Option<usize> {
        if self.ram_data.is_empty() {
            return None;
        }
        Some(
            ((addr - EXTERNAL_RAM_START) as usize + self.get_ram_bank() * RAM_BANK_SIZE)
                % self.ram_data.len(),
        )
    }

    /// As `banked_ram_offset`, for boards that wire RAM straight through with
    /// no bank register (NoMBC, Rocket/Sachen, NT/Makon old).
    #[inline]
    fn unbanked_ram_offset(&self, addr: u16) -> Option<usize> {
        if self.ram_data.is_empty() {
            return None;
        }
        Some((addr - EXTERNAL_RAM_START) as usize % self.ram_data.len())
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
        if !self.has_battery() || self.save_ram().is_empty() {
            return Ok(0);
        }
        let copied = self.load_save_image(bytes);
        // If a save file is attached, flush the current RAM image so the
        // internal sidecar mirrors the freshly-loaded state.
        self.flush_save_image()?;
        Ok(copied)
    }

    /// Copy a save image into the cart's battery-backed buffer — MBC2's
    /// built-in array or the external RAM banks — and report the bytes taken.
    /// The single load policy behind every save-attachment path:
    ///
    /// Only the RAM-sized prefix is taken. An oversized file is legitimate for
    /// the de-facto RTC-carrying `.sav` (an appended footer, read separately by
    /// `read_sav_rtc_footer`), and for the rest it is still the safer of the
    /// options: `attach_save_file_at` opens the file for streaming writes
    /// whether or not it loaded anything, so refusing to load never actually
    /// protected the bytes — it only discarded the user's save as well. Callers
    /// that want a mis-picked file rejected outright go through
    /// `import_save_ram`, which bounds the size before delegating here.
    ///
    /// MBC2 nibble masking is not cosmetic. The built-in RAM is physically
    /// 512 x 4 bits: the upper nibble has no storage cell on the die, which is
    /// why the read path returns `0xF0 | nibble` for the undriven lines. Masking
    /// on load keeps `save_ram()` exports and the streamed sidecar (whose
    /// `write_mbc2_ram_byte` already masks) from carrying bits the silicon
    /// cannot hold.
    fn load_save_image(&mut self, bytes: &[u8]) -> usize {
        let is_mbc2 = matches!(self.get_cartridge_type(), CartridgeType::MBC2 { .. });
        let dst = self.save_ram_mut();
        let n = bytes.len().min(dst.len());
        dst[..n].copy_from_slice(&bytes[..n]);
        if is_mbc2 {
            for b in &mut dst[..n] {
                *b &= 0x0F;
            }
        }
        n
    }

    /// Rewrite the whole attached sidecar from the live save RAM. No-op when
    /// no save file is attached.
    fn flush_save_image(&mut self) -> Result<(), io::Error> {
        let is_mbc2 = matches!(self.get_cartridge_type(), CartridgeType::MBC2 { .. });
        if let Some(ref mut file) = self.save_file {
            file.seek(SeekFrom::Start(0))?;
            // Disjoint field borrows: `save_ram()` would re-borrow all of self.
            let buf: &[u8] = if is_mbc2 { &self.mbc2_ram } else { &self.ram_data };
            file.write_all(buf)?;
            file.flush()?;
        }
        Ok(())
    }

    fn attach_save_file_at(&mut self, save_path: &Path) -> Result<(), io::Error> {
        // Only process save files for cartridges with battery-backed RAM
        if !self.has_battery() || self.host_managed_saves || self.save_ram().is_empty() {
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
            let loaded_data = fs::read(save_path)?;
            self.load_save_image(&loaded_data);
        } else {
            fs::write(save_path, self.save_ram())?;
        }

        // Open file handle for efficient streaming writes
        self.save_file = Some(OpenOptions::new().write(true).open(save_path)?);
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
            // M161's RAM line is permanently disabled; the board also zeroes
            // its header type so it never saves.
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

    // -----------------------------------------------------------------------
    // Header-fact accessors (reporting/tooling; no effect on emulation).
    // -----------------------------------------------------------------------

    /// Human-readable mapper name, e.g. `"MBC5+RAM+Battery"`, `"ROM ONLY"`,
    /// `"HuC1"`. Reflects content-detected unlicensed boards (Sachen, NT, …),
    /// not just the header type byte.
    pub fn mapper_name(&self) -> &'static str {
        use CartridgeType::*;
        match self.get_cartridge_type() {
            // $00 and $08 both decode to NoMBC{battery:false}; the raw type byte
            // is the only thing that tells ROM ONLY from ROM+RAM apart.
            NoMBC { battery: false } => {
                if self.cartridge_type == ROM_RAM { "ROM+RAM" } else { "ROM ONLY" }
            }
            NoMBC { battery: true } => "ROM+RAM+Battery",
            MBC1 { ram: false, .. } => "MBC1",
            MBC1 { ram: true, battery: false } => "MBC1+RAM",
            MBC1 { ram: true, battery: true } => "MBC1+RAM+Battery",
            MBC2 { battery: false } => "MBC2",
            MBC2 { battery: true } => "MBC2+Battery",
            MBC3 { timer: true, ram: false, .. } => "MBC3+RTC+Battery",
            MBC3 { timer: true, ram: true, .. } => "MBC3+RTC+RAM+Battery",
            MBC3 { timer: false, ram: false, battery: false } => "MBC3",
            MBC3 { timer: false, ram: true, battery: false } => "MBC3+RAM",
            MBC3 { timer: false, ram: true, battery: true } => "MBC3+RAM+Battery",
            MBC3 { timer: false, ram: false, battery: true } => "MBC3+Battery",
            MBC5 { rumble: true, ram, battery } => match (ram, battery) {
                (true, true) => "MBC5+Rumble+RAM+Battery",
                (true, false) => "MBC5+Rumble+RAM",
                _ => "MBC5+Rumble",
            },
            MBC5 { rumble: false, ram, battery } => match (ram, battery) {
                (true, true) => "MBC5+RAM+Battery",
                (true, false) => "MBC5+RAM",
                _ => "MBC5",
            },
            MBC7 => "MBC7+Sensor+Rumble+RAM+Battery",
            HuC1 => "HuC1+RAM+Battery",
            HuC3 => "HuC3+RTC+RAM+Battery",
            PocketCamera => "Pocket Camera",
            WisdomTree => "Wisdom Tree",
            Rocket => "Rocket Games",
            Sachen { mmc2: false } => "Sachen MMC1",
            Sachen { mmc2: true } => "Sachen MMC2",
            NtOld { v2: false } => "NT (old, MBC1-style)",
            NtOld { v2: true } => "NT (old, MBC3-style)",
            M161 => "M161",
        }
    }

    /// Total ROM size in bytes (all banks, `rom_banks * 16 KiB`).
    pub fn rom_size_bytes(&self) -> usize {
        self.rom_banks * 0x4000
    }

    /// External save-RAM size in bytes as actually wired (honors the 2 KiB
    /// partial chip and MBC2/MBC7's built-in memory via `ram_data`). 0 = none.
    pub fn ram_size_bytes(&self) -> usize {
        self.ram_data.len()
    }

    /// Destination code ($014A). `None` if the header is unavailable (ROM
    /// detached after a savestate load).
    pub fn destination(&self) -> Option<Destination> {
        self.rom_data.get(0x014A).map(|&b| {
            if b == 0x00 { Destination::Japanese } else { Destination::Overseas }
        })
    }

    /// Publisher name from the licensee code: the new-licensee ASCII pair
    /// ($0144-$0145) when the old code ($014B) is $33, else the old code.
    /// `None` if the header is unavailable or the code is unmapped.
    pub fn licensee(&self) -> Option<&'static str> {
        let old = *self.rom_data.get(0x014B)?;
        if old == 0x33 {
            let a = *self.rom_data.get(0x0144)?;
            let b = *self.rom_data.get(0x0145)?;
            new_licensee(a, b)
        } else {
            old_licensee(old)
        }
    }

    /// Header checksum ($014D) validity — the boot ROM's `x = x - byte - 1`
    /// fold over $0134-$014C. A failing check is what the DMG boot ROM hangs on.
    pub fn header_checksum_valid(&self) -> bool {
        let Some(hdr) = self.rom_data.get(0x0134..=0x014D) else {
            return false;
        };
        let sum = hdr[..0x19].iter().fold(0u8, |a, &b| a.wrapping_sub(b).wrapping_sub(1));
        sum == hdr[0x19]
    }

    /// Global checksum: 16-bit sum of every ROM byte except the two checksum
    /// bytes at $014E-$014F. (Real hardware never verifies it.)
    pub fn global_checksum(&self) -> u16 {
        let mut sum: u16 = 0;
        for (i, &b) in self.rom_data.iter().enumerate() {
            if i != 0x014E && i != 0x014F {
                sum = sum.wrapping_add(b as u16);
            }
        }
        sum
    }

    /// Raw cartridge-type byte ($0147) as stored in the header.
    pub fn cartridge_type_byte(&self) -> u8 {
        self.cartridge_type
    }

    /// Header title ($0134-$0143), printable-ASCII-trimmed. Empty if unreadable.
    pub fn title(&self) -> String {
        let Some(raw) = self.rom_data.get(0x0134..0x0144) else {
            return String::new();
        };
        let end = raw.iter().position(|&b| !(0x20..0x7f).contains(&b)).unwrap_or(raw.len());
        std::str::from_utf8(&raw[..end]).unwrap_or("").trim().to_string()
    }

    /// CRC32 of the whole ROM (the No-Intro key), over the internal buffer with
    /// no copy. `None` if the ROM is detached (post-savestate, before re-attach).
    pub fn rom_crc32(&self) -> Option<u32> {
        (!self.rom_data.is_empty()).then(|| crate::checksum::crc32(&self.rom_data))
    }



















    // --- RTC persistence -------------------------------------------------
    //
    // MBC3 blob: the de-facto community "RTC data" layout, 48 bytes, all fields
    // little-endian. Common tools write this same block as a footer
    // appended to the `.sav`, and libretro cores expose it verbatim as
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
    // Layout: the five live registers (seconds..control), then the latched
    // copies, then a union{time_t,u64} timestamp, written raw with a -4 read
    // leeway for the legacy u32 form (32LE fields + 64LE timestamp, read also
    // accepts the sizeof-4 short form).
    //
    // HuC-3 blob: the de-facto 136-byte layout: the RTC
    // MCU's 256 nibbles packed two per byte (nibble N -> byte N/2, even N in
    // the low half) followed by a u64 LE UNIX timestamp. This carries the
    // architected minute-of-day/day-counter nibbles (0x10-0x15) plus the
    // whole MCU memory (event time, tone, scratch I/O).


















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
    /// Save). Copies `min(src, dst)` bytes so a footer-carrying `.sav` (RTC
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
    /// (0xFF, matching the RAM-less srcE000 cgb04c captures).
    pub(crate) fn dma_sram_bus_read(&self, addr: u16) -> u8 {
        if self.sram_cs_lazy && self.ram_enabled() && !self.ram_data.is_empty() {
            // NOT `banked_ram_offset`: `addr` here reaches $E000-$FFFF, and the
            // captures pin the wrapped decode ($E000 -> $A000), which `addr -
            // EXTERNAL_RAM_START` would not produce.
            let offset = ((addr as usize & 0x1FFF) + self.get_ram_bank() * RAM_BANK_SIZE)
                % self.ram_data.len();
            self.ram_data[offset]
        } else {
            0xFF
        }
    }

    /// Select the board's SRAM chip-select decode (see `dma_sram_bus_read`).
    pub(crate) fn set_sram_cs_lazy(&mut self, lazy: bool) {
        self.sram_cs_lazy = lazy;
    }





    // -----------------------------------------------------------------------
    // POCKET CAMERA (MAC-GBD controller + Mitsubishi M64282FP image sensor)
    //
    // References: Pan Docs "Game Boy Camera" (including its published
    // "Sample code for emulators" image pipeline) and the public
    // gbcam-rev-engineer register/timing documentation (v1.1.1).
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














    /// True for MBC5 rumble cartridges.
    pub fn has_rumble(&self) -> bool {
        matches!(self.get_cartridge_type(), CartridgeType::MBC5 { rumble: true, .. })
    }

    /// Current state of the rumble motor (bit 3 of the last RAM-bank write on
    /// a rumble cart). Always false for non-rumble carts.
    pub fn rumble_active(&self) -> bool {
        matches!(&self.mapper, Mapper::Mbc5(m) if m.rumble_motor)
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
        Arc::make_mut(&mut self.rom_data)[offset] = new;
    }

    /// Power-cycle the mapper: rebuild the cartridge in its power-on state
    /// (`power_on`, the same derivation the constructors use) and carry over
    /// ONLY what survives a real power cycle. The battery-fed domain persists
    /// — cartridge RAM, MBC2 built-in RAM, the MBC3 RTC time registers, the
    /// HuC-3 RTC memory, and their sub-second accumulators — just like
    /// pressing the console's reset/power button, which cuts mapper power but
    /// not the cart battery. Transient hardware inputs (accelerometer tilt,
    /// camera image) and host plumbing (file handles, rom_path,
    /// host_managed_saves, sram_cs_lazy, libretro RTC views, boot-logo seed)
    /// persist too. Everything else — bank registers, enable gates, banking
    /// modes, boot locks, in-flight peripheral state — comes from `fresh`, so
    /// a new field is volatile across reset unless added to the carry list.
    ///
    /// Boot locks (Sachen/Rocket) re-arm here; a subsequent `skip_bios` runs
    /// `skip_boot_handoff` to unlock them when no boot ROM will execute.
    pub fn reset(&mut self) {
        let fresh = Self::power_on(
            RomIdentity {
                rom_data: self.rom_data.clone(), // Arc: refcount bump, no copy
                cartridge_type: self.cartridge_type,
                rom_banks: self.rom_banks,
                ram_banks: self.ram_banks,
                unl_mapper: self.unl_mapper,
                cgb_support: self.cgb_support.clone(),
                mbc1_multicart: self.mbc1_multicart,
            },
            Vec::new(), // discarded: the battery-backed RAM is carried below
        );
        let carried = Cartridge {
            // Battery-fed domain.
            ram_data: std::mem::take(&mut self.ram_data),
            mbc2_ram: std::mem::take(&mut self.mbc2_ram),
            rtc: self.rtc,
            rtc_cycle_accum: self.rtc_cycle_accum,
            huc3_rtc: std::mem::take(&mut self.huc3_rtc),
            // Transient hardware inputs: power cycling the console nulls
            // neither gravity nor the camera scene.
            mbc7_sensor_x: self.mbc7_sensor_x,
            mbc7_sensor_y: self.mbc7_sensor_y,
            cam_image: std::mem::take(&mut self.cam_image),
            // Host plumbing.
            rom_path: self.rom_path.take(),
            save_file: self.save_file.take(),
            rtc_file: self.rtc_file.take(),
            rtc_memory: std::mem::take(&mut self.rtc_memory),
            rtc_memory_synced: std::mem::take(&mut self.rtc_memory_synced),
            rocket_boot_logo: self.rocket_boot_logo,
            host_managed_saves: self.host_managed_saves,
            sram_cs_lazy: self.sram_cs_lazy,
            ..fresh
        };
        *self = carried;
    }



    /// The 48 header-logo bytes the boot ROM would decompress into the VRAM
    /// tiles at $8010: normally the cart's own $0104-$0133, or the locked-mapper
    /// substitution for Sachen MMC1 (`boot_logo_override`). Read straight from
    /// `rom_data` (no bus side effects) so skip_bios never perturbs mapper state.
    pub(crate) fn boot_logo_bytes(&self) -> [u8; 48] {
        if let Some(logo) = self.boot_logo_override() {
            return logo;
        }
        let mut out = [0u8; 48];
        for (i, b) in out.iter_mut().enumerate() {
            *b = self.rom_data.get(0x104 + i).copied().unwrap_or(0xFF);
        }
        out
    }



    /// The 48-byte logo bitmap held in the cartridge header ($0104-$0133). This
    /// is the loaded ROM's own data; the CGB boot ROM copies it through HRAM
    /// while verifying it, so `Mmio` reuses it to reconstruct the post-boot HRAM
    /// residue instead of embedding the bitmap. `None` if the ROM is too short.
    pub(crate) fn header_logo(&self) -> Option<[u8; 48]> {
        let slice = self.rom_data.get(0x0104..0x0134)?;
        let mut logo = [0u8; 48];
        logo.copy_from_slice(slice);
        Some(logo)
    }



}

impl memory::Addressable for Cartridge {
    fn read(&self, addr: u16) -> u8 {
        // Unlicensed-board read front-end: Sachen boot lock + $01xx address
        // descramble, Rocket boot lock + logo window. Licensed carts
        // (UnlMapper::None) skip this entirely.
        let mut addr = addr;
        match self.unl_mapper {
            UnlMapper::SachenMmc1 if addr < 0x8000 => {
                addr = self.sachen_read_addr(addr, false);
            }
            UnlMapper::SachenMmc2 if addr < 0x8000 => {
                addr = self.sachen_read_addr(addr, true);
            }
            UnlMapper::Rocket if addr < 0x8000 => {
                // Advances the lock counter; presents the boot ROM's logo during
                // the locked-CGB window so the boot ROM's check passes.
                if let Some(byte) = self.rocket_locked_logo(addr) {
                    return byte;
                }
            }
            UnlMapper::Vf001(st)
                if (EXTERNAL_RAM_START..=EXTERNAL_RAM_END).contains(&addr) =>
            {
                // Protection value readback through the cart-RAM window;
                // unmatched reads fall through to normal MBC5 RAM.
                if let Some(byte) = Self::vf001_protection_read(st, addr) {
                    return byte;
                }
            }
            _ => {}
        }
        match addr {
            // ROM Bank 0 (0x0000-0x3FFF). Fixed to bank 0 except on MBC1 in
            // banking mode 1, where BANK2 also selects this region.
            mmio::CARTRIDGE_START..=mmio::CARTRIDGE_END => {
                let offset = (addr - mmio::CARTRIDGE_START) as usize + self.rom_bank_bases().0;
                if offset < self.rom_data.len() {
                    self.rom_data[offset]
                } else {
                    0xFF
                }
            }
            // ROM Bank 1-N (switchable)
            mmio::CARTRIDGE_BANK_START..=mmio::CARTRIDGE_BANK_END => {
                let offset =
                    (addr - mmio::CARTRIDGE_BANK_START) as usize + self.rom_bank_bases().1;
                if offset < self.rom_data.len() {
                    self.rom_data[offset]
                } else {
                    0xFF
                }
            }
            // External RAM
            EXTERNAL_RAM_START..=EXTERNAL_RAM_END => {
                match &self.mapper {
                    Mapper::Mbc1(m) if m.has_ram => {
                        if m.ram_enabled
                            && let Some(offset) = self.banked_ram_offset(addr)
                        {
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    Mapper::Mbc2(m) => {
                        // MBC2 has built-in 512x4 RAM. The 512 nibbles echo every
                        // 0x200 bytes across the whole 0xA000-0xBFFF window. Only
                        // the low 4 data bits are stored; the upper 4 read back as
                        // 1s (open data lines), so reads return 0xF0 | nibble.
                        if m.ram_enabled {
                            let offset = (addr - MBC2_RAM_START) as usize % self.mbc2_ram.len();
                            0xF0 | (self.mbc2_ram[offset] & 0x0F)
                        } else {
                            0xFF
                        }
                    }
                    Mapper::Mbc3(m) if m.has_ram => {
                        if m.ram_enabled {
                            // MBC30 wires a third RAM-bank bit: selects 0x00-0x07
                            // are RAM there, 0x00-0x03 on plain MBC3. 0x08-0x0C
                            // are the RTC registers on both.
                            let ram_select_max = if m.is_mbc30(self.geom()) { 0x07 } else { 0x03 };
                            if m.ram_bank <= ram_select_max {
                                // RAM bank access
                                if let Some(offset) = self.banked_ram_offset(addr) {
                                    self.ram_data[offset]
                                } else {
                                    0xFF
                                }
                            } else if (0x08..=0x0C).contains(&m.ram_bank) {
                                // RTC register access
                                self.read_rtc_register(m.ram_bank)
                            } else {
                                0xFF
                            }
                        } else {
                            0xFF
                        }
                    }
                    Mapper::Mbc3(m) if m.timer => {
                        // Timer-only MBC3 (no RAM)
                        if m.ram_enabled && (0x08..=0x0C).contains(&m.ram_bank) {
                            self.read_rtc_register(m.ram_bank)
                        } else {
                            0xFF
                        }
                    }
                    Mapper::Mbc5(m) if m.has_ram => {
                        if m.ram_enabled
                            && let Some(offset) = self.banked_ram_offset(addr)
                        {
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    // Vast Fame VF001 is electrically MBC5+RAM; its protection
                    // reads are served by the front-end above, so a fall-through
                    // read here is plain cart RAM.
                    Mapper::Vf001(m) => {
                        if m.ram_enabled
                            && let Some(offset) = self.banked_ram_offset(addr)
                        {
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    Mapper::Mbc7(m) => {
                        // MBC7 exposes registers, not RAM. They only respond
                        // when BOTH enable stages are unlocked, and only in
                        // A000-AFFF (B000-BFFF just reads 0xFF). The register
                        // is selected by address bits 4-7; bits 0-3 and 8-11
                        // are ignored.
                        if m.ram_enabled && m.state.ram_enabled2 && addr < 0xB000 {
                            match (addr >> 4) & 0x0F {
                                0x2 => (m.state.accel_x & 0xFF) as u8,
                                0x3 => (m.state.accel_x >> 8) as u8,
                                0x4 => (m.state.accel_y & 0xFF) as u8,
                                0x5 => (m.state.accel_y >> 8) as u8,
                                // Ax6x always reads 0x00 (possibly a reserved
                                // Z axis); Ax7x always 0xFF.
                                0x6 => 0x00,
                                0x8 => m.state.eeprom.pin_state(),
                                // Ax0x/Ax1x are write-only (latch control),
                                // Ax7x and Ax9x-AxFx read 0xFF.
                                _ => 0xFF,
                            }
                        } else {
                            0xFF
                        }
                    }
                    Mapper::HuC1(m) => {
                        if m.state.ir_mode {
                            // IR receiver: 0xC1 = light seen, 0xC0 = no light
                            // (Pan Docs HuC1). No IR transport is modeled, so
                            // this always reads the documented idle 0xC0.
                            0xC0
                        } else if let Some(offset) = self.banked_ram_offset(addr) {
                            // RAM is always enabled (no MBC1-style gate).
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    Mapper::Camera(m) => {
                        if m.state.regs_selected {
                            // Register file, mirrored every $80. Only A000 is
                            // readable: bits 1-2 are the stored 1-D filter
                            // set, bit 0 is the live capture-busy flag; bits
                            // 3-7 read '0'. All other registers read $00.
                            if (addr & 0x7F) == 0 {
                                (m.state.regs[0] & 0x06) | (m.state.running as u8)
                            } else {
                                0x00
                            }
                        } else if m.state.running {
                            // "When the capture process is active all RAM
                            // banks will return 00h when read."
                            0x00
                        } else if let Some(offset) = self.banked_ram_offset(addr) {
                            // No read gate: RAM reads are always enabled.
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    Mapper::HuC3(m) => {
                        match m.state.mode {
                            // 0x0 = RAM read-only, 0xA = RAM read/write; both
                            // read the banked external RAM.
                            0x0 | 0xA => {
                                if let Some(offset) = self.banked_ram_offset(addr) {
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
                            0xC => 0x80 | (m.state.rtc_command << 4) | m.state.rtc_result,
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
                    Mapper::NoMbc(_) => {
                        // Pan Docs "No MBC": optional RAM (up to 8KB) is wired
                        // straight through at A000-BFFF -- no banking, no
                        // enable gate. A smaller chip mirrors across the
                        // window (address modulo its size).
                        if let Some(offset) = self.unbanked_ram_offset(addr) {
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    // Rocket/Sachen boards wire any RAM straight through with
                    // no enable gate (RAM is mapped unconditionally).
                    Mapper::Rocket(_) | Mapper::Sachen(_) => {
                        if let Some(offset) = self.unbanked_ram_offset(addr) {
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    // NT/Makon old boards gate RAM MBC3-style ($0A to
                    // $0000-$1FFF), unbanked.
                    Mapper::NtOld(m) if m.ram_enabled => {
                        if let Some(offset) = self.unbanked_ram_offset(addr) {
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
        // Any write can change bank-register state; drop the ROM-base cache
        // unconditionally (recomputing once per write is trivial next read).
        self.rom_bank_cache.set(None);
        match addr {
            // MBC2 register block (0x0000-0x3FFF). MBC2 has a SINGLE register
            // region here, selected by address bit 8: bit8==0 => RAMG (RAM
            // enable), bit8==1 => ROMB (ROM bank, low 4 bits). The 0x2000
            // boundary is irrelevant on MBC2 -- only bit 8 matters -- so handle
            // the whole range here before the generic per-quarter arms.
            RAM_ENABLE_START..=ROM_BANK_SELECT_END if matches!(self.mapper, Mapper::Mbc2(_)) => {
                if let Mapper::Mbc2(m) = &mut self.mapper {
                    if (addr & 0x0100) == 0 {
                        m.ram_enabled = (value & 0x0F) == 0x0A;
                    } else {
                        m.rom_bank_low = (value & 0x0F).max(1);
                    }
                }
            }
            // Wisdom Tree: a single '377 latch loaded from the ADDRESS lines on
            // any $0000-$3FFF write; the data byte is ignored (bank = addr & 0x3F).
            RAM_ENABLE_START..=ROM_BANK_SELECT_END
                if matches!(self.mapper, Mapper::WisdomTree(_)) =>
            {
                if let Mapper::WisdomTree(m) = &mut self.mapper {
                    m.bank = (addr & 0x3F) as u8;
                }
            }
            // M161: the FIRST write anywhere in the whole $0000-$7FFF ROM area
            // latches the 32KB bank from data bits 0-2; later writes are ignored.
            RAM_ENABLE_START..=BANKING_MODE_END if matches!(self.mapper, Mapper::M161(_)) => {
                if let Mapper::M161(m) = &mut self.mapper
                    && !m.state.mapped {
                        m.state.bank = (value & 7) << 1;
                        m.state.mapped = true;
                    }
            }
            // RAM Enable (0x0000-0x1FFF)
            RAM_ENABLE_START..=RAM_ENABLE_END => match &mut self.mapper {
                Mapper::Mbc1(m) => m.ram_enabled = (value & 0x0F) == 0x0A,
                Mapper::Mbc3(m) => m.ram_enabled = (value & 0x0F) == 0x0A,
                Mapper::Mbc5(m) => m.ram_enabled = (value & 0x0F) == 0x0A,
                Mapper::Vf001(m) => m.ram_enabled = (value & 0x0F) == 0x0A,
                // MBC7 stage-1 unlock (stage 2 is 0x40 to 0x4000-0x5FFF).
                Mapper::Mbc7(m) => m.ram_enabled = (value & 0x0F) == 0x0A,
                // HuC1 IR select: $0E maps the IR transceiver, else RAM (no disable).
                Mapper::HuC1(m) => m.state.ir_mode = (value & 0x0F) == 0x0E,
                // HuC3 RAM/RTC/IR select (low nibble).
                Mapper::HuC3(m) => m.state.mode = value & 0x0F,
                // Pocket Camera gates RAM WRITES only.
                Mapper::Camera(m) => m.ram_enabled = (value & 0x0F) == 0x0A,
                // Sachen base ROM bank, latched only while inner bank bits 4-5 are set.
                Mapper::Sachen(m) => {
                    if (m.state.bank & 0x30) == 0x30 {
                        m.state.base = value;
                    }
                }
                Mapper::NtOld(m) => m.ram_enabled = (value & 0x0F) == 0x0A,
                _ => {}
            },
            // ROM Bank Number (0x2000-0x3FFF)
            ROM_BANK_SELECT_START..=ROM_BANK_SELECT_END => match &mut self.mapper {
                Mapper::Mbc1(m) => m.rom_bank_low = (value & 0x1F).max(1), // 5 bits, min 1
                // 7/8 bits, min 1; stored raw, get_rom_bank applies the wired width.
                Mapper::Mbc3(m) => m.rom_bank_low = value.max(1),
                Mapper::Mbc5(m) => {
                    if addr <= 0x2FFF {
                        m.regs.rom_bank_low = value; // low 8 bits; bank 0 allowed
                    } else {
                        m.regs.rom_bank_high = value & 0x01; // upper 1 bit
                    }
                }
                Mapper::Vf001(m) => {
                    if addr <= 0x2FFF {
                        m.regs.rom_bank_low = value;
                    } else {
                        m.regs.rom_bank_high = value & 0x01;
                    }
                }
                Mapper::Mbc7(m) => m.state.rom_bank = value, // like MBC5, bank 0 allowed
                Mapper::HuC1(m) => m.state.rom_bank = value & 0x3F, // 6-bit
                Mapper::HuC3(m) => m.state.rom_bank = value & 0x7F, // 7-bit
                Mapper::Camera(m) => m.state.rom_bank = value & 0x3F, // 6-bit
                // Rocket: two EXACT register addresses; everything else ignored.
                Mapper::Rocket(m) => match addr {
                    0x3F00 => m.state.rom_bank = value.max(1), // inner 16KB bank, 0->1
                    0x3FC0 => m.state.outer = value,           // outer 256KB bank
                    _ => {}
                },
                // Sachen inner ("unmasked") bank register, 0 maps to 1.
                Mapper::Sachen(m) => m.state.bank = value.max(1),
                // NT v1 is 5-bit, v2 8-bit; both remap 0 to 1. $5003 swap applies
                // combinationally in get_rom_bank.
                Mapper::NtOld(m) => {
                    let bank = if m.v2 { value } else { value & 0x1F };
                    m.state.bank = bank.max(1);
                }
                _ => {}
            },
            // RAM Bank Number / Upper ROM Bank Number (0x4000-0x5FFF)
            RAM_BANK_ROM_BANK_HIGH_START..=RAM_BANK_ROM_BANK_HIGH_END => {
                match &mut self.mapper {
                    Mapper::Mbc1(m) => m.bank2 = value & 0x03, // 2 bits
                    Mapper::Mbc3(m) => m.ram_bank = value & 0x0F, // 4-bit RAM/RTC select
                    Mapper::Mbc5(m) => {
                        if m.rumble {
                            // Bit 3 drives the motor; low 3 bits select the RAM bank.
                            m.rumble_motor = (value & 0x08) != 0;
                        }
                        m.regs.ram_bank = value; // 4 bits used
                    }
                    Mapper::Vf001(m) => m.regs.ram_bank = value,
                    // MBC7 stage-2 unlock: exactly 0x40 enables.
                    Mapper::Mbc7(m) => m.state.ram_enabled2 = value == 0x40,
                    Mapper::HuC1(m) => m.state.ram_bank = value,
                    Mapper::HuC3(m) => m.state.ram_bank = value,
                    Mapper::Camera(m) => {
                        // Bit 4 maps the CAM register file; else low 4 bits = RAM bank.
                        if value & 0x10 != 0 {
                            m.state.regs_selected = true;
                        } else {
                            m.state.regs_selected = false;
                            m.state.ram_bank = value & 0x0F;
                        }
                    }
                    // Sachen ROM bank mask, latched only while inner bits 4-5 are set.
                    Mapper::Sachen(m) => {
                        if (m.state.bank & 0x30) == 0x30 {
                            m.state.mask = value;
                        }
                    }
                    // NT/Makon mode registers live in $5000-$5FFF, decoded by A0-A1;
                    // $4000-$4FFF is ignored (v2 rumble data bits aren't wired here).
                    Mapper::NtOld(m) if (addr & 0xF000) == 0x5000 => match addr & 0x03 {
                        0x01 => m.state.base = value & 0x3F, // multicart base, 32KB units
                        0x02 => {
                            // Low nibble selects the sub-game bank-count mask.
                            m.state.bank_mask = match value & 0x0F {
                                0x00 => 31, // 512KB
                                0x08 => 15, // 256KB
                                0x0C => 7,  // 128KB
                                0x0E => 3,  // 64KB
                                0x0F => 1,  // 32KB
                                _ => 31,
                            };
                        }
                        0x03 => m.state.swapped = (value & 0x10) != 0, // bank-line swap (bit 4)
                        _ => {}
                    },
                    _ => {}
                }
                // NT $5002 high nibble $Ex declares 8KB cart RAM (the header says
                // none). Done after the mapper borrow so it can grow ram_data.
                if matches!(self.mapper, Mapper::NtOld(_))
                    && (addr & 0xF000) == 0x5000
                    && (addr & 0x03) == 0x02
                    && (value & 0xF0) == 0xE0
                    && self.ram_data.is_empty()
                {
                    self.ram_data = vec![0xFF; 0x2000];
                    self.ram_banks = 1;
                }
            }
            // VF001 protection register file lives in the (MBC5-unused)
            // $6000-$7FFF range; A10-A11 select the port.
            BANKING_MODE_START..=BANKING_MODE_END
                if matches!(self.unl_mapper, UnlMapper::Vf001(_)) =>
            {
                self.vf001_write(addr, value);
            }
            // Banking Mode Select (0x6000-0x7FFF)
            BANKING_MODE_START..=BANKING_MODE_END => {
                // MBC3 timer carts latch the RTC on ANY write here (no edge
                // needed); MBC1 sets the banking mode bit; others ignore it.
                let latch = match &mut self.mapper {
                    Mapper::Mbc1(m) => {
                        m.mode = value & 0x01;
                        false
                    }
                    Mapper::Mbc3(m) => m.timer,
                    _ => false,
                };
                if latch {
                    self.latch_rtc();
                }
            }
            // External RAM (0xA000-0xBFFF)
            EXTERNAL_RAM_START..=EXTERNAL_RAM_END => {
                // Snapshot the board + its A000-BFFF-relevant registers from an
                // immutable view; the borrow is released before any &mut self
                // engine call (write_ram_byte / write_rtc_register / cam / huc3).
                enum Ext {
                    Banked(bool),
                    Mbc2(bool),
                    Mbc3Ram(bool, u8),
                    Mbc3Rtc(bool, u8),
                    Mbc7(bool),
                    HuC1(bool),
                    Camera(bool, bool),
                    HuC3(u8),
                    Unbanked,
                    Nt(bool),
                    None,
                }
                let plan = match &self.mapper {
                    Mapper::Mbc1(m) => Ext::Banked(m.has_ram && m.ram_enabled),
                    Mapper::Mbc2(m) => Ext::Mbc2(m.ram_enabled),
                    Mapper::Mbc3(m) if m.has_ram => Ext::Mbc3Ram(m.ram_enabled, m.ram_bank),
                    Mapper::Mbc3(m) => Ext::Mbc3Rtc(m.ram_enabled && m.timer, m.ram_bank),
                    Mapper::Mbc5(m) => Ext::Banked(m.has_ram && m.ram_enabled),
                    Mapper::Vf001(m) => Ext::Banked(m.ram_enabled),
                    Mapper::Mbc7(m) => Ext::Mbc7(m.ram_enabled && m.state.ram_enabled2),
                    Mapper::HuC1(m) => Ext::HuC1(m.state.ir_mode),
                    Mapper::Camera(m) => {
                        Ext::Camera(m.state.regs_selected, m.ram_enabled && !m.state.running)
                    }
                    Mapper::HuC3(m) => Ext::HuC3(m.state.mode),
                    Mapper::NoMbc(_) | Mapper::Rocket(_) | Mapper::Sachen(_) => Ext::Unbanked,
                    Mapper::NtOld(m) => Ext::Nt(m.ram_enabled),
                    _ => Ext::None,
                };
                match plan {
                    // Banked, RAMG-gated RAM (MBC1/MBC5/VF001). banked_ram_offset
                    // returns None when the board carries no RAM array.
                    Ext::Banked(true) => {
                        if let Some(offset) = self.banked_ram_offset(addr) {
                            let _ = self.write_ram_byte(offset, value);
                        }
                    }
                    // MBC2 built-in 512x4 RAM, echoing every 0x200 bytes.
                    Ext::Mbc2(true) => {
                        let offset = (addr - MBC2_RAM_START) as usize % self.mbc2_ram.len();
                        let _ = self.write_mbc2_ram_byte(offset, value);
                    }
                    Ext::Mbc3Ram(true, rb) => {
                        let ram_select_max = if self.is_mbc30() { 0x07 } else { 0x03 };
                        if rb <= ram_select_max {
                            if let Some(offset) = self.banked_ram_offset(addr) {
                                let _ = self.write_ram_byte(offset, value);
                            }
                        } else if (0x08..=0x0C).contains(&rb) {
                            self.write_rtc_register(rb, value);
                        }
                    }
                    // Timer-only MBC3 (no RAM): RTC registers only.
                    Ext::Mbc3Rtc(true, rb) if (0x08..=0x0C).contains(&rb) => {
                        self.write_rtc_register(rb, value);
                    }
                    // MBC7 registers respond only with both stages unlocked, in A000-AFFF.
                    Ext::Mbc7(true) => match (addr >> 4) & 0x0F {
                        0x0 => {
                            // Erase the accelerometer latch (re-arm re-latching).
                            if value == 0x55
                                && let Mapper::Mbc7(m) = &mut self.mapper {
                                    m.state.accel_x = 0x8000;
                                    m.state.accel_y = 0x8000;
                                    m.state.accel_latched = false;
                                }
                        }
                        0x1 => {
                            // Latch the current sample, only after an erase.
                            if value == 0xAA {
                                let (sx, sy) = (self.mbc7_sensor_x, self.mbc7_sensor_y);
                                if let Mapper::Mbc7(m) = &mut self.mapper
                                    && !m.state.accel_latched {
                                        m.state.accel_x = Self::mbc7_accel_counts(sx);
                                        m.state.accel_y = Self::mbc7_accel_counts(sy);
                                        m.state.accel_latched = true;
                                    }
                            }
                        }
                        0x8 => self.mbc7_eeprom_bus_write(value),
                        _ => {}
                    },
                    Ext::HuC1(ir) => {
                        if ir {
                            // IR transmitter: bit 0 drives the LED (latched, unobserved).
                            if let Mapper::HuC1(m) = &mut self.mapper {
                                m.state.ir_led = value & 0x01 != 0;
                            }
                        } else if let Some(offset) = self.banked_ram_offset(addr) {
                            // RAM is always enabled (no MBC1-style gate).
                            let _ = self.write_ram_byte(offset, value);
                        }
                    }
                    Ext::Camera(regs_selected, ram_ok) => {
                        if regs_selected {
                            // Register writes always enabled, mirrored every $80.
                            self.cam_reg_write(addr & 0x7F, value);
                        } else if ram_ok && let Some(offset) = self.banked_ram_offset(addr) {
                            // RAM writes need the $0A gate and are ignored mid-capture.
                            let _ = self.write_ram_byte(offset, value);
                        }
                    }
                    Ext::HuC3(mode) => match mode {
                        // RAM read/write. Mode 0x0 (read-only) ignores writes.
                        0xA => {
                            if let Some(offset) = self.banked_ram_offset(addr) {
                                let _ = self.write_ram_byte(offset, value);
                            }
                        }
                        // RTC command/argument mailbox (command bits 6-4, arg 3-0).
                        0xB => {
                            if let Mapper::HuC3(m) = &mut self.mapper {
                                m.state.rtc_command = (value >> 4) & 0x07;
                                m.state.rtc_argument = value & 0x0F;
                            }
                        }
                        // Semaphore: bit 0 clear requests the MCU execute the command.
                        0xD if value & 0x01 == 0 => self.huc3_execute_command(),
                        _ => {}
                    },
                    // NoMBC / Rocket / Sachen: straight-through, ungated.
                    Ext::Unbanked => {
                        if let Some(offset) = self.unbanked_ram_offset(addr) {
                            let _ = self.write_ram_byte(offset, value);
                        }
                    }
                    // NT/Makon old: MBC3-style enable gate, unbanked.
                    Ext::Nt(true) => {
                        if let Some(offset) = self.unbanked_ram_offset(addr) {
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
    use super::camera::{CAM_H, CAM_TILE_BYTES, CAM_W};
    use crate::memory::Addressable;

    /// Minimal in-memory ROM image with the given type/RAM-size header bytes.
    fn make_rom(cartridge_type: u8, ram_size_code: u8) -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[CARTRIDGE_TYPE_OFFSET] = cartridge_type;
        rom[ROM_SIZE_OFFSET] = 0x00;
        rom[RAM_SIZE_OFFSET] = ram_size_code;
        rom
    }

    // Synthetic 48-byte header-logo fixtures. Cartridge/unlicensed-mapper
    // detection keys ONLY on the 48-byte SUM (compared against the LOGO_SUM_*
    // constants), never on the individual bytes, so these stand-ins carry the
    // required sums without embedding any real (copyrighted) logo. Readback
    // assertions in the tests are self-consistent with whatever bytes these hold.
    const fn logo_with_sum(fill: u8, last: u8) -> [u8; 48] {
        let mut a = [fill; 48];
        a[47] = last;
        a
    }
    // Sum == LOGO_SUM_NINTENDO (5446): 47*0x71 + 0x87. Marks a "licensed" header.
    const LICENSED_LOGO: [u8; 48] = logo_with_sum(0x71, 0x87);
    // Sum == LOGO_SUM_ROCKET (2756): 47*0x39 + 0x4D. A Rocket cart's stored logo.
    const ROCKET_LOGO: [u8; 48] = logo_with_sum(0x39, 0x4D);

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

    // Sum == LOGO_SUM_VF001_LOH (4593): 47*0x60 + 0x51. The Vast Fame
    // secondary logo at $0184 on the Legend of Heroes board.
    const VF001_LOGO: [u8; 48] = logo_with_sum(0x60, 0x51);

    /// 1MB MBC5+RAM+BATTERY image carrying the VF001 detection signature: the
    /// secondary VF logo sum at $0184 and the boot protection stub at $32FC.
    /// This is the exact shape `detect_unl_mapper` keys on for Legend of Heroes.
    fn make_vf001_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x100000];
        rom[CARTRIDGE_TYPE_OFFSET] = MBC5_RAM_BATTERY;
        rom[ROM_SIZE_OFFSET] = 0x05; // 1MB / 64 banks
        rom[RAM_SIZE_OFFSET] = 0x02; // 8KB
        rom[0x184..0x1B4].copy_from_slice(&VF001_LOGO);
        rom[VF001_STUB_OFFSET..VF001_STUB_OFFSET + VF001_STUB.len()]
            .copy_from_slice(&VF001_STUB);
        rom
    }

    #[test]
    fn vf001_detects_only_with_logo_and_stub() {
        // Full signature -> VF001.
        let rom = make_vf001_rom();
        assert_eq!(
            Cartridge::detect_unl_mapper(&rom),
            UnlMapper::Vf001(Vf001State::default())
        );
        // Electrically an MBC5+RAM+BATTERY: the header type is truthful.
        let cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(
            cart.get_cartridge_type(),
            CartridgeType::MBC5 { ram: true, battery: true, .. }
        ));
        assert!(cart.has_battery());

        // Correct logo sum but no boot stub -> not VF001 (stays plain MBC5).
        let mut no_stub = make_vf001_rom();
        no_stub[VF001_STUB_OFFSET..VF001_STUB_OFFSET + VF001_STUB.len()].fill(0);
        assert_eq!(Cartridge::detect_unl_mapper(&no_stub), UnlMapper::None);

        // Stub present but the $0184 sum is wrong -> not VF001.
        let mut wrong_logo = make_vf001_rom();
        wrong_logo[0x184] = wrong_logo[0x184].wrapping_add(1);
        assert_eq!(Cartridge::detect_unl_mapper(&wrong_logo), UnlMapper::None);
    }

    /// Write the correct boot-ROM header checksum into $014D.
    fn fix_header_checksum(rom: &mut [u8]) {
        let sum = rom[0x0134..0x014D].iter().fold(0u8, |a, &b| a.wrapping_sub(b).wrapping_sub(1));
        rom[0x014D] = sum;
    }

    #[test]
    fn mapper_name_covers_common_types() {
        let cases: &[(u8, &str)] = &[
            (0x00, "ROM ONLY"),
            (MBC1, "MBC1"),
            (MBC1_RAM_BATTERY, "MBC1+RAM+Battery"),
            (MBC2_BATTERY, "MBC2+Battery"),
            (MBC3_TIMER_RAM_BATTERY, "MBC3+RTC+RAM+Battery"),
            (MBC3_RAM_BATTERY, "MBC3+RAM+Battery"),
            (MBC5_RAM_BATTERY, "MBC5+RAM+Battery"),
            (MBC5_RUMBLE_RAM_BATTERY, "MBC5+Rumble+RAM+Battery"),
            (HUC1_RAM_BATTERY, "HuC1+RAM+Battery"),
            (POCKET_CAMERA, "Pocket Camera"),
        ];
        for &(ty, name) in cases {
            let cart = Cartridge::from_bytes(&make_rom(ty, 0x02)).unwrap();
            assert_eq!(cart.mapper_name(), name, "type {ty:#04x}");
        }
    }

    #[test]
    fn rom_and_ram_size_bytes() {
        // 256 KiB MBC5+RAM+BAT with an 8 KiB RAM code.
        let mut rom = make_sized_rom(MBC5_RAM_BATTERY, 0x03, 0x40000);
        rom[RAM_SIZE_OFFSET] = 0x02; // 8 KiB
        let cart = Cartridge::from_bytes(&rom).unwrap();
        assert_eq!(cart.rom_size_bytes(), 0x40000);
        assert_eq!(cart.ram_size_bytes(), 0x2000);

        // ROM ONLY, no RAM.
        let cart = Cartridge::from_bytes(&make_rom(0x00, 0x00)).unwrap();
        assert_eq!(cart.rom_size_bytes(), 0x8000);
        assert_eq!(cart.ram_size_bytes(), 0);
    }

    #[test]
    fn destination_and_licensee() {
        let mut rom = make_rom(MBC1, 0x00);
        rom[0x014A] = 0x00;
        rom[0x014B] = 0x01; // old licensee: Nintendo
        let cart = Cartridge::from_bytes(&rom).unwrap();
        assert_eq!(cart.destination(), Some(Destination::Japanese));
        assert_eq!(cart.licensee(), Some("Nintendo"));

        // Overseas + new-licensee indirection ($014B == $33 -> read $0144-45).
        let mut rom = make_rom(MBC1, 0x00);
        rom[0x014A] = 0x01;
        rom[0x014B] = 0x33;
        rom[0x0144] = b'0';
        rom[0x0145] = b'8'; // "08" -> Capcom
        let cart = Cartridge::from_bytes(&rom).unwrap();
        assert_eq!(cart.destination(), Some(Destination::Overseas));
        assert_eq!(cart.licensee(), Some("Capcom"));
    }

    #[test]
    fn header_and_global_checksum() {
        let mut rom = make_rom(MBC3_RAM_BATTERY, 0x03);
        fix_header_checksum(&mut rom);
        let cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(cart.header_checksum_valid());
        // global_checksum sums every byte except $014E-$014F.
        let expected: u16 = rom
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != 0x014E && i != 0x014F)
            .fold(0u16, |a, (_, &b)| a.wrapping_add(b as u16));
        assert_eq!(cart.global_checksum(), expected);

        // Corrupt a header byte -> checksum no longer matches.
        let mut bad = rom.clone();
        bad[0x0140] = bad[0x0140].wrapping_add(1);
        let cart = Cartridge::from_bytes(&bad).unwrap();
        assert!(!cart.header_checksum_valid());
    }

    #[test]
    fn vf001_serves_protection_transform_table() {
        let mut cart = Cartridge::from_bytes(&make_vf001_rom()).unwrap();

        // Arm each command by writing its three bytes to port 0 ($7080), pick
        // a select port ($7480/$7880), and read the value back through the RAM
        // window. Ports: A10-A11 of the write address; the read port likewise.
        let arm = |cart: &mut Cartridge, bytes: [u8; 3]| {
            for b in bytes {
                cart.write(0x7080, b);
            }
        };

        // Boot gate: cmd $9A,$B8,$B9 -> $A800 returns $C1 (sel $B9) / $F8 (sel $83).
        arm(&mut cart, [0x9A, 0xB8, 0xB9]);
        cart.write(0x7480, 0xB9); // select via port 1
        assert_eq!(cart.read(0xA800), 0xC1);
        cart.write(0x7480, 0x83);
        assert_eq!(cart.read(0xA800), 0xF8);

        // Second gate: cmd $37,$52,$CD -> $A800 returns $82 (sel $BA) / $8F (sel $A9).
        arm(&mut cart, [0x37, 0x52, 0xCD]);
        cart.write(0x7880, 0xBA); // select via port 2
        assert_eq!(cart.read(0xA800), 0x82);
        cart.write(0x7880, 0xA9);
        assert_eq!(cart.read(0xA800), 0x8F);

        // Bank-switch command drives the MBC5 ROM-bank register to 6.
        arm(&mut cart, [0x7E, 0x29, 0x79]);
        assert!(matches!(&cart.mapper, Mapper::Vf001(m) if m.regs.rom_bank_low == 6));
        assert_eq!(cart.read(0xAFFF), 0x31); // port 3 decoy readback

        // An unarmed read falls through to normal cart RAM (saves still work).
        cart.write(0x0000, 0x0A); // RAMG on
        arm(&mut cart, [0x00, 0x00, 0x00]);
        cart.write(0xA400, 0x5A);
        assert_eq!(cart.read(0xA400), 0x5A);
    }

    #[test]
    fn vf001_protection_state_is_volatile_across_reset() {
        let mut cart = Cartridge::from_bytes(&make_vf001_rom()).unwrap();
        cart.write(0x7080, 0x9A);
        cart.write(0x7080, 0xB8);
        cart.write(0x7080, 0xB9);
        cart.write(0x7480, 0xB9);
        assert_eq!(cart.read(0xA800), 0xC1); // armed
        cart.reset();
        // After a power cycle the register file is blank: the same read no
        // longer matches any command and falls through to RAM (0xFF, RAMG off).
        assert_eq!(cart.read(0xA800), 0xFF);
        assert_eq!(cart.unl_mapper, UnlMapper::Vf001(Vf001State::default()));
    }

    #[test]
    fn licensed_shapes_are_not_misdetected() {
        // Plain 32KB ROM-only cart with the Nintendo logo (e.g. Tetris).
        let mut rom = make_rom(0x00, 0x00);
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        rom[0x134..0x13A].copy_from_slice(b"TETRIS");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);

        // 128KB MBC1 cart titled GAME (the shape of the owner's descrambled
        // Sachen singles): must stay plain MBC1.
        let mut rom = make_sized_rom(0x01, 0x02, 0x20000);
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        rom[0x134..0x138].copy_from_slice(b"GAME");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);
        let cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(cart.get_cartridge_type(), CartridgeType::MBC1 { .. }));

        // Header claims 32KB but the file is 2MB with a normal logo
        // (gbmicrotest shape, type $03): still MBC1, never Wisdom Tree.
        let mut rom = make_sized_rom(0x03, 0x00, 0x200000);
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        rom[0x134..0x13D].copy_from_slice(b"microtest");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);

        // A real 256KB MBC3+RAM+BATTERY ($10) cart NOT titled "TETRIS SET"
        // must stay MBC3 -- M161 detection is gated on the exact title.
        let mut rom = make_sized_rom(0x10, 0x03, 0x40000);
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        rom[0x134..0x13B].copy_from_slice(b"POKEMON");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);
        assert!(matches!(
            Cartridge::from_bytes(&rom).unwrap().get_cartridge_type(),
            CartridgeType::MBC3 { .. }
        ));

        // A genuine 1MB MBC5+RAM+BATTERY cart with the Nintendo logo must stay
        // MBC5: VF001 needs BOTH the VF secondary logo sum at $0184 AND the
        // boot stub, so a licensed MBC5 can never match.
        let mut rom = make_sized_rom(MBC5_RAM_BATTERY, 0x05, 0x100000);
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        rom[0x134..0x139].copy_from_slice(b"MBC5G");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);
    }

    #[test]
    fn m161_latches_a_32kb_bank_once() {
        // Mani 4 in 1 shape: 256KB, header spoofs MBC3+RAM+BAT ($10), title
        // "TETRIS SET" (M161 board).
        let mut rom = make_sized_rom(0x10, 0x03, 0x40000);
        rom[0x134..0x13E].copy_from_slice(b"TETRIS SET");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::M161);

        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(cart.get_cartridge_type(), CartridgeType::M161));
        assert!(!cart.has_battery()); // RAM disabled + zeroed header type

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
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
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
        // Rocket carts store their own logo (sums to 2756), which is what the
        // detection keys on.
        let mut rom = make_sized_rom(0x97, 0x04, 0x80000);
        rom[0x104..0x134].copy_from_slice(&ROCKET_LOGO);
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::Rocket);

        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        cart.skip_boot_handoff(); // no boot ROM: start unlocked
        // Unlocked reads return the raw (Rocket) logo.
        assert_eq!(cart.read(0x0104), ROCKET_LOGO[0]);
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

        // Boot lock: a fresh cart is locked; after 0x30 ROM reads it enters the
        // CGB phase where $0104-$0133 present the logo the boot ROM supplied
        // (the boot ROM check), and after 0x30 more it unlocks. The logo is
        // sourced from the boot ROM at runtime; simulate that here.
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        cart.set_rocket_boot_logo(LICENSED_LOGO);
        for _ in 0..0x31 {
            cart.read(0x0000);
        }
        assert_eq!(cart.read(0x0104), LICENSED_LOGO[0]);
        assert_eq!(cart.read(0x0105), LICENSED_LOGO[1]);
        for _ in 0..0x31 {
            cart.read(0x0000);
        }
        // Unlocked again: raw (Rocket) logo.
        assert_eq!(cart.read(0x0104), ROCKET_LOGO[0]);
    }

    #[test]
    fn sachen_mmc1_descramble_lock_and_masked_banking() {
        // Raw-dump shape: the Nintendo logo lives at the DESCRAMBLED
        // positions of $0104 (CPU reads through the $01xx address swizzle),
        // and the Sachen logo (here: marker bytes) at the |0x80 copy.
        let mut rom = make_sized_rom(0x00, 0x00, 0x20000);
        for i in 0..48u16 {
            rom[Cartridge::sachen_unscramble(0x104 + i) as usize] = LICENSED_LOGO[i as usize];
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
        assert_eq!(cart.read(0x0104), LICENSED_LOGO[0]);
        assert_eq!(cart.read(0x0105), LICENSED_LOGO[1]);
        assert_eq!(cart.read(0x0133), LICENSED_LOGO[47]);

        // Masked outer banking: base/mask only latch while
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
        assert_eq!(fresh.read(0x0104), LICENSED_LOGO[0]);
    }

    #[test]
    fn nt_old2_swap_multicart_and_ram_declare() {
        // Super Mario Special 3 shape: MBC1-spoofing header, Makon "MK"
        // licensee, 256KB.
        let mut rom = make_sized_rom(0x01, 0x03, 0x40000);
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        rom[0x134..0x141].copy_from_slice(b"SUPER MARIO 3");
        rom[0x144] = b'M';
        rom[0x145] = b'K';
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::NtOld2);

        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        // MBC3-style 8-bit bank, 0 -> 1.
        cart.write(0x2000, 0x05);
        assert_eq!(cart.read(0x5000), 5);
        // $5003 bit-swap mode: bank lines reorder combinationally
        // (reorder: out0=in1, out1=in2, out2=in0).
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
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        rom[0x134..0x13A].copy_from_slice(b"SONIC5");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::ForceMbc1);
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(cart.get_cartridge_type(), CartridgeType::MBC1 { ram: false, battery: false }));
        assert!(cart.ram_data.is_empty());
        cart.write(0x2000, 0x0B);
        assert_eq!(cart.read(0x5000), 11);

        // Captain Knick-Knack: type $00 with a Tetris header on a 128KB file.
        let mut rom = make_sized_rom(0x00, 0x00, 0x20000);
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
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
        cart.rtc.seconds = regs.0;
        cart.rtc.minutes = regs.1;
        cart.rtc.hours = regs.2;
        cart.rtc.days_low = regs.3;
        cart.rtc.days_high = regs.4;
    }

    fn mbc3_rtc(cart: &Cartridge) -> (u8, u8, u8, u8, u8) {
        (
            cart.rtc.seconds,
            cart.rtc.minutes,
            cart.rtc.hours,
            cart.rtc.days_low,
            cart.rtc.days_high,
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
        cart.rtc_latched.seconds = 33;
        cart.rtc_latched.minutes = 44;
        cart.rtc_latched.hours = 12;
        cart.rtc_latched.days_low = 0x12;
        cart.rtc_latched.days_high = 0x81;

        let blob = cart.mbc3_rtc_serialize(0x0102_0304_0506_0708);
        assert_eq!(blob.len(), 48);
        // Spot-check the documented layout: LE u32 fields in the de-facto order.
        assert_eq!(&blob[0..4], &[61, 0, 0, 0]);
        assert_eq!(&blob[16..20], &[0xC1, 0, 0, 0]);
        assert_eq!(&blob[20..24], &[33, 0, 0, 0]);
        assert_eq!(&blob[40..48], &0x0102_0304_0506_0708u64.to_le_bytes());

        let mut restored = mbc3_rtc_cart();
        let ts = restored.mbc3_rtc_deserialize(&blob).unwrap();
        assert_eq!(ts, 0x0102_0304_0506_0708);
        assert_eq!(mbc3_rtc(&restored), (61, 5, 17, 0xAB, 0xC1));
        assert_eq!(restored.rtc_latched.seconds, 33);
        assert_eq!(restored.rtc_latched.days_high, 0x81);
    }

    /// The legacy 44-byte variant (32-bit timestamp, from older tools) must be
    /// accepted, mirroring the de-facto format's -4 / sizeof-4 read leeway.
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
        cart.huc3_rtc.mem[0x58] = 0x7; // event-time nibble
        let blob = cart.huc3_rtc_serialize(0xDEAD_BEEF);
        assert_eq!(blob.len(), 136);
        // Nibble packing: nibble N -> byte N/2, even N in the low half. Minutes
        // 0x2A5 -> nibbles 0x10=0x5, 0x11=0xA, 0x12=0x2; days 0x123 ->
        // 0x13=0x3. Byte 8 = nib 0x10|0x11<<4, byte 9 = nib 0x12|0x13<<4.
        assert_eq!(blob[0x08], 0xA5);
        assert_eq!(blob[0x09], 0x32);
        let mut restored = huc3_cart();
        let ts = restored.huc3_rtc_deserialize(&blob).unwrap();
        assert_eq!(ts, 0xDEAD_BEEF);
        assert_eq!(restored.huc3_clock(), (0x2A5, 0x123));
        assert_eq!(restored.huc3_rtc.mem[0x58], 0x7);
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

    /// A `.sav` with a de-facto RTC footer (RAM + 48 bytes) restores both
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
        assert!((11..=13).contains(&s), "seconds {s}");
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

    /// The libretro RETRO_MEMORY_RTC region: stable pointer, de-facto-format
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

    /// HuC-3 carts expose the 136-byte blob through the libretro view.
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
        assert!(matches!(&cart.mapper, Mapper::HuC1(m) if m.state.ir_led));
        cart.write(0xA000, 0x00);
        assert!(matches!(&cart.mapper, Mapper::HuC1(m) if !m.state.ir_led));
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

    /// Trimmed MBC1M dump shape (Mortal Kombat I & II): menu bank + two
    /// contiguous 256KB games, 33 banks total; header MBC1 with a 64-bank
    /// size byte; checksum-valid headers carrying the base logo at file
    /// offsets 0, 0x4000 and 0x44000.
    fn make_trimmed_mbc1m() -> Vec<u8> {
        let mut rom = make_sized_rom(0x01, 0x05, 33 * 0x4000);
        for base in [0usize, 0x4000, 0x44000] {
            rom[base + 0x104..base + 0x134].copy_from_slice(&LICENSED_LOGO);
            rom[base + 0x147] = 0x01;
            rom[base + 0x148] = if base == 0 { 0x05 } else { 0x03 };
            let sum = rom[base + 0x134..base + 0x14D]
                .iter()
                .fold(0u8, |a, &b| a.wrapping_sub(b).wrapping_sub(1));
            rom[base + 0x14D] = sum;
        }
        rom
    }

    #[test]
    fn trimmed_mbc1m_dump_reconstructs_physical_layout() {
        let rom = make_trimmed_mbc1m();
        let out = Cartridge::reconstruct_trimmed_mbc1m(&rom).unwrap();
        assert_eq!(out.len(), 0x100000);
        // Menu keeps slot 0; the rest of its slot is 0xFF padding.
        assert_eq!(out[0x1000], 0);
        assert!(out[0x4000..0x40000].iter().all(|&b| b == 0xFF));
        // Game 1 re-bases 0x4000 -> 0x40000 (file banks 1..17).
        assert_eq!(out[0x40000 + 0x1000], 1);
        assert_eq!(out[0x7C000 + 0x1000], 16);
        // Game 2 re-bases 0x44000 -> 0x80000 (file banks 17..33).
        assert_eq!(out[0x80000 + 0x1000], 17);
        assert_eq!(out[0xBC000 + 0x1000], 32);
        // Empty slot 3.
        assert!(out[0xC0000..].iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn trimmed_mbc1m_loads_as_multicart_and_banks_physically() {
        let rom = make_trimmed_mbc1m();
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(cart.mbc1_multicart);
        assert_eq!(cart.rom_banks, 64);
        // The menu's launch sequence: BANK2 = 1 + mode 1 re-homes 0x0000 to
        // game 1's first bank (physical 0x10); BANK1 selects within the game.
        cart.write(0x2000, 0x01);
        cart.write(0x4000, 0x01);
        cart.write(0x6000, 0x01);
        assert_eq!(cart.read(0x1000), 1); // bank0 window = game 1 home bank
        assert_eq!(cart.read(0x5000), 2); // banked window = game 1 bank 1
    }

    /// `Cartridge::reset` = power cycle: after the MBC1M menu's launch
    /// sequence re-homed the bank-0 window to a game, reset must return every
    /// MBC1 latch to its power-on value so the window reads the menu again
    /// (Mortal Kombat I & II: frontend Reset previously restarted into the
    /// last-selected game).
    #[test]
    fn reset_rehomes_mbc1m_to_menu() {
        let rom = make_trimmed_mbc1m();
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        cart.write(0x0000, 0x0A); // RAMG on
        cart.write(0x2000, 0x01); // BANK1
        cart.write(0x4000, 0x01); // BANK2 -> game 1
        cart.write(0x6000, 0x01); // MODE 1 re-homes the 0x0000 window
        assert_eq!(cart.read(0x1000), 1); // game 1 home bank, not the menu

        cart.reset();
        assert_eq!(cart.read(0x1000), 0); // menu bank back in the 0x0000 window
        let Mapper::Mbc1(m) = &cart.mapper else { panic!("expected MBC1") };
        assert!(!m.ram_enabled);
        assert_eq!(m.rom_bank_low, 1);
        assert_eq!(m.bank2, 0);
        assert_eq!(m.mode, 0);
    }

    /// MBC3 reset: the latch registers and bank selects clear, but the RTC
    /// time itself (and cart RAM) is battery-fed and survives.
    #[test]
    fn reset_clears_mbc3_latches_but_keeps_rtc_time() {
        let mut cart = mbc3_rtc_cart();
        set_mbc3_rtc(&mut cart, (12, 34, 5, 0x67, 0x01));
        cart.write(0x0000, 0x0A); // RAMG on
        cart.write(0x2000, 0x15); // ROM bank
        cart.write(0x4000, 0x08); // map RTC seconds
        cart.write(0x6000, 0x00); // latch edge
        cart.write(0x6000, 0x01);
        assert_eq!(cart.rtc_latched.seconds, 12);
        cart.ram_data[0] = 0x5A; // battery RAM must survive

        cart.reset();
        let Mapper::Mbc3(m) = &cart.mapper else { panic!("expected MBC3") };
        assert!(!m.ram_enabled);
        assert_eq!(m.rom_bank_low, 1);
        assert_eq!(m.ram_bank, 0);
        assert_eq!(
            (
                cart.rtc_latched.seconds,
                cart.rtc_latched.minutes,
                cart.rtc_latched.hours,
                cart.rtc_latched.days_low,
                cart.rtc_latched.days_high,
            ),
            (0, 0, 0, 0, 0)
        );
        assert_eq!(mbc3_rtc(&cart), (12, 34, 5, 0x67, 0x01)); // clock kept ticking
        assert_eq!(cart.ram_data[0], 0x5A);
    }

    /// MBC5 reset: bank registers re-home (ROMB0=1, ROMB1=0, RAMB=0) and the
    /// rumble motor line drops.
    #[test]
    fn reset_rehomes_mbc5_banks_and_stops_rumble() {
        let mut cart = Cartridge::from_bytes(&make_rom(MBC5_RUMBLE_RAM, 0x03)).unwrap();
        cart.write(0x0000, 0x0A);
        cart.write(0x2000, 0x42);
        cart.write(0x3000, 0x01);
        cart.write(0x4000, 0x0A); // RAM bank 2 + motor on
        assert!(cart.rumble_active());

        cart.reset();
        let Mapper::Mbc5(m) = &cart.mapper else { panic!("expected MBC5") };
        assert!(!m.ram_enabled);
        assert_eq!(m.regs.rom_bank_low, 1);
        assert_eq!(m.regs.rom_bank_high, 0);
        assert_eq!(m.regs.ram_bank, 0);
        assert!(!cart.rumble_active());
    }

    /// Completeness proof for reset()'s carry list: hammer a cart's mapper
    /// registers and persist domain, reset, and require the serialized bytes
    /// to equal a same-ROM POWER-ON cart with only the persist domain grafted
    /// in. Any serialized field that reset() fails to return to its power-on
    /// value (or wrongly volatilizes) breaks the byte equality, for every
    /// mapper family.
    #[test]
    fn reset_is_power_on_plus_persist_domain() {
        let roms: Vec<Vec<u8>> = vec![
            make_rom(MBC1_RAM_BATTERY, 0x03),
            make_rom(MBC2_BATTERY, 0x00),
            make_rom(MBC3_TIMER_RAM_BATTERY, 0x03),
            make_rom(MBC5_RUMBLE_RAM, 0x03),
            make_rom(MBC7_SENSOR_RUMBLE_RAM_BATTERY, 0x00),
            make_rom(HUC1_RAM_BATTERY, 0x03),
            make_rom(HUC3, 0x03),
            make_rom(POCKET_CAMERA, 0x03),
            make_trimmed_mbc1m(),
            make_vf001_rom(),
        ];
        for rom in roms {
            let mut cart = Cartridge::from_bytes(&rom).unwrap();
            let ct = cart.cartridge_type;
            // Hammer every mapper-register window (enable gates, bank
            // registers, modes, latches).
            cart.write(0x0000, 0x0A);
            for addr in (0x0000..0x8000u16).step_by(0x100) {
                cart.write(addr | 0x55, 0x03);
            }
            // Dirty the persist domain: battery RAM, RTC time + accumulators.
            if !cart.ram_data.is_empty() {
                cart.ram_data[0] = 0xA5;
            }
            cart.mbc2_ram[3] = 0x0F;
            cart.rtc.seconds = 12;
            cart.rtc.minutes = 34;
            cart.rtc.hours = 5;
            cart.rtc.days_low = 0x67;
            cart.rtc.days_high = 0x01;
            cart.rtc_cycle_accum = 999;
            if !cart.huc3_rtc.mem.is_empty() {
                cart.huc3_rtc.mem[0x10] = 0xA;
            }
            cart.huc3_rtc.accum = 777;

            cart.reset();

            let mut fresh = Cartridge::from_bytes(&rom).unwrap();
            fresh.ram_data = cart.ram_data.clone();
            fresh.mbc2_ram = cart.mbc2_ram.clone();
            fresh.rtc.seconds = cart.rtc.seconds;
            fresh.rtc.minutes = cart.rtc.minutes;
            fresh.rtc.hours = cart.rtc.hours;
            fresh.rtc.days_low = cart.rtc.days_low;
            fresh.rtc.days_high = cart.rtc.days_high;
            fresh.rtc_cycle_accum = cart.rtc_cycle_accum;
            fresh.huc3_rtc.mem = cart.huc3_rtc.mem.clone();
            fresh.huc3_rtc.accum = cart.huc3_rtc.accum;
            assert_eq!(
                bincode::serialize(&cart).unwrap(),
                bincode::serialize(&fresh).unwrap(),
                "cartridge type {ct:#04x}: reset != power-on + persist domain"
            );
        }
    }

    #[test]
    fn attach_rom_expands_trimmed_mbc1m() {
        let rom = make_trimmed_mbc1m();
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        let expanded = cart.detach_rom();
        // Re-attaching the ORIGINAL file bytes (savestate reload path) must
        // produce the same physical image as the constructor did.
        cart.attach_rom(rom);
        assert_eq!(&cart.rom_data[..], &expanded[..]);
    }

    #[test]
    fn trimmed_mbc1m_predicate_rejects_normal_shapes() {
        let rom = make_trimmed_mbc1m();

        // Proper 1MB image: nothing to reconstruct (existing detection path).
        let mut full = rom.clone();
        full.resize(0x100000, 0xFF);
        assert!(Cartridge::reconstruct_trimmed_mbc1m(&full).is_none());

        // Non-MBC1 type byte.
        let mut t = rom.clone();
        t[CARTRIDGE_TYPE_OFFSET] = 0x13;
        assert!(Cartridge::reconstruct_trimmed_mbc1m(&t).is_none());

        // Header ROM-size byte other than 64 banks.
        let mut t = rom.clone();
        t[ROM_SIZE_OFFSET] = 0x04;
        assert!(Cartridge::reconstruct_trimmed_mbc1m(&t).is_none());

        // Uniform filler logo must not self-match.
        let mut t = rom.clone();
        for base in [0usize, 0x4000, 0x44000] {
            t[base + 0x104..base + 0x134].copy_from_slice(&[0u8; 48]);
        }
        assert!(Cartridge::reconstruct_trimmed_mbc1m(&t).is_none());

        // Corrupting game 2's header checksum leaves a >256KB segment: bail.
        let mut t = rom.clone();
        t[0x44000 + 0x14D] ^= 0xFF;
        assert!(Cartridge::reconstruct_trimmed_mbc1m(&t).is_none());

        // Single-header short-of-header MBC1 dump: stays plain MBC1.
        let mut single = make_sized_rom(0x01, 0x05, 33 * 0x4000);
        single[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        let sum = single[0x134..0x14D]
            .iter()
            .fold(0u8, |a, &b| a.wrapping_sub(b).wrapping_sub(1));
        single[0x14D] = sum;
        assert!(Cartridge::reconstruct_trimmed_mbc1m(&single).is_none());
        let cart = Cartridge::from_bytes(&single).unwrap();
        assert!(!cart.mbc1_multicart);
    }

    fn save_test_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rustyboi-{tag}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// MBC2's built-in RAM is physically 512 x 4 bits, so the upper nibble has
    /// no storage cell. Every load path must mask it off, or `save_ram()`
    /// exports and the streamed sidecar carry bits the silicon cannot hold —
    /// and the two entry points must agree byte-for-byte.
    #[test]
    fn mbc2_save_loads_mask_to_four_bits_on_every_path() {
        let image: Vec<u8> = (0..MBC2_RAM_SIZE).map(|i| (i as u8) | 0xF0).collect();

        // Bytes entry point.
        let mut via_bytes = Cartridge::from_bytes(&make_rom(MBC2_BATTERY, 0x00)).unwrap();
        via_bytes.load_sram_bytes(&image).unwrap();
        assert!(
            via_bytes.save_ram().iter().all(|&b| b & 0xF0 == 0),
            "load_sram_bytes left MBC2 upper nibbles set"
        );

        // Path entry point.
        let dir = save_test_dir("mbc2-mask");
        let sav = dir.join("game.sav");
        fs::write(&sav, &image).unwrap();
        let mut via_path = Cartridge::from_bytes(&make_rom(MBC2_BATTERY, 0x00)).unwrap();
        via_path.attach_save_file(&sav).unwrap();
        assert!(
            via_path.save_ram().iter().all(|&b| b & 0xF0 == 0),
            "attach_save_file left MBC2 upper nibbles set"
        );

        assert_eq!(via_bytes.save_ram(), via_path.save_ram());
        // The masking is emulation-invisible (the read path re-masks the
        // undriven upper lines), but the exported image is what the user's
        // .sav ends up holding. RAMG must be open for the array to answer.
        via_path.write(0x0000, 0x0A);
        assert_eq!(via_path.read(0xA001), 0xF0 | (image[1] & 0x0F));

        fs::remove_dir_all(&dir).ok();
    }

    /// An oversized save loads its RAM-sized prefix rather than being silently
    /// discarded — including on MBC2, which used to skip the file entirely
    /// while still opening it for streaming writes.
    #[test]
    fn oversized_save_loads_prefix_instead_of_being_skipped() {
        let dir = save_test_dir("oversize");

        // MBC2: 512-byte array, hand it 512 + a trailing footer.
        let mut image: Vec<u8> = vec![0x03; MBC2_RAM_SIZE];
        image.extend_from_slice(&[0xAB; 64]);
        let sav = dir.join("mbc2.sav");
        fs::write(&sav, &image).unwrap();
        let mut cart = Cartridge::from_bytes(&make_rom(MBC2_BATTERY, 0x00)).unwrap();
        cart.attach_save_file(&sav).unwrap();
        assert!(
            cart.save_ram().iter().all(|&b| b == 0x03),
            "oversized MBC2 save was skipped instead of prefix-loaded"
        );

        // Non-MBC2 keeps its long-standing prefix behavior (the de-facto
        // RTC-footer .sav format depends on it).
        let mut image: Vec<u8> = vec![0x5A; 0x2000];
        image.extend_from_slice(&[0xCD; 48]);
        let sav = dir.join("mbc3.sav");
        fs::write(&sav, &image).unwrap();
        let mut cart = Cartridge::from_bytes(&make_rom(MBC3_RAM_BATTERY, 0x02)).unwrap();
        cart.attach_save_file(&sav).unwrap();
        assert!(cart.save_ram()[..0x2000].iter().all(|&b| b == 0x5A));

        fs::remove_dir_all(&dir).ok();
    }

    /// Build an in-memory zip from (name, bytes) members.
    fn make_zip(members: &[(&str, Vec<u8>)]) -> Vec<u8> {
        use zip::write::SimpleFileOptions;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default();
        for (name, bytes) in members {
            w.start_file(*name, opts).unwrap();
            std::io::Write::write_all(&mut w, bytes).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    /// The extractor prefers a Game Boy extension over a larger sibling, and
    /// falls back to the largest member when no member carries one.
    #[test]
    fn zip_extraction_prefers_rom_extension_then_largest() {
        let rom = make_rom(MBC1, 0x00);
        let zipped = make_zip(&[
            ("readme.txt", vec![0xAA; rom.len() * 2]),
            ("game.gb", rom.clone()),
        ]);
        assert_eq!(Cartridge::extract_rom_from_zip_bytes(&zipped).unwrap(), rom);

        // No ROM extension anywhere: the largest non-directory member wins.
        let big = make_rom(MBC1, 0x00);
        let zipped = make_zip(&[("small.bin", vec![0x11; 16]), ("big.bin", big.clone())]);
        assert_eq!(Cartridge::extract_rom_from_zip_bytes(&zipped).unwrap(), big);

        // Nothing usable at all.
        assert!(Cartridge::extract_rom_from_zip_bytes(&make_zip(&[])).is_err());
    }

    /// `load` (path entry point) and `extract_rom_bytes` (bytes entry point)
    /// must agree, since both now route through one extractor.
    #[test]
    fn zip_path_and_bytes_entry_points_agree() {
        let rom = make_rom(MBC1, 0x00);
        let zipped = make_zip(&[("decoy.txt", vec![0xAA; 4]), ("game.gb", rom.clone())]);

        let dir = std::env::temp_dir().join(format!(
            "rustyboi-zip-test-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("game.zip");
        fs::write(&zip_path, &zipped).unwrap();

        let from_path = Cartridge::load(zip_path.to_str().unwrap()).unwrap();
        assert_eq!(Cartridge::extract_rom_bytes(&zipped).unwrap(), rom);
        assert_eq!(from_path.rom_data[..rom.len()], rom[..]);

        fs::remove_dir_all(&dir).ok();
    }
}
