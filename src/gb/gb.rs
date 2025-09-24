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
    mmio: memory::mmio::MMIO,
    ppu: ppu::PPU,
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
        let mut mmio = memory::mmio::MMIO::new();
        if skip_bios {
            mmio.write(crate::memory::mmio::REG_BOOT_OFF, 1);
        }
        GB {
            cpu,
            mmio,
            ppu: ppu::PPU::new(),
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

    pub fn disable_audio(&mut self) {
        self.audio_output = None;
        println!("Audio output disabled");
    }

    pub fn is_audio_enabled(&self) -> bool {
        self.audio_output.is_some()
    }

    pub fn set_audio_volume(&mut self, volume: f32) {
        if let Some(audio_output) = &self.audio_output {
            audio_output.set_volume(volume);
        }
    }

    pub fn step_instruction(&mut self, collect_audio: bool) -> (Vec<(f32, f32)>, bool) {
        // Check for breakpoint at current PC before executing
        let pc = self.cpu.registers.pc;
        if self.breakpoints.contains(&pc) {
            // Breakpoint hit - don't execute instruction and return (empty audio, breakpoint hit)
            return (Vec::new(), true);
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
        if !audio_samples.is_empty() {
            if let Some(audio_output) = &self.audio_output {
                audio_output.add_samples(&audio_samples);
            }
        }
        
        (audio_samples, false) // No breakpoint hit
    }

    pub fn run_until_frame(&mut self, collect_audio: bool) -> ([u8; ppu::FRAMEBUFFER_SIZE], Vec<(f32, f32)>, bool) {
        let mut all_audio_samples = Vec::new();
        
        loop {
            let (audio_samples, breakpoint_hit) = self.step_instruction(collect_audio);
            
            if breakpoint_hit {
                // Breakpoint hit - return current frame and indicate breakpoint hit
                return (self.ppu.get_frame(), all_audio_samples, true);
            }
            
            if collect_audio {
                all_audio_samples.extend(&audio_samples);
            }
            
            if self.ppu.frame_ready() {
                return (self.ppu.get_frame(), all_audio_samples, false);
            }
        }
    }

    pub fn get_current_frame(&mut self) -> [u8; ppu::FRAMEBUFFER_SIZE] {
        self.ppu.get_frame()
    }

    pub fn get_cpu_registers(&self) -> &cpu::registers::Registers {
        &self.cpu.registers
    }

    pub fn get_ppu_debug_info(&self) -> (&ppu::PPU, [u8; 8]) {
        (&self.ppu, self.ppu.get_fetcher_pixel_buffer())
    }

    pub fn read_memory(&self, address: u16) -> u8 {
        self.mmio.read(address)
    }

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
    pub fn set_input_state(&mut self, a: bool, b: bool, start: bool, select: bool, up: bool, down: bool, left: bool, right: bool) {
        self.mmio.set_input_state(a, b, start, select, up, down, left, right);
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
