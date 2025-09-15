use crate::memory;

const VRAM_START: u16 = 0x8000;
const VRAM_SIZE: usize = 8192; // 8KB
const VRAM_END: u16 = VRAM_START + VRAM_SIZE as u16 - 1;
const RAM_START: u16 = 0xA000;
const RAM_SIZE: usize = 8192; // 8KB
const RAM_END: u16 = RAM_START + RAM_SIZE as u16 - 1;
const WRAM_START: u16 = 0xC000;
const WRAM_SIZE: usize = 4096; // 4KB
const WRAM_END: u16 = WRAM_START + WRAM_SIZE as u16 - 1;

pub struct MMIO {
    vram: memory::Memory<VRAM_START, VRAM_SIZE>,
    ram: memory::Memory<RAM_START, RAM_SIZE>,
    wram: memory::Memory<WRAM_START, WRAM_SIZE>,
}

impl MMIO {
    pub fn new() -> Self {
        MMIO {
            vram: memory::Memory::new(),
            ram: memory::Memory::new(),
            wram: memory::Memory::new(),
        }
    }
}

impl memory::Addressable for MMIO {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            VRAM_START..=VRAM_END => self.vram.read(addr),
            RAM_START..=RAM_END => self.ram.read(addr),
            WRAM_START..=WRAM_END => self.wram.read(addr),
            _ => panic!("Read from unmapped address: {:04X}", addr),
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            VRAM_START..=VRAM_END => self.vram.write(addr, value),
            RAM_START..=RAM_END => self.ram.write(addr, value),
            WRAM_START..=WRAM_END => self.wram.write(addr, value),
            _ => panic!("Write to unmapped address: {:04X}", addr),
        }
    }
}
