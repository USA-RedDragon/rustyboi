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

#[derive(Serialize, Deserialize, Clone, Copy, Debug, clap::ValueEnum)]
pub enum Hardware {
    DMG,  // Original DMG-01
    DMG0, // Very early Japanese DMG-01
    MGB,  // Game Boy Pocket
    SGB,  // Super Game Boy
    SGB2, // Super Game Boy 2
    CGB,  // Game Boy Color, CGB-CPU-01
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
    audio_output: Option<audio::output::AudioOutput>,
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

impl GB {
    pub fn new(hardware: Hardware) -> Self {
        GB {
            cpu: cpu::SM83::new(),
            mmio: memory::mmio::Mmio::new(),
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
        self.mmio.write(crate::ppu::SCX, 0x00);
        self.mmio.write(crate::ppu::WX, 0x00);
        self.mmio.write(crate::ppu::SCY, 0x00);
        self.mmio.write(crate::ppu::WY, 0x00);
        self.mmio.write(crate::input::JOYP, 0xCF);
        self.mmio.write(crate::ppu::LYC, 0x00);
        self.mmio.write(crate::ppu::BGP, 0xFC);
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
            Hardware::DMG0 | Hardware::DMG | Hardware::MGB | Hardware::CGB => 0xF1,
            Hardware::SGB | Hardware::SGB2 => 0xF0,
        });
        self.mmio.write(crate::timer::TIMA, 0x00);
        self.mmio.write(crate::timer::TMA, 0x00);
        self.mmio.write(crate::timer::TAC, 0xF8);
        self.mmio.write(crate::timer::DIV, match self.hardware {
            Hardware::DMG | Hardware::MGB | Hardware::SGB | Hardware::SGB2 | Hardware::CGB => 0xAB,
            Hardware::DMG0 => 0x18,
        });

        self.cpu.registers.a = match self.hardware {
            Hardware::DMG0 | Hardware::DMG | Hardware::SGB => 0x01,
            Hardware::MGB | Hardware::SGB2 => 0xFF,
            Hardware::CGB => 0x11,
        };
        self.cpu.registers.b = match self.hardware {
            Hardware::CGB | Hardware::DMG | Hardware::MGB | Hardware::SGB | Hardware::SGB2 => 0x00,
            Hardware::DMG0 => 0xFF,
        };
        self.cpu.registers.c = match self.hardware {
            Hardware::CGB => 0x00,
            Hardware::DMG0 | Hardware::DMG | Hardware::MGB => 0x13,
            Hardware::SGB | Hardware::SGB2 => 0x14,
        };
        self.cpu.registers.d = match self.hardware {
            Hardware::CGB => 0xFF,
            Hardware::SGB | Hardware::SGB2 | Hardware::DMG0 | Hardware::DMG | Hardware::MGB => 0x00,
        };
        self.cpu.registers.e = match self.hardware {
            Hardware::DMG | Hardware::MGB => 0xD8,
            Hardware::DMG0 => 0xC1,
            Hardware::SGB | Hardware::SGB2 => 0x00,
            Hardware::CGB => 0x56,
        };
        self.cpu.registers.h = match self.hardware {
            Hardware::CGB => 0x00,
            Hardware::DMG0 => 0x84,
            Hardware::DMG | Hardware::MGB => 0x01,
            Hardware::SGB | Hardware::SGB2 => 0xC0,
        };
        self.cpu.registers.l = match self.hardware {
            Hardware::CGB => 0x0D,
            Hardware::DMG0 => 0x03,
            Hardware::DMG | Hardware::MGB => 0x4D,
            Hardware::SGB | Hardware::SGB2 => 0x60,
        };
        self.cpu.registers.set_flag(registers::Flag::Zero, match self.hardware {
            Hardware::DMG | Hardware::CGB | Hardware::MGB => true,
            Hardware::DMG0 | Hardware::SGB | Hardware::SGB2 => false,
        });
        self.cpu.registers.set_flag(registers::Flag::Negative, false);
        self.cpu.registers.set_flag(registers::Flag::HalfCarry, match self.hardware {
            Hardware::DMG0 | Hardware::SGB | Hardware::SGB2 | Hardware::CGB => false,
            Hardware::DMG | Hardware::MGB => self.mmio.read(0x014D) == 0x00,
        });
        self.cpu.registers.set_flag(registers::Flag::Carry, match self.hardware {
            Hardware::DMG0 | Hardware::SGB | Hardware::SGB2 | Hardware::CGB => false,
            Hardware::DMG | Hardware::MGB => self.mmio.read(0x014D) == 0x00,
        });
        self.mmio.write(crate::memory::mmio::REG_BOOT_OFF, 1);
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
            // CGB cartridge on CGB hardware - always OK
            (Hardware::CGB, _) => Ok(()),
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
        if !matches!(self.hardware, Hardware::CGB) {
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

    // Audio management methods
    pub fn enable_audio(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let mut audio_output = audio::output::AudioOutput::new()?;
        audio_output.start()?;
        self.audio_output = Some(audio_output);
        println!("Audio output enabled");
        Ok(())
    }

    pub fn step_instruction(&mut self, collect_audio: bool) -> (bool, u8) {
        // Check for breakpoint at current PC before executing
        let pc = self.cpu.registers.pc;
        if self.breakpoints.contains(&pc) {
            // Breakpoint hit - don't execute instruction and return (empty audio, breakpoint hit)
            return (true, 0);
        }

        // Execute one CPU instruction and step PPU accordingly
        let cycles = self.cpu.step(&mut self.mmio);
        for _ in 0..cycles {
            self.mmio.step_timer(&mut self.cpu);
            self.mmio.step_dma();
            self.mmio.step_audio();
            self.ppu.step(&mut self.cpu, &mut self.mmio);
        }
        
        // Generate audio samples if requested
        let audio_samples = if collect_audio {
            self.mmio.generate_audio_samples(cycles as u32)
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
        // Normal frame should be 70224 cycles (154 scanlines × 456 cycles)
        // If we exceed this, we assume PPU is disabled or stuck
        // and return to avoid audio buildup
        const MAX_CYCLES_PER_FRAME: u32 = 70224;
        
        loop {
            let (breakpoint_hit, cycles) = self.step_instruction(collect_audio);
            cpu_cycles_this_frame += cycles as u32;
            
            if breakpoint_hit {
                // Breakpoint hit - return current frame and indicate breakpoint hit
                return (self.ppu.get_frame(&self.mmio), true);
            }
            
            // Check if PPU has completed a frame
            if self.ppu.frame_ready() {
                return (self.ppu.get_frame(&self.mmio), false);
            }
            
            // If PPU is disabled or taking too long, cap the cycles to prevent audio buildup
            if cpu_cycles_this_frame >= MAX_CYCLES_PER_FRAME {
                // PPU disabled or stuck - return after reasonable cycle count to maintain timing
                return (self.ppu.get_frame(&self.mmio), false);
            }
        }
    }

    pub fn get_current_frame(&mut self) -> Frame {
        self.ppu.get_frame(&self.mmio)
    }

    pub fn get_cpu_registers(&self) -> &cpu::registers::Registers {
        &self.cpu.registers
    }

    pub fn get_ppu_debug_info(&self) -> (&ppu::Ppu, [u8; 8]) {
        (&self.ppu, self.ppu.get_fetcher_pixel_buffer())
    }

    pub fn read_memory(&self, address: u16) -> u8 {
        self.mmio.read(address)
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
