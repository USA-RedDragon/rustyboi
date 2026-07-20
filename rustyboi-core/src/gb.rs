use crate::cartridge;
use crate::cpu;
use crate::cpu::registers;
use crate::memory;
use crate::memory::Addressable;
use crate::ppu;
use crate::audio;
use crate::sgb_system_palette;

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io;

/// TV region of the host the machine is plugged into. Only the SGB1 cares: it
/// has no crystal of its own and divides the host SNES's master clock by 5, so
/// an NTSC and a PAL SNES clock the same cartridge at different rates. Every
/// other model (including the SGB2, which *does* have its own crystal) runs at
/// the DMG's 4.194304 MHz regardless.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, clap::ValueEnum, PartialEq, Eq)]
pub enum Region {
    #[default]
    Ntsc,
    Pal,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, clap::ValueEnum, PartialEq, Eq)]
pub enum Hardware {
    DMG,  // Original DMG-01
    DMG0, // Very early Japanese DMG-01
    MGB,  // Game Boy Pocket
    SGB,  // Super Game Boy
    SGB2, // Super Game Boy 2
    CGB0, // Game Boy Color, CGB-CPU-0 (earliest revision; post-boot DIV phase
          // leads CGB-A..E by 512 master-cc, plus the CGB-B-or-earlier APU
          // length-glitch gate below)
    CGBB, // Game Boy Color, CPU-CGB-A/B (boot state == CGB; differs only in
          // the CGB-B-or-earlier APU length-glitch gate (CGB silicon at
          // revision B or older). SameSuite *_extra_length_clocking-cgbB)
    CGB,  // Game Boy Color, CGB-CPU-A..E
    CGBE, // Game Boy Color, CPU-CGB-D/E APU revision (boot state == CGB; the
          // observable difference is the APU C-vs-D/E gate set (CGB-C-or-earlier
          // vs CGB-D/E). Default CGB models cgb04c/CPU-CGB-C —
          // the reference-capture silicon; SameSuite hardware is CPU-CGB-E.)
    AGB,  // Game Boy Advance in GBC-compatibility mode (CGB + isAgb() diffs)
}

/// The DMG's crystal: 4.194304 MHz. Every model but the SGB1 runs at this rate.
pub(crate) const DMG_CPU_HZ: u32 = 4_194_304;
/// NTSC SGB1: the NTSC SNES master clock (21.477270 MHz) / 5. ~2.4% fast.
pub(crate) const SGB_NTSC_CPU_HZ: u32 = 21_477_270 / 5;
/// PAL SGB1: the PAL SNES master clock (21.281370 MHz) / 5. ~1.5% fast.
pub(crate) const SGB_PAL_CPU_HZ: u32 = 21_281_370 / 5;

impl Hardware {
    /// The machine's CPU/dot clock in Hz — a **real-time** quantity only. The
    /// emulated dot timeline is deliberately unaffected (a frame is 70224 dots
    /// on every model); this rate maps that timeline onto wall-clock seconds,
    /// so it sets audio pitch, the host sample cadence, and the presented frame
    /// rate, and nothing else.
    ///
    /// The SGB1 has no crystal: it divides the host SNES's master clock by 5,
    /// so it runs ~2.4% fast on NTSC and ~1.5% fast on PAL. The SGB2 added its
    /// own crystal precisely to fix that, so it is region-independent and
    /// exactly DMG-rate.
    pub fn cpu_hz(self, region: Region) -> u32 {
        match self {
            Hardware::SGB => match region {
                Region::Ntsc => SGB_NTSC_CPU_HZ,
                Region::Pal => SGB_PAL_CPU_HZ,
            },
            _ => DMG_CPU_HZ,
        }
    }

    /// AGB (GBA-in-GBC-mode) hardware. AGB behaves like CGB everywhere except
    /// the small AGB-vs-CGB diff set (PPU line-153/last-line/LYC timing,
    /// APU ch3 wave-RAM, GBA_FLAG power-on registers).
    pub(crate) fn is_agb(self) -> bool {
        matches!(self, Hardware::AGB)
    }

    /// Whether this hardware runs the CGB feature set (CGB or AGB). Used to
    /// decide CGB-vs-DMG behavior; AGB is a CGB for all CGB-feature purposes.
    pub(crate) fn is_cgb_like(self) -> bool {
        matches!(self, Hardware::CGB0 | Hardware::CGBB | Hardware::CGB | Hardware::CGBE | Hardware::AGB)
    }

    /// CGB-B-or-earlier APU revision gate (CGB silicon at revision B or
    /// older): the NRx4 length-enable extra-clock glitch
    /// fires regardless of the written bit-6 value ("current value is
    /// irrelevant on CGB-B and older"). SameSuite
    /// channel_*_extra_length_clocking-cgb0B/-cgb0/-cgbB validate this fork.
    pub(crate) fn is_cgb_b_or_earlier(self) -> bool {
        matches!(self, Hardware::CGB0 | Hardware::CGBB)
    }

    /// CGB-D/E APU revision gate (CGB silicon newer than revision C). The
    /// default `CGB` models cgb04c (CPU-CGB-C, the reference-capture silicon);
    /// `CGBE` models the CPU-CGB-D/E silicon SameSuite was validated on.
    /// AGB intentionally stays on the C side: rustyboi's AGB model is pinned
    /// to the AGB reference oracle (a strict revision order would place AGB > CGB_E).
    pub(crate) fn is_cgb_d_or_later(self) -> bool {
        matches!(self, Hardware::CGBE)
    }

    /// Which analog output stage this machine wires up. Pan Docs: the high-pass
    /// "is more aggressive on GBA than on GBC, which itself is more aggressive
    /// than on DMG"; blargg measured the MGB on the CGB side of that split.
    /// The SGBs feed the SNES's own audio path, but their Game-Boy-side APU is
    /// DMG silicon, so they take the DMG filter.
    pub(crate) fn analog_model(self) -> crate::audio::AnalogModel {
        match self {
            Hardware::DMG | Hardware::DMG0 | Hardware::SGB | Hardware::SGB2 => {
                crate::audio::AnalogModel::Dmg
            }
            Hardware::AGB => crate::audio::AnalogModel::Agb,
            Hardware::MGB | Hardware::CGB0 | Hardware::CGBB | Hardware::CGB | Hardware::CGBE => {
                crate::audio::AnalogModel::CgbMgb
            }
        }
    }
}

/// How a cartridge pairs with a given piece of hardware. No variant means the
/// ROM won't run — it describes what the pairing does, so a consumer can decide
/// whether (and how) to tell the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compatibility {
    /// DMG cartridge on any hardware, or a CGB cartridge on CGB/AGB hardware.
    Full,
    /// A CGB-compatible cartridge on DMG-class hardware: runs, but in DMG
    /// (monochrome) mode with no CGB features.
    DmgModeFallback,
    /// A CGB-only cartridge on DMG-class hardware: boots to the cartridge's own
    /// hardware-mismatch screen instead of the game.
    CgbOnlyOnDmg,
}

/// Classify how `cartridge` pairs with `hardware`. Pure function of the
/// cartridge header's CGB flag and the hardware model.
pub(crate) fn cartridge_compatibility(hardware: Hardware, cartridge: &cartridge::Cartridge) -> Compatibility {
    match (hardware, cartridge.get_cgb_support()) {
        (h, cartridge::CgbSupport::Only) if !h.is_cgb_like() => Compatibility::CgbOnlyOnDmg,
        (h, cartridge::CgbSupport::Compatible) if !h.is_cgb_like() => Compatibility::DmgModeFallback,
        _ => Compatibility::Full,
    }
}

#[derive(Serialize, Deserialize)]
pub struct GB {
    cpu: cpu::SM83,
    mmio: memory::mmio::Mmio,
    ppu: ppu::Ppu,
    hardware: Hardware,
    // The DMG/MGB mono base palette (presentation only — composes with the PPU's
    // colour correction to colour a `Frame::Monochrome`). Not machine state, so
    // it is skipped in the savestate; the frontend re-applies it after a restore.
    #[serde(skip, default)]
    dmg_palette: DmgPaletteChoice,
    // How a mono game is colourized on SGB/SGB2 (presentation only, same
    // contract as `dmg_palette`: skipped in the savestate, re-applied by the
    // frontend after a restore). Ignored on every other model.
    #[serde(skip, default)]
    sgb_palette: SgbPaletteChoice,
    // Host TV region — only an SGB1 reads it (its clock is the host SNES's / 5).
    // Real-time mapping only, never machine state: the dot timeline is identical
    // in both regions, so it is skipped in the savestate and the frontend
    // re-applies it after a restore exactly like `dmg_palette`.
    #[serde(skip, default)]
    region: Region,
    #[serde(skip, default)]
    skip_bios: bool,
    #[serde(skip, default)]
    breakpoints: HashSet<u16>,
    // A user-forced CGB DMG-compatibility palette id (overriding the boot ROM's
    // title-hash auto-pick when a DMG game runs on CGB hardware). Boot-time only
    // — the palette is latched into CGB registers during skip_bios, so this need
    // not survive a savestate (the state already carries the applied palette).
    #[serde(skip, default)]
    forced_compat_palette: Option<u8>,
    // `+ Send` so a cloned GB (whose audio_output is always None) can be moved
    // to a worker thread for off-thread savestate serialization with NO unsafe:
    // GB is `Send` iff every field is, and this was the only field that wasn't.
    // Every AudioOutput sink (platform Output, session CaptureSink) is Send.
    #[serde(skip)]
    audio_output: Option<Box<dyn audio::AudioOutput + Send>>,
}

impl Clone for GB {
    fn clone(&self) -> Self {
        GB {
            cpu: self.cpu.clone(),
            mmio: self.mmio.clone(),
            ppu: self.ppu.clone(),
            hardware: self.hardware,
            dmg_palette: self.dmg_palette,
            sgb_palette: self.sgb_palette,
            region: self.region,
            skip_bios: self.skip_bios,
            breakpoints: self.breakpoints.clone(),
            forced_compat_palette: self.forced_compat_palette,
            audio_output: None, // Don't clone audio output - it will be recreated if needed
        }
    }
}

/// The presented frame: always RGB888, 160×144, row-major (`[r,g,b, r,g,b, …]`,
/// `FRAMEBUFFER_SIZE * 3` bytes). The core has already applied everything visual
/// — the DMG base palette + LCD correction for a monochrome model, or the
/// CGB/AGB/SGB colour (LCD-corrected) for a colour one — so every frontend just
/// blits these bytes. The correction-independent shade indices, which the test
/// suite compares against (palette/correction never enter correctness), stay
/// available via [`GB::dmg_shade_frame`].
pub struct Frame(pub Box<[u8; ppu::FRAMEBUFFER_SIZE * 3]>);

impl Frame {
    /// The RGB888 pixel bytes.
    pub fn rgb(&self) -> &[u8; ppu::FRAMEBUFFER_SIZE * 3] {
        &self.0
    }
}

/// The DMG/MGB monochrome base palette (the colour "character"), orthogonal to
/// the [`ColorCorrection`](ppu::ColorCorrection) toggle it composes with: the
/// palette picks the four base colours, correction then applies the model's
/// screen response (`Linear` = raw, `Lcd` = as the panel renders it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DmgPaletteChoice {
    Grayscale,
    #[default]
    Green,
    Pocket,
}

impl DmgPaletteChoice {
    /// All choices, in Settings-menu display order.
    pub const ALL: [DmgPaletteChoice; 3] =
        [Self::Grayscale, Self::Green, Self::Pocket];

    /// The model's default base palette: Green for the DMG-01, the Game Boy
    /// Pocket's grey for the MGB, neutral grey elsewhere (SGB has no LCD; CGB/AGB
    /// only hit this as a mono fallback).
    pub(crate) fn default_for(hardware: Hardware) -> Self {
        match hardware {
            Hardware::DMG | Hardware::DMG0 => Self::Green,
            Hardware::MGB => Self::Pocket,
            _ => Self::Grayscale,
        }
    }

    /// The four shade colours (index 0 = lightest .. 3 = darkest), composing the
    /// base palette with `correction`: `Linear` is the raw palette, `Lcd` its
    /// screen-rendered variant. Neutral grey has no LCD tint (same either way).
    pub fn shades(self, correction: ppu::ColorCorrection) -> [[u8; 3]; 4] {
        use ppu::ColorCorrection::{Lcd, Linear};
        match (self, correction) {
            (Self::Grayscale, _) => [[255, 255, 255], [170, 170, 170], [85, 85, 85], [0, 0, 0]],
            // Classic DMG green, raw.
            (Self::Green, Linear) => {
                [[0x9B, 0xBC, 0x0F], [0x8B, 0xAC, 0x0F], [0x30, 0x62, 0x30], [0x0F, 0x38, 0x0F]]
            }
            // DMG green as the LCD panel renders it (lighter, gamma-tinted).
            (Self::Green, Lcd) => {
                [[0xE0, 0xF8, 0xD0], [0x88, 0xC0, 0x70], [0x34, 0x68, 0x56], [0x08, 0x18, 0x20]]
            }
            // Game Boy Pocket grey, raw.
            (Self::Pocket, Linear) => {
                [[0xC4, 0xCF, 0xA1], [0x8B, 0x95, 0x6D], [0x4D, 0x53, 0x3C], [0x1F, 0x1F, 0x1F]]
            }
            // Pocket as the LCD renders it (SameBoy GB_PALETTE_MGB olive).
            (Self::Pocket, Lcd) => {
                [[0xC2, 0xCE, 0x93], [0x81, 0x8D, 0x66], [0x3A, 0x4C, 0x3A], [0x07, 0x10, 0x0E]]
            }
        }
    }

    /// [`shades`](Self::shades) with an opaque alpha byte, for RGBA consumers.
    pub fn shades_rgba(self, correction: ppu::ColorCorrection) -> [[u8; 4]; 4] {
        self.shades(correction).map(|[r, g, b]| [r, g, b, 0xFF])
    }

    /// A short human label for the Settings menu.
    pub fn label(self) -> &'static str {
        match self {
            Self::Grayscale => "Grayscale",
            Self::Green => "Green",
            Self::Pocket => "Game Boy Pocket",
        }
    }

    /// Stable lowercase id (libretro option keys / CLI `--palette`).
    pub fn option_id(self) -> &'static str {
        match self {
            Self::Grayscale => "grayscale",
            Self::Green => "green",
            Self::Pocket => "pocket",
        }
    }

    /// Parse a palette id/name (accepts historical aliases), or `None`.
    pub fn from_option_id(id: &str) -> Option<Self> {
        match id.to_lowercase().as_str() {
            "grayscale" | "gray" | "grey" => Some(Self::Grayscale),
            "green" | "greenlinear" | "greenlcd" | "original" | "gameboy" | "lcd" | "dmg" => {
                Some(Self::Green)
            }
            "pocket" | "mgb" => Some(Self::Pocket),
            _ => None,
        }
    }
}

/// How a DMG game running on SGB hardware is colourized (presentation only,
/// like [`DmgPaletteChoice`]). A real SGB never shows a mono game in grayscale:
/// the SNES-side firmware applies one of its 32 built-in system palettes, `1-A`
/// by default, or that game's signature palette if it recognizes the title.
///
/// [`Auto`](Self::Auto) reproduces the hardware default and is therefore the
/// default here; [`Grayscale`](Self::Grayscale) opts back out to the plain
/// shade ramp.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SgbPaletteChoice {
    /// The firmware's own pick: the recognized per-game palette, else `1-A`.
    #[default]
    Auto,
    /// A user-forced system palette, `0..=31` (`1-A`..`4-H`).
    System(u8),
    /// No colourization — the raw DMG shade ramp.
    Grayscale,
}

impl SgbPaletteChoice {
    /// All choices, in Settings-menu display order: Auto, the 32 system
    /// palettes `1-A`..`4-H`, then Grayscale.
    pub const ALL: [SgbPaletteChoice; 34] = {
        let mut all = [SgbPaletteChoice::Auto; 34];
        let mut i = 0;
        while i < 32 {
            all[i + 1] = SgbPaletteChoice::System(i as u8);
            i += 1;
        }
        all[33] = SgbPaletteChoice::Grayscale;
        all
    };

    /// The four presentation colours, or `None` for `Grayscale` (which defers
    /// to the caller's mono ramp). `title`/licensees come from the cart header
    /// and are only consulted by `Auto`.
    pub fn shades(
        self,
        title: &[u8; 16],
        old_licensee: u8,
        new_licensee: [u8; 2],
    ) -> Option<[[u8; 3]; 4]> {
        // The SGB has no LCD, so these are always the raw linear colours —
        // `ColorCorrection` never applies (pinned by a test).
        self.shades_rgb555(title, old_licensee, new_licensee)
            .map(|p| {
                p.map(|w| {
                    let (r, g, b) = ppu::controller::rgb555_to_rgb888(w);
                    [r, g, b]
                })
            })
    }

    /// The same four colours as [`shades`](Self::shades), still as the RGB555
    /// words the firmware stores. The SGB border compositor works in RGB555,
    /// so it takes this form directly.
    pub(crate) fn shades_rgb555(
        self,
        title: &[u8; 16],
        old_licensee: u8,
        new_licensee: [u8; 2],
    ) -> Option<[u16; 4]> {
        let index = match self {
            Self::Grayscale => return None,
            Self::Auto => {
                sgb_system_palette::select_auto(title, old_licensee, new_licensee)
            }
            Self::System(i) => i.min(31),
        };
        Some(sgb_system_palette::SGB_SYSTEM_PALETTES[index as usize])
    }

    /// A short human label for the Settings menu.
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::System(i) => sgb_system_palette::label(i),
            Self::Grayscale => "Grayscale",
        }
    }

    /// Stable lowercase id (libretro option keys / CLI `--sgb-palette`).
    pub fn option_id(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::System(i) => sgb_system_palette::option_id(i),
            Self::Grayscale => "grayscale",
        }
    }

    /// Parse a palette id (`"auto"`, `"1a"`..`"4h"`, `"grayscale"`), or `None`.
    pub fn from_option_id(id: &str) -> Option<Self> {
        let id = id.to_lowercase();
        match id.as_str() {
            "auto" => Some(Self::Auto),
            "grayscale" | "gray" | "grey" | "none" => Some(Self::Grayscale),
            _ => (0u8..32)
                .find(|&i| sgb_system_palette::option_id(i) == id)
                .map(Self::System),
        }
    }
}

#[allow(dead_code)] // no in-tree caller; `pub` was masking dead_code. Unwired-peripheral and
    // unfinished-feature code lives here — check the feature roadmap before deleting.
/// The four DMG-shade colours for a model's default palette under `correction`.
/// The single source of truth for mono → RGB in the media sweep (which has no
/// user palette override); frontends use a user-selected [`DmgPaletteChoice`].
pub(crate) fn mono_shades(hardware: Hardware, correction: ppu::ColorCorrection) -> [[u8; 3]; 4] {
    DmgPaletteChoice::default_for(hardware).shades(correction)
}

/// The ® tile the boot ROM leaves at VRAM $8190-$819F (tile index 0x19),
/// interleaved even/0x00 bitplane layout. This is the only boot-logo tile that
/// is boot-ROM-internal rather than a decompression of the cart's own header
/// logo (`seed_boot_logo_vram` derives $8010-$818F from the cart header at
/// runtime, so no Nintendo logo bitmap is embedded here). A generic
/// registered-trademark glyph, not the Nintendo wordmark.
const REGISTERED_MARK_TILE: [u8; 0x10] = [
    0x3c, 0x00, 0x42, 0x00, 0xb9, 0x00, 0xa5, 0x00, 0xb9, 0x00, 0xa5, 0x00, 0x42, 0x00, 0x3c, 0x00,
];

impl GB {
    /// Apply every model-derived hardware flag to a power-on [`memory::mmio::Mmio`].
    ///
    /// The single source of truth for machine identity: `GB::new` and
    /// `GB::reset` both seed through here, so the two cannot drift. `Mmio::reset`
    /// rebuilds itself out of `Mmio::new`, so without this an in-place reset
    /// silently degraded a CGB/AGB/SGB machine to the power-on defaults —
    /// invisibly, because the cart-derived `cgb_features_enabled` DOES survive a
    /// reset and keeps the display in colour. libretro's `retro_reset` is the
    /// only production caller of the in-place path.
    ///
    /// The revision-gate predicates live in [`memory::mmio::Mmio::reseed_hardware_flags`],
    /// which the savestate reload path runs too; what is added on top of it here
    /// is the seeding a savestate carries by itself but a fresh or just-reset
    /// `Mmio` does not.
    fn seed_hardware_flags(mmio: &mut memory::mmio::Mmio, hardware: Hardware, region: Region) {
        mmio.reseed_hardware_flags(hardware);
        mmio.set_mgb(matches!(hardware, Hardware::MGB));
        mmio.set_cgb_de(hardware.is_cgb_d_or_later());
        // CGB vs DMG APU gating (NRx1-writable-while-off exception, post-boot
        // APU clock anchor, ch4 deferred-trigger fork). Seeded here — before
        // any audio write can anchor the SPU clock — so a session that runs
        // the REAL boot ROM (never calls skip_bios) still gets CGB semantics.
        mmio.set_audio_boot_cgb(hardware.is_cgb_like());
        if matches!(hardware, Hardware::SGB | Hardware::SGB2) {
            mmio.enable_sgb();
        }
        mmio.set_cpu_hz(hardware.cpu_hz(region));
    }

    pub fn new(hardware: Hardware) -> Self {
        let mut mmio = memory::mmio::Mmio::new();
        Self::seed_hardware_flags(&mut mmio, hardware, Region::default());
        GB {
            cpu: cpu::SM83::new(),
            mmio,
            ppu: ppu::Ppu::new(),
            skip_bios: false,
            hardware,
            region: Region::default(),
            dmg_palette: DmgPaletteChoice::default_for(hardware),
            sgb_palette: SgbPaletteChoice::default(),
            breakpoints: HashSet::new(),
            forced_compat_palette: None,
            audio_output: None, // Audio will be enabled when needed
        }
    }

    /// Force a specific CGB DMG-compatibility palette id (see
    /// [`cgb_compat_palette::COMBO_SCHEMES`](crate::cgb_compat_palette::COMBO_SCHEMES)),
    /// overriding the boot ROM's title-hash auto-pick for a DMG game running on
    /// CGB hardware. `None` restores the automatic selection. Takes effect at the
    /// next `skip_bios`; no effect on DMG hardware or CGB titles.
    pub fn set_forced_compat_palette(&mut self, id: Option<u8>) {
        self.forced_compat_palette = id;
    }

