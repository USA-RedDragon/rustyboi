use crate::cartridge;
use crate::cpu;
use crate::cpu::registers;
use crate::memory;
use crate::memory::Addressable;
use crate::ppu;

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;

pub type DisplayCallback = Box<dyn Fn(&[u8; ppu::FRAMEBUFFER_SIZE])>;

#[derive(Serialize, Deserialize)]
pub struct GB {
    cpu: cpu::SM83,
    mmio: memory::mmio::MMIO,
    ppu: ppu::PPU,
    #[serde(skip, default)]
    skip_bios: bool,
    #[serde(skip, default)]
    display_callback: Option<DisplayCallback>,
}

impl Clone for GB {
    fn clone(&self) -> Self {
        GB {
            cpu: self.cpu.clone(),
            mmio: self.mmio.clone(),
            ppu: self.ppu.clone(),
            skip_bios: self.skip_bios,
            display_callback: None,
        }
    }
}

impl GB {
    pub fn new(skip_bios: bool) -> Self {
        let mut cpu = cpu::SM83::new();
        cpu.registers.reset(skip_bios);
        GB {
            cpu,
            mmio: memory::mmio::MMIO::new(),
            ppu: ppu::PPU::new(),
            skip_bios,
            display_callback: None,
        }
    }

    pub fn set_display_callback(&mut self, display: DisplayCallback) {
        self.display_callback = Some(display);
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
            self.ppu.step(&mut self.cpu, &mut self.mmio);
        }
    }

    pub fn run_until_frame(&mut self) -> [u8; ppu::FRAMEBUFFER_SIZE] {
        // Run CPU/PPU until a frame is ready - simple loop
        loop {
            self.step_instruction();
            
            if self.ppu.frame_ready() {
                return self.ppu.get_frame();
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

    pub fn run(&mut self) {
        loop {
            let frame = self.run_until_frame();
            
            if let Some(display) = &mut self.display_callback {
                display(&frame);
            }
        }
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
        self.cpu.registers.reset(self.skip_bios);
    }
}
