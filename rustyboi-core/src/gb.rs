use crate::cartridge;
use crate::cpu;
use crate::cpu::registers;
use crate::memory;
use crate::memory::Addressable;
use crate::ppu;
use crate::audio;

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io;

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

impl Hardware {
    /// AGB (GBA-in-GBC-mode) hardware. AGB behaves like CGB everywhere except
    /// the small AGB-vs-CGB diff set (PPU line-153/last-line/LYC timing,
    /// APU ch3 wave-RAM, GBA_FLAG power-on registers).
    pub fn is_agb(self) -> bool {
        matches!(self, Hardware::AGB)
    }

    /// Whether this hardware runs the CGB feature set (CGB or AGB). Used to
    /// decide CGB-vs-DMG behavior; AGB is a CGB for all CGB-feature purposes.
    pub fn is_cgb_like(self) -> bool {
        matches!(self, Hardware::CGB0 | Hardware::CGBB | Hardware::CGB | Hardware::CGBE | Hardware::AGB)
    }

    /// CGB-B-or-earlier APU revision gate (CGB silicon at revision B or
    /// older): the NRx4 length-enable extra-clock glitch
    /// fires regardless of the written bit-6 value ("current value is
    /// irrelevant on CGB-B and older"). SameSuite
    /// channel_*_extra_length_clocking-cgb0B/-cgb0/-cgbB validate this fork.
    pub fn is_cgb_b_or_earlier(self) -> bool {
        matches!(self, Hardware::CGB0 | Hardware::CGBB)
    }