    pub fn skip_bios(&mut self) {
        self.skip_bios = true;
        self.cpu.registers.pc = 0x0100;
        self.cpu.registers.sp = 0xFFFE;

        // Unlicensed boards with boot-time lock sequencing (Sachen, Rocket)
        // model the cart as a real boot ROM would find it; with the boot
        // skipped they must start in the unlocked state.
        if let Some(cart) = self.mmio.get_cartridge_mut() {
            cart.skip_boot_handoff();
        }

        self.mmio.write(crate::ppu::LCD_CONTROL, 0x91);
        self.ppu.sync_lcdc_from_mmio(&self.mmio);
        self.mmio.write(crate::ppu::SCX, 0x00);
        self.mmio.write(crate::ppu::WX, 0x00);
        self.mmio.write(crate::ppu::SCY, 0x00);
        self.mmio.write(crate::ppu::WY, 0x00);
        // Post-boot JOYP (FF00). The DMG/MGB boot ROM hands off with both select
        // lines low (reads 0xCF). The SGB boot ROM leaves both lines high (0xFF)
        // after its packet handshake. On CGB the hand-off is cart-type dependent
        // (like the boot DIV counter): a CGB-flagged cart takes the full-CGB path
        // and leaves the lines low (0xCF, the fexx_ffxx_dumper_cgb oracle), while a
        // DMG-flagged cart runs the DMG-compat path and leaves them high (0xFF,
        // mooneye boot_hwio-C). Write a select-line pattern (bits 4-5); the read
        // path forces bits 6-7 to 1, so 0x30 -> 0xFF and 0x00 -> 0xCF.
        let joyp_lines_high = match self.hardware {
            Hardware::DMG0 | Hardware::DMG | Hardware::MGB => false,
            Hardware::SGB | Hardware::SGB2 => true,
            Hardware::CGB0 | Hardware::CGB | Hardware::CGBB | Hardware::CGBE | Hardware::AGB => {
                !self.should_enable_cgb_features()
            }
        };
        let joyp_init = if joyp_lines_high { 0x3F } else { 0xCF };
        self.mmio.write(crate::input::JOYP, joyp_init);
        self.mmio.write(crate::ppu::LYC, 0x00);
        self.mmio.write(crate::ppu::BGP, 0xFC);
        // OBP0/OBP1 post-boot value (the post-boot ffxxDump oracle reads
        // 0x48/0x49): DMG leaves them uninitialised reading 0xFF; the CGB boot
        // ROM zeroes the obj-palette I/O so FF48/FF49 read 0x00 (the
        // fexx_ffxx_dumper_cgb oracle reads 0x00 at FF48/FF49).
        let obp_init = match self.hardware {
            Hardware::CGB | Hardware::CGBB | Hardware::CGBE | Hardware::AGB => 0x00,
            _ => 0xFF,
        };
        self.mmio.write(crate::ppu::OBP0, obp_init);
        self.mmio.write(crate::ppu::OBP1, obp_init);
        self.mmio.write(registers::INTERRUPT_FLAG, 0xE1);
        self.mmio.write(registers::INTERRUPT_ENABLE, 0x00);
        self.mmio.write(crate::audio::NR10, 0x80);
        self.mmio.write(crate::audio::NR11, 0xBF);
        self.mmio.write(crate::audio::NR12, 0xF3);
        self.mmio.write(crate::audio::NR14, 0xBF);
        self.mmio.write(crate::audio::NR21, 0x3F);
        self.mmio.write(crate::audio::NR22, 0x00);
        self.mmio.write(crate::audio::NR24, 0xBF);
        // NR31/NR41 are deliberately absent from this table: they read 0xFF
        // regardless of the stored value, and writing them while the APU is
        // off is a DMG length-counter load (counter 1) — a leak the real boot
        // ROMs never produce, since they write no NRx1 but NR11.
        self.mmio.write(crate::audio::NR30, 0x7F);
        self.mmio.write(crate::audio::NR32, 0x9F);
        self.mmio.write(crate::audio::NR33, 0xFF);
        self.mmio.write(crate::audio::NR34, 0xBF);
        self.mmio.write(crate::audio::NR42, 0x00);
        self.mmio.write(crate::audio::NR43, 0x00);
        self.mmio.write(crate::audio::NR44, 0xBF);
        self.mmio.write(crate::audio::NR50, 0x77);
        self.mmio.write(crate::audio::NR51, 0xF3);
        self.mmio.write(crate::audio::NR52, match self.hardware {
            Hardware::DMG0 | Hardware::DMG | Hardware::MGB | Hardware::CGB0 | Hardware::CGBB | Hardware::CGB | Hardware::CGBE | Hardware::AGB => 0xF1,
            Hardware::SGB | Hardware::SGB2 => 0xF0,
        });
        self.mmio.write(crate::timer::TIMA, 0x00);
        self.mmio.write(crate::timer::TMA, 0x00);
        self.mmio.write(crate::timer::TAC, 0xF8);
        self.mmio.write(crate::timer::DIV, match self.hardware {
            Hardware::DMG | Hardware::MGB | Hardware::SGB | Hardware::SGB2 | Hardware::CGB0 | Hardware::CGBB | Hardware::CGB | Hardware::CGBE | Hardware::AGB => 0xAB,
            Hardware::DMG0 => 0x18,
        });

        self.cpu.registers.a = match self.hardware {
            Hardware::DMG0 | Hardware::DMG | Hardware::SGB => 0x01,
            Hardware::MGB | Hardware::SGB2 => 0xFF,
            // Post-boot A register: cgb*0x10 | 0x01 (0x11 for CGB & AGB).
            Hardware::CGB0 | Hardware::CGBB | Hardware::CGB | Hardware::CGBE | Hardware::AGB => 0x11,
        };
        self.cpu.registers.b = match self.hardware {
            // Post-boot B register: cgb & agb. AGB sets B bit0 (the
            // GBA-detection flag games read at boot); CGB/others leave B=0.
            Hardware::AGB => 0x01,
            Hardware::CGB0 | Hardware::CGBB | Hardware::CGB | Hardware::CGBE | Hardware::DMG | Hardware::MGB | Hardware::SGB | Hardware::SGB2 => 0x00,
            Hardware::DMG0 => 0xFF,
        };
        self.cpu.registers.c = match self.hardware {
            Hardware::CGB0 | Hardware::CGBB | Hardware::CGB | Hardware::CGBE | Hardware::AGB => 0x00,
            Hardware::DMG0 | Hardware::DMG | Hardware::MGB => 0x13,
            Hardware::SGB | Hardware::SGB2 => 0x14,
        };
        self.cpu.registers.d = match self.hardware {
            Hardware::CGB0 | Hardware::CGBB | Hardware::CGB | Hardware::CGBE | Hardware::AGB => 0xFF,
            Hardware::SGB | Hardware::SGB2 | Hardware::DMG0 | Hardware::DMG | Hardware::MGB => 0x00,
        };
        self.cpu.registers.e = match self.hardware {
            Hardware::DMG | Hardware::MGB => 0xD8,
            Hardware::DMG0 => 0xC1,
            Hardware::SGB | Hardware::SGB2 => 0x00,
            Hardware::CGB0 | Hardware::CGBB | Hardware::CGB | Hardware::CGBE | Hardware::AGB => 0x56,
        };
        self.cpu.registers.h = match self.hardware {
            Hardware::CGB0 | Hardware::CGBB | Hardware::CGB | Hardware::CGBE | Hardware::AGB => 0x00,
            Hardware::DMG0 => 0x84,
            Hardware::DMG | Hardware::MGB => 0x01,
            Hardware::SGB | Hardware::SGB2 => 0xC0,
        };
        self.cpu.registers.l = match self.hardware {
            Hardware::CGB0 | Hardware::CGBB | Hardware::CGB | Hardware::CGBE | Hardware::AGB => 0x0D,
            Hardware::DMG0 => 0x03,
            Hardware::DMG | Hardware::MGB => 0x4D,
            Hardware::SGB | Hardware::SGB2 => 0x60,
        };
        // CGB boot ROM in DMG-compatibility mode (a non-CGB cart on CGB/AGB
        // hardware) leaves a different DE/HL than full-CGB mode. The CGB-CPU-04
        // boot ROM produces D=00 E=08 H=00 L=7C for a DMG cart (captured by
        // running cgb_boot.bin with a CGB-flag=0x00 cart); full-CGB carts get the
        // D=FF E=56 L=0D set above. mooneye boot_regs-cgb (CGB flag 0x00) checks
        // exactly this DMG-compat register set. A/F/B/C are unchanged (B keeps
        // the AGB GBA-detection bit). Only applies on CGB-like hardware running a
        // DMG cart; full-CGB carts and pure DMG hardware are untouched.
        if self.hardware.is_cgb_like() && !self.should_enable_cgb_features() {
            self.cpu.registers.d = 0x00;
            self.cpu.registers.e = 0x08;
            self.cpu.registers.h = 0x00;
            self.cpu.registers.l = 0x7C;
        }
        // Post-boot Zero flag. Per Pan Docs Power_Up_Sequence CPU-register table
        // (confirmed by mooneye boot_regs-A / boot_regs-cgb): CGB leaves Z=1, but
        // AGB leaves Z=0. The CGB-AGB boot ROM's last flag-touching op is an
        // `inc b` on the GBA-detection value: on CGB B stays $00, on AGB it is
        // incremented to $01, so the `inc` clears Z on AGB (and only sets Z if the
        // pre-inc B were $FF, which it is not for these test carts).
        self.cpu.registers.set_flag(registers::Flag::Zero, match self.hardware {
            Hardware::DMG | Hardware::CGB0 | Hardware::CGBB | Hardware::CGB | Hardware::CGBE | Hardware::MGB => true,
            Hardware::AGB | Hardware::DMG0 | Hardware::SGB | Hardware::SGB2 => false,
        });
        self.cpu.registers.set_flag(registers::Flag::Negative, false);
        // DMG/MGB post-boot H/C reflect the boot ROM's final header-checksum
        // `ADD A,(0x14D)`: a valid ROM has `A == 256 - rom[0x14D]` there, so the add
        // carries iff rom[0x14D] != 0 (C), and half-carries iff its low nibble != 0
        // (H). The previous `== 0x00` was inverted (gave F=0x80 where real hardware
        // gives 0xB0). DMG0/SGB/CGB leave H/C clear.
        self.cpu.registers.set_flag(registers::Flag::HalfCarry, match self.hardware {
            Hardware::DMG0 | Hardware::SGB | Hardware::SGB2 | Hardware::CGB0 | Hardware::CGBB | Hardware::CGB | Hardware::CGBE | Hardware::AGB => false,
            Hardware::DMG | Hardware::MGB => (self.mmio.read(0x014D) & 0x0F) != 0,
        });
        self.cpu.registers.set_flag(registers::Flag::Carry, match self.hardware {
            Hardware::DMG0 | Hardware::SGB | Hardware::SGB2 | Hardware::CGB0 | Hardware::CGBB | Hardware::CGB | Hardware::CGBE | Hardware::AGB => false,
            Hardware::DMG | Hardware::MGB => self.mmio.read(0x014D) != 0x00,
        });
        if self.hardware.is_cgb_like() {
            self.mmio.write(crate::memory::mmio::REG_VBK, 0x7E);
            self.mmio.write(crate::memory::mmio::REG_SVBK, 0xF8);
            // RP/IR (0xFF56) power-on: bits 1-5 hold 0x3E so the masked read
            // returns 0x3E (post-boot ffxxDump oracle). Bits 0,6,7 start clear.
            self.mmio.set_io_register(0xFF56, 0x3E);
        }

        // Work-RAM power-on contents (post-boot WRAM oracle). Fill via the
        // normal bus, walking SVBK so each CGB bank receives its slice; fixed
        // bank 0 lives at 0xC000, the banked region at 0xD000.
        {
            let cgb = self.hardware.is_cgb_like();
            let banks = if cgb { 8 } else { 2 };
            let mut wram = vec![0u8; banks * 0x1000];
            crate::memory::init_wram_powerup(cgb, &mut wram);
            if cgb {
                // The no-boot state models the capture session behind the
                // `.dump` region oracles, whose power-on WRAM had the
                // C200-C20F line in the 00 phase (canonical `setInitialCgbWram`
                // has it in the FF phase) plus one flipped bit: C208=01.
                // Observed via the oamdmasrcC000_gdmasrcC0F0 dumps, whose GDMA
                // overruns the ROM's C000-C1FF fill into C200-C22F.
                // `skip_bios_with_boot_residue` restores the canonical bytes
                // for the `.bin` dumper oracles (wram_dumper reads these
                // cells). Same session-family split as the FEAX / VRAM-logo
                // seeds.
                wram[0x0200..0x0208].fill(0x00);
                wram[0x0208] = 0x01;
            }
            for (i, b) in wram[0x0000..0x1000].iter().enumerate() {
                self.mmio.write(0xC000 + i as u16, *b);
            }
            for bank in 1..banks {
                if cgb {
                    self.mmio
                        .write(crate::memory::mmio::REG_SVBK, bank as u8);
                }
                let base = bank * 0x1000;
                for (i, b) in wram[base..base + 0x1000].iter().enumerate() {
                    self.mmio.write(0xD000 + i as u16, *b);
                }
            }
            if cgb {
                self.mmio.write(crate::memory::mmio::REG_SVBK, 0xF8);
            }
        }

        self.mmio.write(crate::memory::mmio::REG_BOOT_OFF, 1);

        // Post-boot DIV phase. `write(DIV)` above resets the counter, so set the
        // hardware boot value of the 16-bit internal counter directly (its low
        // 16 bits drive DIV and the TIMA/serial/APU pre-tick phase).
        // Values are sourced empirically from the mooneye boot_div assert
        // chains (each boot_div-<rev> ROM reads DIV six times at fixed NOP
        // offsets; the value is the unique post-boot 16-bit counter that
        // reproduces that revision's fingerprint on rustyboi's timer) and from
        // the hwtest CGB anchors (start_inc_1/_2 read DIV directly).
        //
        // CGB/AGB counters are CART-TYPE dependent (resolved 2026-07-02; this
        // was previously documented as a two-oracle conflict). The CGB boot ROM
        // has two hand-off paths: for CGB-flagged carts it hands off with the
        // DIV counter at 0x1EA0; for DMG-flagged carts it additionally runs the
        // DMG-compat setup (logo-checksum palette lookup + KEY0 latch), handing
        // off 0x7D8 cc later at 0x2678. Both anchors are real hardware:
        //   - CGB cart  -> 0x1EA0: the hwtest CGB refs (start_inc_1/_2
        //     out1E/out1F, tc00_start_2, fexx_ffxx_dumper, 11 ch1/ch2 boot-phase
        //     sound tests) and BullyGB's initial-DIV check — all CGB-flagged
        //     carts. (== the post-boot cycleCounter 0x102A0 -
        //     divLastUpdate -0x1C00, low 16 bits.)
        //   - DMG cart  -> 0x2678: mooneye misc/boot_div-cgbABCDE — a
        //     DMG-flagged cart, so Gekkio's fingerprint measured the compat
        //     path.
        // Mechanical confirmation: executing the real CGB boot ROM
        // (bios/cgb_boot.bin) in-emulator hands off at DIV_CTR 0x1E9D for a
        // CGB cart vs 0x2675 for a DMG cart (--validate-bios), reproducing
        // both anchors with the same ~3 cc residual.
        let cgb_cart = self.should_enable_cgb_features();
        let boot_counter: u16 = match self.hardware {
            Hardware::CGB | Hardware::CGBB | Hardware::CGBE => {
                if cgb_cart { 0x1EA0 } else { 0x2678 }
            }
            // boot_div-cgb0 fingerprint (29 2a 2a 2b 2c 2e), a DMG cart, so
            // this pins the CGB0 compat path only. CGB0's boot ROM differs from
            // CGB-A..E's, so its CGB-cart value cannot be inferred from the
            // 0x7D8 delta; CGB0 is only used for the mooneye boot rows (all DMG
            // carts). Verified: passes mooneye boot_div-cgb0.
            Hardware::CGB0 => 0x2884,
            // boot_div-A fingerprint (27 28 28 29 2a 2c), a DMG cart: AGB
            // compat path == CGB + 4 master-cc. The AGB boot ROM is the CGB
            // boot ROM with a trivial tail difference (B=1 hand-off), so the +4
            // carries to the CGB-cart path: 0x1EA4 == the post-boot
            // 0x102A0 + agb*4 (the AGB bootstrap oracle's counter).
            // Verified: passes mooneye boot_div-A. AGB is opt-in, outside the
            // default suites.
            Hardware::AGB => {
                if cgb_cart { 0x1EA4 } else { 0x267C }
            }
            Hardware::DMG | Hardware::MGB => 0xABCC,
            // SGB boot_div fingerprint (d9 da da db dc de). The SGB CPU uses the
            // DMG-style single-speed timer. Unlike every other revision, the SGB
            // boot ROM's DURATION is CART-CONTENT dependent: it bit-bangs the
            // cartridge header ($104..$14F, which includes the $14E-$14F global
            // checksum) as a giant packet over the $FF00 port to the SNES, and a
            // set bit transmits one M-cycle (4 T) FASTER than a reset bit. So the
            // hand-off DIV counter is BASE - 4*popcount(header[$104..$14F]).
            //
            // Both mooneye SGB ROMs constrain this identically:
            //   boot_div-S  header popcount 266 -> 0xDC88 - 4*266 = 0xD860
            //   boot_div2-S header popcount 270 -> 0xDC88 - 4*270 = 0xD850
            // boot_div2-S is byte-for-byte boot_div-S except a different global
            // checksum ($A796 vs $1234; +4 set bits) and +4 compensating leading
            // NOPs. The +16 T of extra program is exactly cancelled by the -16 T
            // shorter boot (4 bits * 4 T), landing both reads on the identical
            // divider values -> both pass with fingerprint d9 da da db dc de.
            // BASE 0xDC88 is the single anchor pinned by both ROMs (implied BASE is
            // 0xDC88 for each). See Pan Docs / gg8 Bootstrap-ROM notes on the SGB
            // header-packet bit timing.
            Hardware::SGB | Hardware::SGB2 => {
                let popcount: u32 = (0x0104u16..=0x014F)
                    .map(|a| self.mmio.read(a).count_ones())
                    .sum();
                0xDC88u16.wrapping_sub((4 * popcount) as u16)
            }
            // boot_div-dmg0 fingerprint (19 1a 1a 1b 1c 1e). Verified: passes
            // mooneye boot_div-dmg0.
            Hardware::DMG0 => 0x1830,
        };
        self.mmio.set_timer_internal_counter(boot_counter);

        // Post-boot APU state. The boot ROM enables the APU and leaves channel 1
        // mid-tone. The channel-register writes above were dropped where the
        // APU-enable gate applies (everything but the NRx1 length bits, which
        // stay writable while off on DMG — set_post_bios_state re-seeds those
        // counters to the hardware post-boot values, so no boot-table write
        // leaks through). Apply the exact post-boot APU state directly
        // (must follow the timer-counter set so the duty phase has the right cc).
        // Wave RAM differs between DMG and CGB (post-boot I/O dumps).
        let cgb = self.hardware.is_cgb_like();
        let wave_ram: [u8; 16] = if cgb {
            [0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF,
             0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF]
        } else {
            [0x71, 0x72, 0xD5, 0x91, 0x58, 0xBB, 0x2A, 0xFA,
             0xCF, 0x3C, 0x54, 0x75, 0x48, 0xCF, 0x8F, 0xD9]
        };
        for (i, b) in wave_ram.iter().enumerate() {
            self.mmio.write(crate::audio::WAV_START + i as u16, *b);
        }
        // SGB/SGB2 hand off with channel 1 already stopped (no Game-Boy-side boot
        // chime), so NR52 reads 0xF0; every other model leaves ch1 running (0xF1).
        let ch1_active = !matches!(self.hardware, Hardware::SGB | Hardware::SGB2);
        self.mmio.set_post_bios_audio_state(cgb, ch1_active);

        // Post-boot power-on OAM / unusable-region / HRAM contents (post-boot
        // I/O+OAM+HRAM dumps). The boot ROM leaves these untouched, so they
        // hold the hardware power-on pattern the fexx_* dumpers read back.
        self.mmio.set_post_bios_ioamhram(cgb);

        // Post-boot CGB palette RAM. The boot ROM leaves BG palette RAM
        // all-white and OBJ palette RAM holding the hardware power-on dump
        // (the cgbObjpDump oracle). A program that renders a sprite
        // without writing FF6A/FF6B observes these values; without this the
        // OBJ palette is all-zero (black). Matches scx_during_m3_spx2 etc.
        //
        // DMG cart on CGB hardware (CGB features OFF): the CGB boot ROM instead
        // installs the per-game DMG-compatibility colored palette (title-hash
        // lookup in the Nintendo table; unrecognized titles get the default
        // dark-green scheme), and the PPU renders the DMG game through it. Seed
        // that palette so a DMG cart shows in the boot ROM's colors: Tetris /
        // Zelda / Mario get their signature colorization, dmg-acid2 and the
        // hwtest ROMs the default. Buttons held at power-on override the
        // automatic choice like a combo held during the boot logo.
        if cgb {
            if self.should_enable_cgb_features() {
                self.mmio.set_post_bios_cgb_palettes();
            } else {
                let mut title = [0u8; 16];
                for (i, b) in title.iter_mut().enumerate() {
                    *b = self.mmio.read(0x0134 + i as u16);
                }
                let new_licensee = [self.mmio.read(0x0144), self.mmio.read(0x0145)];
                let old_licensee = self.mmio.read(0x014B);
                let mut id = crate::cgb_compat_palette::select_palette_id(
                    &title,
                    old_licensee,
                    new_licensee,
                );
                if let Some(combo) = crate::cgb_compat_palette::key_combo_palette_id(
                    self.mmio.dmg_compat_key_combo(),
                ) {
                    id = combo;
                }
                // A user-forced scheme overrides both the title-hash pick and any
                // held-button combo. `None` (the default) leaves them untouched,
                // so the automatic path stays byte-identical.
                if let Some(forced) = self.forced_compat_palette {
                    id = forced;
                }
                let pal = crate::cgb_compat_palette::palettes_for_id(id);
                self.mmio.set_cgb_compat_dmg_palettes(&pal);
            }
        }

        // Post-boot VRAM contents. The boot ROM decompresses the Nintendo logo
        // from the cart header into the BG tile area (0x8010-0x819F) and writes
        // the logo tilemap (tile indices) at 0x9904-0x9910 / 0x9924-0x992F.
        // These bytes are the exact post-boot bank-0 VRAM the vram_dumper oracle
        // captures; real games can read them, so this is legitimate skip_bios
        // fidelity. VBK was set to bank 0 above, so plain bus writes land in
        // bank 0. Restricted to DMG/MGB: the CGB oamdma vram dumpers GDMA over
        // 0x8000 and assert the remaining VRAM is zero, so a CGB logo regresses
        // them (the CGB references for those tests assume cleared VRAM);
        // the CGB vram_dumper logo cannot be matched without that regression.
        if !cgb {
            self.seed_boot_logo_vram();
            // DMG/MGB logo tilemap: row 0 tiles 1..=12 + ® (25) at 0x9904, row 1
            // tiles 13..=24 at 0x9924.
            for (i, t) in (1u8..=12).enumerate() {
                self.mmio.write(0x9904 + i as u16, t);
            }
            self.mmio.write(0x9910, 25);
            for (i, t) in (13u8..=24).enumerate() {
                self.mmio.write(0x9924 + i as u16, t);
            }
        } else if !self.should_enable_cgb_features() {
            // DMG cart on CGB (compat mode): the real CGB boot ROM also leaves
            // the logo tile data — including the ® tile at 0x8190 — in VRAM
            // bank 0 (the post-boot VRAM oracle seeds 0x8010-0x819F for cgb too;
            // only the tilemap is DMG-only). mealybug's compat sprites render
            // tile 0x19 (®) straight from this boot residue (m3_obp0_change
            // cgb_c); without it the sprite is all-transparent. The CGB-cart
            // dumper compromise above is unaffected: those are CGB-feature
            // carts, never compat mode.
            self.seed_boot_logo_vram();
        }

        // Post-boot PPU frame phase. The boot ROM leaves the LCD enabled and the
        // PPU deep into a frame (the post-boot `videoCycles` oracle): the game
        // starts in VBlank at LY=144 (CGB) / LY=153 (DMG), not a fresh LY=0 OAM
        // search. Seed that here so the first instruction's LY/STAT reads match
        // hardware (display_startstate). Must follow the LCDC=0x91 write above.
        self.ppu.set_post_bios_state(&mut self.mmio, self.hardware == Hardware::DMG0);
    }

