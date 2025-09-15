use crate::{cpu::opcodes, memory, memory::Addressable};

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
        (self.fetch(mmio).execute)(self, mmio);
    }

    fn fetch(&mut self, mmio: &mut memory::mmio::MMIO) -> &opcodes::Opcode {
        let opcode = &opcodes::OPCODES[mmio.read(self.r_pc) as usize];
        self.r_pc = self.r_pc.wrapping_add(opcode.length.into());
        opcode
    }
}
