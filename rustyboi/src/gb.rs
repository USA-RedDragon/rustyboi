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
#[derive(Serialize, Deserialize)]
pub struct GB {
    cpu: cpu::SM83,
    mmio: memory::mmio::Mmio,
    ppu: ppu::Ppu,
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
            skip_bios: self.skip_bios,
            breakpoints: self.breakpoints.clone(),
            audio_output: None, // Don't clone audio output - it will be recreated if needed
        }
    }
}

impl GB {
    pub fn new(skip_bios: bool) -> Self {
        let mut cpu = cpu::SM83::new();
        cpu.registers.reset(skip_bios);
        let mut mmio = memory::mmio::Mmio::new();
        if skip_bios {
            mmio.write(crate::memory::mmio::REG_BOOT_OFF, 1);
        }
        GB {
            cpu,
            mmio,
            ppu: ppu::Ppu::new(),
            skip_bios,
            breakpoints: HashSet::new(),
            audio_output: None, // Audio will be enabled when needed
        }
    }

    pub fn insert(&mut self, cartridge: cartridge::Cartridge) {
        self.mmio.insert_cartridge(cartridge);
        if self.mmio.read(0x014D) == 0x00 {
            self.cpu.registers.set_flag(registers::Flag::Carry, true);
            self.cpu.registers.set_flag(registers::Flag::HalfCarry, true);
            println!("Warning: ROM without header checksum");
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

    pub fn run_until_frame(&mut self, collect_audio: bool) -> ([u8; ppu::FRAMEBUFFER_SIZE], bool) {
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
                return (self.ppu.get_frame(), true);
            }
            
            // Check if PPU has completed a frame
            if self.ppu.frame_ready() {
                return (self.ppu.get_frame(), false);
            }
            
            // If PPU is disabled or taking too long, cap the cycles to prevent audio buildup
            if cpu_cycles_this_frame >= MAX_CYCLES_PER_FRAME {
                // PPU disabled or stuck - return after reasonable cycle count to maintain timing
                return (self.ppu.get_frame(), false);
            }
        }
    }

    pub fn get_current_frame(&mut self) -> [u8; ppu::FRAMEBUFFER_SIZE] {
        self.ppu.get_frame()
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
        self.cpu.registers.reset(self.skip_bios);
        if self.skip_bios {
            self.mmio.write(crate::memory::mmio::REG_BOOT_OFF, 1);
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