    /// Write the boot-ROM Nintendo logo into VRAM bank 0 via the normal bus.
    /// VBK is bank 0 at this point in `skip_bios`, so plain writes land there.
    ///
    /// Header-logo-substituting carts (Sachen MMC1) get the boot expansion of
    /// THEIR logo instead: the real boot ROM reads the header through the
    /// locked mapper and decompresses whatever it sees, and those games check
    /// the resulting VRAM tiles as copy protection (the same expansion is
    /// poked in when no bootstrap is emulated).
    fn seed_boot_logo_vram(&mut self) {
        // $8010-$818F is exactly the boot ROM's decompression of the cart's own
        // header logo ($0104-$0133), so derive it from the header the user
        // supplied rather than embedding the Nintendo bitmap. Sachen MMC1 carts
        // substitute their locked-mapper logo (see `boot_logo_bytes`).
        if let Some(logo) = self.mmio.get_cartridge().map(|c| c.boot_logo_bytes()) {
            // The DMG boot ROM's logo decompression: each header byte yields two
            // pixel-doubled bitplane-0 bytes, each written twice (row doubling)
            // at even offsets from 0x8010.
            for (i, &b) in logo.iter().enumerate() {
                let b = b as u16;
                let hi = ((b) & 0x80)
                    | ((b >> 1) & 0x60)
                    | ((b >> 2) & 0x18)
                    | ((b >> 3) & 0x06)
                    | ((b >> 4) & 0x01);
                let lo = ((b << 4) & 0x80)
                    | ((b << 3) & 0x60)
                    | ((b << 2) & 0x18)
                    | ((b << 1) & 0x06)
                    | (b & 0x01);
                let base = 0x8010 + (i as u16) * 8;
                self.mmio.write(base, hi as u8);
                self.mmio.write(base + 2, hi as u8);
                self.mmio.write(base + 4, lo as u8);
                self.mmio.write(base + 6, lo as u8);
            }
        }
        // The ® tile at $8190 is boot-ROM-internal, independent of the header.
        for (i, b) in REGISTERED_MARK_TILE.iter().enumerate() {
            self.mmio.write(0x8190 + i as u16, *b);
        }
    }

    /// Post-boot state as captured WITH the boot ROM having run, for the SRAM
    /// dumper oracles (`vram_dumper`, `fexx_ffxx_dumper`) whose `.bin` references
    /// were produced after the boot ROM executed. On top of the normal
    /// `skip_bios` no-boot state this also seeds the boot-ROM-final residue that
    /// the no-boot path deliberately omits (because the `.dump` region oracles
    /// were captured WITHOUT the boot ROM and need the zeroed/0x18 state):
    ///   - CGB: the Nintendo logo in VRAM bank 0 (the post-boot VRAM oracle)
    ///     and the canonical 0x08-tail feax shadow (the feaxDump oracle).
    ///   - DMG: the logo is already seeded by `skip_bios`; no extra residue
    ///     (the canonical post-boot DMG OAM is already applied).
    ///
    /// Select this per-oracle (SRAM dump) in the runner; the no-boot
    /// `skip_bios` must stay in use for the `.dump` region oracles.
    pub fn skip_bios_with_boot_residue(&mut self) {
        self.skip_bios();
        if self.hardware.is_cgb_like() {
            self.seed_boot_logo_vram();
            self.mmio.set_cgb_boot_residue_feax();
            // Restore the canonical `setInitialCgbWram` bytes over the
            // `.dump`-session deltas skip_bios applies at C200-C208 (the
            // wram_dumper `.bin` oracle reads these cells).
            for (addr, value) in [
                (0xC200u16, 0xFFu8), (0xC201, 0xFB), (0xC202, 0xFF), (0xC203, 0xFF),
                (0xC204, 0xFF), (0xC205, 0xFF), (0xC206, 0xFF), (0xC207, 0xFF),
                (0xC208, 0x00),
            ] {
                self.mmio.write(addr, value);
            }
        }
    }

    /// Insert a cartridge and return how it pairs with the current hardware.
    ///
    /// The core never logs about mismatches — the returned [`Compatibility`]
    /// lets the consumer decide whether to surface anything to the user. Note
    /// that no variant prevents the ROM from running: even [`Compatibility::CgbOnlyOnDmg`]
    /// boots (to the cartridge's own hardware-mismatch screen), which is a
    /// faithful thing for an emulator to show.
    pub fn insert(&mut self, cartridge: cartridge::Cartridge) -> Compatibility {
        let compatibility = cartridge_compatibility(self.hardware, &cartridge);

        // SGB command-unlock gate (Pan Docs "SGB Unlocking"): only carts whose
        // header declares SGB support ($0146 == $03, $014B == $33) may drive
        // SGB packets. No-op on non-SGB hardware.
        let sgb_unlocked = cartridge.supports_sgb();

        self.mmio.insert_cartridge(cartridge);
        self.mmio.set_sgb_unlocked(sgb_unlocked);

        // Update CGB features enablement based on hardware and cartridge compatibility
        let cgb_enabled = self.should_enable_cgb_features();
        self.mmio.set_cgb_features_enabled(cgb_enabled);

        compatibility
    }

    /// Check if CGB features should be enabled
    /// CGB features are enabled when:
    /// 1. Hardware is CGB, AND
    /// 2. Cartridge supports CGB (Compatible or Only)
    pub fn should_enable_cgb_features(&self) -> bool {
        if !self.hardware.is_cgb_like() {
            return false;
        }

        // Check if cartridge supports CGB
        if let Some(cartridge) = self.mmio.get_cartridge() {
            cartridge.supports_cgb()
        } else {
            false
        }
    }

    pub fn load_bios(&mut self, path: &str) -> Result<(), std::io::Error> {
        self.mmio.load_bios(path)?;
        Ok(())
    }

    /// Load a boot ROM from raw bytes (WASM-clean; no filesystem access). The
    /// bytes must be a real DMG (256-byte) or CGB (2304-byte) boot ROM matching
    /// the emulated model — the length-appropriate CRC is verified, so a wrong or
    /// mismatched image returns an error and the machine keeps its prior state.
    pub fn load_bios_bytes(&mut self, bytes: &[u8]) -> Result<(), std::io::Error> {
        self.mmio.load_bios_bytes(bytes)?;
        Ok(())
    }

    /// Run the REAL boot ROM from power-on (PC=0x0000) until it hands off to the
    /// cartridge. Mirrors a hardware-faithful testrunner, which executes the
    /// boot ROM before every test instead of seeding a synthetic post-boot state.
    ///
    /// Preconditions: a cartridge is inserted and a matching boot ROM is loaded
    /// (`load_bios`). The CPU/peripherals are at their hardware power-on values
    /// (the default `SM83::new` / `Mmio::new`, PC=0). Returns the number of
    /// instructions executed. Handoff is detected when the boot ROM unmaps
    /// itself (writes FF50, so the overlay is gone) — exactly when execution
    /// would leave boot-ROM space.
    ///
    /// For CGB hardware the boot ROM needs the CGB register set live (VBK/SVBK/
    /// HDMA/palettes) regardless of cart support, so CGB features are forced on
    /// for the duration of boot; afterwards they are reconciled to the cart's
    /// actual support (the boot ROM has already latched KEY0 DMG-compat).
    pub fn run_boot_rom(&mut self) -> usize {
        if !self.has_bios() {
            return 0;
        }
        // Real power-on register/PC state. SM83::new already zeroes everything
        // and PC=0; be explicit so this works even if a skip path ran before.
        self.cpu.registers = registers::Registers::new();
        self.cpu.registers.pc = 0x0000;
        self.cpu.registers.sp = 0x0000;
        self.skip_bios = false;

        let cgb = self.hardware.is_cgb_like();
        if cgb {
            // Let the CGB boot ROM drive the full CGB register set.
            self.mmio.set_cgb_features_enabled(true);
        }

        // Seed the hardware power-on RAM garbage BEFORE the boot ROM runs
        // (mirrors initializing the I/O+OAM+HRAM region before loading the boot ROM). The boot ROM
        // overwrites what it writes and leaves OAM/HRAM/feax/wave RAM as this
        // power-on pattern — which the fexx_*/dumper oracles read back. Our
        // power-on memory init is all-zero, so without this the dumper regions
        // would read zero (a real-boot-vs-skip_bios discrepancy).
        self.mmio.seed_power_on_ram(cgb);
        // Wave RAM power-on pattern (DMG boot ROM does not touch it; the CGB
        // boot ROM initialises sound itself, so only seed it for DMG).
        if !cgb {
            let wave: [u8; 16] = [
                0x71, 0x72, 0xD5, 0x91, 0x58, 0xBB, 0x2A, 0xFA,
                0xCF, 0x3C, 0x54, 0x75, 0x48, 0xCF, 0x8F, 0xD9,
            ];
            for (i, b) in wave.iter().enumerate() {
                self.mmio.write(crate::audio::WAV_START + i as u16, *b);
            }
            // DMG OBP0/OBP1 power-on read 0xFF (uninitialised). The DMG boot ROM
            // does not write them, so seed the power-on value pre-boot.
            self.mmio.set_io_register(crate::ppu::OBP0, 0xFF);
            self.mmio.set_io_register(crate::ppu::OBP1, 0xFF);
        }

        // Step until the boot ROM unmaps itself (FF50 written). Guard with a
        // generous instruction ceiling so a bad ROM can never wedge the runner.
        let mut steps = 0usize;
        const MAX_BOOT_STEPS: usize = 50_000_000;
        while self.mmio.bios_mapped() && steps < MAX_BOOT_STEPS {
            self.step_instruction(false);
            steps += 1;
        }

        if cgb {
            // Reconcile CGB feature state to the cart now that boot has latched
            // KEY0 DMG-compat. (DMG carts on a CGB run with features off.)
            let cgb_enabled = self.should_enable_cgb_features();
            self.mmio.set_cgb_features_enabled(cgb_enabled);
        }
        steps
    }

    /// Check if a ROM cartridge is loaded
    pub fn has_rom(&self) -> bool {
        self.mmio.get_cartridge().is_some()
    }

    /// Check if a BIOS is loaded
    pub fn has_bios(&self) -> bool {
        self.mmio.has_bios()
    }

    /// Install the SNES-side Super Game Boy firmware (`sgb1.sfc`/`sgb2.sfc`)
    /// from raw bytes (WASM-clean; no filesystem access) and seed the system
    /// border it carries. Unrecognised images are rejected; see
    /// [`Mmio::load_sgb_firmware_bytes`](crate::memory::mmio::Mmio::load_sgb_firmware_bytes).
    /// Inert on non-SGB hardware.
    pub fn load_sgb_firmware_bytes(&mut self, bytes: &[u8]) -> Result<(), std::io::Error> {
        self.mmio.load_sgb_firmware_bytes(bytes)
    }

    /// Whether an SGB firmware dump is installed (so the default border shows).
    pub fn has_sgb_firmware(&self) -> bool {
        self.mmio.has_sgb_firmware()
    }

    /// Engage the per-sample channel tap ([ch1..4], nr50, nr51, enabled) —
    /// recording/measurement companion to `enable_audio`.
    pub fn set_channel_tap(&mut self, on: bool) {
        self.mmio.set_channel_tap(on);
    }

    /// Take tapped channel samples accumulated since the last drain.
    pub fn drain_channel_tap(&mut self) -> Vec<audio::ChannelSample> {
        self.mmio.drain_channel_tap()
    }

    // Audio management methods
    pub fn enable_audio(&mut self, mut output: Box<dyn audio::AudioOutput + Send>) -> Result<(), Box<dyn std::error::Error>> {
        if self.audio_output.is_some() {
            // Audio already enabled
            return Ok(());
        }
        output.start()?;
        self.audio_output = Some(output);
        Ok(())
    }

    pub fn step_instruction(&mut self, collect_audio: bool) -> (bool, u32) {
        // Check for breakpoint at current PC before executing. The is_empty
        // guard keeps the common no-breakpoints case from paying a HashSet
        // hash per instruction.
        let pc = self.cpu.registers.pc;
        if !self.breakpoints.is_empty() && self.breakpoints.contains(&pc) {
            // Breakpoint hit - don't execute instruction and return (empty audio, breakpoint hit)
            return (true, 0);
        }

        // Plain-STOP low-power mode (Pan Docs "Reducing Power Consumption"):
        // the main oscillator is stopped, so the CPU and every clocked
        // peripheral — DIV/timer, PPU, APU, serial, OAM-DMA/HDMA, i.e.
        // master_cc itself — freeze coherently until "one of the P10 to P13
        // lines going low": a pressed button in a JOYP-SELECTED group (a ROM
        // that deselected both groups sleeps until reset, as on hardware; the
        // press itself raises IF.4 through the normal input edge path, so an
        // enabled joypad interrupt dispatches after the wake). Cycles are
        // still reported so the callers' cycle-capped frame loops keep
        // serving the frozen panel to the frontend; no audio is generated
        // (the APU is stopped). The cart-local MBC3 RTC crystal really keeps
        // counting through STOP on hardware but rides master_cc here, so it
        // freezes too (accepted simplification; no licensed ROM STOPs).
        // On wake the world advances 8 T-cycles before the CPU resumes;
        // execution then continues at the post-STOP pc set by the opcode
        // (opcodes::stop): past both bytes (2-byte form) or at the operand
        // byte (1-byte interrupt-pending form).
        let is_double_speed = self.mmio.is_double_speed_mode();

        if self.cpu.stopped {
            let cycles = if self.mmio.read(crate::input::JOYP) & 0x0F != 0x0F {
                self.cpu.stopped = false;
                let mut bus = cpu::Bus::new(&mut self.mmio, &mut self.ppu);
                bus.tick_remaining(8);
                // STOP-wake semantics are asserted against raw master_cc by
                // hardware tests; never leave the wake advance carried.
                bus.flush_all_lag();
                8
            } else {
                4
            };
            // The APU is frozen along with the rest of the machine, but the
            // host's stream is not: emitting nothing across a STOP window
            // starves the sink, and slides recorded audio ahead of the video it
            // was captured against. Emit the nominal count instead, through the
            // same path the running case uses so the channel tap and the sink
            // stay sample-aligned. Since the generators are stopped, every
            // sample re-reads the same held DAC levels and the analog stage
            // decays them to true silence rather than hard-cutting.
            self.emit_audio(collect_audio, cycles, is_double_speed);
            return (false, cycles);
        }

        self.ppu.step_scheduled_stat_events(&mut self.mmio);

        // Execute one CPU instruction. Every peripheral (incl. the PPU) is
        // ticked inline by `Bus` at each memory access's true cycle, so reads
        // observe — and writes mutate — live state; the remaining internal
        // cycles are ticked afterward.
        let cycles = {
            let mut bus = cpu::Bus::new(&mut self.mmio, &mut self.ppu);
            let cycles = self.cpu.step(&mut bus);
            bus.tick_remaining(cycles);
            // STOP freezes master_cc at the exact stop cc; never park the
            // stopping instruction's tail across the frozen window.
            if self.cpu.stopped {
                bus.flush_all_lag();
            }
            cycles
        };

        self.emit_audio(collect_audio, cycles, is_double_speed);

        (false, cycles) // No breakpoint hit
    }

    /// Down-sample `cycles` worth of APU output into the channel tap and the
    /// audio sink. The single emission point for both the running and the STOP
    /// paths — the sweep harness drains tap and sink per frame and they must
    /// not desync.
    fn emit_audio(&mut self, collect_audio: bool, cycles: u32, is_double_speed: bool) {
        if !collect_audio {
            return;
        }
        // In double speed mode, audio runs at normal speed, so we need to adjust the cycle count
        let audio_cycles = if is_double_speed { cycles / 2 } else { cycles };
        let audio_samples = self.mmio.generate_audio_samples(audio_cycles);

        // Send audio samples directly to output as they're generated
        if !audio_samples.is_empty()
            && let Some(audio_output) = &mut self.audio_output {
                audio_output.add_samples(&audio_samples);
        }
    }

    /// Advance nothing; convert the PPU's just-completed raw frame into the
    /// presented always-RGB [`Frame`], applying the DMG base palette + colour
    /// correction to a monochrome frame (colour frames are already corrected).
    fn presented_frame(&mut self) -> Frame {
        match self.ppu.get_frame(&self.mmio) {
            ppu::RenderedFrame::Color(rgb) => Frame(rgb),
            ppu::RenderedFrame::Monochrome(idx) => {
                let shades = self
                    .sgb_presentation_shades()
                    .unwrap_or_else(|| self.dmg_palette.shades(self.ppu.cgb_color_conversion()));
                let mut rgb = vec![0u8; ppu::FRAMEBUFFER_SIZE * 3].into_boxed_slice();
                for (i, &s) in idx.iter().enumerate() {
                    let c = shades[(s as usize) & 3];
                    rgb[i * 3] = c[0];
                    rgb[i * 3 + 1] = c[1];
                    rgb[i * 3 + 2] = c[2];
                }
                Frame(rgb.try_into().expect("FRAMEBUFFER_SIZE * 3"))
            }
        }
    }

    /// The raw DMG shade indices (0..=3) of the current frame — the
    /// correction/palette-independent representation the test suite compares.
    /// Meaningful only for a monochrome frame (see [`GB::frame_renders_color`]).
    pub(crate) fn dmg_shade_frame(&self) -> &[u8; ppu::FRAMEBUFFER_SIZE] {
        self.ppu.dmg_shade_frame()
    }

    /// The *presented* DMG shade indices (0..=3): the mono correctness domain the
    /// test suite grades against — palette/correction-independent like
    /// [`dmg_shade_frame`](GB::dmg_shade_frame) but with the panel blank (LCD off
    /// / first frame after enable) and SGB mask applied, matching what the
    /// presented [`Frame`] shows. Use this, not the raw back buffer, to grade a
    /// monochrome frame.
    pub fn presented_shade_frame(&self) -> Box<[u8; ppu::FRAMEBUFFER_SIZE]> {
        self.ppu.presented_dmg_shades(&self.mmio)
    }

    /// Whether the current frame is colour (CGB/AGB, or a colorized SGB) — i.e.
    /// the presented [`Frame`] carries real colour rather than palette-coloured
    /// DMG shades. Gates hash colour frames directly and mono frames via
    /// [`dmg_shade_frame`](GB::dmg_shade_frame).
    pub fn frame_renders_color(&self) -> bool {
        self.ppu.renders_color(&self.mmio) || self.mmio.sgb().is_some_and(|s| s.colorized)
    }

    /// Set the DMG/MGB mono base palette (presentation only).
    pub fn set_dmg_palette(&mut self, palette: DmgPaletteChoice) {
        self.dmg_palette = palette;
    }

    /// The current DMG/MGB mono base palette.
    pub fn dmg_palette(&self) -> DmgPaletteChoice {
        self.dmg_palette
    }

    /// Set how a mono game is colourized on SGB hardware (presentation only;
    /// no effect on other models).
    pub fn set_sgb_palette(&mut self, palette: SgbPaletteChoice) {
        self.sgb_palette = palette;
    }

    /// The current SGB colourization choice.
    pub fn sgb_palette(&self) -> SgbPaletteChoice {
        self.sgb_palette
    }

    /// The four RGB colours the SGB firmware would show this cart in, or `None`
    /// when SGB colourization does not apply (not SGB hardware, or the user
    /// asked for plain grayscale) and the caller should use its mono ramp.
    fn sgb_presentation_shades(&self) -> Option<[[u8; 3]; 4]> {
        self.sgb_presentation_shades_rgb555().map(|p| {
            p.map(|w| {
                let (r, g, b) = ppu::controller::rgb555_to_rgb888(w);
                [r, g, b]
            })
        })
    }

