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
          // leads CGB-A..E by 512 master-cc, the only observable difference)
    CGB,  // Game Boy Color, CGB-CPU-A..E
    CGBE, // Game Boy Color, CPU-CGB-D/E APU revision (boot state == CGB; the
          // observable difference is the APU C-vs-D/E gate set, SameBoy
          // `model <= GB_MODEL_CGB_C`. Default CGB models cgb04c/CPU-CGB-C —
          // the gambatte-capture silicon; SameSuite hardware is CPU-CGB-E.)
    AGB,  // Game Boy Advance in GBC-compatibility mode (CGB + isAgb() diffs)
}

impl Hardware {
    /// AGB (GBA-in-GBC-mode) hardware. AGB behaves like CGB everywhere except
    /// the small Gambatte isAgb() diff set (PPU line-153/last-line/LYC timing,
    /// APU ch3 wave-RAM, GBA_FLAG power-on registers).
    pub fn is_agb(self) -> bool {
        matches!(self, Hardware::AGB)
    }

    /// Whether this hardware runs the CGB feature set (CGB or AGB). Used to
    /// decide CGB-vs-DMG behavior; AGB is a CGB for all CGB-feature purposes.
    pub fn is_cgb_like(self) -> bool {
        matches!(self, Hardware::CGB0 | Hardware::CGB | Hardware::CGBE | Hardware::AGB)
    }

    /// CGB-D/E APU revision gate (SameBoy `model > GB_MODEL_CGB_C`). The
    /// default `CGB` models cgb04c (CPU-CGB-C, the gambatte-capture silicon);
    /// `CGBE` models the CPU-CGB-D/E silicon SameSuite was validated on.
    /// AGB intentionally stays on the C side: rustyboi's AGB model is pinned
    /// to the Gambatte-AGB oracle set (SameBoy would order AGB > CGB_E).
    pub fn is_cgb_d_or_later(self) -> bool {
        matches!(self, Hardware::CGBE)
    }
}

#[derive(Serialize, Deserialize)]
pub struct GB {
    cpu: cpu::SM83,
    mmio: memory::mmio::Mmio,
    ppu: ppu::Ppu,
    hardware: Hardware,
    #[serde(skip, default)]
    skip_bios: bool,
    #[serde(skip, default)]
    breakpoints: HashSet<u16>,
    #[serde(skip)]
    audio_output: Option<Box<dyn audio::AudioOutput>>,
}

impl Clone for GB {
    fn clone(&self) -> Self {
        GB {
            cpu: self.cpu.clone(),
            mmio: self.mmio.clone(),
            ppu: self.ppu.clone(),
            hardware: self.hardware,
            skip_bios: self.skip_bios,
            breakpoints: self.breakpoints.clone(),
            audio_output: None, // Don't clone audio output - it will be recreated if needed
        }
    }
}

pub enum Frame {
    Monochrome([u8; ppu::FRAMEBUFFER_SIZE]),
    Color([u8; ppu::FRAMEBUFFER_SIZE * 3]),
}