    /// CGB-D/E APU revision gate (CGB silicon newer than revision C). The
    /// default `CGB` models cgb04c (CPU-CGB-C, the reference-capture silicon);
    /// `CGBE` models the CPU-CGB-D/E silicon SameSuite was validated on.
    /// AGB intentionally stays on the C side: rustyboi's AGB model is pinned
    /// to the AGB reference oracle (a strict revision order would place AGB > CGB_E).
    pub fn is_cgb_d_or_later(self) -> bool {
        matches!(self, Hardware::CGBE)
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
pub fn cartridge_compatibility(hardware: Hardware, cartridge: &cartridge::Cartridge) -> Compatibility {
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
    pub fn default_for(hardware: Hardware) -> Self {
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

/// The four DMG-shade colours for a model's default palette under `correction`.
/// The single source of truth for mono → RGB in the media sweep (which has no
/// user palette override); frontends use a user-selected [`DmgPaletteChoice`].
pub fn mono_shades(hardware: Hardware, correction: ppu::ColorCorrection) -> [[u8; 3]; 4] {
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
    pub fn new(hardware: Hardware) -> Self {
        let mut mmio = memory::mmio::Mmio::new();
        mmio.set_serial_cgb(hardware.is_cgb_like());
        mmio.set_agb(hardware.is_agb());
        mmio.set_mgb(matches!(hardware, Hardware::MGB));
        mmio.set_apu_cgb_de(hardware.is_cgb_d_or_later());
        mmio.set_cgb_de(hardware.is_cgb_d_or_later());
        mmio.set_apu_cgb_le_b(hardware.is_cgb_b_or_earlier());
        mmio.set_apu_cgb_b(matches!(hardware, Hardware::CGBB));
        // CGB-C-and-older PCM read glitch (CGB silicon at revision C or older).
        // Real CPU-CGB-C silicon has it too, but the default
        // CGB model intentionally keeps the SameSuite-calibrated D/E-clean
        // reads (same convention as the nrx2 zombie glitch): the internal
        // SameSuite rows for the non-revision-suffixed channel tests grade
        // against tables real CGB-C fails, and no cgb04c capture
        // pins the glitch. Only the explicit pre-C revisions consume it.
        mmio.set_apu_pcm_c_glitch(matches!(hardware, Hardware::CGB0 | Hardware::CGBB));
        // NRx4 square step-back parity gate (all revisions except CGB-D/E):
        // CGB-C-and-earlier AND AGB gate the step-back on
        // `sample_countdown & 1`; CGB-D/E apply it unconditionally. The default
        // CGB keeps the unconditional cgb04c placement, so only the
        // explicit pre-D / AGB revisions take the parity fork.
        mmio.set_apu_step_back_parity(matches!(
            hardware,
            Hardware::CGB0 | Hardware::CGBB | Hardware::AGB
        ));
        if matches!(hardware, Hardware::SGB | Hardware::SGB2) {
            mmio.enable_sgb();
        }
        GB {
            cpu: cpu::SM83::new(),
            mmio,
            ppu: ppu::Ppu::new(),
            skip_bios: false,
            hardware,
            dmg_palette: DmgPaletteChoice::default_for(hardware),
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
        self.mmio.write(crate::audio::NR30, 0x7F);
        self.mmio.write(crate::audio::NR31, 0xFF);
        self.mmio.write(crate::audio::NR32, 0x9F);
        self.mmio.write(crate::audio::NR33, 0xFF);
        self.mmio.write(crate::audio::NR34, 0xBF);
        self.mmio.write(crate::audio::NR41, 0xFF);
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
        // Record the CGB flag before any audio write anchors the SPU clock, so
        // the boot SPU `cycleCounter_` high-bit constant (0x1E00/0x2400) is right.
        self.mmio.set_audio_boot_cgb(self.hardware.is_cgb_like());

        // Post-boot APU state. The boot ROM enables the APU and leaves channel 1
        // mid-tone; channel registers are gated behind APU-enable, so the writes
        // above were dropped. Apply the exact post-boot APU state directly
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

    /// How the currently-inserted cartridge pairs with the current hardware.
    ///
    /// Returns [`Compatibility::Full`] when no cartridge is loaded.
    pub fn cartridge_compatibility(&self) -> Compatibility {
        match self.mmio.get_cartridge() {
            Some(cartridge) => cartridge_compatibility(self.hardware, cartridge),
            None => Compatibility::Full,
        }
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
        if self.cpu.stopped {
            if self.mmio.read(crate::input::JOYP) & 0x0F != 0x0F {
                self.cpu.stopped = false;
                let mut bus = cpu::Bus::new(&mut self.mmio, &mut self.ppu);
                bus.tick_remaining(8);
                // STOP-wake semantics are asserted against raw master_cc by
                // hardware tests; never leave the wake advance carried.
                bus.flush_all_lag();
                return (false, 8);
            }
            return (false, 4);
        }

        self.ppu.step_scheduled_stat_events(&mut self.mmio);

        // Execute one CPU instruction. Every peripheral (incl. the PPU) is
        // ticked inline by `Bus` at each memory access's true cycle, so reads
        // observe — and writes mutate — live state; the remaining internal
        // cycles are ticked afterward.
        let is_double_speed = self.mmio.is_double_speed_mode();
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

        // Generate audio samples if requested
        let audio_samples = if collect_audio {
            // In double speed mode, audio runs at normal speed, so we need to adjust the cycle count
            let audio_cycles = if is_double_speed { cycles / 2 } else { cycles };
            self.mmio.generate_audio_samples(audio_cycles)
        } else {
            Vec::new()
        };

        // Send audio samples directly to output as they're generated
        if !audio_samples.is_empty()
            && let Some(audio_output) = &mut self.audio_output {
                audio_output.add_samples(&audio_samples);
        }

        (false, cycles) // No breakpoint hit
    }

    /// Advance nothing; convert the PPU's just-completed raw frame into the
    /// presented always-RGB [`Frame`], applying the DMG base palette + colour
    /// correction to a monochrome frame (colour frames are already corrected).
    fn presented_frame(&mut self) -> Frame {
        match self.ppu.get_frame(&self.mmio) {
            ppu::RenderedFrame::Color(rgb) => Frame(rgb),
            ppu::RenderedFrame::Monochrome(idx) => {
                let shades = self.dmg_palette.shades(self.ppu.cgb_color_conversion());
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
    pub fn dmg_shade_frame(&self) -> &[u8; ppu::FRAMEBUFFER_SIZE] {
        self.ppu.dmg_shade_frame()
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
        self.ppu.sgb_composited_frame(&self.mmio)
    }

    pub fn set_cgb_color_conversion(&mut self, conversion: ppu::ColorCorrection) {
        self.ppu.set_cgb_color_conversion(conversion);
    }

    /// The four DMG-shade colours (index 0 = lightest) for this machine's model
    /// and colour-correction setting; see [`mono_shades`].
    pub fn mono_shades(&self) -> [[u8; 3]; 4] {
        mono_shades(self.hardware, self.ppu.cgb_color_conversion())
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

    // ---- Dirty-line probe (scanline-renderer feasibility study) ----
    // Optional observer; off by default. When no probe is attached the
    // register-write path is byte-identical to an un-instrumented build.

    /// Attach a fresh dirty-line probe.
    pub fn attach_dirty_probe(&mut self) {
        self.ppu.attach_dirty_probe();
    }

    /// Detach and return the probe with its accumulated counters.
    pub fn take_dirty_probe(&mut self) -> Option<Box<ppu::DirtyLineProbe>> {
        self.ppu.take_dirty_probe()
    }

    /// Borrow the probe for reading counters.
    pub fn dirty_probe(&self) -> Option<&ppu::DirtyLineProbe> {
        self.ppu.dirty_probe()
    }

    /// Fold the just-completed frame into the probe totals. Call once per frame
    /// returned by `run_until_frame`.
    pub fn dirty_probe_end_frame(&mut self) {
        self.ppu.dirty_probe_end_frame();
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

    /// Plug one end of a link cable into this instance (the other end goes to
    /// a second instance, possibly owned by another window/process transport).
    pub fn attach_link_peer(&mut self, peer: crate::serial::LinkPeer) {
        self.mmio.attach_link(peer);
    }

    pub fn link_attached(&self) -> bool {
        self.mmio.link_attached()
    }

    /// Unplug the link-port device (back to a disconnected cable).
    pub fn detach_serial_device(&mut self) {
        self.mmio.detach_serial_device();
    }

    /// Point two GBC instances' IR ports at each other (Pan Docs "GBC Infrared
    /// Communication"). Each side's emitter (RP bit 0) illuminates the other's
    /// receiver (RP bit 1). The harness pumps both instances (any interleave);
    /// the shared channel carries the emitter level between their timelines. Use
    /// for GBC<->GBC IR: Pokémon G/S/C Mystery Gift, TCG "Card Pop", Pokémon
    /// Pinball score exchange, Bomberman trades.
    pub fn connect_ir(a: &mut GB, b: &mut GB) {
        let (la, lb) = crate::ir::IrLink::pair();
        a.mmio.attach_ir(la);
        b.mmio.attach_ir(lb);
    }

    /// Plug one end of a shared IR channel into this instance (the other end
    /// goes to a second instance, possibly behind a socket/process transport).
    pub fn attach_ir_peer(&mut self, link: crate::ir::IrLink) {
        self.mmio.attach_ir(link);
    }

    /// Diagnostic self-test: make this instance's IR port see its own emitter.
    pub fn set_ir_loopback(&mut self) {
        self.mmio.set_ir_loopback();
    }

    pub fn ir_attached(&self) -> bool {
        self.mmio.ir_attached()
    }

    /// Unplug the IR partner (back to a lone GBC that never sees light).
    pub fn detach_ir(&mut self) {
        self.mmio.detach_ir();
    }

    /// Connect 2-4 Game Boys through a 4-Player Adapter (DMG-07). The adapter is
    /// the clock master, so each Game Boy uses external-clock serial; the shared
    /// hub runs the Pan Docs ping/transmission protocol. The frontend pumps all
    /// instances (any interleave), exactly like [`GB::connect_link`]. Player IDs
    /// are assigned by attach order (1..N).
    pub fn connect_four_player(gbs: &mut [&mut GB]) {
        let ports = crate::dmg07::FourPlayerPort::hub(gbs.len());
        for (gb, port) in gbs.iter_mut().zip(ports) {
            gb.mmio.attach_four_player(port);
        }
    }

    /// Plug one DMG-07 port into this instance (the other ports go to other
    /// instances, possibly behind a socket/process transport).
    pub fn attach_four_player_port(&mut self, port: crate::dmg07::FourPlayerPort) {
        self.mmio.attach_four_player(port);
    }

    pub fn four_player_attached(&self) -> bool {
        self.mmio.four_player_attached()
    }

    /// Plug a Mobile Adapter GB into the link port. The adapter answers the
    /// libmobile packet protocol (session begin/end, config read/write); live
    /// networking is out of scope (see `crate::mobile`).
    pub fn attach_mobile_adapter(&mut self) {
        self.mmio.attach_mobile_adapter(crate::mobile::MobileAdapter::new());
    }

    /// True once a game has completed the START "NINTENDO" handshake with an
    /// attached Mobile Adapter (i.e. detected it and begun a session).
    pub fn mobile_session_started(&self) -> bool {
        self.mmio.mobile_adapter().is_some_and(|m| m.session_started())
    }

    pub fn printer_attached(&self) -> bool {
        self.mmio.printer().is_some()
    }

    /// Debug/test: the in-flight serial transfer's completion event cc
    /// (None while idle or while a link transfer holds for the peer).
    pub fn serial_transfer_complete_at(&self) -> Option<u64> {
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

    pub fn to_state_file(&mut self, path: &str) -> Result<(), io::Error> {
        fs::write(path, self.to_state_bytes()?)?;
        Ok(())
    }

    /// Serialize the whole machine to a savestate byte buffer. WASM-clean (no
    /// filesystem): the caller owns the bytes. Uses a compact binary format
    /// (bincode) — `serde_bytes` blobs (VRAM/WRAM/OAM/framebuffers) become
    /// length-prefixed byte runs, not JSON number-arrays, so a snapshot is
    /// ~its raw size instead of megabytes of text (inline web rewind was
    /// stalling on the JSON encode). Mirrors `to_state_file`, so a state saved
    /// one way round-trips through the other.
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
    /// `to_state_bytes` (or `to_state_file`). Re-derives the `#[serde(skip)]`
    /// cartridge-flag cache exactly as `from_state_file` does. WASM-clean.
    pub fn from_state_bytes(bytes: &[u8]) -> Result<Self, io::Error> {
        let mut gb: GB =
            bincode::deserialize(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        gb.post_load_fixup();
        Ok(gb)
    }

    pub fn reset(&mut self) {
        self.mmio.reset();
        self.ppu.reset();
        self.cpu.halted = false;
        self.cpu.stopped = false;
        self.cpu.ime_enable_delay = 0;
        self.mmio.clear_delayed_writes();
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
        // Channel 4's DAC is on and volume is 15, so the enabled channel emits a
        // nonzero level whenever the LFSR output bit is high. A live (advancing)
        // LFSR flips that bit, so the stream must contain BOTH a nonzero level
        // and a zero level. A frozen/latched channel would hold one value.
        let saw_high = samples.iter().any(|&(l, _)| l != 0.0);
        let saw_low = samples.iter().any(|&(l, _)| l == 0.0);
        assert!(saw_high, "channel 4 produced no output at all (LFSR/DAC dead)");
        assert!(
            saw_low && saw_high,
            "channel 4 output is constant -> LFSR is latched (never advances)"
        );

        // Stronger check: the stream must have many transitions, not just a
        // one-time settle. Count level changes across the captured samples.
        let transitions = samples
            .windows(2)
            .filter(|w| (w[0].0 != 0.0) != (w[1].0 != 0.0))
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
        // fills the gaps with a continuous ~0.1 noise floor, so silent windows
        // collapse to zero. Require both a loud hit and multiple silent gaps.
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
            if rms > 0.15 {
                loud += 1;
            }
            i += win;
        }
        assert!(loud > 4, "drumroll never played (loud windows = {loud})");
        assert!(
            silent >= 4,
            "noise channel did not fall silent between drum hits \
             (silent 100 ms windows = {silent}) -> ch4 latched into a continuous \
             buzz on the non-CGB path (Pokémon intro drumroll)"
        );
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
