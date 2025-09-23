use crate::cartridge;
use crate::cpu;
use crate::cpu::registers;
use crate::memory;
use crate::memory::Addressable;
use crate::ppu;

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
}

impl Clone for GB {
    fn clone(&self) -> Self {
        GB {
            cpu: self.cpu.clone(),
            mmio: self.mmio.clone(),
            ppu: self.ppu.clone(),
            skip_bios: self.skip_bios,
            breakpoints: self.breakpoints.clone(),
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

    pub fn step_instruction(&mut self) {
        // Execute one CPU instruction and step PPU accordingly
        let cycles = self.cpu.step(&mut self.mmio);
        for _ in 0..cycles {
            self.mmio.step_timer(&mut self.cpu);
            self.mmio.step_dma();
            self.ppu.step(&mut self.cpu, &mut self.mmio);
        }
    }

    pub fn step_instruction_with_breakpoint_check(&mut self) -> bool {
        // Check for breakpoint at current PC before executing
        let pc = self.cpu.registers.pc;
        if self.breakpoints.contains(&pc) {
            // Breakpoint hit - don't execute instruction and return false to pause
            return false;
        }

        // No breakpoint, execute normally
        self.step_instruction();
        true // Continue execution
    }

    pub fn run_until_frame(&mut self) -> [u8; ppu::FRAMEBUFFER_SIZE] {
        loop {
            self.step_instruction();
            
            if self.ppu.frame_ready() {
                return self.ppu.get_frame();
            }
        }
    }

    pub fn run_until_frame_with_breakpoints(&mut self) -> ([u8; ppu::FRAMEBUFFER_SIZE], bool) {
        loop {
            if !self.step_instruction_with_breakpoint_check() {
                // Breakpoint hit - return current frame and indicate breakpoint hit
                return (self.ppu.get_frame(), true);
            }
            
            if self.ppu.frame_ready() {
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
