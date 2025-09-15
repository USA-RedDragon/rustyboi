use crate::cpu;
use crate::memory;

pub struct GB {
    cpu: cpu::SM83,
    mmio: memory::mmio::MMIO,
}

impl GB {
    pub fn new() -> Self {
        GB {
            cpu: cpu::SM83::new(),
            mmio: memory::mmio::MMIO::new(),
        }
    }

    pub fn step(&mut self) {
        self.cpu.step(&mut self.mmio);
    }
}
