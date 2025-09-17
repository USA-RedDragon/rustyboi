use crate::{cpu::opcodes, cpu::registers, memory, memory::Addressable};


pub struct SM83 {
    pub registers: registers::Registers,
}

impl SM83 {
    pub fn new() -> Self {
        SM83 { registers: registers::Registers::new() }
    }

    pub fn step(&mut self, mmio: &mut memory::mmio::MMIO) -> u8 {
        let mut cycles = 0;
        if self.registers.ime && mmio.read(registers::INTERRUPT_FLAG) != 0 {
            let mut interrupt: Option<registers::InterruptFlag> = None;

            if self.get_interrupt_enable_flag(registers::InterruptFlag::Joypad, mmio) && self.get_interrupt_flag(registers::InterruptFlag::Joypad, mmio) {
                interrupt = Some(registers::InterruptFlag::Joypad);
            } else if self.get_interrupt_enable_flag(registers::InterruptFlag::Serial, mmio) && self.get_interrupt_flag(registers::InterruptFlag::Serial, mmio) {
                interrupt = Some(registers::InterruptFlag::Serial);
            } else if self.get_interrupt_enable_flag(registers::InterruptFlag::Timer, mmio) && self.get_interrupt_flag(registers::InterruptFlag::Timer, mmio) {
                interrupt = Some(registers::InterruptFlag::Timer);
            } else if self.get_interrupt_enable_flag(registers::InterruptFlag::LCD, mmio) && self.get_interrupt_flag(registers::InterruptFlag::LCD, mmio) {
                interrupt = Some(registers::InterruptFlag::LCD);
            } else if self.get_interrupt_enable_flag(registers::InterruptFlag::VBlank, mmio) && self.get_interrupt_flag(registers::InterruptFlag::VBlank, mmio) {
                interrupt = Some(registers::InterruptFlag::VBlank);
            }

            if let Some(flag) = interrupt {
                self.registers.ime = false;
                self.registers.set_flag(registers::Flag::Carry, false);
                self.registers.set_flag(registers::Flag::HalfCarry, false);
                self.registers.set_flag(registers::Flag::Negative, false);
                self.registers.set_flag(registers::Flag::Zero, false);

                self.registers.sp -= 2;
                mmio.write(self.registers.sp, (self.registers.pc & 0x00FF) as u8);
                mmio.write(self.registers.sp + 1, (self.registers.pc >> 8) as u8);
                self.registers.pc = match flag {
                    registers::InterruptFlag::VBlank => 0x40,
                    registers::InterruptFlag::LCD => 0x48,
                    registers::InterruptFlag::Timer => 0x50,
                    registers::InterruptFlag::Serial => 0x58,
                    registers::InterruptFlag::Joypad => 0x60,
                };
                self.set_interrupt_flag(flag, false, mmio);
                cycles += 5; // Interrupt handling takes 5 extra cycles
            }
        }
        let opcode = mmio.read(self.registers.pc);
        self.registers.pc += 1;
        self.execute(opcode, mmio) + cycles
    }

    pub fn set_interrupt_flag(&mut self, flag: registers::InterruptFlag, value: bool, mmio: &mut memory::mmio::MMIO) {
        if self.registers.ime && value {
            mmio.write(registers::INTERRUPT_FLAG, mmio.read(registers::INTERRUPT_FLAG) | flag as u8);
        } else {
            mmio.write(registers::INTERRUPT_FLAG, mmio.read(registers::INTERRUPT_FLAG) & !(flag as u8));
        }
    }

    pub fn get_interrupt_flag(&self, flag: registers::InterruptFlag, mmio: &memory::mmio::MMIO) -> bool {
        (mmio.read(registers::INTERRUPT_FLAG) & (flag as u8)) != 0
    }

    pub fn get_interrupt_enable_flag(&self, flag: registers::InterruptFlag, mmio: &memory::mmio::MMIO) -> bool {
        (mmio.read(registers::INTERRUPT_ENABLE) & (flag as u8)) != 0
    }

