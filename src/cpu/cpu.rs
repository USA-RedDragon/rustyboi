use crate::{cpu::opcodes, cpu::registers, memory, memory::Addressable};


pub struct SM83 {
    pub registers: registers::Registers,
}

impl SM83 {
    pub fn new() -> Self {
        SM83 { registers: registers::Registers::new() }
    }

    pub fn step(&mut self, mmio: &mut memory::mmio::MMIO) -> u8 {
        let opcode = mmio.read(self.registers.pc);
        self.registers.pc += 1;
        self.execute(opcode, mmio)
    }

    fn execute(&mut self, opcode: u8, mmio: &mut memory::mmio::MMIO) -> u8 {
        match opcode {
            0x00 => opcodes::nop(self, mmio),
            0x01 => opcodes::ld_bc_imm(self, mmio),
            0x04 => opcodes::inc_b(self, mmio),
            0x05 => opcodes::dec_b(self, mmio),
            0x06 => opcodes::ld_b_imm(self, mmio),
            0x0C => opcodes::inc_c(self, mmio),
            0x0D => opcodes::dec_c(self, mmio),
            0x0E => opcodes::ld_c_imm(self, mmio),
            0x11 => opcodes::ld_de_imm(self, mmio),
            0x14 => opcodes::inc_d(self, mmio),
            0x15 => opcodes::dec_d(self, mmio),
            0x16 => opcodes::ld_d_imm(self, mmio),
            0x1C => opcodes::inc_e(self, mmio),
            0x1D => opcodes::dec_e(self, mmio),
            0x1E => opcodes::ld_e_imm(self, mmio),
            0x20 => opcodes::jr_nz_imm(self, mmio),
            0x21 => opcodes::ld_hl_imm(self, mmio),
            0x24 => opcodes::inc_h(self, mmio),
            0x25 => opcodes::dec_h(self, mmio),
            0x26 => opcodes::ld_h_imm(self, mmio),
            0x28 => opcodes::jr_z_imm(self, mmio),
            0x2C => opcodes::inc_l(self, mmio),
            0x2D => opcodes::dec_l(self, mmio),
            0x2E => opcodes::ld_l_imm(self, mmio),
            0x30 => opcodes::jr_nc_imm(self, mmio),
            0x32 => opcodes::ld_memory_hl_dec_a(self, mmio),
            0x33 => opcodes::inc_sp(self, mmio),
            0x34 => opcodes::inc_memory_hl(self, mmio),
            0x38 => opcodes::jr_c_imm(self, mmio),
            0x3B => opcodes::dec_sp(self, mmio),
            0x3C => opcodes::inc_a(self, mmio),
            0x3D => opcodes::dec_a(self, mmio),
            0x3E => opcodes::ld_a_imm(self, mmio),
            0xA0 => opcodes::and_b(self, mmio),
            0xA1 => opcodes::and_c(self, mmio),
            0xA2 => opcodes::and_d(self, mmio),
            0xA3 => opcodes::and_e(self, mmio),
            0xA4 => opcodes::and_h(self, mmio),
            0xA5 => opcodes::and_l(self, mmio),
            0xA7 => opcodes::and_a(self, mmio),
            0xA8 => opcodes::xor_b(self, mmio),
            0xA9 => opcodes::xor_c(self, mmio),
            0xAA => opcodes::xor_d(self, mmio),
            0xAB => opcodes::xor_e(self, mmio),
            0xAC => opcodes::xor_h(self, mmio),
            0xAD => opcodes::xor_l(self, mmio),
            0xAF => opcodes::xor_a(self, mmio),
            0xC3 => opcodes::jp_imm(self, mmio),
            0xCB => self.execute_cb(mmio),
            0xDE => opcodes::sbc_a_imm(self, mmio),
            0xE0 => opcodes::ldh_memory_imm_a(self, mmio),
            0xF0 => opcodes::ldh_a_memory_imm(self, mmio),
            0xF1 => opcodes::pop_af(self, mmio),
            0xF3 => opcodes::di(self, mmio),
            0xFB => opcodes::ei(self, mmio),
            0xFE => opcodes::cp_imm(self, mmio),
            _ => unimplemented!("Opcode 0x{:02X} not implemented at PC 0x{:04X}", opcode, self.registers.pc - 1),
        }
    }

    fn execute_cb(&mut self, mmio: &mut memory::mmio::MMIO) -> u8 {
        let opcode = mmio.read(self.registers.pc);
        self.registers.pc += 1;
        match opcode {
            _ => unimplemented!("CB Opcode 0x{:02X} not implemented at PC 0x{:04X}", opcode, self.registers.pc - 1),
        }
    }
}
