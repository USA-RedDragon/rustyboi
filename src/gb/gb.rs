use crate::cartridge;
use crate::cpu;
use crate::cpu::registers;
use crate::memory;
use crate::memory::Addressable;
use crate::ppu;

use std::time::{Duration, Instant};

const CPU_FREQ: u128 = 4_194_304; // Hz
const NANO: u128 = 1_000_000_000u128;
const BATCH_CYCLES: u64 = 500;   // batch size
const BUSY_WAIT_NS: u128 = 50_000; // 50 µs busy-wait

pub type DisplayCallback = Box<dyn Fn(&[u8; ppu::FRAMEBUFFER_SIZE])>;

pub struct GB {
    cpu: cpu::SM83,
    mmio: memory::mmio::MMIO,
    ppu: ppu::PPU,
    display_callback: Option<DisplayCallback>,
}

impl GB {
    pub fn new() -> Self {
        GB {
            cpu: cpu::SM83::new(),
            mmio: memory::mmio::MMIO::new(),
            ppu: ppu::PPU::new(),
            display_callback: None,
        }
    }

    pub fn set_display_callback(&mut self, display: DisplayCallback) {
        self.display_callback = Some(display);
    }

    pub fn insert(&mut self, cartridge: cartridge::Cartridge) {
        self.mmio.insert_cartridge(cartridge);
        self.cpu.registers.reset(true);
        if self.mmio.read(0x014D) == 0x00 {
            self.cpu.registers.set_flag(registers::Flag::Carry, true);
            self.cpu.registers.set_flag(registers::Flag::HalfCarry, true);
            println!("Warning: ROM without header checksum");
        }
    }

    pub fn load_bios(&mut self, path: &str) -> Result<(), std::io::Error> {
        self.mmio.load_bios(path)?;
        self.cpu.registers.reset(false);
        Ok(())
    }

    /// Helper method that runs the CPU and PPU for one batch and handles timing.
    /// Returns the number of cycles executed in this batch.
    fn step_batch(&mut self, total_cycles: &mut u128, start_time: &Instant) -> u64 {
        let mut batch_cycles = 0;

        // Step CPU until batch is filled
        while batch_cycles < BATCH_CYCLES {
            let cycles = self.cpu.step(&mut self.mmio) as u64;
            batch_cycles += cycles;

            // Check for events mid-batch
            let next_event = self.ppu.next_event_in_cycles();
            if batch_cycles > next_event {
                let excess = batch_cycles - next_event;
                batch_cycles -= excess;
                break;
            }
        }

        // Advance hardware by batch cycles
        for _ in 0..batch_cycles {
            self.ppu.step(&mut self.cpu, &mut self.mmio);
        }

        *total_cycles += batch_cycles as u128;

        // Sleep + busy-wait to maintain 4.194 MHz
        let target_ns = (*total_cycles * NANO) / CPU_FREQ;
        let elapsed_ns = start_time.elapsed().as_nanos();

        if target_ns > elapsed_ns {
            let remaining_ns = target_ns - elapsed_ns;

            if remaining_ns > BUSY_WAIT_NS {
                std::thread::sleep(Duration::from_nanos((remaining_ns - BUSY_WAIT_NS) as u64));
            }

            while start_time.elapsed().as_nanos() < target_ns {}
        }

        batch_cycles
    }

    pub fn run_until_frame(&mut self) -> [u8; ppu::FRAMEBUFFER_SIZE] {
        let mut total_cycles: u128 = 0;
        let start_time = Instant::now();

        loop {
            self.step_batch(&mut total_cycles, &start_time);

            // Render frame if ready
            if self.ppu.frame_ready() {
                return self.ppu.get_frame();
            }
        }
    }

    pub fn run(&mut self) {
        let mut total_cycles: u128 = 0;
        let start_time = Instant::now();

        loop {
            self.step_batch(&mut total_cycles, &start_time);

            // Render frame if ready
            if self.ppu.frame_ready() {
                if let Some(display) = &mut self.display_callback {
                    display(&self.ppu.get_frame());
                }
            }
        }
    }
}