    fn execute(&mut self, opcode: u8, mmio: &mut memory::mmio::MMIO) -> u8 {
        match opcode {
            0x00 => opcodes::nop(self, mmio),
            0x01 => opcodes::ld_bc_imm(self, mmio),
            0x02 => opcodes::ld_memory_bc_a(self, mmio),
            0x04 => opcodes::inc_b(self, mmio),
            0x05 => opcodes::dec_b(self, mmio),
            0x06 => opcodes::ld_b_imm(self, mmio),
            0x0B => opcodes::dec_bc(self, mmio),
            0x0C => opcodes::inc_c(self, mmio),
            0x0D => opcodes::dec_c(self, mmio),
            0x0E => opcodes::ld_c_imm(self, mmio),
            0x11 => opcodes::ld_de_imm(self, mmio),
            0x12 => opcodes::ld_memory_de_a(self, mmio),
            0x14 => opcodes::inc_d(self, mmio),
            0x15 => opcodes::dec_d(self, mmio),
            0x16 => opcodes::ld_d_imm(self, mmio),
            0x1B => opcodes::dec_de(self, mmio),
            0x1C => opcodes::inc_e(self, mmio),
            0x1D => opcodes::dec_e(self, mmio),
            0x1E => opcodes::ld_e_imm(self, mmio),
            0x20 => opcodes::jr_nz_imm(self, mmio),
            0x21 => opcodes::ld_hl_imm(self, mmio),
            0x24 => opcodes::inc_h(self, mmio),
            0x25 => opcodes::dec_h(self, mmio),
            0x26 => opcodes::ld_h_imm(self, mmio),
            0x28 => opcodes::jr_z_imm(self, mmio),
            0x2A => opcodes::ld_a_memory_hl_inc(self, mmio),
            0x2B => opcodes::dec_hl(self, mmio),
            0x2C => opcodes::inc_l(self, mmio),
            0x2D => opcodes::dec_l(self, mmio),
            0x2E => opcodes::ld_l_imm(self, mmio),
            0x2F => opcodes::cpl(self, mmio),
            0x30 => opcodes::jr_nc_imm(self, mmio),
            0x31 => opcodes::ld_sp_imm(self, mmio),
            0x32 => opcodes::ld_memory_hl_dec_a(self, mmio),
            0x33 => opcodes::inc_sp(self, mmio),
            0x34 => opcodes::inc_memory_hl(self, mmio),
            0x36 => opcodes::ld_memory_hl_imm(self, mmio),
            0x38 => opcodes::jr_c_imm(self, mmio),
            0x3B => opcodes::dec_sp(self, mmio),
            0x3C => opcodes::inc_a(self, mmio),
            0x3D => opcodes::dec_a(self, mmio),
            0x3E => opcodes::ld_a_imm(self, mmio),
            0x40 => opcodes::ld_b_b(self, mmio),
            0x41 => opcodes::ld_b_c(self, mmio),
            0x42 => opcodes::ld_b_d(self, mmio),
            0x43 => opcodes::ld_b_e(self, mmio),
            0x44 => opcodes::ld_b_h(self, mmio),
            0x45 => opcodes::ld_b_l(self, mmio),
            0x47 => opcodes::ld_b_a(self, mmio),
            0x48 => opcodes::ld_c_b(self, mmio),
            0x49 => opcodes::ld_c_c(self, mmio),
            0x4A => opcodes::ld_c_d(self, mmio),
            0x4B => opcodes::ld_c_e(self, mmio),
            0x4C => opcodes::ld_c_h(self, mmio),
            0x4D => opcodes::ld_c_l(self, mmio),
            0x4F => opcodes::ld_c_a(self, mmio),
            0x50 => opcodes::ld_d_b(self, mmio),
            0x51 => opcodes::ld_d_c(self, mmio),
            0x52 => opcodes::ld_d_d(self, mmio),
            0x53 => opcodes::ld_d_e(self, mmio),
            0x54 => opcodes::ld_d_h(self, mmio),
            0x55 => opcodes::ld_d_l(self, mmio),
            0x57 => opcodes::ld_d_a(self, mmio),
            0x58 => opcodes::ld_e_b(self, mmio),
            0x59 => opcodes::ld_e_c(self, mmio),
            0x5A => opcodes::ld_e_d(self, mmio),
            0x5B => opcodes::ld_e_e(self, mmio),
            0x5C => opcodes::ld_e_h(self, mmio),
            0x5D => opcodes::ld_e_l(self, mmio),
            0x5F => opcodes::ld_e_a(self, mmio),
            0x60 => opcodes::ld_h_b(self, mmio),
            0x61 => opcodes::ld_h_c(self, mmio),
            0x62 => opcodes::ld_h_d(self, mmio),
            0x63 => opcodes::ld_h_e(self, mmio),
            0x64 => opcodes::ld_h_h(self, mmio),
            0x65 => opcodes::ld_h_l(self, mmio),
            0x67 => opcodes::ld_h_a(self, mmio),
            0x68 => opcodes::ld_l_b(self, mmio),
            0x69 => opcodes::ld_l_c(self, mmio),
            0x6A => opcodes::ld_l_d(self, mmio),
            0x6B => opcodes::ld_l_e(self, mmio),
            0x6C => opcodes::ld_l_h(self, mmio),
            0x6D => opcodes::ld_l_l(self, mmio),
            0x6F => opcodes::ld_l_a(self, mmio),
            0x78 => opcodes::ld_a_b(self, mmio),
            0x79 => opcodes::ld_a_c(self, mmio),
            0x7A => opcodes::ld_a_d(self, mmio),
            0x7B => opcodes::ld_a_e(self, mmio),
            0x7C => opcodes::ld_a_h(self, mmio),
            0x7D => opcodes::ld_a_l(self, mmio),
            0x7F => opcodes::ld_a_a(self, mmio),
            0x80 => opcodes::add_b(self, mmio),
            0x81 => opcodes::add_c(self, mmio),
            0x82 => opcodes::add_d(self, mmio),
            0x83 => opcodes::add_e(self, mmio),
            0x84 => opcodes::add_h(self, mmio),
            0x85 => opcodes::add_l(self, mmio),
            0x86 => opcodes::add_memory_hl(self, mmio),
            0x87 => opcodes::add_a(self, mmio),
            0x90 => opcodes::sub_b(self, mmio),
            0x91 => opcodes::sub_c(self, mmio),
            0x92 => opcodes::sub_d(self, mmio),
            0x93 => opcodes::sub_e(self, mmio),
            0x94 => opcodes::sub_h(self, mmio),
            0x95 => opcodes::sub_l(self, mmio),
            0x96 => opcodes::sub_memory_hl(self, mmio),
            0x97 => opcodes::sub_a(self, mmio),
            0xA0 => opcodes::and_b(self, mmio),
            0xA1 => opcodes::and_c(self, mmio),
            0xA2 => opcodes::and_d(self, mmio),
            0xA3 => opcodes::and_e(self, mmio),
            0xA4 => opcodes::and_h(self, mmio),
            0xA5 => opcodes::and_l(self, mmio),
            0xA6 => opcodes::and_memory_hl(self, mmio),
            0xA7 => opcodes::and_a(self, mmio),
            0xA8 => opcodes::xor_b(self, mmio),
            0xA9 => opcodes::xor_c(self, mmio),
            0xAA => opcodes::xor_d(self, mmio),
            0xAB => opcodes::xor_e(self, mmio),
            0xAC => opcodes::xor_h(self, mmio),
            0xAD => opcodes::xor_l(self, mmio),
            0xAE => opcodes::xor_memory_hl(self, mmio),
            0xAF => opcodes::xor_a(self, mmio),
            0xB0 => opcodes::or_b(self, mmio),
            0xB1 => opcodes::or_c(self, mmio),
            0xB2 => opcodes::or_d(self, mmio),
            0xB3 => opcodes::or_e(self, mmio),
            0xB4 => opcodes::or_h(self, mmio),
            0xB5 => opcodes::or_l(self, mmio),
            0xB6 => opcodes::or_memory_hl(self, mmio),
            0xB7 => opcodes::or_a(self, mmio),
            0xC3 => opcodes::jp_imm(self, mmio),
            0xC9 => opcodes::ret(self, mmio),
            0xCB => self.execute_cb(mmio),
            0xCD => opcodes::call_imm(self, mmio),
            0xDE => opcodes::sbc_a_imm(self, mmio),
            0xE0 => opcodes::ldh_memory_imm_a(self, mmio),
            0xE2 => opcodes::ld_memory_c_a(self, mmio),
            0xEA => opcodes::ld_memory_imm_a_16(self, mmio),
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