    /// [`sgb_presentation_shades`](Self::sgb_presentation_shades) in the
    /// RGB555 domain the SGB border compositor works in.
    fn sgb_presentation_shades_rgb555(&self) -> Option<[u16; 4]> {
        if !matches!(self.hardware, Hardware::SGB | Hardware::SGB2) {
            return None;
        }
        let mut title = [0u8; 16];
        for (i, b) in title.iter_mut().enumerate() {
            *b = self.mmio.read(0x0134 + i as u16);
        }
        let new_licensee = [self.mmio.read(0x0144), self.mmio.read(0x0145)];
        let old_licensee = self.mmio.read(0x014B);
        self.sgb_palette
            .shades_rgb555(&title, old_licensee, new_licensee)
    }

    /// Set the host TV region. Only an SGB1 changes behaviour (its clock is the
    /// host SNES's / 5); it retunes the APU's host-sample cadence and nothing
    /// else — the dot timeline is byte-identical in both regions.
    pub fn set_region(&mut self, region: Region) {
        self.region = region;
        self.mmio.set_cpu_hz(self.hardware.cpu_hz(region));
    }

    /// The current host TV region.
    pub fn region(&self) -> Region {
        self.region
    }

    /// This machine's real-time CPU clock in Hz (see [`Hardware::cpu_hz`]).
    pub fn cpu_hz(&self) -> u32 {
        self.hardware.cpu_hz(self.region)
    }

    pub fn run_until_frame(&mut self, collect_audio: bool) -> (Frame, bool) {
        let mut cpu_cycles_this_frame = 0u32;
        // Normal frame should be 70224 PPU dots (154 scanlines × 456 dots)
        // If we exceed this, we assume PPU is disabled or stuck
        // and return to avoid audio buildup
        const MAX_NORMAL_SPEED_CPU_CYCLES_PER_FRAME: u32 = 70224;

        loop {
            let (breakpoint_hit, cycles) = self.step_instruction(collect_audio);
            cpu_cycles_this_frame += cycles;

            if breakpoint_hit {
                // Breakpoint hit - return current frame and indicate breakpoint hit
                return (self.presented_frame(), true);
            }

            // Check if PPU has completed a frame
            if self.ppu.frame_ready() {
                // SGB *_TRN commands read a 4KB block from the displayed frame
                // during the VBlank after the command (no-op on non-SGB hardware).
                self.mmio.service_sgb_vram_transfer(self.ppu.dmg_shade_frame());
                return (self.presented_frame(), false);
            }

            // If PPU is disabled or taking too long, cap the cycles to prevent audio buildup
            let max_cpu_cycles_per_frame = if self.mmio.is_double_speed_mode() {
                MAX_NORMAL_SPEED_CPU_CYCLES_PER_FRAME * 2
            } else {
                MAX_NORMAL_SPEED_CPU_CYCLES_PER_FRAME
            };
            if cpu_cycles_this_frame >= max_cpu_cycles_per_frame {
                // PPU disabled or stuck - return after reasonable cycle count to maintain timing
                return (self.presented_frame(), false);
            }
        }
    }

    pub fn run_until_lcd_frame(
        &mut self,
        collect_audio: bool,
        max_cycles: u32,
    ) -> Result<(Frame, bool), &'static str> {
        let mut cpu_cycles = 0u32;

