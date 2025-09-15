use crate::{cpu::opcodes, memory, memory::Addressable};

pub struct SM83 {
    pub r_a: u8, // A, accumulator register
    pub r_f: u8, // F, flags register
    pub r_b: u8, // B
    pub r_c: u8, // C
    pub r_d: u8, // D
    pub r_e: u8, // E
    pub r_h: u8, // H
    pub r_l: u8, // L

    // 16-bit registers
    pub r_pc: u16, // PC (Program Counter)
    pub r_sp: u16, // SP (Stack Pointer)
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
        let (opcode, length) = Self::fetch(self.r_pc, mmio);
        self.r_pc = self.r_pc.wrapping_add(length as u16);
        (opcode.execute)(self, mmio);
        println!("Executed opcode: {}\tCycles: {}", opcode.name, opcode.cycles);
    }

    fn fetch(pc: u16, mmio: &memory::mmio::MMIO) -> (&opcodes::Opcode, u8) {
        let opcode = mmio.read(pc);
        let mut opcode_obj = &opcodes::OPCODES[opcode as usize];
        let mut length = opcode_obj.length;
        if opcode == 0xCB { // Special CB-prefixed opcodes
            opcode_obj = &opcodes::CB_OPCODES[mmio.read(pc + 1) as usize];
            length += opcode_obj.length;
        }
        (opcode_obj, length)
    }
}