/// Boot-ROM-decompressed Nintendo logo tiles as they land in VRAM bank 0
/// (0x8010-0x819F). Gambatte `setInitialVram` writes the logo to the even
/// bytes only (`vram[0x10 + i*2]`), leaving the odd plane zero; this array is
/// already in that interleaved even/0x00 byte layout. Used both by the DMG
/// `skip_bios` post-boot VRAM and by the boot-residue variant for CGB.
const BOOT_LOGO_TILES: [u8; 0x190] = [
    0xf0, 0x00, 0xf0, 0x00, 0xfc, 0x00, 0xfc, 0x00, 0xfc, 0x00, 0xfc, 0x00, 0xf3, 0x00, 0xf3, 0x00,
    0x3c, 0x00, 0x3c, 0x00, 0x3c, 0x00, 0x3c, 0x00, 0x3c, 0x00, 0x3c, 0x00, 0x3c, 0x00, 0x3c, 0x00,
    0xf0, 0x00, 0xf0, 0x00, 0xf0, 0x00, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf3, 0x00, 0xf3, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xcf, 0x00, 0xcf, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x0f, 0x00, 0x0f, 0x00, 0x3f, 0x00, 0x3f, 0x00, 0x0f, 0x00, 0x0f, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc0, 0x00, 0xc0, 0x00, 0x0f, 0x00, 0x0f, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf0, 0x00, 0xf0, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xf3, 0x00, 0xf3, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc0, 0x00, 0xc0, 0x00,
    0x03, 0x00, 0x03, 0x00, 0x03, 0x00, 0x03, 0x00, 0x03, 0x00, 0x03, 0x00, 0xff, 0x00, 0xff, 0x00,
    0xc0, 0x00, 0xc0, 0x00, 0xc0, 0x00, 0xc0, 0x00, 0xc0, 0x00, 0xc0, 0x00, 0xc3, 0x00, 0xc3, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xfc, 0x00, 0xfc, 0x00,
    0xf3, 0x00, 0xf3, 0x00, 0xf0, 0x00, 0xf0, 0x00, 0xf0, 0x00, 0xf0, 0x00, 0xf0, 0x00, 0xf0, 0x00,
    0x3c, 0x00, 0x3c, 0x00, 0xfc, 0x00, 0xfc, 0x00, 0xfc, 0x00, 0xfc, 0x00, 0x3c, 0x00, 0x3c, 0x00,
    0xf3, 0x00, 0xf3, 0x00, 0xf3, 0x00, 0xf3, 0x00, 0xf3, 0x00, 0xf3, 0x00, 0xf3, 0x00, 0xf3, 0x00,
    0xf3, 0x00, 0xf3, 0x00, 0xc3, 0x00, 0xc3, 0x00, 0xc3, 0x00, 0xc3, 0x00, 0xc3, 0x00, 0xc3, 0x00,
    0xcf, 0x00, 0xcf, 0x00, 0xcf, 0x00, 0xcf, 0x00, 0xcf, 0x00, 0xcf, 0x00, 0xcf, 0x00, 0xcf, 0x00,
    0x3c, 0x00, 0x3c, 0x00, 0x3f, 0x00, 0x3f, 0x00, 0x3c, 0x00, 0x3c, 0x00, 0x0f, 0x00, 0x0f, 0x00,
    0x3c, 0x00, 0x3c, 0x00, 0xfc, 0x00, 0xfc, 0x00, 0x00, 0x00, 0x00, 0x00, 0xfc, 0x00, 0xfc, 0x00,
    0xfc, 0x00, 0xfc, 0x00, 0xf0, 0x00, 0xf0, 0x00, 0xf0, 0x00, 0xf0, 0x00, 0xf0, 0x00, 0xf0, 0x00,
    0xf3, 0x00, 0xf3, 0x00, 0xf3, 0x00, 0xf3, 0x00, 0xf3, 0x00, 0xf3, 0x00, 0xf0, 0x00, 0xf0, 0x00,
    0xc3, 0x00, 0xc3, 0x00, 0xc3, 0x00, 0xc3, 0x00, 0xc3, 0x00, 0xc3, 0x00, 0xff, 0x00, 0xff, 0x00,
    0xcf, 0x00, 0xcf, 0x00, 0xcf, 0x00, 0xcf, 0x00, 0xcf, 0x00, 0xcf, 0x00, 0xc3, 0x00, 0xc3, 0x00,
    0x0f, 0x00, 0x0f, 0x00, 0x0f, 0x00, 0x0f, 0x00, 0x0f, 0x00, 0x0f, 0x00, 0xfc, 0x00, 0xfc, 0x00,
    0x3c, 0x00, 0x42, 0x00, 0xb9, 0x00, 0xa5, 0x00, 0xb9, 0x00, 0xa5, 0x00, 0x42, 0x00, 0x3c, 0x00,
];