        loop {
            let (breakpoint_hit, cycles) = self.step_instruction(collect_audio);
            cpu_cycles = cpu_cycles.saturating_add(cycles);

            if breakpoint_hit {
                return Ok((self.presented_frame(), true));
            }

            if self.ppu.frame_ready() {
                // SGB *_TRN commands read a 4KB block from the displayed frame
                // during the VBlank after the command. Service any pending
                // transfer at the frame boundary (no-op on non-SGB hardware).
                self.mmio.service_sgb_vram_transfer(self.ppu.dmg_shade_frame());
                return Ok((self.presented_frame(), false));
            }

            if cpu_cycles >= max_cycles {
                return Err("timed out waiting for LCD frame");
            }
        }
    }

    pub fn get_current_frame(&mut self) -> Frame {
        self.presented_frame()
    }

    /// Immutable view of the Super Game Boy state (None on non-SGB hardware).
    /// Frontends use this for mask/border presentation.
    pub fn sgb(&self) -> Option<&crate::sgb::Sgb> {
        self.mmio.sgb()
    }

    /// Full 256x224 Super Game Boy output (RGB888): the game's transferred
    /// border composited around the (masked, colorized) GB screen at
    /// (48, 40). None on non-SGB hardware or before the game transfers a
    /// border, so callers fall back to the standard frame.
    ///
    /// Off-screen accessor by design: it does NOT consume `frame_ready` and
    /// the 160x144 `Frame` path (`run_until_frame` / `get_current_frame`) is
    /// byte-identical whether or not this is called. Frontends that present
    /// SGB borders call it after `run_until_frame` returns a frame; see
    /// `ppu::SGB_FRAME_WIDTH/HEIGHT`.
    pub fn sgb_composited_frame(&self) -> Option<Box<[u8; ppu::SGB_FRAME_SIZE * 3]>> {
        // Before the game sends a palette command the centre is uncolorized;
        // feed the compositor the system palette the firmware would have
        // picked for this cart so it matches the plain 160x144 presentation
        // (which takes the same choice via `sgb_presentation_shades`).
        let uncolorized = self
            .sgb_presentation_shades_rgb555()
            .unwrap_or(ppu::controller::SGB_BOOT_SHADES);
        self.ppu.sgb_composited_frame(&self.mmio, uncolorized)
    }

    pub fn set_cgb_color_conversion(&mut self, conversion: ppu::ColorCorrection) {
        self.ppu.set_cgb_color_conversion(conversion);
    }

    pub fn set_fetch_debug_events_enabled(&mut self, enabled: bool) {
        self.ppu.set_fetch_debug_events_enabled(enabled);
    }

    pub fn take_fetch_debug_events(&mut self) -> Vec<ppu::FetchDebugEvent> {
        self.ppu.take_fetch_debug_events()
    }

    pub fn take_pixel_debug_events(&mut self) -> Vec<ppu::PixelDebugEvent> {
        self.ppu.take_pixel_debug_events()
    }

    pub fn get_cpu_registers(&self) -> &cpu::registers::Registers {
        &self.cpu.registers
    }

    pub fn get_ime_enable_delay(&self) -> u8 {
        self.cpu.ime_enable_delay
    }

    pub fn get_ppu_debug_info(&self) -> (&ppu::Ppu, [u8; 8]) {
        (&self.ppu, self.ppu.get_fetcher_pixel_buffer())
    }

    pub fn read_memory(&self, address: u16) -> u8 {
        self.mmio.read(address)
    }

    /// Catch lazily-advanced peripherals (the APU) up to the current cc so a
    /// following out-of-band `read_memory` observes live state. CPU-visible
    /// reads sync automatically; host/debug reads bypass the bus and must call
    /// this first when they target APU registers.
    pub fn sync_lazy_peripherals(&mut self) {
        // Resolve any carried cross-instruction lag first so the world state
        // an out-of-band read observes is fully current.
        if self.mmio.cpu_lag() > 0 {
            let mut bus = cpu::Bus::new(&mut self.mmio, &mut self.ppu);
            bus.flush_all_lag();
        }
        self.mmio.sync_apu();
    }

    /// Select the inserted board's SRAM chip-select decode (test-fixture
    /// modeling; see `Cartridge::dma_sram_bus_read`). Call after `insert`.
    pub fn set_cart_sram_cs_lazy(&mut self, lazy: bool) {
        self.mmio.set_cart_sram_cs_lazy(lazy);
    }

    /// Master cycle counter (abs_cc) for timing trace reconciliation vs cctracer.
    pub fn master_cc(&self) -> u64 {
        self.mmio.master_cc()
    }

    /// Write a byte through the memory bus. Used by the libretro frontend to
    /// apply per-frame GameShark RAM pokes.
    pub fn write_memory(&mut self, address: u16, value: u8) {
        self.mmio.write(address, value);
    }

    /// Plug a Game Boy Printer into the link port. The port defaults to a
    /// disconnected cable (byte-identical serial behavior); attaching is an
    /// explicit frontend action.
    pub fn attach_printer(&mut self) {
        self.mmio.attach_printer();
    }

    /// Connect two GB instances with a link cable (both ends attached).
    /// The frontend/harness pumps both instances (`step_instruction` /
    /// `run_until_frame`, any interleave); byte exchange happens at the
    /// correct cc through the shared [`crate::serial::LinkCable`]. Whichever
    /// side starts an internal-clock transfer is the master for that byte;
    /// an external-clock side completes when the master's window deposits.
    pub fn connect_link(a: &mut GB, b: &mut GB) {
        let (pa, pb) = crate::serial::LinkCable::pair();
        a.mmio.attach_link(pa);
        b.mmio.attach_link(pb);
    }

    #[allow(dead_code)] // KEEP (owner decision 2026-07-20): implemented peripheral awaiting frontend
    // wiring, not rot. No in-tree caller, so `dead_code` fires; do not delete.
    /// Plug one end of a link cable into this instance (the other end goes to
    /// a second instance, possibly owned by another window/process transport).
    pub(crate) fn attach_link_peer(&mut self, peer: crate::serial::LinkPeer) {
        self.mmio.attach_link(peer);
    }

    #[allow(dead_code)] // KEEP (owner decision 2026-07-20): implemented peripheral awaiting frontend
    // wiring, not rot. No in-tree caller, so `dead_code` fires; do not delete.
    pub(crate) fn link_attached(&self) -> bool {
        self.mmio.link_attached()
    }

    /// Unplug the link-port device (back to a disconnected cable).
    pub fn detach_serial_device(&mut self) {
        self.mmio.detach_serial_device();
    }

    #[allow(dead_code)] // KEEP (owner decision 2026-07-20): implemented peripheral awaiting frontend
    // wiring, not rot. No in-tree caller, so `dead_code` fires; do not delete.
    /// Point two GBC instances' IR ports at each other (Pan Docs "GBC Infrared
    /// Communication"). Each side's emitter (RP bit 0) illuminates the other's
    /// receiver (RP bit 1). The harness pumps both instances (any interleave);
    /// the shared channel carries the emitter level between their timelines. Use
    /// for GBC<->GBC IR: Pokémon G/S/C Mystery Gift, TCG "Card Pop", Pokémon
    /// Pinball score exchange, Bomberman trades.
    pub(crate) fn connect_ir(a: &mut GB, b: &mut GB) {
        let (la, lb) = crate::ir::IrLink::pair();
        a.mmio.attach_ir(la);
        b.mmio.attach_ir(lb);
    }

    #[allow(dead_code)] // KEEP (owner decision 2026-07-20): implemented peripheral awaiting frontend
    // wiring, not rot. No in-tree caller, so `dead_code` fires; do not delete.
    /// Plug one end of a shared IR channel into this instance (the other end
    /// goes to a second instance, possibly behind a socket/process transport).
    pub(crate) fn attach_ir_peer(&mut self, link: crate::ir::IrLink) {
        self.mmio.attach_ir(link);
    }

    #[allow(dead_code)] // KEEP (owner decision 2026-07-20): implemented peripheral awaiting frontend
    // wiring, not rot. No in-tree caller, so `dead_code` fires; do not delete.
    /// Diagnostic self-test: make this instance's IR port see its own emitter.
    pub(crate) fn set_ir_loopback(&mut self) {
        self.mmio.set_ir_loopback();
    }

    #[allow(dead_code)] // KEEP (owner decision 2026-07-20): implemented peripheral awaiting frontend
    // wiring, not rot. No in-tree caller, so `dead_code` fires; do not delete.
    pub(crate) fn ir_attached(&self) -> bool {
        self.mmio.ir_attached()
    }

    #[allow(dead_code)] // KEEP (owner decision 2026-07-20): implemented peripheral awaiting frontend
    // wiring, not rot. No in-tree caller, so `dead_code` fires; do not delete.
    /// Unplug the IR partner (back to a lone GBC that never sees light).
    pub(crate) fn detach_ir(&mut self) {
        self.mmio.detach_ir();
    }

    #[allow(dead_code)] // KEEP (owner decision 2026-07-20): implemented peripheral awaiting frontend
    // wiring, not rot. No in-tree caller, so `dead_code` fires; do not delete.
    /// Connect 2-4 Game Boys through a 4-Player Adapter (DMG-07). The adapter is
    /// the clock master, so each Game Boy uses external-clock serial; the shared
    /// hub runs the Pan Docs ping/transmission protocol. The frontend pumps all
    /// instances (any interleave), exactly like [`GB::connect_link`]. Player IDs
    /// are assigned by attach order (1..N).
    pub(crate) fn connect_four_player(gbs: &mut [&mut GB]) {
        let ports = crate::dmg07::FourPlayerPort::hub(gbs.len());
        for (gb, port) in gbs.iter_mut().zip(ports) {
            gb.mmio.attach_four_player(port);
        }
    }

    #[allow(dead_code)] // KEEP (owner decision 2026-07-20): implemented peripheral awaiting frontend
    // wiring, not rot. No in-tree caller, so `dead_code` fires; do not delete.
    /// Plug one DMG-07 port into this instance (the other ports go to other
    /// instances, possibly behind a socket/process transport).
    pub(crate) fn attach_four_player_port(&mut self, port: crate::dmg07::FourPlayerPort) {
        self.mmio.attach_four_player(port);
    }

    #[allow(dead_code)] // KEEP (owner decision 2026-07-20): implemented peripheral awaiting frontend
    // wiring, not rot. No in-tree caller, so `dead_code` fires; do not delete.
    pub(crate) fn four_player_attached(&self) -> bool {
        self.mmio.four_player_attached()
    }

    #[allow(dead_code)] // KEEP (owner decision 2026-07-20): implemented peripheral awaiting frontend
    // wiring, not rot. No in-tree caller, so `dead_code` fires; do not delete.
    /// Plug a Mobile Adapter GB into the link port. The adapter answers the
    /// libmobile packet protocol (session begin/end, config read/write); live
    /// networking is out of scope (see `crate::mobile`).
    pub(crate) fn attach_mobile_adapter(&mut self) {
        self.mmio.attach_mobile_adapter(crate::mobile::MobileAdapter::new());
    }

    #[allow(dead_code)] // KEEP (owner decision 2026-07-20): implemented peripheral awaiting frontend
    // wiring, not rot. No in-tree caller, so `dead_code` fires; do not delete.
    /// True once a game has completed the START "NINTENDO" handshake with an
    /// attached Mobile Adapter (i.e. detected it and begun a session).
    pub(crate) fn mobile_session_started(&self) -> bool {
        self.mmio.mobile_adapter().is_some_and(|m| m.session_started())
    }

    pub fn printer_attached(&self) -> bool {
        self.mmio.printer().is_some()
    }

    #[allow(dead_code)] // KEEP (owner decision 2026-07-20): implemented peripheral awaiting frontend
    // wiring, not rot. No in-tree caller, so `dead_code` fires; do not delete.
    /// Debug/test: the in-flight serial transfer's completion event cc
    /// (None while idle or while a link transfer holds for the peer).
    pub(crate) fn serial_transfer_complete_at(&self) -> Option<u64> {
        self.mmio.serial_transfer_complete_at()
    }

    /// Drain completed printer sheets (empty when no printer is attached or
    /// nothing has printed since the last drain).
    pub fn take_printer_sheets(&mut self) -> Vec<crate::printer::PrintSheet> {
        self.mmio
            .printer_mut()
            .map(|p| p.take_completed())
            .unwrap_or_default()
    }

    /// Mutable handle to the inserted cartridge (libretro save-RAM / RTC /
    /// rumble / Game Genie access).
    pub fn cartridge_mut(&mut self) -> Option<&mut cartridge::Cartridge> {
        self.mmio.get_cartridge_mut()
    }

    /// Immutable handle to the inserted cartridge.
    pub fn cartridge(&self) -> Option<&cartridge::Cartridge> {
        self.mmio.get_cartridge()
    }

    /// Fixed WRAM bank 0 (0xC000-0xCFFF) for libretro memory maps.
    pub fn wram_bank0_mut(&mut self) -> &mut [u8] {
        self.mmio.wram_bank0_slice_mut()
    }

    /// Switchable WRAM bank window (0xD000-0xDFFF) for libretro memory maps.
    pub fn wram_bank1_mut(&mut self) -> &mut [u8] {
        self.mmio.wram_bank1_slice_mut()
    }

    /// High RAM (0xFF80-0xFFFE) for libretro memory maps.
    pub fn hram_mut(&mut self) -> &mut [u8] {
        self.mmio.hram_slice_mut()
    }

    /// Video RAM bank 0 (0x8000-0x9FFF) for libretro memory maps.
    pub fn vram_mut(&mut self) -> &mut [u8] {
        self.mmio.vram_slice_mut()
    }

    /// Read RGB555 color from CGB background palette RAM
    pub fn read_bg_palette_data(&self, palette_idx: u8, color_idx: u8) -> u16 {
        let (low, high) = self.mmio.read_bg_palette_data(palette_idx, color_idx);
        (high as u16) << 8 | (low as u16)
    }

    /// Read RGB555 color from CGB object palette RAM
    pub fn read_obj_palette_data(&self, palette_idx: u8, color_idx: u8) -> u16 {
        let (low, high) = self.mmio.read_obj_palette_data(palette_idx, color_idx);
        (high as u16) << 8 | (low as u16)
    }

    /// Read from specific VRAM bank for debugging (CGB only)
    pub fn read_vram_bank(&self, bank: u8, address: u16) -> u8 {
        self.mmio.read_vram_bank(bank, address)
    }

    /// 16-bit internal timer/DIV counter (for state snapshots / diagnostics).
    pub fn timer_internal_counter(&self) -> u16 {
        self.mmio.timer_internal_counter()
    }

    /// Raw CGB BG palette RAM byte pair for a palette/color slot, ignoring the
    /// `cgb_features_enabled` bus gate (which blanks reads to 0xFF for a DMG
    /// cart on CGB and would hide the DMG-compat palette from snapshots).
    pub fn bg_palette_pair(&self, palette: u8, color: u8) -> u16 {
        let (low, high) = self.mmio.bg_palette_pair_raw(palette, color);
        (high as u16) << 8 | (low as u16)
    }

    /// Raw CGB OBJ palette RAM byte pair for a palette/color slot (see
    /// `bg_palette_pair`).
    pub fn obj_palette_pair(&self, palette: u8, color: u8) -> u16 {
        let (low, high) = self.mmio.obj_palette_pair_raw(palette, color);
        (high as u16) << 8 | (low as u16)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_state_file(path: &str) -> Result<Self, io::Error> {
        let saved_state = fs::read(path)?;
        Self::from_state_bytes(&saved_state)
    }

    /// Re-seed all the `#[serde(skip)]` derived/mirror state after a savestate
    /// deserialize: cartridge-flag cache, sub-module hardware-revision flags (which
    /// otherwise revert to default-CGB), and the CPU-mirror flags. The ROM image
    /// itself is re-attached separately by the frontend via `reattach_rom`.
    fn post_load_fixup(&mut self) {
        self.mmio.resync_cart_flags();
        // The `hardware` identity survives serialization; re-apply the setters
        // GB::new ran so timer-AGB / APU-revision behavior does not silently
        // revert to default-CGB after a load.
        self.mmio.reseed_hardware_flags(self.hardware);
        self.mmio.set_cgb_de(self.hardware.is_cgb_d_or_later());
        self.mmio.set_mgb(matches!(self.hardware, Hardware::MGB));
        // CPU-mirror flags (halt / STOP-window) re-derived from the serialized CPU.
        self.mmio
            .sync_cpu_mirror_flags(self.cpu.halted, self.cpu.stop_unhalt_cycles > 0);
    }

    /// Re-attach the ROM image to a savestate-restored machine. The runtime
    /// cartridge state (RAM, bank registers, RTC) came back through serde; only
    /// the read-only ROM (`#[serde(skip)]`) must be supplied. Returns `false` when
    /// the state carried no cartridge (old pre-cartridge-serialize state) so the
    /// caller falls back to a fresh `insert`. Re-derives `cart_has_clock`.
    pub fn reattach_rom(&mut self, rom: &[u8]) -> bool {
        self.mmio.reattach_rom(rom)
    }

    /// Whether a serde-restored cartridge is present but still awaiting its ROM
    /// image (i.e. `reattach_rom` must be called before the machine can run).
    pub fn cartridge_needs_rom(&self) -> bool {
        self.mmio.cartridge_needs_rom()
    }

    /// Clone the raw ROM image out of the currently-attached cartridge so a load
    /// path can carry it into a freshly-deserialized machine. `None` when no cart
    /// is inserted (or its ROM is not attached).
    pub fn detach_rom_bytes(&self) -> Option<Vec<u8>> {
        self.mmio
            .get_cartridge()
            .filter(|c| c.has_rom())
            .map(|c| c.detach_rom())
    }

    /// Serialize the whole machine to a savestate byte buffer. WASM-clean (no
    /// filesystem): the caller owns the bytes. Uses a compact binary format
    /// (bincode) — `serde_bytes` blobs (VRAM/WRAM/OAM/framebuffers) become
    /// length-prefixed byte runs, not JSON number-arrays, so a snapshot is
    /// ~its raw size instead of megabytes of text (inline web rewind was
    /// stalling on the JSON encode).    ///
    /// Deliberately unversioned while the project is pre-release: the layout
    /// moves freely and old states are simply invalid. A stale buffer therefore
    /// fails as an opaque bincode error, or — if it happens to decode — yields a
    /// wrong machine. Add a magic/version header before the first release.
    pub fn to_state_bytes(&mut self) -> Result<Vec<u8>, io::Error> {
        // Canonicalize: resolve any carried cross-instruction lag so the
        // serialized machine state is schedule-independent (the carry decision
        // depends on non-serialized perf caches; leaving it in the state would
        // make byte-compares of otherwise-identical machines diverge).
        if self.mmio.cpu_lag() > 0 {
            let mut bus = cpu::Bus::new(&mut self.mmio, &mut self.ppu);
            bus.flush_all_lag();
        }
        bincode::serialize(&self).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Reconstruct a machine from a savestate buffer produced by
    /// `to_state_bytes`. Re-derives the `#[serde(skip)]`
    /// cartridge-flag cache exactly as `from_state_file` does. WASM-clean.
    ///
    /// Unversioned (see `to_state_bytes`): a buffer from a different layout is
    /// rejected only insofar as bincode happens to notice.
    pub fn from_state_bytes(bytes: &[u8]) -> Result<Self, io::Error> {
        let mut gb: GB = bincode::deserialize(bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        gb.post_load_fixup();
        Ok(gb)
    }

    pub fn reset(&mut self) {
        self.mmio.reset();
        // `Mmio::reset` hands back a power-on Mmio, which knows nothing about
        // the model it is part of. Re-apply the identity from `self.hardware`
        // (the surviving source of truth) exactly as construction does, or the
        // machine silently continues as a default CGB. `self.region`, not the
        // default, so a region set after construction survives too.
        Self::seed_hardware_flags(&mut self.mmio, self.hardware, self.region);
        // The SGB header unlock gate is cart-derived, and the cart outlives the
        // reset while the SGB receiver does not — `seed_hardware_flags` installs
        // a fresh, unlocked one. Re-derive the gate from the surviving cart
        // exactly as `insert` does, or a reset SGB would start honouring packet
        // traffic from a cart that never declared SGB support.
        let sgb_unlocked = self
            .mmio
            .get_cartridge()
            .is_some_and(cartridge::Cartridge::supports_sgb);
        self.mmio.set_sgb_unlocked(sgb_unlocked);
        self.ppu.reset();
        self.cpu.halted = false;
        self.cpu.stopped = false;
        self.cpu.ime_enable_delay = 0;
        if self.skip_bios {
            self.skip_bios();
        } else {
            self.cpu.registers = cpu::registers::Registers::new();
        }
    }

    // Input methods to update button states
    pub fn set_input_state(&mut self, state: crate::input::ButtonState) {
        self.mmio.set_input_state(state);
    }

    // Breakpoint management methods
    pub fn add_breakpoint(&mut self, address: u16) {
        self.breakpoints.insert(address);
    }

    pub fn remove_breakpoint(&mut self, address: u16) {
        self.breakpoints.remove(&address);
    }

    pub fn clear_breakpoints(&mut self) {
        self.breakpoints.clear();
    }

    pub fn get_breakpoints(&self) -> &HashSet<u16> {
        &self.breakpoints
    }
}

#[cfg(test)]
mod stop_tests {
    //! Plain-STOP (low-power mode) micro-checks against the Pan Docs STOP
    //! behavior chart (Lior Halphon, "Reducing Power Consumption"): the
    //! STOP-mode / HALT-mode / NOP forks, the 1-vs-2-byte opcode length, the
    //! DIV reset, the whole-machine clock freeze, and the selected-line-only
    //! joypad wake. The armed-KEY1 speed-switch path (owned by the age spsw /
    //! speedchange suites) gets a tripwire sanity check only.
    use super::*;
    use crate::input::ButtonState;

    /// Minimal 32KB NoMBC ROM: `code` at 0x0100, everything else zero.
    /// `cgb_flag` goes to 0x0143 (0x80 = CGB-compat, needed for KEY1).
    fn rom_with(code: &[u8], cgb_flag: u8) -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x143] = cgb_flag;
        // 0x147/0x148/0x149 already 0: NoMBC, 32KB, no RAM.
        rom[0x100..0x100 + code.len()].copy_from_slice(code);
        rom
    }

    fn gb_with(code: &[u8], hardware: Hardware, cgb_flag: u8) -> GB {
        let mut gb = GB::new(hardware);
        gb.insert(cartridge::Cartridge::from_bytes(&rom_with(code, cgb_flag)).unwrap());
        gb.skip_bios();
        gb
    }

    fn step_n(gb: &mut GB, n: usize) {
        for _ in 0..n {
            gb.step_instruction(false);
        }
    }

    /// Step until `pred` holds, bounded; panics on timeout.
    fn step_until(gb: &mut GB, bound: usize, what: &str, pred: impl Fn(&GB) -> bool) {
        for _ in 0..bound {
            if pred(gb) {
                return;
            }
            gb.step_instruction(false);
        }
        panic!("step_until timed out waiting for {what}");
    }

    /// Two CGB instances with their IR ports connected: one side's emitter
    /// (RP bit 0) must illuminate the other side's receiver (RP bit 1), only
    /// while the reader has read enabled (bits 6-7 set), and never its own.
    /// Pan Docs "GBC Infrared Communication".
    #[test]
    fn cgb_ir_couples_emitter_to_peer_receiver_via_rp() {
        let mut a = gb_with(&[], Hardware::CGB, 0x80);
        let mut b = gb_with(&[], Hardware::CGB, 0x80);
        GB::connect_ir(&mut a, &mut b);
        assert!(a.ir_attached() && b.ir_attached());

        // Both enable reading ($C0), emitters off: each receiver reads bit 1 = 1
        // ("no signal").
        a.write_memory(0xFF56, 0xC0);
        b.write_memory(0xFF56, 0xC0);
        assert_eq!(a.read_memory(0xFF56) & 0x02, 0x02);
        assert_eq!(b.read_memory(0xFF56) & 0x02, 0x02);

        // A lights its emitter ($C1 = read-enable + LED on). B, with read
        // enabled, now sees the signal (bit 1 -> 0); A does not see its own LED.
        a.write_memory(0xFF56, 0xC1);
        assert_eq!(b.read_memory(0xFF56) & 0x02, 0x00, "B must see A's emitter");
        assert_eq!(a.read_memory(0xFF56) & 0x02, 0x02, "A must not see its own");

        // With read disabled (bits 6-7 clear) bit 1 reads 1 regardless of light.
        b.write_memory(0xFF56, 0x01);
        assert_eq!(b.read_memory(0xFF56) & 0x02, 0x02, "read disabled -> no signal");

        // A turns the emitter off: B (read re-enabled) sees darkness again.
        a.write_memory(0xFF56, 0xC0);
        b.write_memory(0xFF56, 0xC0);
        assert_eq!(b.read_memory(0xFF56) & 0x02, 0x02);

        // A lone instance (detached) never sees light.
        a.detach_ir();
        b.write_memory(0xFF56, 0xC1); // B emits
        a.write_memory(0xFF56, 0xC0);
        assert_eq!(a.read_memory(0xFF56) & 0x02, 0x02, "detached -> no partner");
    }

    /// A Game Boy with a Mobile Adapter must complete the START "NINTENDO"
    /// handshake over real internal-clock serial: driving the libmobile packet
    /// by hand (SB = byte, SC = internal clock + start), the adapter begins a
    /// session. Grounded in libmobile serial.c/commands.c.
    #[test]
    fn mobile_adapter_start_handshake_over_internal_clock_serial() {
        let mut gb = gb_with(&[], Hardware::DMG, 0x00);
        gb.attach_mobile_adapter();
        assert!(!gb.mobile_session_started());

        // Build the START packet: $99 $66, header [cmd,0,0,len], data, checksum,
        // then the device-id / idle bytes that clock the ack + response start.
        let data = b"NINTENDO";
        let header = [0x10u8, 0, 0, data.len() as u8];
        let mut sum = 0u16;
        let mut packet = vec![0x99u8, 0x66];
        for &b in header.iter().chain(data.iter()) {
            packet.push(b);
            sum = sum.wrapping_add(b as u16);
        }
        packet.push((sum >> 8) as u8);
        packet.push(sum as u8);
        packet.push(0x88); // Game Boy device-id exchange
        packet.push(0x00); // acknowledge skip
        packet.push(0x4B); // idle -> process + begin response

        for &byte in &packet {
            gb.write_memory(0xFF01, byte); // SB = our byte
            gb.write_memory(0xFF02, 0x81); // SC: internal clock (bit0=1), start
            step_until(&mut gb, 100_000, "serial xfer complete", |g| {
                g.read_memory(0xFF02) & 0x80 == 0
            });
        }
        assert!(
            gb.mobile_session_started(),
            "the NINTENDO handshake must begin a Mobile Adapter session"
        );
    }

    /// A Game Boy plugged into a DMG-07 must receive the ping stream over
    /// external-clock serial: the adapter clocks each transfer and deposits its
    /// protocol byte. Driving the transfers by hand (SB = reply, SC = external
    /// clock + start), the received stream is `FE, STAT, STAT, STAT, ...` with
    /// the STAT reporting P1 connected once we ACK. Pan Docs "4-Player Adapter".
    #[test]
    fn dmg07_ping_stream_reaches_the_gameboy_over_external_clock() {
        let mut gb = gb_with(&[], Hardware::DMG, 0x00);
        let port = crate::dmg07::FourPlayerPort::hub(2).into_iter().next().unwrap();
        gb.attach_four_player_port(port);
        assert!(gb.four_player_attached());

        let mut received = Vec::new();
        for _ in 0..8 {
            gb.write_memory(0xFF01, 0x88); // ACK everything so P1 connects
            gb.write_memory(0xFF02, 0x80); // external clock (bit0=0), start
            step_until(&mut gb, 100_000, "serial xfer complete", |g| {
                g.read_memory(0xFF02) & 0x80 == 0
            });
            received.push(gb.read_memory(0xFF01));
        }
        // The adapter broadcasts ping headers ($FE) followed by STAT bytes.
        assert!(received.contains(&0xFE), "must receive the ping header, got {received:02X?}");
        // After ACKing, a STAT byte reports P1 connected (bit 4) with player ID 1.
        assert!(
            received.iter().any(|&b| b & 0x17 == 0x11),
            "must receive a P1-connected STAT byte, got {received:02X?}"
        );
    }

    /// A STOP window freezes the machine, but NOT the host's audio clock. The
    /// stopped path used to return before the audio block, so a STOP emitted
    /// zero samples: the host stream starves, and a recording's audio slides
    /// earlier than the video it was captured with. It must emit the same
    /// nominal count per frame as a running machine — the APU being frozen
    /// changes what the samples contain, not how many there are.
    #[test]
    fn a_stopped_frame_emits_the_same_sample_count_as_a_running_one() {
        use crate::audio::AudioOutput;
        use std::sync::{Arc, Mutex};

        struct Cap(Arc<Mutex<Vec<(f32, f32)>>>);
        impl AudioOutput for Cap {
            fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
                Ok(())
            }
            fn add_samples(&mut self, s: &[(f32, f32)]) {
                self.0.lock().unwrap().extend_from_slice(s);
            }
        }

        // Per-frame sample counts over `frames` frames, discarding the first
        // (the STOP machine spends part of it still running).
        fn per_frame_counts(code: &[u8], frames: usize) -> Vec<usize> {
            let mut gb = gb_with(code, Hardware::DMG, 0x00);
            let buf = Arc::new(Mutex::new(Vec::new()));
            gb.enable_audio(Box::new(Cap(buf.clone()))).unwrap();
            let mut counts = Vec::new();
            let mut seen = 0;
            for _ in 0..frames {
                gb.run_until_frame(true);
                let now = buf.lock().unwrap().len();
                counts.push(now - seen);
                seen = now;
            }
            counts.remove(0);
            counts
        }

        // JOYP=$30 deselects both button groups, so this STOP never wakes.
        let stopped = per_frame_counts(&[0x3E, 0x30, 0xE0, 0x00, 0x10, 0x00, 0x18, 0xFE], 6);
        // A machine that never stops: `jr self`.
        let running = per_frame_counts(&[0x18, 0xFE], 6);

        // 70224 dots / (4194304/44100) = 738.4 pairs, so frames alternate
        // between 738 and 739 as the fractional accumulator carries. A stopped
        // machine steps in fixed 4-cycle quanta where a running one steps whole
        // instructions, so the two land that carry on different frames; what
        // must match is the rate, not the per-frame cadence.
        assert_eq!(
            stopped.iter().sum::<usize>(),
            running.iter().sum::<usize>(),
            "a STOP window must emit the nominal sample count, not starve the \
             stream (stopped {stopped:?} vs running {running:?})"
        );
        assert!(
            stopped.iter().all(|&n| (738..=739).contains(&n)),
            "every stopped frame is one nominal frame of audio, got {stopped:?}"
        );
    }

    const BTN_NONE: ButtonState = ButtonState {
        a: false,
        b: false,
        start: false,
        select: false,
        up: false,
        down: false,
        left: false,
        right: false,
    };

    /// Chart: no button held, no speed switch, no interrupt pending ->
    /// "STOP is a 2-byte opcode, STOP mode is entered, DIV is reset."
    /// Plus: full clock freeze while stopped, wake only on a SELECTED
    /// joypad line going low, DIV still 0 on wake (the roadmap micro-check).
    #[test]
    fn stop_resets_div_freezes_clock_and_wakes_on_selected_button() {
        // 0100: ld a,$10 ; ldh (00),a   JOYP=$10 -> P15 low: BUTTONS group
        //                               selected, d-pad deselected (the select
        //                               bits are active-low)
        // 0104: stop $00                case C
        // 0106: inc a                   post-wake marker
        // 0107: jr self
        let mut gb = gb_with(&[0x3E, 0x10, 0xE0, 0x00, 0x10, 0x00, 0x3C, 0x18, 0xFE], Hardware::DMG, 0x00);
        step_n(&mut gb, 2);
        assert_ne!(gb.mmio.read(0xFF04), 0, "premise: DIV nonzero before STOP");
        assert_eq!(gb.cpu.registers.pc, 0x0104);

        gb.step_instruction(false); // STOP
        assert!(gb.cpu.stopped, "STOP mode entered");
        assert!(!gb.cpu.halted);
        assert_eq!(gb.cpu.registers.pc, 0x0106, "2-byte form: operand consumed");
        assert_eq!(gb.mmio.read(0xFF04), 0, "DIV reset on STOP entry");

        // Clock freeze: nothing advances while stopped.
        let cc = gb.mmio.master_cc();
        for _ in 0..2000 {
            let (bp, cycles) = gb.step_instruction(false);
            assert!(!bp);
            assert_eq!(cycles, 4, "frozen steps report pacing cycles");
        }
        assert_eq!(gb.mmio.master_cc(), cc, "master_cc frozen during STOP");
        assert_eq!(gb.mmio.read(0xFF04), 0, "DIV held at 0 (timer stopped)");
        assert!(gb.cpu.stopped);

        // A key in the DESELECTED d-pad group must NOT wake (P14 high).
        gb.set_input_state(ButtonState { right: true, ..BTN_NONE });
        step_n(&mut gb, 50);
        assert!(gb.cpu.stopped, "deselected-group press does not terminate STOP");
        assert_eq!(gb.mmio.master_cc(), cc);

        // A SELECTED button line going low terminates STOP; the wake charges
        // 8 T-cycles of world advance before the CPU resumes.
        gb.set_input_state(ButtonState { a: true, ..BTN_NONE });
        let (_, wake_cycles) = gb.step_instruction(false);
        assert!(!gb.cpu.stopped, "selected-line low terminates STOP");
        assert_eq!(wake_cycles, 8);
        assert_eq!(gb.mmio.master_cc(), cc + 8);
        assert_eq!(gb.mmio.read(0xFF04), 0, "DIV == 0 observed on wake");

        // Execution resumes past the 2-byte STOP: the marker runs.
        let a_before = gb.cpu.registers.a;
        gb.step_instruction(false); // inc a
        assert_eq!(gb.cpu.registers.a, a_before.wrapping_add(1));
        assert_eq!(gb.cpu.registers.pc, 0x0107);
    }

    /// Chart: no button held, no speed switch, interrupt pending ->
    /// "STOP is a 1-byte opcode, STOP mode is entered, DIV is reset" —
    /// the pending interrupt neither prevents nor terminates STOP mode,
    /// and the byte after $10 executes as an instruction on wake.
    #[test]
    fn stop_irq_pending_is_one_byte_and_still_enters_stop_mode() {
        // 0100: xor a ; ldh (00),a      JOYP=$00 -> both groups selected
        // 0103: ld a,1
        // 0105: ldh (FF),a              IE = VBlank
        // 0107: ldh (0F),a              IF = VBlank (pending, IME off)
        // 0109: stop                    case D, operand...
        // 010A: inc a                   ...which must EXECUTE on wake
        // 010B: jr self
        let mut gb = gb_with(
            &[0xAF, 0xE0, 0x00, 0x3E, 0x01, 0xE0, 0xFF, 0xE0, 0x0F, 0x10, 0x3C, 0x18, 0xFE],
            Hardware::DMG,
            0x00,
        );
        step_n(&mut gb, 5);
        assert_eq!(gb.cpu.registers.pc, 0x0109);
        gb.step_instruction(false); // STOP
        assert!(gb.cpu.stopped, "STOP mode entered despite pending IE&IF");
        assert_eq!(gb.cpu.registers.pc, 0x010A, "1-byte form: operand NOT consumed");
        assert_eq!(gb.mmio.read(0xFF04), 0, "DIV reset");

        let cc = gb.mmio.master_cc();
        step_n(&mut gb, 100);
        assert!(gb.cpu.stopped, "pending interrupt does not terminate STOP mode");
        assert_eq!(gb.mmio.master_cc(), cc);

        gb.set_input_state(ButtonState { start: true, ..BTN_NONE });
        gb.step_instruction(false); // wake
        assert!(!gb.cpu.stopped);
        gb.step_instruction(false); // the operand byte: inc a (IME off, no service)
        assert_eq!(gb.cpu.registers.a, 2, "STOP operand executed as an instruction");
        assert_eq!(gb.cpu.registers.pc, 0x010B);
    }

    /// Chart: button held (selected line low) + interrupt pending ->
    /// "STOP is a 1-byte opcode, mode doesn't change, DIV doesn't reset."
    #[test]
    fn stop_button_held_irq_pending_is_a_one_byte_nop() {
        let mut gb = gb_with(
            &[0xAF, 0xE0, 0x00, 0x3E, 0x01, 0xE0, 0xFF, 0xE0, 0x0F, 0x10, 0x3C, 0x18, 0xFE],
            Hardware::DMG,
            0x00,
        );
        gb.set_input_state(ButtonState { a: true, ..BTN_NONE });
        step_n(&mut gb, 5);
        assert_eq!(gb.cpu.registers.pc, 0x0109);
        let div = gb.mmio.read(0xFF04);
        assert_ne!(div, 0, "premise: DIV nonzero");
        gb.step_instruction(false); // STOP
        assert!(!gb.cpu.stopped, "mode doesn't change");
        assert!(!gb.cpu.halted, "mode doesn't change");
        assert_eq!(gb.cpu.registers.pc, 0x010A, "1-byte form");
        // DIV keeps counting (it may tick across the step) — but is NOT reset.
        let post = gb.mmio.read(0xFF04);
        assert!(post == div || post == div.wrapping_add(1), "DIV not reset (pre {div}, post {post})");
        gb.step_instruction(false); // inc a executes immediately
        assert_eq!(gb.cpu.registers.a, 2);
    }

    /// Chart: button held (selected line low), no interrupt pending ->
    /// "STOP is a 2-byte opcode, HALT mode is entered, DIV is not reset."
    /// HALT exits on the next enabled interrupt (VBlank here, IME off).
    #[test]
    fn stop_button_held_no_irq_enters_halt_mode() {
        // 0100: xor a ; ldh (00),a      JOYP=$00
        // 0103: ld a,1 ; ldh (FF),a     IE = VBlank, IF stays clear
        // 0107: stop $00                case A
        // 0109: inc a
        // 010A: jr self
        let mut gb = gb_with(
            &[0xAF, 0xE0, 0x00, 0x3E, 0x01, 0xE0, 0xFF, 0x10, 0x00, 0x3C, 0x18, 0xFE],
            Hardware::DMG,
            0x00,
        );
        gb.set_input_state(ButtonState { b: true, ..BTN_NONE });
        step_n(&mut gb, 4);
        assert_eq!(gb.cpu.registers.pc, 0x0107);
        // The held-button edge raised IF.4 (joypad) and the post-boot seed
        // carries IF.0; clear IF so no interrupt is pending at the STOP (the
        // chart branch under test).
        gb.mmio.write(0xFF0F, 0x00);
        let div = gb.mmio.read(0xFF04);
        gb.step_instruction(false); // STOP
        assert!(gb.cpu.halted, "HALT mode entered");
        assert!(!gb.cpu.stopped);
        assert_eq!(gb.cpu.registers.pc, 0x0109, "2-byte form");
        assert_eq!(gb.mmio.read(0xFF04), div, "DIV not reset");

        // The clock keeps running in HALT mode (unlike STOP mode): the PPU
        // reaches VBlank, IF.0 wakes the CPU, and the marker executes.
        step_until(&mut gb, 200_000, "HALT exit", |gb| !gb.cpu.halted);
        step_until(&mut gb, 10, "marker", |gb| gb.cpu.registers.pc == 0x010A);
        assert_eq!(gb.cpu.registers.a, 2);
    }

    /// Pan Docs panel behavior: a plain STOP with the LCD enabled turns a CGB
    /// panel black and a DMG panel blank/white (outside mode 3). The pre-STOP
    /// frame shows the boot logo, so the paint is observable.
    #[test]
    fn stop_panel_goes_dark_cgb_black_dmg_white() {
        // 0100: xor a ; ldh (00),a          JOYP=$00
        // 0103: ld a,1 ; ldh (FF),a         IE = VBlank
        // 0107: xor a ; ldh (0F),a ; halt ; nop   (x3: let 3 frames display)
        // ...
        // 0113: stop $00
        // 0115: jr self
        let code = [
            0xAF, 0xE0, 0x00, 0x3E, 0x01, 0xE0, 0xFF, 0xAF, 0xE0, 0x0F, 0x76, 0x00, 0xAF, 0xE0,
            0x0F, 0x76, 0x00, 0xAF, 0xE0, 0x0F, 0x76, 0x00, 0x10, 0x00, 0x18, 0xFE,
        ];
        for (hw, cgb) in [(Hardware::CGB, true), (Hardware::DMG, false)] {
            let mut gb = gb_with(&code, hw, 0x00);
            for _ in 0..3 {
                gb.run_until_frame(false);
            }
            // Premise: the pre-STOP CGB frame is NOT already all-black (a
            // rendered blank line is white 0xFF), so all-black afterwards
            // proves the STOP paint; the DMG frame shows the boot logo
            // (non-zero shades), so all-white afterwards proves it too.
            // Compare in the canonical domain: colour by RGB, mono by shade
            // index (palette/correction-independent).
            let pre_ok = if gb.frame_renders_color() {
                gb.get_current_frame().rgb().iter().any(|&b| b != 0x00)
            } else {
                gb.dmg_shade_frame().iter().any(|&s| s != 0)
            };
            assert!(pre_ok, "premise: pre-STOP frame distinguishable ({hw:?})");
            step_until(&mut gb, 2_000_000, "STOP entry", |gb| gb.cpu.stopped);
            if gb.frame_renders_color() {
                assert!(cgb, "color frame implies CGB here");
                assert!(
                    gb.get_current_frame().rgb().iter().all(|&b| b == 0x00),
                    "CGB STOP panel is black"
                );
            } else {
                assert!(!cgb);
                assert!(gb.dmg_shade_frame().iter().all(|&s| s == 0), "DMG STOP panel is blank/white");
            }
        }
    }

    /// Armed-KEY1 tripwire: STOP with a speed switch armed takes the existing
    /// CGB switch path (owned byte-exactly by age spsw / speedchange)
    /// and must NOT enter the new low-power freeze.
    #[test]
    fn stop_with_armed_key1_still_speed_switches() {
        // 0100: ld a,1 ; ldh (4D),a     KEY1.0 arm
        // 0104: stop $00
        // 0106: jr self
        let mut gb = gb_with(&[0x3E, 0x01, 0xE0, 0x4D, 0x10, 0x00, 0x18, 0xFE], Hardware::CGB, 0x80);
        step_n(&mut gb, 2);
        assert!(!gb.mmio.is_double_speed_mode());
        gb.step_instruction(false); // STOP -> switch
        assert!(gb.mmio.is_double_speed_mode(), "speed switch performed");
        assert!(!gb.cpu.stopped, "armed path does not enter low-power STOP");
        // The 0x20000-cycle unhalt window drains while the world advances.
        let cc = gb.mmio.master_cc();
        step_n(&mut gb, 64);
        assert!(gb.mmio.master_cc() > cc, "world keeps running through the window");
    }
}

