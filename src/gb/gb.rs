use crate::cartridge;
use crate::cpu;
use crate::cpu::registers;
use crate::memory;
use crate::memory::Addressable;
use crate::ppu;

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::time::{Duration, Instant};

#[derive(Serialize, Deserialize)]
pub struct GB {
    cpu: cpu::SM83,
    mmio: memory::mmio::MMIO,
    ppu: ppu::PPU,
    #[serde(skip, default)]
    skip_bios: bool,
    #[serde(skip, default = "Instant::now")]
    last_frame_time: Instant,
}

impl Clone for GB {
    fn clone(&self) -> Self {
        GB {
            cpu: self.cpu.clone(),
            mmio: self.mmio.clone(),
            ppu: self.ppu.clone(),
            skip_bios: self.skip_bios,
            last_frame_time: Instant::now(),
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
            last_frame_time: Instant::now(),
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
            self.ppu.step(&mut self.cpu, &mut self.mmio);
        }
    }

    pub fn run_until_frame(&mut self) -> Option<[u8; ppu::FRAMEBUFFER_SIZE]> {
        const TARGET_FRAME_TIME: Duration = Duration::from_micros(16750); // ~59.7 fps
        let now = Instant::now();
        let elapsed_since_last_frame = now.duration_since(self.last_frame_time);

        // Only update if enough time has passed
        if elapsed_since_last_frame < TARGET_FRAME_TIME {
            let remaining = TARGET_FRAME_TIME - elapsed_since_last_frame;
            // Sleep for most of the remaining time
            if remaining > Duration::from_micros(100) {
                std::thread::sleep(remaining - Duration::from_micros(50));
            }
            // Spin for precision
            while self.last_frame_time.elapsed() < TARGET_FRAME_TIME {
                std::hint::spin_loop();
            }
        }
        
        self.last_frame_time = Instant::now();

        // Catch panics from the Game Boy emulator
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Run CPU/PPU until a frame is ready - simple loop
            loop {
                self.step_instruction();
                
                if self.ppu.frame_ready() {
                    return self.ppu.get_frame();
                }
            }
        }));

        match result {
            Ok(frame_data) => Some(frame_data),
            Err(panic_info) => {
                // Convert panic info to a string for debugging
                let error_msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                    format!("Emulator panic: {}", s)
                } else if let Some(s) = panic_info.downcast_ref::<String>() {
                    format!("Emulator panic: {}", s)
                } else {
                    "Emulator panic: Unknown error".to_string()
                };

                println!("Game Boy emulator crashed: {}", error_msg);
                None
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
}
