use crate::{cpu::opcodes, cpu::registers, memory, memory::Addressable};


pub struct SM83 {
    pub registers: registers::Registers,
}

impl SM83 {
    pub fn new() -> Self {
        SM83 { registers: registers::Registers::new() }
    }

    pub fn step(&mut self, mmio: &mut memory::mmio::MMIO) {
        let pc = self.registers.get(registers::Register::PC);
        let opcode = mmio.read(pc);
        let mut opcode_obj = &opcodes::OPCODES[opcode as usize];
        let mut length = opcode_obj.length;

        if opcode == 0xCB { // Special CB-prefixed opcodes
            opcode_obj = &opcodes::CB_OPCODES[mmio.read(pc + 1) as usize];
            length += opcode_obj.length;
        }

        self.registers.set(registers::Register::PC, pc.wrapping_add(length as u16));
        println!("Executed opcode: {}\tCycles: {}", opcode_obj.name, opcode_obj.cycles);

        (opcode_obj.execute)(self, mmio);
    }

}