impl GB {
    pub fn new(hardware: Hardware) -> Self {
        let mut mmio = memory::mmio::Mmio::new();
        mmio.set_serial_cgb(hardware.is_cgb_like());
        mmio.set_agb(hardware.is_agb());
        mmio.set_apu_cgb_de(hardware.is_cgb_d_or_later());
        if matches!(hardware, Hardware::SGB | Hardware::SGB2) {
            mmio.enable_sgb();
        }
        GB {
            cpu: cpu::SM83::new(),
            mmio,
            ppu: ppu::Ppu::new(),
            skip_bios: false,
            hardware,
            breakpoints: HashSet::new(),
            audio_output: None, // Audio will be enabled when needed
        }
    }

    pub fn skip_bios(&mut self) {
        self.skip_bios = true;
        self.cpu.registers.pc = 0x0100;
        self.cpu.registers.sp = 0xFFFE;

        self.mmio.write(crate::ppu::LCD_CONTROL, 0x91);
        self.ppu.sync_lcdc_from_mmio(&self.mmio);
        self.mmio.write(crate::ppu::SCX, 0x00);
        self.mmio.write(crate::ppu::WX, 0x00);
        self.mmio.write(crate::ppu::SCY, 0x00);
        self.mmio.write(crate::ppu::WY, 0x00);
        self.mmio.write(crate::input::JOYP, 0xCF);
        self.mmio.write(crate::ppu::LYC, 0x00);
        self.mmio.write(crate::ppu::BGP, 0xFC);
        // OBP0/OBP1 post-boot value (Gambatte setInitial ffxxDump 0x48/0x49,
        // mem_dumps.h): DMG leaves them uninitialised reading 0xFF; the CGB boot
        // ROM zeroes the obj-palette I/O so FF48/FF49 read 0x00 (the
        // fexx_ffxx_dumper_cgb oracle reads 0x00 at FF48/FF49).
        let obp_init = match self.hardware {
            Hardware::CGB | Hardware::CGBE | Hardware::AGB => 0x00,
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
            Hardware::DMG0 | Hardware::DMG | Hardware::MGB | Hardware::CGB0 | Hardware::CGB | Hardware::CGBE | Hardware::AGB => 0xF1,
            Hardware::SGB | Hardware::SGB2 => 0xF0,
        });
        self.mmio.write(crate::timer::TIMA, 0x00);
        self.mmio.write(crate::timer::TMA, 0x00);
        self.mmio.write(crate::timer::TAC, 0xF8);
        self.mmio.write(crate::timer::DIV, match self.hardware {
            Hardware::DMG | Hardware::MGB | Hardware::SGB | Hardware::SGB2 | Hardware::CGB0 | Hardware::CGB | Hardware::CGBE | Hardware::AGB => 0xAB,
            Hardware::DMG0 => 0x18,
        });