#[cfg(test)]
mod savestate_roundtrip_tests {
    //! A savestate must fully round-trip the machine — including the PPU's live
    //! sprite pipeline (OamReader snapshot + per-slot OBJ size) and OAM — so a
    //! mid-frame restore resumes with identical output and no sprite loss. This
    //! is the regression for the rewind "sprites vanish / PPU corrupts" bug: the
    //! OamReader snapshot was `#[serde(skip)]`, so a restored state scanned an
    //! all-zero sprite buffer.
    use super::*;

    /// Advance one frame and return its canonical bytes — colour by RGB, mono by
    /// shade index — so a restore mismatch is a real emulation divergence, not a
    /// presentation-palette difference (the palette is not saved).
    fn frame_bytes(gb: &mut GB) -> Vec<u8> {
        let (frame, _) = gb.run_until_frame(false);
        if gb.frame_renders_color() {
            frame.0.to_vec()
        } else {
            gb.dmg_shade_frame().to_vec()
        }
    }

    /// Load a ROM into a booted GB, or `None` if the ROM file is absent (the
    /// gb-test-roms submodule is optional in some checkouts).
    fn gb_from_rom(path: &str, hardware: Hardware) -> Option<GB> {
        let rom = fs::read(path).ok()?;
        let mut gb = GB::new(hardware);
        gb.insert(cartridge::Cartridge::from_bytes(&rom).ok()?);
        gb.skip_bios();
        Some(gb)
    }

    /// Save the state mid-frame (partway through a scanline, LCD on with sprites
    /// on screen), restore into a fresh machine, and prove the restored machine
    /// produces byte-identical frames for several frames afterward. Before the
    /// fix the OamReader snapshot was dropped, so the restored machine lost every
    /// sprite on its next mode-2 scan.
    #[test]
    fn dmg_acid2_midframe_state_roundtrips_sprites() {
        let Some(mut gb) = gb_from_rom("../gb-test-roms/dmg-acid2/dmg-acid2.gb", Hardware::DMG)
        else {
            eprintln!("skipping: dmg-acid2.gb not present");
            return;
        };

        // Settle the reference image (draws BG + sprites).
        for _ in 0..30 {
            gb.run_until_frame(false);
        }
        // Advance partway into a scanline so the save lands mid-render with the
        // sprite pipeline (OamReader snapshot, scan_slot_large) holding live
        // state — the state that used to be dropped, blanking every sprite.
        gb.run_until_frame(false);
        for _ in 0..2000 {
            gb.step_instruction(false);
        }

        let state = gb.to_state_bytes().expect("serialize");

        // The `cartridge` field is `#[serde(skip)]` (the multi-MB ROM stays out
        // of the state), so the frontend re-attaches the live ROM after a load.
        // Model that here; without it the machine has no ROM and cannot run.
        let restore = |state: &[u8]| -> GB {
            let mut g = GB::from_state_bytes(state).expect("deserialize");
            g.mmio.debug_graft_cartridge(&gb.mmio);
            g
        };

        // Run the original and a freshly-restored copy in lockstep; every frame
        // must match, proving the restored PPU/OAM sprite pipeline resumes
        // identically. Regression guard for the vanished-sprites bug (OamReader /
        // scan_slot_large were `#[serde(skip)]`, so the restored machine scanned
        // an all-zero sprite buffer and lost every sprite).
        let mut restored = restore(&state);
        let mut any_content = false;
        for frame in 0..10 {
            let orig = frame_bytes(&mut gb);
            let redo = frame_bytes(&mut restored);
            assert_eq!(orig, redo, "frame {frame} differs after state restore");
            if orig.iter().any(|&p| p != orig[0]) {
                any_content = true;
            }
        }
        assert!(any_content, "test rendered a blank frame (no content)");
    }

    /// OAM bytes must round-trip verbatim through a savestate (the sprite table
    /// itself, independent of the render pipeline).
    #[test]
    fn oam_bytes_roundtrip() {
        let Some(mut gb) = gb_from_rom("../gb-test-roms/dmg-acid2/dmg-acid2.gb", Hardware::DMG)
        else {
            eprintln!("skipping: dmg-acid2.gb not present");
            return;
        };
        for _ in 0..30 {
            gb.run_until_frame(false);
        }
        let oam_before: Vec<u8> = (0xFE00..=0xFE9F).map(|a| gb.read_memory(a)).collect();
        let state = gb.to_state_bytes().expect("serialize");
        let restored = GB::from_state_bytes(&state).expect("deserialize");
        let oam_after: Vec<u8> = (0xFE00..=0xFE9F).map(|a| restored.read_memory(a)).collect();
        assert_eq!(oam_before, oam_after, "OAM not preserved across savestate");
    }

    /// Re-attach the live ROM to a savestate-restored machine exactly as the
    /// frontends do (the ROM image is `#[serde(skip)]`), returning the restored
    /// machine ready to run.
    fn restore_with_rom(state: &[u8], live: &GB) -> GB {
        let mut g = GB::from_state_bytes(state).expect("deserialize");
        if g.cartridge_needs_rom() {
            let rom = live.detach_rom_bytes().expect("live ROM");
            assert!(g.reattach_rom(&rom), "reattach_rom");
        }
        g
    }

    /// Core fidelity assertion: from a machine paused at some capture point, a
    /// state saved + restored (ROM re-attached) must step byte-identically for
    /// `frames` frames — both the full serialized machine AND the frame pixels
    /// must match every frame. Also proves the state carries NO ROM (small).
    fn assert_roundtrip(mut gb: GB, frames: usize, label: &str) {
        let state = gb.to_state_bytes().expect("serialize");
        // The ROM image must NOT be embedded in the state (`rom_data` is
        // `#[serde(skip)]`; serializing it into every rewind-ring snapshot would be
        // fatal). The direct proof: the deserialized cartridge comes back present
        // but WITHOUT its ROM, so the frontend must re-attach it.
        {
            let bare = GB::from_state_bytes(&state).expect("deserialize");
            assert!(
                bare.cartridge_needs_rom(),
                "{label}: cartridge restored with ROM already attached — ROM was serialized"
            );
        }
        let mut restored = restore_with_rom(&state, &gb);
        // The restored machine's own re-serialization must equal the original's
        // (every reachable field round-tripped, ignoring the skipped ROM).
        assert_eq!(
            state,
            restored.to_state_bytes().expect("re-serialize"),
            "{label}: restored state not byte-identical at frame 0"
        );
        for frame in 0..frames {
            let orig = frame_bytes(&mut gb);
            let redo = frame_bytes(&mut restored);
            assert_eq!(orig, redo, "{label}: frame {frame} pixels differ after restore");
            assert_eq!(
                gb.to_state_bytes().expect("serialize"),
                restored.to_state_bytes().expect("serialize"),
                "{label}: full machine state diverged at frame {frame}"
            );
        }
    }

    /// Full-machine round-trip fidelity across hardware revisions and capture
    /// points that exercise the newly-serialized anchors (cartridge runtime
    /// state, hardware-revision flags, HALT/OAM-DMA/HDMA mirrors). Permanent
    /// guard for the "state does not fully round-trip the machine" gap.
    #[test]
    fn savestate_full_roundtrip_fidelity() {
        // (rom, hardware) covering DMG / CGB / CGB-E / AGB / SGB. dmg-acid2 is a
        // plain (no-MBC) cart; cgb-acid2 drives the CGB feature set.
        let cases: &[(&str, Hardware)] = &[
            ("../gb-test-roms/dmg-acid2/dmg-acid2.gb", Hardware::DMG),
            ("../gb-test-roms/cgb-acid2/cgb-acid2.gbc", Hardware::CGB),
            ("../gb-test-roms/cgb-acid2/cgb-acid2.gbc", Hardware::CGBE),
            ("../gb-test-roms/cgb-acid2/cgb-acid2.gbc", Hardware::AGB),
            ("../gb-test-roms/dmg-acid2/dmg-acid2.gb", Hardware::SGB),
        ];
        let mut ran = false;
        for &(path, hw) in cases {
            let Some(mut gb) = gb_from_rom(path, hw) else {
                eprintln!("skipping {path} ({hw:?}): ROM not present");
                continue;
            };
            ran = true;
            // Settle a few frames, then save mid-scanline so the capture straddles
            // an active render (mode 2/3, OAM-DMA-adjacent, HDMA-eligible on CGB).
            for _ in 0..20 {
                gb.run_until_frame(false);
            }
            gb.run_until_frame(false);
            for _ in 0..1500 {
                gb.step_instruction(false);
            }
            assert_roundtrip(gb, 6, &format!("{path} {hw:?} midframe"));
        }

        // Capture-point variants that exercise the specific gap anchors: save
        // while the CPU is parked in HALT and while an OAM-DMA is mid-transfer.
        if let Some(mut gb) = gb_from_rom("../gb-test-roms/dmg-acid2/dmg-acid2.gb", Hardware::DMG) {
            ran = true;
            for _ in 0..20 {
                gb.run_until_frame(false);
            }
            // Kick an OAM DMA (source 0xC000) then step a couple M-cycles so the
            // save lands with dma_active + dma_pos mid-transfer.
            gb.write_memory(0xFF46, 0xC0);
            for _ in 0..3 {
                gb.step_instruction(false);
            }
            assert_roundtrip(gb, 4, "dmg mid-OAM-DMA");
        }

        // MBC3 + RTC cartridge: proves the RAM, bank registers, and live RTC
        // (seconds/…/day-high + sub-second accumulator) survive the round-trip.
        if let Some(mut gb) = gb_from_rom("../gb-test-roms/rtc3test/rtc3test.gb", Hardware::DMG) {
            ran = true;
            for _ in 0..30 {
                gb.run_until_frame(false);
            }
            assert_roundtrip(gb, 4, "rtc3test MBC3-RTC");
        }

        if !ran {
            eprintln!("skipping savestate_full_roundtrip_fidelity: no test ROMs present");
        }
    }

    /// The APU half of the round-trip, asserted on the *audio* the restored
    /// machine goes on to produce rather than on its serialized bytes: with all
    /// four channels live (sweep, both envelope directions, length counters
    /// armed, wave RAM loaded, a stepping LFSR), a restored machine must emit a
    /// sample-for-sample identical stream. ROM-free and self-driving, so it
    /// pins the channels themselves rather than some ROM's incidental mix.
    ///
    /// The state-bytes comparisons elsewhere in this module cannot catch an APU
    /// field that restores wrong-but-serializes-the-same; only re-running the
    /// mixer can. Regression guard for the dead-field removal that dropped the
    /// per-channel `length_enabled`/`fs_step` mirrors and the noise envelope's
    /// `volume_timer`/`volume_direction`.
    #[test]
    fn savestate_roundtrips_a_fully_live_apu() {
        use std::sync::{Arc, Mutex};

        type Samples = Arc<Mutex<Vec<(f32, f32)>>>;

        #[derive(Clone)]
        struct Cap(Samples);
        impl audio::AudioOutput for Cap {
            fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
                Ok(())
            }
            fn add_samples(&mut self, s: &[(f32, f32)]) {
                self.0.lock().unwrap().extend_from_slice(s);
            }
        }

        /// A machine with every channel programmed and triggered. Length is
        /// enabled (NRx4 bit 6) on CH1/CH3 and left off on CH2/CH4, so both
        /// arms of the length path are live across the restore.
        fn live_apu_machine() -> (GB, Samples) {
            let rom = vec![0u8; 0x8000];
            let mut gb = GB::new(Hardware::DMG);
            gb.insert(cartridge::Cartridge::from_bytes(&rom).unwrap());
            gb.skip_bios();
            let buf = Arc::new(Mutex::new(Vec::new()));
            gb.enable_audio(Box::new(Cap(buf.clone()))).unwrap();

            gb.write_memory(0xFF26, 0x80); // NR52: APU on
            gb.write_memory(0xFF25, 0xFF); // NR51: every channel to both sides
            gb.write_memory(0xFF24, 0x77); // NR50: full volume both sides

            // CH1: sweeping square, decreasing envelope, length armed.
            gb.write_memory(0xFF10, 0x35); // NR10: sweep period 3, down, shift 5
            gb.write_memory(0xFF11, 0x80); // NR11: duty 2, length load 0
            gb.write_memory(0xFF12, 0xF3); // NR12: vol 15, decrease, period 3
            gb.write_memory(0xFF13, 0x00); // NR13: freq low
            gb.write_memory(0xFF14, 0xC5); // NR14: trigger + length enable

            // CH2: square, INCREASING envelope, length off.
            gb.write_memory(0xFF16, 0x40); // NR21: duty 1
            gb.write_memory(0xFF17, 0x28); // NR22: vol 2, increase, period 0
            gb.write_memory(0xFF18, 0x80); // NR23: freq low
            gb.write_memory(0xFF19, 0x86); // NR24: trigger, length disabled

            // CH3: wave, length armed, a non-uniform pattern so the sample
            // index (not just the DAC level) is observable.
            gb.write_memory(0xFF1A, 0x80); // NR30: DAC on
            for i in 0..16u16 {
                gb.write_memory(0xFF30 + i, (i as u8) * 0x11);
            }
            gb.write_memory(0xFF1B, 0x00); // NR31: length load
            gb.write_memory(0xFF1C, 0x20); // NR32: volume 100%
            gb.write_memory(0xFF1D, 0x00); // NR33: freq low
            gb.write_memory(0xFF1E, 0xC6); // NR34: trigger + length enable

            // CH4: noise, decreasing envelope, stepping LFSR, length off.
            gb.write_memory(0xFF20, 0x00); // NR41: length load
            gb.write_memory(0xFF21, 0xF1); // NR42: vol 15, decrease, period 1
            gb.write_memory(0xFF22, 0x37); // NR43: mid divisor/shift
            gb.write_memory(0xFF23, 0x80); // NR44: trigger, length disabled
            (gb, buf)
        }

        let (mut gb, orig_buf) = live_apu_machine();
        // Settle mid-envelope/mid-sweep so the capture point is not a boundary.
        for _ in 0..6 {
            gb.run_until_frame(true);
        }

        let state = gb.to_state_bytes().expect("serialize");
        let mut restored = restore_with_rom(&state, &gb);
        // The sink is `#[serde(skip)]`, so the restored machine has none; the
        // live machine keeps the one it already has (`enable_audio` no-ops when
        // a sink is attached), so drop the settle-phase capture instead.
        let redo_buf = Arc::new(Mutex::new(Vec::new()));
        restored.enable_audio(Box::new(Cap(redo_buf.clone()))).unwrap();
        orig_buf.lock().unwrap().clear();

        for frame in 0..8 {
            gb.run_until_frame(true);
            restored.run_until_frame(true);
            assert_eq!(
                gb.to_state_bytes().expect("serialize"),
                restored.to_state_bytes().expect("serialize"),
                "machine state diverged after APU restore at frame {frame}"
            );
        }

