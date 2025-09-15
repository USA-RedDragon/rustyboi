use crate::memory;
use crate::memory::Addressable;

pub struct SM83 {
    r_a: u8, // A, accumulator register
	r_f: u8, // F, flags register
	r_b: u8, // B
	r_c: u8, // C
	r_d: u8, // D
	r_e: u8, // E
	r_h: u8, // H
	r_l: u8, // L

	// 16-bit registers
	r_pc: u16, // PC (Program Counter)
	r_sp: u16, // SP (Stack Pointer)
}

impl SM83 {
    pub fn new() -> Self {
        SM83 {
            r_a: 0,
            r_f: 0,
            r_b: 0,
            r_c: 0,
            r_d: 0,
            r_e: 0,
            r_h: 0,
            r_l: 0,
            r_pc: 0,
            r_sp: 0,
        }
    }

    pub fn step(&mut self, mmio: &mut memory::mmio::MMIO) {
        let opcode = self.fetch(mmio);
        // Decode and execute the opcode
        self.execute(opcode);
    }

    fn fetch(&mut self, mmio: &mut memory::mmio::MMIO) -> u8 {
        let opcode = mmio.read(self.r_pc);
        self.r_pc = self.r_pc.wrapping_add(1);
        opcode
    }

    fn execute(&mut self, opcode: u8) -> u8 {
        match opcode {
            0x00 => self.nop(),
            // Add more opcodes here
            _ => unimplemented!("Opcode {:02X} not implemented", opcode),
        }
    }

    fn nop(&self) -> u8 {
        1 // NOP takes 1 machine cycle
    }
}