        self.cpu.registers.a = match self.hardware {
            Hardware::DMG0 | Hardware::DMG | Hardware::SGB => 0x01,
            Hardware::MGB | Hardware::SGB2 => 0xFF,
            // Gambatte setPostBiosState: a = cgb*0x10 | 0x01 (0x11 for CGB & AGB).
            Hardware::CGB0 | Hardware::CGB | Hardware::CGBE | Hardware::AGB => 0x11,
        };
        self.cpu.registers.b = match self.hardware {
            // Gambatte setPostBiosState: b = cgb & agb. AGB sets B bit0 (the
            // GBA-detection flag games read at boot); CGB/others leave B=0.
            Hardware::AGB => 0x01,
            Hardware::CGB0 | Hardware::CGB | Hardware::CGBE | Hardware::DMG | Hardware::MGB | Hardware::SGB | Hardware::SGB2 => 0x00,
            Hardware::DMG0 => 0xFF,
        };
        self.cpu.registers.c = match self.hardware {
            Hardware::CGB0 | Hardware::CGB | Hardware::CGBE | Hardware::AGB => 0x00,
            Hardware::DMG0 | Hardware::DMG | Hardware::MGB => 0x13,
            Hardware::SGB | Hardware::SGB2 => 0x14,
        };
        self.cpu.registers.d = match self.hardware {
            Hardware::CGB0 | Hardware::CGB | Hardware::CGBE | Hardware::AGB => 0xFF,
            Hardware::SGB | Hardware::SGB2 | Hardware::DMG0 | Hardware::DMG | Hardware::MGB => 0x00,
        };
        self.cpu.registers.e = match self.hardware {
            Hardware::DMG | Hardware::MGB => 0xD8,
            Hardware::DMG0 => 0xC1,
            Hardware::SGB | Hardware::SGB2 => 0x00,
            Hardware::CGB0 | Hardware::CGB | Hardware::CGBE | Hardware::AGB => 0x56,
        };
        self.cpu.registers.h = match self.hardware {
            Hardware::CGB0 | Hardware::CGB | Hardware::CGBE | Hardware::AGB => 0x00,
            Hardware::DMG0 => 0x84,
            Hardware::DMG | Hardware::MGB => 0x01,
            Hardware::SGB | Hardware::SGB2 => 0xC0,
        };
        self.cpu.registers.l = match self.hardware {
            Hardware::CGB0 | Hardware::CGB | Hardware::CGBE | Hardware::AGB => 0x0D,
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
            Hardware::DMG | Hardware::CGB0 | Hardware::CGB | Hardware::CGBE | Hardware::MGB => true,
            Hardware::AGB | Hardware::DMG0 | Hardware::SGB | Hardware::SGB2 => false,
        });
        self.cpu.registers.set_flag(registers::Flag::Negative, false);
        // DMG/MGB post-boot H/C reflect the boot ROM's final header-checksum
        // `ADD A,(0x14D)`: a valid ROM has `A == 256 - rom[0x14D]` there, so the add
        // carries iff rom[0x14D] != 0 (C), and half-carries iff its low nibble != 0
        // (H). The previous `== 0x00` was inverted (gave F=0x80 where real hardware
        // gives 0xB0). DMG0/SGB/CGB leave H/C clear.
        self.cpu.registers.set_flag(registers::Flag::HalfCarry, match self.hardware {
            Hardware::DMG0 | Hardware::SGB | Hardware::SGB2 | Hardware::CGB0 | Hardware::CGB | Hardware::CGBE | Hardware::AGB => false,
            Hardware::DMG | Hardware::MGB => (self.mmio.read(0x014D) & 0x0F) != 0,
        });
        self.cpu.registers.set_flag(registers::Flag::Carry, match self.hardware {
            Hardware::DMG0 | Hardware::SGB | Hardware::SGB2 | Hardware::CGB0 | Hardware::CGB | Hardware::CGBE | Hardware::AGB => false,
            Hardware::DMG | Hardware::MGB => self.mmio.read(0x014D) != 0x00,
        });
        if self.hardware.is_cgb_like() {
            self.mmio.write(crate::memory::mmio::REG_VBK, 0x7E);
            self.mmio.write(crate::memory::mmio::REG_SVBK, 0xF8);
            // RP/IR (0xFF56) power-on: bits 1-5 hold 0x3E so the masked read
            // returns 0x3E (Gambatte ffxxDump). Bits 0,6,7 start clear.
            self.mmio.set_io_register(0xFF56, 0x3E);
        }

        // Work-RAM power-on contents (Gambatte setInitial*Wram). Fill via the
        // normal bus, walking SVBK so each CGB bank receives its slice; fixed
        // bank 0 lives at 0xC000, the banked region at 0xD000.
        {
            let cgb = self.hardware.is_cgb_like();
            let banks = if cgb { 8 } else { 2 };
            let mut wram = vec![0u8; banks * 0x1000];
            crate::memory::init_wram_powerup(cgb, &mut wram);
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
        // the Gambatte hwtest CGB anchors (start_inc_1/_2 read DIV directly).
        //
        // CGB/AGB counters are CART-TYPE dependent (resolved 2026-07-02; this
        // was previously documented as a two-oracle conflict). The CGB boot ROM
        // has two hand-off paths: for CGB-flagged carts it hands off with the
        // DIV counter at 0x1EA0; for DMG-flagged carts it additionally runs the
        // DMG-compat setup (logo-checksum palette lookup + KEY0 latch), handing
        // off 0x7D8 cc later at 0x2678. Both anchors are real hardware:
        //   - CGB cart  -> 0x1EA0: Gambatte's hwtest CGB refs (start_inc_1/_2
        //     out1E/out1F, tc00_start_2, fexx_ffxx_dumper, 11 ch1/ch2 boot-phase
        //     sound tests) and BullyGB's initial-DIV check — all CGB-flagged
        //     carts. (== Gambatte setPostBiosState cycleCounter 0x102A0 -
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
            Hardware::CGB | Hardware::CGBE => {
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
            // carries to the CGB-cart path: 0x1EA4 == Gambatte setPostBiosState
            // 0x102A0 + agb*4 (the Gambatte-AGB bootstrap oracle's counter).
            // Verified: passes mooneye boot_div-A. AGB is opt-in, outside the
            // default suites.
            Hardware::AGB => {
                if cgb_cart { 0x1EA4 } else { 0x267C }
            }
            Hardware::DMG | Hardware::MGB => 0xABCC,
            // boot_div-S / boot_div2-S fingerprint (d9 da da db dc de). The SGB
            // CPU uses the DMG-style single-speed timer; this is the post-boot
            // counter that reproduces the SGB boot_div fingerprint. Verified:
            // passes mooneye boot_div-S and boot_div2-S.
            Hardware::SGB | Hardware::SGB2 => 0xD860,
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
        // above were dropped. Apply Gambatte's exact post-boot APU state directly
        // (must follow the timer-counter set so the duty phase has the right cc).
        // Wave RAM differs between DMG and CGB (Gambatte ioamhram dumps).
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
        self.mmio.set_post_bios_audio_state(cgb);

        // Post-boot power-on OAM / unusable-region / HRAM contents (Gambatte
        // setInitial*Ioamhram). The boot ROM leaves these untouched, so they
        // hold the hardware power-on pattern the fexx_* dumpers read back.
        self.mmio.set_post_bios_ioamhram(cgb);

        // Post-boot CGB palette RAM. The boot ROM leaves BG palette RAM
        // all-white and OBJ palette RAM holding the hardware power-on dump
        // (Gambatte initstate cgbObjpDump). A program that renders a sprite
        // without writing FF6A/FF6B observes these values; without this the
        // OBJ palette is all-zero (black). Matches scx_during_m3_spx2 etc.
        //
        // DMG cart on CGB hardware (CGB features OFF): the CGB boot ROM instead
        // installs a fixed DMG-compatibility colored palette, and the PPU renders
        // the DMG game through it. Seed that palette so a DMG cart shows in the
        // boot ROM's colors (dmg-acid2-on-CGB) rather than grayscale.
        if cgb {
            if self.should_enable_cgb_features() {
                self.mmio.set_post_bios_cgb_palettes();
            } else {
                self.mmio.set_cgb_compat_dmg_palettes();
            }
        }

        // Post-boot VRAM contents. The boot ROM decompresses the Nintendo logo
        // from the cart header into the BG tile area (0x8010-0x819F) and writes
        // the logo tilemap (tile indices) at 0x9904-0x9910 / 0x9924-0x992F.
        // These bytes are the exact post-boot bank-0 VRAM Gambatte's vram_dumper
        // captures; real games can read them, so this is legitimate skip_bios
        // fidelity. VBK was set to bank 0 above, so plain bus writes land in
        // bank 0. Restricted to DMG/MGB: the CGB oamdma vram dumpers GDMA over
        // 0x8000 and assert the remaining VRAM is zero, so a CGB logo regresses
        // them (Gambatte's CGB references for those tests assume cleared VRAM);
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
            // bank 0 (Gambatte setInitialVram seeds 0x8010-0x819F for cgb too;
            // only the tilemap is DMG-only). mealybug's compat sprites render
            // tile 0x19 (®) straight from this boot residue (m3_obp0_change
            // cgb_c); without it the sprite is all-transparent. The CGB-cart
            // dumper compromise above is unaffected: those are CGB-feature
            // carts, never compat mode.
            self.seed_boot_logo_vram();
        }

        // Post-boot PPU frame phase. The boot ROM leaves the LCD enabled and the
        // PPU deep into a frame (Gambatte setInitialState `videoCycles`): the game
        // starts in VBlank at LY=144 (CGB) / LY=153 (DMG), not a fresh LY=0 OAM
        // search. Seed that here so the first instruction's LY/STAT reads match
        // hardware (display_startstate). Must follow the LCDC=0x91 write above.
        self.ppu.set_post_bios_state(&mut self.mmio);
    }

    /// Write the boot-ROM Nintendo logo into VRAM bank 0 via the normal bus.
    /// VBK is bank 0 at this point in `skip_bios`, so plain writes land there.
    fn seed_boot_logo_vram(&mut self) {
        for (i, b) in BOOT_LOGO_TILES.iter().enumerate() {
            self.mmio.write(0x8010 + i as u16, *b);
        }
    }

    /// Post-boot state as captured WITH the boot ROM having run, for the SRAM
    /// dumper oracles (`vram_dumper`, `fexx_ffxx_dumper`) whose `.bin` references
    /// were produced after the boot ROM executed. On top of the normal
    /// `skip_bios` no-boot state this also seeds the boot-ROM-final residue that
    /// the no-boot path deliberately omits (because the `.dump` region oracles
    /// were captured WITHOUT the boot ROM and need the zeroed/0x18 state):
    ///   - CGB: the Nintendo logo in VRAM bank 0 (Gambatte `setInitialVram`,
    ///     mem_dumps.h:3032) and the canonical 0x08-tail feax shadow
    ///     (`setInitialCgbIoamhram` feaxDump, mem_dumps.h:3138).
    ///   - DMG: the logo is already seeded by `skip_bios`; no extra residue
    ///     (the canonical `setInitialDmgIoamhram` OAM is already applied).
    /// Select this per-oracle (SRAM dump) in the runner; the no-boot
    /// `skip_bios` must stay in use for the `.dump` region oracles.
    pub fn skip_bios_with_boot_residue(&mut self) {
        self.skip_bios();
        if self.hardware.is_cgb_like() {
            self.seed_boot_logo_vram();
            self.mmio.set_cgb_boot_residue_feax();
        }
    }

    pub fn insert(&mut self, cartridge: cartridge::Cartridge) {
        // Validate hardware compatibility
        if let Err(msg) = self.validate_cartridge_compatibility(&cartridge) {
            eprintln!("Warning: {}", msg);
        }
        
        self.mmio.insert_cartridge(cartridge);
        
        // Update CGB features enablement based on hardware and cartridge compatibility
        let cgb_enabled = self.should_enable_cgb_features();
        self.mmio.set_cgb_features_enabled(cgb_enabled);
    }

    /// Validate that the cartridge is compatible with the current hardware
    fn validate_cartridge_compatibility(&self, cartridge: &cartridge::Cartridge) -> Result<(), String> {
        let cgb_support = cartridge.get_cgb_support();
        
        match (self.hardware, &cgb_support) {
            // CGB-only cartridge on non-CGB hardware
            (Hardware::DMG | Hardware::DMG0 | Hardware::MGB | Hardware::SGB | Hardware::SGB2, cartridge::CgbSupport::Only) => {
                Err("CGB-only cartridge cannot run on DMG hardware".to_string())
            }
            // CGB cartridge on CGB/AGB hardware - always OK
            (Hardware::CGB0 | Hardware::CGB | Hardware::CGBE | Hardware::AGB, _) => Ok(()),
            // DMG cartridge on any hardware - always OK  
            (_, cartridge::CgbSupport::None) => Ok(()),
            // CGB-compatible cartridge on DMG hardware - OK but will run in DMG mode
            (_, cartridge::CgbSupport::Compatible) => Ok(()),
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

    /// Run the REAL boot ROM from power-on (PC=0x0000) until it hands off to the
    /// cartridge. Mirrors Gambatte's testrunner, which executes the boot ROM
    /// before every test instead of seeding a synthetic post-boot state.
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
        // (mirrors Gambatte initializing ioamhram before loadBios). The boot ROM
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

    // Audio management methods
    pub fn enable_audio(&mut self, mut output: Box<dyn audio::AudioOutput>) -> Result<(), Box<dyn std::error::Error>> {
        if self.audio_output.is_some() {
            // Audio already enabled
            return Ok(());
        }
        output.start()?;
        self.audio_output = Some(output);
        Ok(())
    }

    pub fn step_instruction(&mut self, collect_audio: bool) -> (bool, u32) {
        // Check for breakpoint at current PC before executing
        let pc = self.cpu.registers.pc;
        if self.breakpoints.contains(&pc) {
            // Breakpoint hit - don't execute instruction and return (empty audio, breakpoint hit)
            return (true, 0);
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
                return (self.ppu.get_frame(&self.mmio), true);
            }
            
            // Check if PPU has completed a frame
            if self.ppu.frame_ready() {
                return (self.ppu.get_frame(&self.mmio), false);
            }
            
            // If PPU is disabled or taking too long, cap the cycles to prevent audio buildup
            let max_cpu_cycles_per_frame = if self.mmio.is_double_speed_mode() {
                MAX_NORMAL_SPEED_CPU_CYCLES_PER_FRAME * 2
            } else {
                MAX_NORMAL_SPEED_CPU_CYCLES_PER_FRAME
            };
            if cpu_cycles_this_frame >= max_cpu_cycles_per_frame {
                // PPU disabled or stuck - return after reasonable cycle count to maintain timing
                return (self.ppu.get_frame(&self.mmio), false);
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
                return Ok((self.ppu.get_frame(&self.mmio), true));
            }

            if self.ppu.frame_ready() {
                // SGB *_TRN commands read a 4KB VRAM block during the VBlank
                // after the command. Service any pending transfer at the frame
                // boundary (no-op on non-SGB hardware).
                self.mmio.service_sgb_vram_transfer();
                return Ok((self.ppu.get_frame(&self.mmio), false));
            }

            if cpu_cycles >= max_cycles {
                return Err("timed out waiting for LCD frame");
            }
        }
    }

    pub fn get_current_frame(&mut self) -> Frame {
        self.ppu.get_frame(&self.mmio)
    }

    pub fn set_cgb_color_conversion(&mut self, conversion: ppu::CgbColorConversion) {
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

    /// Master cycle counter (abs_cc) for timing trace reconciliation vs cctracer.
    pub fn master_cc(&self) -> u64 {
        self.mmio.master_cc()
    }

    /// Write a byte through the memory bus. Used by the libretro frontend to
    /// apply per-frame GameShark RAM pokes.
    pub fn write_memory(&mut self, address: u16, value: u8) {
        self.mmio.write(address, value);
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

    /// Raw CGB BG palette RAM byte pair for a palette/color slot.
    pub fn bg_palette_pair(&self, palette: u8, color: u8) -> u16 {
        self.read_bg_palette_data(palette, color)
    }

    /// Raw CGB OBJ palette RAM byte pair for a palette/color slot.
    pub fn obj_palette_pair(&self, palette: u8, color: u8) -> u16 {
        self.read_obj_palette_data(palette, color)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_state_file(path: &str) -> Result<Self, io::Error> {
        let saved_state = fs::read_to_string(path)?;
        let gb = serde_json::from_str(&saved_state)?;
        Ok(gb)
    }

    pub fn to_state_file(&self, path: &str) -> Result<(), io::Error> {
        let serialized = serde_json::to_string(&self)?;
        fs::write(path, serialized)?;
        Ok(())
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