        let orig = orig_buf.lock().unwrap();
        let redo = redo_buf.lock().unwrap();
        assert!(!orig.is_empty(), "no audio was generated");
        // The mix must be non-trivial, or an all-silence stream would match
        // itself and prove nothing.
        assert!(
            orig.iter().any(|&(l, _)| l != orig[0].0),
            "captured audio is a constant level; the channels were not live"
        );
        assert_eq!(orig.len(), redo.len(), "restored machine emitted a different sample count");
        assert_eq!(*orig, *redo, "restored APU produced a different audio stream");
    }

    /// Analog continuity across a load. The output high-pass and the DAC-off
    /// fade are continuous RC state; if they restored to a default the filter
    /// would restart from zero and ring out a step transient on every load —
    /// and RetroArch's rewind unserializes once per frame, so that transient
    /// would land on every rewound frame.
    ///
    /// Asserted two ways: the restored machine's samples must equal the live
    /// machine's continuation bit-for-bit, and the single step ACROSS the
    /// save/load seam must be no larger than the tone's own steady-state
    /// sample-to-sample step (a restarted filter shows up as a jump there).
    #[test]
    fn savestate_is_analog_continuous_across_a_load() {
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct Cap(Arc<Mutex<Vec<(f32, f32)>>>);
        impl audio::AudioOutput for Cap {
            fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
                Ok(())
            }
            fn add_samples(&mut self, s: &[(f32, f32)]) {
                self.0.lock().unwrap().extend_from_slice(s);
            }
        }

        // A steady tone: one square channel, flat envelope, no sweep, no
        // length. The high-pass settles to a periodic steady state, which is
        // what makes a restart transient visible.
        let rom = vec![0u8; 0x8000];
        let mut gb = GB::new(Hardware::DMG);
        gb.insert(cartridge::Cartridge::from_bytes(&rom).unwrap());
        gb.skip_bios();
        let buf = Arc::new(Mutex::new(Vec::new()));
        gb.enable_audio(Box::new(Cap(buf.clone()))).unwrap();
        gb.write_memory(0xFF26, 0x80); // NR52: APU on
        gb.write_memory(0xFF25, 0x22); // NR51: channel 2 to both sides
        gb.write_memory(0xFF24, 0x77); // NR50: full volume
        gb.write_memory(0xFF16, 0x80); // NR21: duty 2
        gb.write_memory(0xFF17, 0xF0); // NR22: vol 15, no envelope stepping
        gb.write_memory(0xFF18, 0x00); // NR23: freq low
        gb.write_memory(0xFF19, 0x86); // NR24: trigger, length disabled

        // Settle the high-pass well past its time constant.
        for _ in 0..30 {
            gb.run_until_frame(true);
        }

        // The steady-state step size, measured on the settled tone.
        buf.lock().unwrap().clear();
        gb.run_until_frame(true);
        let before = buf.lock().unwrap().clone();
        assert!(before.len() > 64, "too few samples to characterise the tone");
        let max_step = before
            .windows(2)
            .map(|w| (w[1].0 - w[0].0).abs())
            .fold(0.0f32, f32::max);
        assert!(max_step > 0.0, "the tone is a constant; nothing to compare");
        let seam_from = before.last().unwrap().0;

        // Save at this instant, then advance the live machine and a restored
        // copy over the same span.
        let state = gb.to_state_bytes().expect("serialize");
        let mut restored = restore_with_rom(&state, &gb);
        let redo_buf = Arc::new(Mutex::new(Vec::new()));
        restored.enable_audio(Box::new(Cap(redo_buf.clone()))).unwrap();
        buf.lock().unwrap().clear();
        gb.run_until_frame(true);
        restored.run_until_frame(true);
        let after_live = buf.lock().unwrap().clone();
        let after_restored = redo_buf.lock().unwrap().clone();

        assert_eq!(
            after_live, after_restored,
            "restored machine's analog output diverged from the live continuation"
        );

        // The seam itself: no step larger than the tone's own steady-state step.
        let seam_step = (after_restored[0].0 - seam_from).abs();
        assert!(
            seam_step <= max_step,
            "analog discontinuity across the load: seam step {seam_step} exceeds \
             the tone's steady-state step {max_step} (the filter restarted)"
        );
    }

    /// The hardware-revision flags (`#[serde(skip)]` on the timer/APU sub-structs)
    /// must be re-derived on load from the serialized `hardware` identity, not
    /// silently revert to default-CGB. A round-tripped AGB must still report AGB.
    #[test]
    fn savestate_reseeds_hardware_flags() {
        let Some(mut gb) = gb_from_rom("../gb-test-roms/cgb-acid2/cgb-acid2.gbc", Hardware::AGB)
        else {
            eprintln!("skipping: cgb-acid2 not present");
            return;
        };
        let state = gb.to_state_bytes().expect("serialize");
        let restored = restore_with_rom(&state, &gb);
        assert!(
            restored.mmio.is_agb(),
            "AGB hardware flag lost across savestate load"
        );
    }

    /// A serializable machine with no ROM dependency, so the container test
    /// below runs everywhere (the suite ROMs are optional).
    fn container_test_machine() -> GB {
        let mut gb = GB::new(Hardware::DMG);
        gb.insert(cartridge::Cartridge::from_bytes(&vec![0u8; 0x8000]).unwrap());
        gb.skip_bios();
        for _ in 0..2 {
            gb.run_until_frame(false);
        }
        gb
    }

    /// The savestate container is bare bincode — no magic, no version, nothing
    /// to validate (deliberate while pre-release). What must still hold is that
    /// a round-trip is byte-identical and that malformed input never panics:
    /// `from_state_bytes` is reachable from untrusted files, the web drop
    /// handler and libretro's `retro_unserialize`, so a panic there is a crash,
    /// not a rejected load. A wrong-layout buffer that bincode happens to accept
    /// yields a wrong machine and no error — that is the accepted trade here.
    #[test]
    fn savestate_round_trips_and_never_panics_on_malformed_input() {
        let mut gb = container_test_machine();
        let state = gb.to_state_bytes().expect("serialize");

        let mut restored = GB::from_state_bytes(&state).expect("round-trip");
        assert_eq!(
            restored.to_state_bytes().expect("re-serialize"),
            state,
            "round-trip is not byte-identical"
        );

        for len in 0..64.min(state.len()) {
            let _ = GB::from_state_bytes(&state[..len]);
        }
        for i in 0..32.min(state.len()) {
            let mut bad = state.clone();
            bad[i] ^= 0xFF;
            let _ = GB::from_state_bytes(&bad);
        }
        for junk in [&b""[..], &b"RBST"[..], &b"not a savestate at all"[..]] {
            assert!(GB::from_state_bytes(junk).is_err(), "{junk:?} accepted");
        }
    }


    /// Regression: the DMG noise channel (channel 4) must keep advancing its
    /// LFSR while it plays. The per-dot APU step-skip optimization
    /// (`Mmio::step_audio` returns early on dots where the APU clock is
    /// unmoved) must never freeze channel 4 — a frozen LFSR latches
    /// `current_sample`, collapsing the output to a constant "buzz". We drive a
    /// steady-volume noise tone (no envelope decay, length disabled) and assert
    /// the captured sample stream genuinely flips over time: the LFSR output bit
    /// toggles, so both a nonzero and a zero level appear, with many
    /// transitions. A latched channel would emit one constant level and fail.
    #[test]
    fn dmg_noise_channel_lfsr_keeps_advancing() {
        use crate::audio::AudioOutput;
        use std::sync::{Arc, Mutex};

        struct Cap(Arc<Mutex<Vec<(f32, f32)>>>);
        impl AudioOutput for Cap {
            fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
                Ok(())
            }
            fn add_samples(&mut self, s: &[(f32, f32)]) {
                self.0.lock().unwrap().extend_from_slice(s);
            }
        }

        // Minimal 32KB NoMBC ROM (all NOPs); skip the boot ROM.
        let rom = vec![0u8; 0x8000];
        let mut gb = GB::new(Hardware::DMG);
        gb.insert(cartridge::Cartridge::from_bytes(&rom).unwrap());
        gb.skip_bios();
        let buf = Arc::new(Mutex::new(Vec::new()));
        gb.enable_audio(Box::new(Cap(buf.clone()))).unwrap();

        // Program a sustained noise tone on channel 4 only.
        gb.write_memory(0xFF26, 0x80); // NR52: APU on
        gb.write_memory(0xFF25, 0x88); // NR51: route only channel 4 to L+R
        gb.write_memory(0xFF24, 0x77); // NR50: full volume both sides
        gb.write_memory(0xFF20, 0x00); // NR41: length load (unused, length off)
        gb.write_memory(0xFF21, 0xF0); // NR42: volume 15, no envelope stepping
        gb.write_memory(0xFF22, 0x37); // NR43: mid divisor/shift -> audible noise
        gb.write_memory(0xFF23, 0x80); // NR44: trigger, length disabled

        // Run a handful of frames so the LFSR has stepped many times.
        for _ in 0..8 {
            gb.run_until_frame(true);
        }

        let samples = buf.lock().unwrap();
        assert!(!samples.is_empty(), "no audio was generated");
        // Channel 4's DAC is on and volume is 15, so the enabled channel sits at
        // one of two distinct analog levels depending on the LFSR output bit. A
        // live (advancing) LFSR keeps flipping between them; a frozen/latched
        // one holds a single level, which the output high-pass then pulls to 0.
        //
        // Which two levels those are is the DAC convention's business, not this
        // test's: assert only that there ARE two, well separated, and that the
        // stream keeps crossing between them.
        let lo = samples.iter().map(|&(l, _)| l).fold(f32::INFINITY, f32::min);
        let hi = samples
            .iter()
            .map(|&(l, _)| l)
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            hi - lo > 0.05,
            "channel 4 output spans only {:.5} -> LFSR is latched (never advances), \
             or the channel is dead entirely",
            hi - lo
        );

        // Stronger check: the stream must keep crossing between the two levels,
        // not settle once. Count midpoint crossings across the capture.
        let mid = 0.5 * (lo + hi);
        let transitions = samples
            .windows(2)
            .filter(|w| (w[0].0 > mid) != (w[1].0 > mid))
            .count();
        assert!(
            transitions > 100,
            "channel 4 output barely changes ({transitions} transitions) -> \
             LFSR is not advancing per-dot as it should"
        );
    }

    /// Regression for the Pokémon R/B/Y GameFreak-intro drumroll latch on the
    /// non-CGB (DMG/SGB) APU path. The DMG-only `dmg_delayed_start` deferral
    /// re-applies the NR44 trigger 6 cycles later; that re-application is the
    /// trigger *taking effect* and must actually start the channel. The bug had
    /// it re-arm another deferral whenever `alignment & 3` was odd — and a fixed
    /// 6-cycle step only flips bit 1, so an odd alignment stays odd forever. A
    /// game that re-triggers ch4 every frame at such an alignment then re-defers
    /// every 6 cc, clearing `env_clock` each time, so the envelope volume never
    /// steps: instead of a decaying drum hit the channel latches into continuous
    /// noise (the "repeating drumroll" the user heard on DMG but not CGB).
    ///
    /// The exact latch depends on the game's CPU-driven write alignment, which a
    /// synthetic register-poke harness does not reproduce, so this drives the
    /// real ROM. It runs the ~9-16 s intro on DMG and requires the noise output
    /// to fall silent between drum hits (rhythmic), matching hardware/CGB. On the
    /// bug the noise floor never drops. Skipped when the ROM is unavailable
    /// (set `POKEMON_BLUE_ROM` to its path; falls back to a repo-relative path).
    #[test]
    fn dmg_pokemon_intro_drumroll_is_rhythmic_not_latched() {
        use crate::audio::AudioOutput;
        use std::sync::{Arc, Mutex};

        struct Cap(Arc<Mutex<Vec<(f32, f32)>>>);
        impl AudioOutput for Cap {
            fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
                Ok(())
            }
            fn add_samples(&mut self, s: &[(f32, f32)]) {
                self.0.lock().unwrap().extend_from_slice(s);
            }
        }

        let path = std::env::var("POKEMON_BLUE_ROM").unwrap_or_else(|_| {
            "../roms/pokemon-blue.gb".to_string()
        });
        let Ok(rom) = fs::read(&path) else {
            eprintln!("skipping dmg_pokemon_intro_drumroll: ROM not present ({path})");
            return;
        };

        let mut gb = GB::new(Hardware::DMG);
        gb.insert(cartridge::Cartridge::from_bytes(&rom).expect("load ROM"));
        gb.skip_bios();
        let buf = Arc::new(Mutex::new(Vec::new()));
        gb.enable_audio(Box::new(Cap(buf.clone()))).unwrap();

        // Run through the intro; the diagnostic drumroll section (percussion
        // with clear inter-hit silence gaps) sits around 20.5-24 s.
        for _ in 0..1600 {
            gb.run_until_frame(true);
        }

        // Over 100 ms RMS windows in that section, a rhythmic drumroll produces
        // both loud hits AND several near-silent gaps between them. The latch bug
        // fills the gaps with a continuous noise floor, so silent windows
        // collapse to zero. Require both a loud hit and multiple silent gaps.
        //
        // The measured distribution over this window is sharply trimodal, with
        // two wide gaps to put the thresholds in: true silence at or below
        // 0.004 (most windows land on the high-pass's ~7e-6 residual, i.e. the
        // analog stage really does settle to zero rather than to a DC offset),
        // decay/attack tails from 0.017 to 0.092, and hits from 0.14 up to 0.32.
        // 0.01 and 0.12 sit in the middle of those gaps. Of 35 windows, 10 come
        // out silent and 19 loud, so the required counts keep ~2x margin.
        let samples = buf.lock().unwrap();
        assert!(!samples.is_empty(), "no audio generated");
        let win = 44100 / 10; // 100 ms
        let start = (44100.0 * 20.5) as usize;
        let end = ((44100.0 * 24.0) as usize).min(samples.len());
        assert!(start + win <= end, "capture too short for the drumroll window");
        let mut silent = 0usize;
        let mut loud = 0usize;
        let mut i = start;
        while i + win <= end {
            let rms = (samples[i..i + win]
                .iter()
                .map(|&(l, _)| l * l)
                .sum::<f32>()
                / win as f32)
                .sqrt();
            if rms < 0.01 {
                silent += 1;
            }
            if rms > 0.12 {
                loud += 1;
            }
            i += win;
        }
        assert!(loud > 8, "drumroll never played (loud windows = {loud})");
        assert!(
            silent >= 6,
            "noise channel did not fall silent between drum hits \
             (silent 100 ms windows = {silent}) -> ch4 latched into a continuous \
             buzz on the non-CGB path (Pokémon intro drumroll)"
        );
    }
}

#[cfg(test)]
mod apu_boot_semantics_tests {
    //! Boot-path APU gating: `boot_cgb` must hold from construction (a
    //! real-BIOS CGB session never calls `skip_bios`, so `GB::new` is the only
    //! place that can seed it), and `skip_bios` must hand off the hardware
    //! power-on length counters (0 for CH2/CH3/CH4 — the boot ROMs never write
    //! NR21/NR31/NR41 — and 64 for CH1 from the boot ROM's NR11=0x80 write).
    use super::*;
    use crate::audio::{NR14, NR22, NR24, NR30, NR34, NR41, NR42, NR43, NR44, NR52};

    /// Blank 32KB NoMBC ROM (all NOPs).
    fn nop_rom_gb(hardware: Hardware) -> GB {
        let mut gb = GB::new(hardware);
        gb.insert(cartridge::Cartridge::from_bytes(&vec![0u8; 0x8000]).unwrap());
        gb
    }

    /// One length tick every 16384 T-cycles (256 Hz) in single speed.
    const LENGTH_TICK_T: u64 = 16384;

    /// Drive at least `budget` T-cycles of machine time through the CPU.
    fn step_t_cycles(gb: &mut GB, mut budget: u64) {
        while budget > 0 {
            let (_, cycles) = gb.step_instruction(false);
            budget = budget.saturating_sub(cycles.max(4) as u64);
        }
    }

    fn nr52(gb: &mut GB) -> u8 {
        gb.sync_lazy_peripherals();
        gb.read_memory(NR52)
    }

    /// A CGB constructed for a real-BIOS run (NO `skip_bios`) must reject NRx1
    /// writes while the APU is off: the accept-while-off length-load exception
    /// is DMG-only, gated on `boot_cgb`, which only `GB::new` can seed on this
    /// path. With the write rejected, CH4's length counter stays at the
    /// power-on 0, so a trigger with length enabled reloads it to 64 and the
    /// channel survives well past one length tick. An accepted write
    /// (NR41=0x3F -> counter 1) kills the channel at the first tick.
    #[test]
    fn cgb_rejects_nrx1_while_apu_off_without_skip_bios() {
        let mut gb = nop_rom_gb(Hardware::CGB);

        assert_eq!(
            nr52(&mut gb) & 0x80,
            0x00,
            "APU must be off at power-on (before any boot ROM ran)"
        );

        // Write NR41 while the APU is off. DMG accepts this as a length load
        // (counter 1); CGB must drop it.
        gb.write_memory(NR41, 0x3F);

        // Power the APU on and trigger CH4 with length DISABLED, without
        // rewriting NR41: a power-on counter of 0 reloads to the full 64 on
        // trigger; an accepted write-while-off (counter 1) is kept as-is.
        // Splitting trigger and length-enable into two writes keeps the
        // trigger+enable extra-length-clock reload quirk out of the picture,
        // so the outcome is length-period-phase independent.
        gb.write_memory(NR52, 0x80);
        gb.write_memory(NR42, 0xF0); // DAC on, no envelope
        gb.write_memory(NR43, 0x00);
        gb.write_memory(NR44, 0x80); // trigger, length disabled

        // (covers the DMG-path 6-cycle deferred noise trigger)
        step_t_cycles(&mut gb, 2 * LENGTH_TICK_T);
        assert_ne!(nr52(&mut gb) & 0x08, 0, "CH4 did not start on trigger");

        // Now enable the length counter without retriggering.
        gb.write_memory(NR44, 0x40);

        // ~3 length periods: a counter of 1 is dead within one tick in any
        // phase; the 64 the trigger reloaded from 0 is not.
        step_t_cycles(&mut gb, 3 * LENGTH_TICK_T);
        assert_ne!(
            nr52(&mut gb) & 0x08,
            0,
            "CH4 died within 3 length ticks of enabling length -> the NR41 \
             write while the APU was off was accepted on CGB (DMG-only \
             exception leaked: boot_cgb was not seeded at construction)"
        );

        // And the counter really was reloaded to 64 by the trigger, not left
        // free-running: the channel must expire by ~64 ticks.
        step_t_cycles(&mut gb, 70 * LENGTH_TICK_T);
        assert_eq!(
            nr52(&mut gb) & 0x08,
            0,
            "CH4 never expired -> length counter was not reloaded to 64"
        );
    }

    /// After `skip_bios`, the hidden length counters must be the hardware
    /// post-boot values: 0 for CH2/CH3/CH4 (so a trigger with length enabled
    /// reloads to 64/256/64) and 64 for CH1. The old boot table's NR21/NR31/
    /// NR41 writes leaked DMG length loads of 1, killing CH3/CH4 one tick
    /// after any length-enabled trigger that did not rewrite NRx1.
    #[test]
    fn skip_bios_seeds_power_on_length_counters() {
        for hardware in [Hardware::DMG, Hardware::CGB] {
            let mut gb = nop_rom_gb(hardware);
            gb.skip_bios();

            assert_ne!(nr52(&mut gb) & 0x80, 0, "{hardware:?}: APU off after skip_bios");

            // Trigger all four channels with length DISABLED and NO NRx1
            // writes (NRx2/NR30 only turn the DACs on): a power-on counter of
            // 0 reloads to the full max on trigger (64/256/64); a leaked
            // length load of 1 is kept as-is. Trigger and length-enable are
            // separate writes so the trigger+enable extra-length-clock reload
            // quirk cannot rescue a counter of 1 (phase-independent outcome).
            gb.write_memory(NR22, 0xF0);
            gb.write_memory(NR24, 0x80);
            gb.write_memory(NR30, 0x80);
            gb.write_memory(NR34, 0x80);
            gb.write_memory(NR42, 0xF0);
            gb.write_memory(NR44, 0x80);
            gb.write_memory(NR14, 0x80); // CH1: retrigger, counter carries ~64

            // (covers the DMG-path 6-cycle deferred noise trigger)
            step_t_cycles(&mut gb, 2 * LENGTH_TICK_T);
            assert_eq!(
                nr52(&mut gb) & 0x0F,
                0x0F,
                "{hardware:?}: not all channels started on trigger"
            );

            // Enable every length counter without retriggering.
            gb.write_memory(NR14, 0x40);
            gb.write_memory(NR24, 0x40);
            gb.write_memory(NR34, 0x40);
            gb.write_memory(NR44, 0x40);

            // ~3 length ticks: a leaked counter of 1 is dead within one tick
            // in any phase; the max-reloads (and CH1's carried 64) are not.
            step_t_cycles(&mut gb, 3 * LENGTH_TICK_T);
            let alive = nr52(&mut gb) & 0x0F;
            assert_eq!(
                alive,
                0x0F,
                "{hardware:?}: channel(s) died within 3 length ticks of \
                 enabling length (NR52 low nibble {alive:#06b}) -> skip_bios \
                 seeded garbage length counters (leaked DMG length loads of 1)"
            );

            // By ~73 ticks the 64-count channels (CH1/CH2/CH4) have expired;
            // CH3's 256-reload keeps it alive.
            step_t_cycles(&mut gb, 70 * LENGTH_TICK_T);
            let alive = nr52(&mut gb) & 0x0F;
            assert_eq!(
                alive,
                0x04,
                "{hardware:?}: after ~73 length ticks expected only CH3 alive \
                 (NR52 low nibble {alive:#06b}): CH1/CH2/CH4 carry 64, CH3 256"
            );

            // And CH3 expires by ~269 ticks (256-reload, not free-running).
            step_t_cycles(&mut gb, 196 * LENGTH_TICK_T);
            let alive = nr52(&mut gb) & 0x0F;
            assert_eq!(
                alive, 0,
                "{hardware:?}: CH3 never expired -> counter was not reloaded to 256"
            );
        }
    }
}

#[cfg(test)]
mod forced_compat_palette_tests {
    //! The user-selectable CGB DMG-compatibility palette override
    //! (`set_forced_compat_palette`) must actually recolour a DMG game running
    //! on CGB hardware, and `None` must leave the boot ROM's automatic pick
    //! untouched.
    use super::*;

    /// A blank 32KB DMG-only cart (header CGB flag 0x00), so it boots on CGB in
    /// DMG-compatibility mode and the boot ROM colorizes it.
    fn dmg_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x143] = 0x00;
        rom
    }

    /// The installed CGB BG palette 0 (all four RGB555 colours) after boot. A
    /// blank screen shows only colour 0 (white for every scheme), so we compare
    /// the whole palette — colours 1-3 are what distinguish the schemes.
    fn cgb_bg_palette(forced: Option<u8>) -> [u16; 4] {
        let mut gb = GB::new(Hardware::CGB);
        gb.insert(cartridge::Cartridge::from_bytes(&dmg_rom()).unwrap());
        gb.set_forced_compat_palette(forced);
        gb.skip_bios();
        // `bg_palette_pair` reads the raw palette RAM (the DMG-compat mode leaves
        // `cgb_features_enabled` off, which would gate the FF69 read path).
        [
            gb.bg_palette_pair(0, 0),
            gb.bg_palette_pair(0, 1),
            gb.bg_palette_pair(0, 2),
            gb.bg_palette_pair(0, 3),
        ]
    }

    #[test]
    fn forced_scheme_recolours_dmg_on_cgb() {
        let auto = cgb_bg_palette(None);
        // An unknown (non-Nintendo) title's auto pick is the default id 0x7C, so
        // forcing that id is a no-op...
        assert_eq!(cgb_bg_palette(Some(0x7C)), auto, "forcing the auto id changes nothing");
        // ...while forcing the "Up" scheme (id 0x12) installs a different palette.
        assert_ne!(cgb_bg_palette(Some(0x12)), auto, "forcing a different scheme must recolour");
    }
}

#[cfg(test)]
mod sgb_palette_tests {
    //! A DMG game on SGB hardware must come out colourized, not grey: a real
    //! Super Game Boy powers on with system palette `1-A` and swaps in a
    //! recognized game's own palette. These are presentation-only checks — the
    //! frame stays `RenderedFrame::Monochrome`, so the suite/TAS grading domain
    //! (`presented_shade_frame`) is untouched.
    use super::*;

    /// A blank 32KB mono cart. `title`/`old_licensee` go into the header so the
    /// firmware's title recognition has something to match.
    fn rom(title: &[u8], old_licensee: u8) -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x134..0x134 + title.len()].copy_from_slice(title);
        rom[0x14B] = old_licensee;
        rom
    }

    fn gb(hardware: Hardware, title: &[u8], old_licensee: u8) -> GB {
        let mut gb = GB::new(hardware);
        gb.insert(cartridge::Cartridge::from_bytes(&rom(title, old_licensee)).unwrap());
        gb.skip_bios();
        gb
    }

    /// The RGB triple the presented frame uses for DMG shade 0 (the whole
    /// screen on a blank cart), i.e. colour 0 of whatever palette applied.
    fn presented_shade0(gb: &mut GB) -> [u8; 3] {
        let frame = gb.presented_frame();
        [frame.0[0], frame.0[1], frame.0[2]]
    }

    fn color(index: u8, slot: usize) -> [u8; 3] {
        let word = sgb_system_palette::SGB_SYSTEM_PALETTES[index as usize][slot];
        let (r, g, b) = ppu::controller::rgb555_to_rgb888(word);
        [r, g, b]
    }

    /// Out of the box, an unrecognized cart on an SGB presents system palette
    /// 1-A — not the grey ramp a DMG would show.
    #[test]
    fn sgb_default_presents_1a() {
        for hw in [Hardware::SGB, Hardware::SGB2] {
            let mut gb = gb(hw, b"HOMEBREW", 0x00);
            assert_eq!(gb.sgb_palette(), SgbPaletteChoice::Auto);
            assert_eq!(presented_shade0(&mut gb), color(0, 0), "{hw:?} must present 1-A");
            assert_ne!(presented_shade0(&mut gb), [255, 255, 255], "{hw:?} must not be grey");
        }
    }

    /// A recognized Nintendo title gets its own palette, and a DMG runs the
    /// same cart in mono — the colourization is SGB-only.
    #[test]
    fn recognized_title_and_non_sgb_models() {
        let mut sgb = gb(Hardware::SGB, b"TETRIS", 0x01);
        assert_eq!(presented_shade0(&mut sgb), color(17, 0)); // 3-B
        assert_ne!(presented_shade0(&mut sgb), color(0, 0));

        let mut dmg = gb(Hardware::DMG, b"TETRIS", 0x01);
        assert_eq!(
            presented_shade0(&mut dmg),
            DmgPaletteChoice::default_for(Hardware::DMG)
                .shades(dmg.ppu.cgb_color_conversion())[0]
        );
    }

    /// The explicit overrides: a forced system palette wins over Auto, and
    /// Grayscale opts back out to the mono ramp.
    #[test]
    fn forced_and_grayscale_overrides() {
        let mut gb = gb(Hardware::SGB, b"TETRIS", 0x01);
        gb.set_sgb_palette(SgbPaletteChoice::System(31));
        assert_eq!(presented_shade0(&mut gb), color(31, 0)); // 4-H

        gb.set_sgb_palette(SgbPaletteChoice::Grayscale);
        assert_eq!(gb.sgb_presentation_shades(), None);
        assert_eq!(
            presented_shade0(&mut gb),
            DmgPaletteChoice::default_for(Hardware::SGB).shades(gb.ppu.cgb_color_conversion())[0]
        );
    }

    /// The SGB has no LCD panel, so its palettes are always the raw linear
    /// colours: `ColorCorrection` must not touch the SGB presentation path.
    #[test]
    fn color_correction_does_not_affect_sgb_palettes() {
        let mut gb = gb(Hardware::SGB, b"ZELDA", 0x01);
        gb.set_cgb_color_conversion(ppu::ColorCorrection::Linear);
        let linear = presented_shade0(&mut gb);
        gb.set_cgb_color_conversion(ppu::ColorCorrection::Lcd);
        assert_eq!(presented_shade0(&mut gb), linear);
        assert_eq!(linear, color(5, 0)); // 1-F
    }

    /// The selector's plumbing surface (labels/ids/round-trip), mirroring the
    /// `DmgPaletteChoice` contract the frontends consume.
    #[test]
    fn choice_ids_round_trip() {
        assert_eq!(SgbPaletteChoice::ALL.len(), 34);
        assert_eq!(SgbPaletteChoice::ALL[0], SgbPaletteChoice::Auto);
        assert_eq!(SgbPaletteChoice::ALL[1], SgbPaletteChoice::System(0));
        assert_eq!(SgbPaletteChoice::ALL[32], SgbPaletteChoice::System(31));
        assert_eq!(SgbPaletteChoice::ALL[33], SgbPaletteChoice::Grayscale);
        for choice in SgbPaletteChoice::ALL {
            assert_eq!(SgbPaletteChoice::from_option_id(choice.option_id()), Some(choice));
            assert!(!choice.label().is_empty());
        }
        assert_eq!(SgbPaletteChoice::System(0).label(), "1-A");
        assert_eq!(SgbPaletteChoice::System(31).option_id(), "4h");
        assert_eq!(SgbPaletteChoice::from_option_id("2H"), Some(SgbPaletteChoice::System(15)));
        assert_eq!(SgbPaletteChoice::from_option_id("nope"), None);
    }

    /// The presentation choice must not leak into the savestate or the mono
    /// grading domain (`#[serde(skip)]`), so suites/TAS/rewind stay identical.
    #[test]
    fn palette_is_not_machine_state() {
        let mut a = gb(Hardware::SGB, b"TETRIS", 0x01);
        let mut b = gb(Hardware::SGB, b"TETRIS", 0x01);
        b.set_sgb_palette(SgbPaletteChoice::System(9));
        a.run_until_frame(false);
        b.run_until_frame(false);
        assert_eq!(a.presented_shade_frame(), b.presented_shade_frame());
        assert!(!a.frame_renders_color(), "non-aware SGB frames must stay mono-graded");
        assert_eq!(
            bincode::serialize(&a).unwrap(),
            bincode::serialize(&b).unwrap(),
            "sgb_palette must stay out of the savestate"
        );
    }
}

#[cfg(test)]
mod clock_tests {
    //! The SGB1's SNES-derived clock.
    //!
    //! **Why these are Rust tests and not a first-party test ROM.** A Game Boy
    //! cannot observe its own crystal: DIV, the timer, LY, and serial all
    //! divide down from the *same* clock, so every ratio a ROM can measure is
    //! identical on an SGB1 and a DMG. This change deliberately leaves the dot
    //! timeline untouched (a frame is 70224 dots on every model), so a test ROM
    //! would produce byte-identical output before and after it — a
    //! non-discriminating oracle. What actually changed is the *real-time*
    //! mapping: how many wall-clock seconds those dots take and what pitch the
    //! host DAC plays them at. That is host-side and only assertable here.
    use super::*;

    /// Every model runs at the DMG's crystal rate except the SGB1, which has
    /// none and divides the host SNES's master clock by 5.
    #[test]
    fn cpu_hz_per_model_and_region() {
        // NTSC SNES 21.477270 MHz / 5; PAL SNES 21.281370 MHz / 5.
        assert_eq!(Hardware::SGB.cpu_hz(Region::Ntsc), 4_295_454);
        assert_eq!(Hardware::SGB.cpu_hz(Region::Pal), 4_256_274);

        // The SGB2 gained its own crystal precisely to fix the SGB1's drift, so
        // it is DMG-rate and region-independent — the whole point of the model.
        for region in [Region::Ntsc, Region::Pal] {
            assert_eq!(Hardware::SGB2.cpu_hz(region), DMG_CPU_HZ, "SGB2 {region:?}");
            for hw in [
                Hardware::DMG,
                Hardware::DMG0,
                Hardware::MGB,
                Hardware::CGB0,
                Hardware::CGBB,
                Hardware::CGB,
                Hardware::CGBE,
                Hardware::AGB,
            ] {
                assert_eq!(hw.cpu_hz(region), DMG_CPU_HZ, "{hw:?} {region:?}");
            }
        }
    }

    /// The documented speedups, from the ratio rather than the raw figures.
    #[test]
    fn sgb1_runs_fast_by_the_documented_margins() {
        let pct = |hz: u32| (f64::from(hz) / f64::from(DMG_CPU_HZ) - 1.0) * 100.0;
        let ntsc = pct(Hardware::SGB.cpu_hz(Region::Ntsc));
        let pal = pct(Hardware::SGB.cpu_hz(Region::Pal));
        assert!((ntsc - 2.4).abs() < 0.05, "NTSC SGB1 is {ntsc:.2}% fast, expected ~2.4%");
        assert!((pal - 1.5).abs() < 0.05, "PAL SGB1 is {pal:.2}% fast, expected ~1.5%");
        // NTSC must be the faster of the two, and the SGB2 must not move at all.
        assert!(ntsc > pal);
        assert_eq!(pct(Hardware::SGB2.cpu_hz(Region::Ntsc)), 0.0);
    }

    /// A fixed number of dots must yield FEWER host samples on an SGB1 — that
    /// deficit is exactly what pitches its audio up and what the frame cadence
    /// must compensate for. Measured through the public sample path, not the
    /// stored ratio, so it pins observable behaviour.
    fn samples_over_one_frame(hw: Hardware, region: Region) -> usize {
        let mut gb = GB::new(hw);
        gb.set_region(region);
        // One frame of dots, fed in one go: the dot count is model-independent.
        gb.mmio.generate_audio_samples(70_224).len()
    }

    #[test]
    fn a_frame_of_dots_yields_fewer_samples_on_an_sgb1() {
        let dmg = samples_over_one_frame(Hardware::DMG, Region::Ntsc);
        let sgb_ntsc = samples_over_one_frame(Hardware::SGB, Region::Ntsc);
        let sgb_pal = samples_over_one_frame(Hardware::SGB, Region::Pal);

        // 70224 dots / (4194304/44100) = 738.4 pairs at DMG rate.
        assert_eq!(dmg, 738);
        assert!(sgb_ntsc < dmg, "NTSC SGB1 {sgb_ntsc} should be < DMG {dmg}");
        assert!(sgb_pal < dmg, "PAL SGB1 {sgb_pal} should be < DMG {dmg}");
        assert!(sgb_ntsc < sgb_pal, "NTSC SGB1 is the faster clock");

        // Every other model is region-independent and DMG-rate — the SGB2's
        // own crystal is the entire reason it exists.
        for hw in [Hardware::DMG, Hardware::SGB2, Hardware::CGB, Hardware::AGB] {
            assert_eq!(samples_over_one_frame(hw, Region::Ntsc), dmg, "{hw:?} NTSC");
            assert_eq!(samples_over_one_frame(hw, Region::Pal), dmg, "{hw:?} PAL");
        }
    }

    /// `GB::new` defaults to NTSC and `set_region` round-trips.
    #[test]
    fn region_defaults_to_ntsc_and_round_trips() {
        let mut gb = GB::new(Hardware::SGB);
        assert_eq!(gb.region(), Region::Ntsc);
        assert_eq!(gb.cpu_hz(), 4_295_454);
        gb.set_region(Region::Pal);
        assert_eq!(gb.region(), Region::Pal);
        assert_eq!(gb.cpu_hz(), 4_256_274);
    }

    /// **THE regression guard.** The clock is a real-time mapping and NOTHING
    /// else: two machines differing only in region must remain byte-identical
    /// in the dot domain forever. This is what keeps all 28 suites, TAS
    /// replays, and savestates model-independent — if a future change lets
    /// `cpu_hz` leak into dot-domain timing (DIV, LY, serial, the frame cap),
    /// this diverges immediately.
    #[test]
    fn region_never_touches_the_dot_timeline() {
        let run = |region: Region| {
            let mut gb = GB::new(Hardware::SGB);
            gb.set_region(region);
            gb.skip_bios();
            for _ in 0..4 {
                gb.run_until_frame(false);
            }
            gb.to_state_bytes().expect("savestate")
        };
        assert_eq!(
            run(Region::Ntsc),
            run(Region::Pal),
            "region changed emulated machine state — the dot clock must be fixed"
        );

        // The serial link stall timeout is denominated in dots (4 frames), not
        // seconds, so it too is model-independent.
        assert_eq!(crate::serial::LINK_STALL_TIMEOUT_CC, 4 * 70224);
    }
}

#[cfg(test)]
mod sgb_default_border_tests {
    //! The SGB's own power-on border, decompressed at runtime from the user's
    //! SNES-side firmware dump (see [`crate::sgb_firmware`]). A real Super
    //! Game Boy shows this until the running game replaces it with CHR_TRN +
    //! PCT_TRN.
    //!
    //! No artwork is embedded in the repo, so the firmware-backed tests are
    //! skip-if-absent (mirroring `cgb_compat_palette::tables_match_cgb_boot_bin`)
    //! and pin only shapes and a hash, never bytes.
    use super::*;
    use crate::sgb_firmware;

    /// `[sgb1, sgb2]` paired with the model each drives, or empty when the
    /// user has no dumps.
    fn firmwares() -> Vec<(Vec<u8>, Hardware)> {
        sgb_firmware::firmware_test::dumps()
            .into_iter()
            .zip([Hardware::SGB, Hardware::SGB2])
            .collect()
    }

    fn machine(hardware: Hardware) -> GB {
        let mut gb = GB::new(hardware);
        gb.insert(cartridge::Cartridge::from_bytes(&vec![0u8; 0x8000]).unwrap());
        gb.skip_bios();
        gb
    }

    /// FNV-1a 64. Only used to pin decoded output without storing any of it.
    fn digest(border: &sgb_firmware::SgbBorder) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let bytes = border
            .tiles
            .iter()
            .copied()
            .chain(border.map.iter().copied())
            .chain(border.pals.iter().flat_map(|w| w.to_le_bytes()));
        for b in bytes {
            h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01B3);
        }
        h
    }

    /// Without firmware an SGB has no border and frontends fall back to the
    /// plain 160x144 frame — today's behaviour, unchanged.
    #[test]
    fn no_firmware_means_no_border() {
        for hw in [Hardware::SGB, Hardware::SGB2] {
            let gb = machine(hw);
            assert!(!gb.has_sgb_firmware());
            assert!(gb.sgb().expect("SGB receiver").border().is_none());
            assert!(gb.sgb_composited_frame().is_none(), "{hw:?}");
        }
    }

    /// Loading the firmware seeds the system border, in the exact shape
    /// `Sgb::border()`'s size gate wants: the 3488-byte (SGB1) / 4096-byte
    /// (SGB2) tileset zero-padded to 0x2000, the full 0x800 tilemap, and all
    /// eight SNES BG palettes.
    #[test]
    fn firmware_seeds_the_system_border() {
        for (rom, hw) in firmwares() {
            let mut gb = machine(hw);
            gb.load_sgb_firmware_bytes(&rom)
                .unwrap_or_else(|e| panic!("{hw:?} firmware rejected: {e}"));
            assert!(gb.has_sgb_firmware());

            let (tiles, map, pals) = gb.sgb().unwrap().border().expect("system border");
            assert_eq!(tiles.len(), sgb_firmware::BORDER_TILES_LEN, "{hw:?}");
            assert_eq!(map.len(), sgb_firmware::BORDER_MAP_LEN, "{hw:?}");
            assert_eq!(pals.len(), sgb_firmware::BORDER_PAL_COLORS, "{hw:?}");
            // The tail past the real tileset is padding, i.e. transparent.
            let used = if hw == Hardware::SGB { 3488 } else { 4096 };
            assert!(tiles[used..].iter().all(|&b| b == 0), "{hw:?} padding");
            assert!(tiles[..used].iter().any(|&b| b != 0), "{hw:?} has artwork");

            // ...and the frontends' composite is now available.
            let frame = gb.sgb_composited_frame().expect("composited frame");
            assert_eq!(frame.len(), ppu::SGB_FRAME_SIZE * 3);
        }
    }

    /// The decoded border is pinned by hash, so a regression in the SGB1-LZ
    /// decoder or in the asset offsets is caught without any artwork entering
    /// the repo. Both models must decode to distinct borders.
    #[test]
    fn decoded_border_is_stable() {
        let fws = firmwares();
        if fws.is_empty() {
            return;
        }
        // Cross-checked against an independent Python implementation of the
        // $01:D6BB format: same digest, so the decoder agrees byte-for-byte.
        const GOLDEN: [u64; 2] = [3_328_962_800_932_883_815, 656_769_387_348_559_691];
        let mut seen = Vec::new();
        for ((rom, hw), want) in fws.iter().zip(GOLDEN) {
            let border = sgb_firmware::extract_border(rom).expect("border decodes");
            let got = digest(&border);
            assert_eq!(got, want, "{hw:?} decoded border digest");
            seen.push(got);
        }
        assert_ne!(seen[0], seen[1], "SGB1 and SGB2 ship different borders");
    }

    /// The firmware border must not be mistaken for the boot-ROM path: the
    /// two validators are independent and each rejects the other's images.
    #[test]
    fn firmware_and_boot_rom_paths_are_separate() {
        let mut gb = machine(Hardware::SGB);
        // A boot-ROM-sized image is not SGB firmware.
        assert!(gb.load_sgb_firmware_bytes(&[0u8; 256]).is_err());
        assert!(gb.load_sgb_firmware_bytes(&[0u8; 2304]).is_err());
        assert!(!gb.has_sgb_firmware());
        // ...and firmware-sized garbage is rejected by the CRC gate.
        assert!(
            gb.load_sgb_firmware_bytes(&vec![0u8; sgb_firmware::SGB1_FIRMWARE_LEN])
                .is_err()
        );
        assert!(gb.sgb().unwrap().border().is_none());
    }

    /// Loading the firmware onto a DMG/CGB is accepted but inert: there is no
    /// SGB receiver to hold a border and nothing about the machine changes.
    #[test]
    fn non_sgb_hardware_ignores_the_firmware() {
        let fws = firmwares();
        if fws.is_empty() {
            return;
        }
        let mut gb = machine(Hardware::CGB);
        gb.load_sgb_firmware_bytes(&fws[0].0).expect("accepted");
        assert!(gb.sgb().is_none());
        assert!(gb.sgb_composited_frame().is_none());
    }
}

#[cfg(test)]
mod reset_identity_tests {
    //! `GB::reset` is a power cycle, not a model downgrade.
    //!
    //! `GB::new` seeds a dozen model-derived flags into the `Mmio` (serial
    //! CGB, AGB, MGB, the SGB unit, the real-time CPU clock and the six APU
    //! revision gates). `Mmio::reset` rebuilds itself out of `Mmio::new`, so
    //! each of those is a power-on default again unless `GB::reset` re-applies
    //! it. libretro's `retro_reset` is the only production caller, so a user
    //! resetting a CGB/AGB/SGB core there silently continued on a degraded
    //! machine — and because the cart-derived `cgb_features_enabled` IS
    //! carried, the display stayed in colour and hid it.
    //!
    //! Every assertion compares the reset machine against a freshly
    //! constructed one of the same model, so what is pinned is "reset ==
    //! power cycle" rather than a hand-copied constant.
    use super::*;
    use crate::audio::NR52;
    use crate::timer::{DIV, TAC, TIMA};

    /// Wave RAM ($FF30-$FF3F).
    const WAVE_RAM: u16 = 0xFF30;

    /// 32KB NoMBC ROM of NOPs, booted past the BIOS. The CGB flag at $0143 is
    /// set because the SC probe below needs CGB features actually enabled: the
    /// `Mmio` read path ORs the fast-clock-select bit back in under DMG-compat,
    /// which would mask `serial.cgb` on a DMG-only cart.
    fn machine(hardware: Hardware) -> GB {
        let mut rom = vec![0u8; 0x8000];
        rom[0x143] = 0x80;
        let mut gb = GB::new(hardware);
        gb.insert(cartridge::Cartridge::from_bytes(&rom).unwrap());
        gb.skip_bios();
        gb
    }

    /// Drive at least `budget` T-cycles of machine time through the CPU.
    fn step_t(gb: &mut GB, mut budget: u64) {
        while budget > 0 {
            let (_, cycles) = gb.step_instruction(false);
            budget = budget.saturating_sub(cycles.max(4) as u64);
        }
    }

    /// SC ($FF02) unused-bit read-back: 0x7C on CGB-family silicon, 0x7E
    /// everywhere else. Rides `serial.cgb`, which only `set_serial_cgb` sets.
    fn sc(gb: &GB) -> u8 {
        gb.read_memory(crate::serial::SC)
    }

    /// Wave-RAM-while-playing signature. With ch3 running, AGB returns 0xFF
    /// for every wave-RAM read while CGB hands back the byte the channel is
    /// currently on. Rides `wave.agb`, reachable only through `set_agb`'s
    /// fanout into the APU.
    fn wave_signature(gb: &mut GB) -> Vec<u8> {
        gb.write_memory(NR52, 0x80);
        for i in 0..16u16 {
            gb.write_memory(WAVE_RAM + i, (i as u8).wrapping_mul(17));
        }
        gb.write_memory(crate::audio::NR30, 0x80);
        gb.write_memory(crate::audio::NR34, 0x80);
        let mut out = Vec::new();
        for _ in 0..8 {
            step_t(gb, 64);
            gb.sync_lazy_peripherals();
            out.push(gb.read_memory(WAVE_RAM));
        }
        out
    }

    /// TAC re-enable signature. Only an AGB timer bumps TIMA when a frequency
    /// change moves the feeding DIV bit high->low, so sweeping (old freq, new
    /// freq, delay) separates an AGB timer from a CGB one. Rides
    /// `timer.is_agb`, reachable only through `set_agb`'s fanout.
    fn tac_signature(gb: &mut GB) -> Vec<u8> {
        let mut out = Vec::new();
        for old in 0..4u8 {
            for new in 0..4u8 {
                for delay in [4u64, 8, 16, 32, 64, 128] {
                    gb.write_memory(TAC, 0x04 | old);
                    gb.write_memory(DIV, 0);
                    gb.write_memory(TIMA, 0);
                    step_t(gb, delay);
                    gb.write_memory(TAC, 0x04 | new);
                    out.push(gb.read_memory(TIMA));
                }
            }
        }
        out
    }

    /// The observables are only worth asserting on if they separate the models
    /// they claim to separate.
    #[test]
    fn identity_observables_discriminate() {
        assert_eq!(sc(&machine(Hardware::CGB)), 0x7C, "CGB SC read-back");
        assert_eq!(sc(&machine(Hardware::AGB)), 0x7C, "AGB SC read-back");
        assert_eq!(sc(&machine(Hardware::MGB)), 0x7E, "MGB SC read-back");
        assert_eq!(sc(&machine(Hardware::SGB)), 0x7E, "SGB SC read-back");

        assert!(machine(Hardware::SGB).sgb().is_some(), "SGB unit present");
        assert!(machine(Hardware::CGB).sgb().is_none(), "no SGB unit on CGB");

        assert_ne!(
            wave_signature(&mut machine(Hardware::AGB)),
            wave_signature(&mut machine(Hardware::CGB)),
            "wave-RAM probe must separate an AGB APU from a CGB one"
        );
        assert_ne!(
            tac_signature(&mut machine(Hardware::AGB)),
            tac_signature(&mut machine(Hardware::CGB)),
            "TAC probe must separate an AGB timer from a CGB one"
        );
    }

    /// The model a machine was built as must still be that model after a reset.
    #[test]
    fn model_identity_survives_in_place_reset() {
        for hardware in [
            Hardware::CGB,
            Hardware::CGBE,
            Hardware::AGB,
            Hardware::SGB,
            Hardware::MGB,
        ] {
            let mut gb = machine(hardware);
            let before_sc = sc(&gb);
            let before_sgb = gb.sgb().is_some();

            gb.reset();

            assert_eq!(sc(&gb), before_sc, "{hardware:?}: SC identity lost across reset");
            assert_eq!(
                gb.sgb().is_some(),
                before_sgb,
                "{hardware:?}: SGB unit lost across reset"
            );

            // The APU revision gates, against a machine power-cycled the other way.
            let mut reference = machine(hardware);
            assert_eq!(
                wave_signature(&mut gb),
                wave_signature(&mut reference),
                "{hardware:?}: APU revision gates lost across reset"
            );
        }
    }

    /// A reset AGB must still be an AGB in all three places `set_agb` fans out
    /// to: the `Mmio` itself, the timer, and the APU channels.
    #[test]
    fn agb_fanout_survives_in_place_reset() {
        let mut gb = machine(Hardware::AGB);
        gb.reset();

        assert!(gb.mmio.is_agb(), "Mmio forgot it is an AGB after reset");
        assert_eq!(
            tac_signature(&mut gb),
            tac_signature(&mut machine(Hardware::AGB)),
            "timer forgot it is an AGB after reset"
        );
        assert_eq!(
            wave_signature(&mut gb),
            wave_signature(&mut machine(Hardware::AGB)),
            "APU forgot it is an AGB after reset"
        );
    }

    /// Catch-all for the serialized half of the identity (`is_mgb`, `cgb_de`,
    /// `serial.cgb`, the SGB unit): a reset machine and a freshly built one
    /// must serialize to the same bytes.
    ///
    /// This is also what pins the cart-derived SGB command-unlock gate, whose
    /// only other observable would be a full JOYP packet drive: the receiver
    /// `seed_hardware_flags` installs is unlocked, so without `reset`
    /// re-deriving the gate a reset SGB starts honouring packets from a cart
    /// that never declared SGB support.
    #[test]
    fn reset_machine_serializes_like_a_fresh_one() {
        for hardware in [
            Hardware::CGB,
            Hardware::CGBE,
            Hardware::AGB,
            Hardware::SGB,
            Hardware::MGB,
        ] {
            let mut gb = machine(hardware);
            gb.reset();
            let mut reference = machine(hardware);
            let after = gb.to_state_bytes().unwrap();
            let fresh = reference.to_state_bytes().unwrap();
            // Report the first divergence; the states are multi-KB, so
            // assert_eq! here would bury the run in two byte dumps.
            let first_diff = after.iter().zip(fresh.iter()).position(|(a, b)| a != b);
            assert!(
                first_diff.is_none() && after.len() == fresh.len(),
                "{hardware:?}: reset machine does not match a power-cycled one \
                 (len {} vs {}, first differing byte {first_diff:?})",
                after.len(),
                fresh.len()
            );
        }
    }
}
