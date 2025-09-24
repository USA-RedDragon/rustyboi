use crate::{cpu, cpu::registers, memory, memory::Addressable};

pub fn nop(_cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
    4
}

pub fn stop(_cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
    unimplemented!("STOP instruction is not implemented");
}

pub fn undefined(_cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
    panic!("Attempted to execute an undefined opcode");
}

pub fn dec_memory_hl(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let old_value = mmio.read(addr);
    let new_value = old_value.wrapping_sub(1);
    mmio.write(addr, new_value);
    cpu.registers.set_flag(registers::Flag::Zero, new_value == 0);
    cpu.registers.set_flag(registers::Flag::Negative, true);
    cpu.registers.set_flag(registers::Flag::HalfCarry, (old_value & 0x0F) == 0x00);
    12
}

pub fn rlca(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
    let old_carry = (cpu.registers.a & 0x80) >> 7;
    cpu.registers.a = (cpu.registers.a << 1) | old_carry;
    cpu.registers.set_flag(registers::Flag::Zero, false);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, old_carry == 1);
    4
}

pub fn adc_a_memory_hl(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let value = mmio.read(addr);
    let carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1 } else { 0 };
    let a = cpu.registers.a;
    let result = (a as u16) + (value as u16) + (carry as u16);
    
    cpu.registers.a = (result & 0xFF) as u8;
    cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.a == 0);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::Carry, result > 0xFF);
    cpu.registers.set_flag(registers::Flag::HalfCarry, ((a & 0x0F) + (value & 0x0F) + carry) > 0x0F);
    8
}

pub fn rlc_hl(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let old_value = mmio.read(addr);
    let new_value = (old_value << 1) | ((old_value & 0x80) >> 7);
    mmio.write(addr, new_value);
    cpu.registers.set_flag(registers::Flag::Zero, new_value == 0);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, (old_value & 0x80) != 0);
    16
}

pub fn rrc_hl(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let old_value = mmio.read(addr);
    let new_value = (old_value >> 1) | ((old_value & 0x01) << 7);
    mmio.write(addr, new_value);
    cpu.registers.set_flag(registers::Flag::Zero, new_value == 0);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, (old_value & 0x01) != 0);
    16
}

pub fn rl_hl(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let old_value = mmio.read(addr);
    let old_carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1 } else { 0 };
    let new_value = (old_value << 1) | old_carry;
    mmio.write(addr, new_value);
    cpu.registers.set_flag(registers::Flag::Zero, new_value == 0);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, (old_value & 0x80) != 0);
    16
}

pub fn rr_hl(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let old_value = mmio.read(addr);
    let old_carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1 } else { 0 };
    let new_value = (old_value >> 1) | (old_carry << 7);
    mmio.write(addr, new_value);
    cpu.registers.set_flag(registers::Flag::Zero, new_value == 0);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, (old_value & 0x01) != 0);
    16
}

pub fn sla_hl(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let old_value = mmio.read(addr);
    let new_value = old_value << 1;
    mmio.write(addr, new_value);
    cpu.registers.set_flag(registers::Flag::Zero, new_value == 0);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, (old_value & 0x80) != 0);
    16
}

pub fn sra_hl(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let old_value = mmio.read(addr);
    let new_value = (old_value >> 1) | (old_value & 0x80);
    mmio.write(addr, new_value);
    cpu.registers.set_flag(registers::Flag::Zero, new_value == 0);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, (old_value & 0x01) != 0);
    16
}

pub fn srl_hl(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let old_value = mmio.read(addr);
    let new_value = old_value >> 1;
    mmio.write(addr, new_value);
    cpu.registers.set_flag(registers::Flag::Zero, new_value == 0);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, (old_value & 0x01) != 0);
    16
}

pub fn swap_hl(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let old_value = mmio.read(addr);
    let new_value = (old_value << 4) | (old_value.rotate_right(4));
    mmio.write(addr, new_value);
    cpu.registers.set_flag(registers::Flag::Zero, new_value == 0);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, false);
    16
}

pub fn daa(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
    let mut a = cpu.registers.a;
    let mut adjust = 0;
    let mut carry = cpu.registers.get_flag(registers::Flag::Carry);

    if cpu.registers.get_flag(registers::Flag::HalfCarry) || (!cpu.registers.get_flag(registers::Flag::Negative) && (a & 0x0F) > 0x09) {
        adjust |= 0x06;
    }
    if carry || (!cpu.registers.get_flag(registers::Flag::Negative) && a > 0x99) {
        adjust |= 0x60;
        carry = true;
    }

    if cpu.registers.get_flag(registers::Flag::Negative) {
        a = a.wrapping_sub(adjust);
    } else {
        a = a.wrapping_add(adjust);
    }

    cpu.registers.a = a;
    cpu.registers.set_flag(registers::Flag::Zero, a == 0);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, carry);
    4
}

pub fn jp_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let low = mmio.read(cpu.registers.pc) as u16;
    let high = mmio.read(cpu.registers.pc + 1) as u16;
    let addr = (high << 8) | low;
    cpu.registers.pc = addr;
    16
}

pub fn jr_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let offset = mmio.read(cpu.registers.pc) as i8;
    cpu.registers.pc += 1;
    cpu.registers.pc = ((cpu.registers.pc as i16) + (offset as i16)) as u16;
    12
}

pub fn rrca(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
    let old_carry = cpu.registers.a & 0x01;
    cpu.registers.a = (cpu.registers.a >> 1) | (old_carry << 7);
    cpu.registers.set_flag(registers::Flag::Zero, false);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, old_carry == 1);
    4
}

pub fn ld_memory_imm_16_sp(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let low = mmio.read(cpu.registers.pc) as u16;
    let high = mmio.read(cpu.registers.pc + 1) as u16;
    let addr = (high << 8) | low;
    cpu.registers.pc += 2;
    mmio.write(addr, (cpu.registers.sp & 0x00FF) as u8);
    mmio.write(addr + 1, ((cpu.registers.sp & 0xFF00) >> 8) as u8);
    20
}

pub fn add_sp_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let offset = mmio.read(cpu.registers.pc) as i8;
    cpu.registers.pc += 1;
    let sp = cpu.registers.sp;
    let result = (sp as i16).wrapping_add(offset as i16) as u16;
    cpu.registers.sp = result;

    cpu.registers.set_flag(registers::Flag::Zero, false);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::Carry, ((sp & 0xFF) + (offset as u16 & 0xFF)) > 0xFF);
    cpu.registers.set_flag(registers::Flag::HalfCarry, ((sp & 0x0F) + (offset as u16 & 0x0F)) > 0x0F);
    16
}

pub fn sbc_a_memory_hl(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let value = mmio.read(addr);
    let carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1 } else { 0 };
    let a = cpu.registers.a;
    let result = (a as i16) - (value as i16) - (carry as i16);
    
    cpu.registers.a = (result & 0xFF) as u8;
    cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.a == 0);
    cpu.registers.set_flag(registers::Flag::Negative, true);
    cpu.registers.set_flag(registers::Flag::Carry, result < 0);
    cpu.registers.set_flag(registers::Flag::HalfCarry, ((a & 0x0F) as i16 - (value & 0x0F) as i16 - (carry as i16)) < 0);
    8
}

pub fn halt(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
    cpu.halted = true;
    4
}

pub fn ld_hl_sp_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let offset = mmio.read(cpu.registers.pc) as i8;
    cpu.registers.pc += 1;
    let sp = cpu.registers.sp;
    let result = (sp as i16).wrapping_add(offset as i16) as u16;
    
    cpu.registers.h = ((result & 0xFF00) >> 8) as u8;
    cpu.registers.l = (result & 0xFF) as u8;

    cpu.registers.set_flag(registers::Flag::Zero, false);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::Carry, ((sp & 0xFF) + (offset as u16 & 0xFF)) > 0xFF);
    cpu.registers.set_flag(registers::Flag::HalfCarry, ((sp & 0x0F) + (offset as u16 & 0x0F)) > 0x0F);
    12
}

pub fn ld_sp_hl(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
    let hl = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    cpu.registers.sp = hl;
    8
}

pub fn inc_sp(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
    cpu.registers.sp = cpu.registers.sp.wrapping_add(1);
    // INC SP does not affect any flags
    8
}

pub fn dec_sp(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
    cpu.registers.sp = cpu.registers.sp.wrapping_sub(1);
    // DEC SP does not affect any flags
    8
}

pub fn rra(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
    let old_carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1 } else { 0 };
    let new_carry = cpu.registers.a & 0x01;
    cpu.registers.a = (cpu.registers.a >> 1) | (old_carry << 7);
    cpu.registers.set_flag(registers::Flag::Zero, false);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, new_carry == 1);
    4
}

pub fn adc_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let value = mmio.read(cpu.registers.pc);
    cpu.registers.pc += 1;
    let carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1 } else { 0 };
    let a = cpu.registers.a;
    let result = (a as u16) + (value as u16) + (carry as u16);
    
    cpu.registers.a = (result & 0xFF) as u8;
    cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.a == 0);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::Carry, result > 0xFF);
    cpu.registers.set_flag(registers::Flag::HalfCarry, ((a & 0x0F) + (value & 0x0F) + carry) > 0x0F);
    8
}

pub fn xor_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let value = mmio.read(cpu.registers.pc);
    cpu.registers.pc += 1;
    let result = cpu.registers.a ^ value;
    cpu.registers.a = result;
    cpu.registers.set_flag(registers::Flag::Zero, result == 0);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, false);
    8
}

pub fn add_hl_sp(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
    let hl = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let sp = cpu.registers.sp;
    let result = hl as u32 + sp as u32;
    
    cpu.registers.h = ((result & 0xFF00) >> 8) as u8;
    cpu.registers.l = (result & 0xFF) as u8;

    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::Carry, result > 0xFFFF);
    cpu.registers.set_flag(registers::Flag::HalfCarry, ((hl & 0x0FFF) + (sp & 0x0FFF)) > 0x0FFF);
    8
}

pub fn cp_memory_hl(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let value = mmio.read(addr);
    let result = cpu.registers.a.wrapping_sub(value);
    cpu.registers.set_flag(registers::Flag::Zero, result == 0);
    cpu.registers.set_flag(registers::Flag::Negative, true);
    cpu.registers.set_flag(registers::Flag::Carry, cpu.registers.a < value);
    cpu.registers.set_flag(registers::Flag::HalfCarry, (cpu.registers.a & 0x0F) < (value & 0x0F));
    8
}

pub fn ret(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    cpu.registers.pc = mmio.read(cpu.registers.sp) as u16;
    cpu.registers.pc |= (mmio.read(cpu.registers.sp + 1) as u16) << 8;
    cpu.registers.sp += 2;
    16
}

pub fn ccf(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
    let current_carry = cpu.registers.get_flag(registers::Flag::Carry);
    cpu.registers.set_flag(registers::Flag::Carry, !current_carry);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    4
}

pub fn ld_a_memory_c(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let addr = 0xFF00 | (cpu.registers.c as u16);
    cpu.registers.a = mmio.read(addr);
    8
}

pub fn reti(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    cpu.registers.pc = mmio.read(cpu.registers.sp) as u16;
    cpu.registers.pc |= (mmio.read(cpu.registers.sp + 1) as u16) << 8;
    cpu.registers.sp += 2;
    cpu.registers.ime = true;
    16
}

pub fn scf(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
    cpu.registers.set_flag(registers::Flag::Carry, true);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    4
}

pub fn and_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let value = mmio.read(cpu.registers.pc);
    cpu.registers.pc += 1;
    let result = cpu.registers.a & value;
    cpu.registers.a = result;
    cpu.registers.set_flag(registers::Flag::Zero, result == 0);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, true);
    cpu.registers.set_flag(registers::Flag::Carry, false);
    8
}

pub fn or_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let value = mmio.read(cpu.registers.pc);
    cpu.registers.pc += 1;
    let result = cpu.registers.a | value;
    cpu.registers.a = result;
    cpu.registers.set_flag(registers::Flag::Zero, result == 0);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, false);
    8
}

pub fn cpl(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
    cpu.registers.a = !cpu.registers.a;
    cpu.registers.set_flag(registers::Flag::Negative, true);
    cpu.registers.set_flag(registers::Flag::HalfCarry, true);
    4
}

pub fn di(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
    cpu.registers.ime = false;
    4
}

pub fn ei(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
    cpu.registers.ime = true;
    4
}

pub fn rla(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
    let old_carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1 } else { 0 };
    let new_carry = (cpu.registers.a & 0x80) >> 7;
    cpu.registers.a = (cpu.registers.a << 1) | old_carry;
    cpu.registers.set_flag(registers::Flag::Zero, false);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, new_carry == 1);
    4
}

pub fn ld_memory_hl_inc_a(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    mmio.write(addr, cpu.registers.a);
    let new_addr = addr.wrapping_add(1);
    cpu.registers.h = (new_addr >> 8) as u8;
    cpu.registers.l = (new_addr & 0x00FF) as u8;
    8
}

pub fn ld_memory_hl_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let value = mmio.read(cpu.registers.pc);
    mmio.write(addr, value);
    cpu.registers.pc += 1;
    12
}

pub fn ld_memory_imm_a_16(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let low = mmio.read(cpu.registers.pc) as u16;
    let high = mmio.read(cpu.registers.pc + 1) as u16;
    let addr = (high << 8) | low;
    mmio.write(addr, cpu.registers.a);
    cpu.registers.pc += 2;
    16
}

pub fn ld_sp_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let low = mmio.read(cpu.registers.pc) as u16;
    let high = mmio.read(cpu.registers.pc + 1) as u16;
    let value = (high << 8) | low;
    cpu.registers.sp = value;
    cpu.registers.pc += 2;
    12
}

pub fn ld_a_memory_hl_inc(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    cpu.registers.a = mmio.read(addr);
    let new_addr = addr.wrapping_add(1);
    cpu.registers.h = (new_addr >> 8) as u8;
    cpu.registers.l = (new_addr & 0x00FF) as u8;
    8
}

pub fn ld_a_memory_hl_dec(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    cpu.registers.a = mmio.read(addr);
    let new_addr = addr.wrapping_sub(1);
    cpu.registers.h = (new_addr >> 8) as u8;
    cpu.registers.l = (new_addr & 0x00FF) as u8;
    8
}

pub fn ld_memory_c_a(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let addr = 0xFF00 | (cpu.registers.c as u16);
    mmio.write(addr, cpu.registers.a);
    8
}

pub fn call_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let low = mmio.read(cpu.registers.pc) as u16;
    let high = mmio.read(cpu.registers.pc + 1) as u16;
    let addr = (high << 8) | low;
    cpu.registers.pc += 2;

    cpu.registers.sp = cpu.registers.sp.wrapping_sub(2);
    mmio.write(cpu.registers.sp, (cpu.registers.pc & 0x00FF) as u8);
    mmio.write(cpu.registers.sp + 1, (cpu.registers.pc >> 8) as u8);

    cpu.registers.pc = addr;
    24
}

pub fn cp_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let value = mmio.read(cpu.registers.pc);
    cpu.registers.pc += 1;
    let result = cpu.registers.a.wrapping_sub(value);
    cpu.registers.set_flag(registers::Flag::Zero, result == 0);
    cpu.registers.set_flag(registers::Flag::Negative, true);
    cpu.registers.set_flag(registers::Flag::Carry, cpu.registers.a < value);
    cpu.registers.set_flag(registers::Flag::HalfCarry, (cpu.registers.a & 0x0F) < (value & 0x0F));
    8
}

pub fn add_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let value = mmio.read(cpu.registers.pc);
    cpu.registers.pc += 1;
    let a = cpu.registers.a;
    let result = (a as u16) + (value as u16);
    
    cpu.registers.a = (result & 0xFF) as u8;
    cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.a == 0);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::Carry, result > 0xFF);
    cpu.registers.set_flag(registers::Flag::HalfCarry, (a & 0x0F) + (value & 0x0F) > 0x0F);
    8
}

pub fn sub_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let value = mmio.read(cpu.registers.pc);
    cpu.registers.pc += 1;
    let a = cpu.registers.a;
    let result = (a as i16) - (value as i16);
    
    cpu.registers.a = (result & 0xFF) as u8;
    cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.a == 0);
    cpu.registers.set_flag(registers::Flag::Negative, true);
    cpu.registers.set_flag(registers::Flag::Carry, result < 0);
    cpu.registers.set_flag(registers::Flag::HalfCarry, (a & 0x0F) < (value & 0x0F));
    8
}

pub fn ldh_a_memory_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let offset = mmio.read(cpu.registers.pc) as u16;
    let addr = 0xFF00 | offset;
    cpu.registers.a = mmio.read(addr);
    cpu.registers.pc += 1;
    12
}

pub fn ldh_memory_imm_a(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let offset = mmio.read(cpu.registers.pc) as u16;
    let addr = 0xFF00 | offset;
    mmio.write(addr, cpu.registers.a);
    cpu.registers.pc += 1;
    12
}

pub fn ld_memory_hl_dec_a(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    mmio.write(addr, cpu.registers.a);
    let new_addr = addr.wrapping_sub(1);
    cpu.registers.h = (new_addr >> 8) as u8;
    cpu.registers.l = (new_addr & 0x00FF) as u8;
    8
}

pub fn sbc_a_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let value = mmio.read(cpu.registers.pc);
    cpu.registers.pc += 1;

    let carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1 } else { 0 };
    let a = cpu.registers.a;
    let result = (a as i16) - (value as i16) - (carry as i16);
    
    cpu.registers.a = (result & 0xFF) as u8;
    cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.a == 0);
    cpu.registers.set_flag(registers::Flag::Negative, true);
    cpu.registers.set_flag(registers::Flag::Carry, result < 0);
    cpu.registers.set_flag(registers::Flag::HalfCarry, (a & 0x0F) < ((value & 0x0F) + carry));
    8
}

pub fn ld_a_memory_imm_16(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let low = mmio.read(cpu.registers.pc) as u16;
    let high = mmio.read(cpu.registers.pc + 1) as u16;
    let addr = (high << 8) | low;
    cpu.registers.a = mmio.read(addr);
    cpu.registers.pc += 2;
    16
}

pub fn jp_hl(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    cpu.registers.pc = addr;
    4
}

macro_rules! make_jp_cond {
    ($name:ident, $cond:expr) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
            if $cond(cpu) {
                let low = mmio.read(cpu.registers.pc) as u16;
                let high = mmio.read(cpu.registers.pc + 1) as u16;
                let addr = (high << 8) | low;
                cpu.registers.pc = addr;
                16
            } else {
                cpu.registers.pc += 2;
                12
            }
        }
    };
}
macro_rules! make_inc_register {
    ($name:ident, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            cpu.registers.$reg = cpu.registers.$reg.wrapping_add(1);
            cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.$reg == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (cpu.registers.$reg & 0x0F) == 0);
            4
        }
    };
}

macro_rules! make_dec_register {
    ($name:ident, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            cpu.registers.$reg = cpu.registers.$reg.wrapping_sub(1);
            cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.$reg == 0);
            cpu.registers.set_flag(registers::Flag::Negative, true);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (cpu.registers.$reg & 0x0F) == 0x0F);
            4
        }
    };
}

macro_rules! make_ld_register_imm {
    ($name:ident, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
            let val = mmio.read(cpu.registers.pc);
            cpu.registers.$reg = val as u8;
            cpu.registers.pc += 1;
            8
        }
    };
}

macro_rules! make_inc_memory {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
            let addr = ((cpu.registers.$reg1 as u16) << 8) | (cpu.registers.$reg2 as u16);
            let old_value = mmio.read(addr);
            let new_value = old_value.wrapping_add(1);
            mmio.write(addr, new_value);
            cpu.registers.set_flag(registers::Flag::Zero, new_value == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (old_value & 0x0F) == 0x0F);
            12
        }
    };
}

pub fn pop_af(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
    let addr = cpu.registers.sp;
    let f_value = mmio.read(addr) & 0xF0; // Only upper 4 bits are valid for F register
    let a_value = mmio.read(addr.wrapping_add(1));
    cpu.registers.sp = cpu.registers.sp.wrapping_add(2);
    cpu.registers.f = f_value;
    cpu.registers.a = a_value;
    12
}

macro_rules! make_alu_add_register {
    ($name:ident, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            let a = cpu.registers.a;
            let operand = cpu.registers.$reg;
            let result = a as u16 + operand as u16;
            
            cpu.registers.a = (result & 0xFF) as u8;
            cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.a == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::Carry, result > 0xFF);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (a & 0x0F) + (operand & 0x0F) > 0x0F);
            4
        }
    };
}

macro_rules! make_alu_cp_register {
    ($name:ident, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            let a = cpu.registers.a;
            let operand = cpu.registers.$reg;
            let result = a.wrapping_sub(operand);
            cpu.registers.set_flag(registers::Flag::Zero, result == 0);
            cpu.registers.set_flag(registers::Flag::Negative, true);
            cpu.registers.set_flag(registers::Flag::Carry, a < operand);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (a & 0x0F) < (operand & 0x0F));
            4
        }
    };
}

macro_rules! make_alu_adc_register {
    ($name:ident, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            let a = cpu.registers.a;
            let operand = cpu.registers.$reg;
            let carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1u8 } else { 0u8 };
            let result = a as u16 + operand as u16 + carry as u16;

            cpu.registers.a = (result & 0xFF) as u8;
            cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.a == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::Carry, result > 0xFF);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (a & 0x0F) + (operand & 0x0F) + carry > 0x0F);
            4
        }
    };
}

macro_rules! make_alu_sub_register {
    ($name:ident, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            let a = cpu.registers.a;
            let operand = cpu.registers.$reg;
            let result = a.wrapping_sub(operand);
            
            cpu.registers.a = result;
            cpu.registers.set_flag(registers::Flag::Zero, result == 0);
            cpu.registers.set_flag(registers::Flag::Negative, true);
            cpu.registers.set_flag(registers::Flag::Carry, a < operand);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (a & 0x0F) < (operand & 0x0F));
            4
        }
    };
}

macro_rules! make_alu_and_register {
    ($name:ident, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            let result = cpu.registers.a & cpu.registers.$reg;
            cpu.registers.a = result;
            cpu.registers.set_flag(registers::Flag::Zero, result == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, true);
            cpu.registers.set_flag(registers::Flag::Carry, false);
            4
        }
    };
}

macro_rules! make_alu_or_register {
    ($name:ident, $op:tt, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            let result = cpu.registers.a $op cpu.registers.$reg;
            cpu.registers.a = result;
            cpu.registers.set_flag(registers::Flag::Zero, result == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, false);
            cpu.registers.set_flag(registers::Flag::Carry, false);
            4
        }
    };
}

macro_rules! make_alu_add_mem_hl {
    ($name:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
            let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
            let a = cpu.registers.a;
            let operand = mmio.read(addr);
            let result = a as u16 + operand as u16;
            
            cpu.registers.a = (result & 0xFF) as u8;
            cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.a == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::Carry, result > 0xFF);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (a & 0x0F) + (operand & 0x0F) > 0x0F);
            8
        }
    };
}

macro_rules! make_alu_sub_mem_hl {
    ($name:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
            let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
            let a = cpu.registers.a;
            let operand = mmio.read(addr);
            let result = a.wrapping_sub(operand);
            
            cpu.registers.a = result;
            cpu.registers.set_flag(registers::Flag::Zero, result == 0);
            cpu.registers.set_flag(registers::Flag::Negative, true);
            cpu.registers.set_flag(registers::Flag::Carry, a < operand);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (a & 0x0F) < (operand & 0x0F));
            8
        }
    };
}

macro_rules! make_alu_and_mem_hl {
    ($name:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
            let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
            let value = mmio.read(addr);
            let result = cpu.registers.a & value;
            cpu.registers.a = result;
            cpu.registers.set_flag(registers::Flag::Zero, result == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, true);
            cpu.registers.set_flag(registers::Flag::Carry, false);
            8
        }
    };
}

macro_rules! make_alu_or_mem_hl {
    ($name:ident, $op:tt) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
            let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
            let value = mmio.read(addr);
            let result = cpu.registers.a $op value;
            cpu.registers.a = result;
            cpu.registers.set_flag(registers::Flag::Zero, result == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, false);
            cpu.registers.set_flag(registers::Flag::Carry, false);
            8
        }
    };
}

macro_rules! make_ld_16_bit_imm {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
            let low = mmio.read(cpu.registers.pc) as u16;
            let high = mmio.read(cpu.registers.pc + 1) as u16;
            let value = (high << 8) | low;
            cpu.registers.$reg1 = (value >> 8) as u8;
            cpu.registers.$reg2 = (value & 0x00FF) as u8;
            cpu.registers.pc += 2;
            12
        }
    };
}

macro_rules! make_jr_cond {
    ($name:ident, $cond:expr) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
            let offset = mmio.read(cpu.registers.pc) as i8;
            cpu.registers.pc += 1;
            if $cond(cpu) {
                cpu.registers.pc = ((cpu.registers.pc as i16) + (offset as i16)) as u16;
                12
            } else {
                8
            }
        }
    };
}

macro_rules! make_ret_cond {
    ($name:ident, $cond:expr) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
            if $cond(cpu) {
                cpu.registers.pc = mmio.read(cpu.registers.sp) as u16;
                cpu.registers.pc |= (mmio.read(cpu.registers.sp + 1) as u16) << 8;
                cpu.registers.sp += 2;
                20
            } else {
                8
            }
        }
    };
}

macro_rules! make_dec_combined_register {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            let value = ((cpu.registers.$reg1 as u16) << 8) | (cpu.registers.$reg2 as u16);
            let new_value = value.wrapping_sub(1);
            cpu.registers.$reg1 = (new_value >> 8) as u8;
            cpu.registers.$reg2 = (new_value & 0x00FF) as u8;
            8
        }
    };
}

macro_rules! make_reset_bit_memory_hl {
    ($name:ident, $bit:expr) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
            let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
            let value = mmio.read(addr);
            let new_value = value & !(1 << $bit);
            mmio.write(addr, new_value);
            16
        }
    };
}

macro_rules! make_set_bit_memory_hl {
    ($name:ident, $bit:expr) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
            let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
            let value = mmio.read(addr);
            let new_value = value | (1 << $bit);
            mmio.write(addr, new_value);
            16
        }
    };
}

macro_rules! make_bit_memory_hl {
    ($name:ident, $bit:expr) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
            let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
            let value = mmio.read(addr);
            let bit_set = (value & (1 << $bit)) != 0;
            cpu.registers.set_flag(registers::Flag::Zero, !bit_set);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, true);
            12
        }
    };
}

macro_rules! make_inc_combined_register {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            let value = ((cpu.registers.$reg1 as u16) << 8) | (cpu.registers.$reg2 as u16);
            let new_value = value.wrapping_add(1);
            cpu.registers.$reg1 = (new_value >> 8) as u8;
            cpu.registers.$reg2 = (new_value & 0x00FF) as u8;
            8
        }
    };
}

macro_rules! make_ld_register_register {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            cpu.registers.$reg1 = cpu.registers.$reg2;
            4
        }
    };
}

macro_rules! make_ld_register_register_self {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub fn $name(_cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            4
        }
    };
}

macro_rules! make_ld_memory_combined_register_a {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
            let addr = ((cpu.registers.$reg1 as u16) << 8) | (cpu.registers.$reg2 as u16);
            mmio.write(addr, cpu.registers.a);
            8
        }
    };
}

macro_rules! make_bit_register {
    ($name:ident, $bit:expr, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            let bit_set = (cpu.registers.$reg & (1 << $bit)) != 0;
            cpu.registers.set_flag(registers::Flag::Zero, !bit_set);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, true);
            8
        }
    };
}

macro_rules! make_set_bit_register {
    ($name:ident, $bit:expr, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            cpu.registers.$reg |= 1 << $bit;
            8
        }
    };
}

macro_rules! make_res_bit_register {
    ($name:ident, $bit:expr, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            cpu.registers.$reg &= !(1 << $bit);
            8
        }
    };
}

macro_rules! make_ld_register_memory_combined {
    ($name:ident, $reg1:ident, $reg2:ident, $reg3:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
            let addr = ((cpu.registers.$reg2 as u16) << 8) | (cpu.registers.$reg3 as u16);
            cpu.registers.$reg1 = mmio.read(addr);
            8
        }
    };
}

macro_rules! make_push_combined_register {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
            cpu.registers.sp = cpu.registers.sp.wrapping_sub(2);
            mmio.write(cpu.registers.sp, cpu.registers.$reg2);
            mmio.write(cpu.registers.sp + 1, cpu.registers.$reg1);
            16
        }
    };
}

macro_rules! make_pop_combined_register {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
            let addr = cpu.registers.sp;
            let low = mmio.read(addr);
            let high = mmio.read(addr.wrapping_add(1));
            cpu.registers.sp = cpu.registers.sp.wrapping_add(2);
            cpu.registers.$reg2 = low;
            cpu.registers.$reg1 = high;
            12
        }
    };
}

macro_rules! make_rl_register {
    ($name:ident, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            let old_carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1 } else { 0 };
            let new_carry = (cpu.registers.$reg & 0x80) >> 7;
            cpu.registers.$reg = (cpu.registers.$reg << 1) | old_carry;
            cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.$reg == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, false);
            cpu.registers.set_flag(registers::Flag::Carry, new_carry == 1);
            8
        }
    };
}

macro_rules! make_rr_register {
    ($name:ident, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            let old_carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1 } else { 0 };
            let new_carry = cpu.registers.$reg & 0x01;
            cpu.registers.$reg = (cpu.registers.$reg >> 1) | (old_carry << 7);
            cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.$reg == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, false);
            cpu.registers.set_flag(registers::Flag::Carry, new_carry == 1);
            8
        }
    };
}

macro_rules! make_swap_register {
    ($name:ident, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            let high_nibble = (cpu.registers.$reg & 0xF0) >> 4;
            let low_nibble = (cpu.registers.$reg & 0x0F) << 4;
            cpu.registers.$reg = high_nibble | low_nibble;
            cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.$reg == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, false);
            cpu.registers.set_flag(registers::Flag::Carry, false);
            8
        }
    };
}

macro_rules! make_rst {
    ($name:ident, $addr:expr) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
            cpu.registers.sp = cpu.registers.sp.wrapping_sub(2);
            mmio.write(cpu.registers.sp, (cpu.registers.pc & 0x00FF) as u8);
            mmio.write(cpu.registers.sp + 1, (cpu.registers.pc >> 8) as u8);
            cpu.registers.pc = $addr;
            16
        }
    };
}

macro_rules! make_add_hl_combined_register {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            let hl = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
            let operand = ((cpu.registers.$reg1 as u16) << 8) | (cpu.registers.$reg2 as u16);
            let result = hl as u32 + operand as u32;

            cpu.registers.h = ((result & 0xFF00) >> 8) as u8;
            cpu.registers.l = (result & 0x00FF) as u8;

            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, ((hl & 0x0FFF) + (operand & 0x0FFF)) > 0x0FFF);
            cpu.registers.set_flag(registers::Flag::Carry, result > 0xFFFF);
            8
        }
    };
}

macro_rules! make_sla_register {
    ($name:ident, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            let new_carry = (cpu.registers.$reg & 0x80) >> 7;
            cpu.registers.$reg <<= 1;
            cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.$reg == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, false);
            cpu.registers.set_flag(registers::Flag::Carry, new_carry == 1);
            8
        }
    };
}

macro_rules! make_sra_register {
    ($name:ident, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            let new_carry = cpu.registers.$reg & 0x01;
            let msb = cpu.registers.$reg & 0x80; // Preserve the most significant bit
            cpu.registers.$reg = (cpu.registers.$reg >> 1) | msb;
            cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.$reg == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, false);
            cpu.registers.set_flag(registers::Flag::Carry, new_carry == 1);
            8
        }
    };
}

macro_rules! make_srl_register {
    ($name:ident, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            let new_carry = cpu.registers.$reg & 0x01;
            cpu.registers.$reg >>= 1;
            cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.$reg == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, false);
            cpu.registers.set_flag(registers::Flag::Carry, new_carry == 1);
            8
        }
    };
}

macro_rules! make_rlc_register {
    ($name:ident, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            let new_carry = (cpu.registers.$reg & 0x80) >> 7;
            cpu.registers.$reg = (cpu.registers.$reg << 1) | new_carry;
            cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.$reg == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, false);
            cpu.registers.set_flag(registers::Flag::Carry, new_carry == 1);
            8
        }
    };
}

macro_rules! make_rrc_register {
    ($name:ident, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            let new_carry = cpu.registers.$reg & 0x01;
            cpu.registers.$reg = (cpu.registers.$reg >> 1) | (new_carry << 7);
            cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.$reg == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, false);
            cpu.registers.set_flag(registers::Flag::Carry, new_carry == 1);
            8
        }
    };
}

macro_rules! make_ld_memory_hl_register {
    ($name:ident, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
            let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
            mmio.write(addr, cpu.registers.$reg);
            8
        }
    };
}

macro_rules! make_call_cond {
    ($name:ident, $cond:expr) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::Mmio) -> u8 {
            let low = mmio.read(cpu.registers.pc) as u16;
            let high = mmio.read(cpu.registers.pc + 1) as u16;
            let addr = (high << 8) | low;
            cpu.registers.pc += 2;
            if $cond(cpu) {
                cpu.registers.sp = cpu.registers.sp.wrapping_sub(2);
                mmio.write(cpu.registers.sp, (cpu.registers.pc & 0x00FF) as u8);
                mmio.write(cpu.registers.sp + 1, (cpu.registers.pc >> 8) as u8);
                cpu.registers.pc = addr;
                24
            } else {
                12
            }
        }
    };
}

macro_rules! make_sbc_a_register {
    ($name:ident, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::Mmio) -> u8 {
            let a = cpu.registers.a;
            let operand = cpu.registers.$reg;
            let carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1u8 } else { 0u8 };
            let result = a.wrapping_sub(operand).wrapping_sub(carry);

            cpu.registers.a = result;
            cpu.registers.set_flag(registers::Flag::Zero, result == 0);
            cpu.registers.set_flag(registers::Flag::Negative, true);
            cpu.registers.set_flag(registers::Flag::Carry, (a as u16) < (operand as u16 + carry as u16));
            cpu.registers.set_flag(registers::Flag::HalfCarry, (a & 0x0F) < ((operand & 0x0F) + carry));
            4
        }
    };
}

make_rl_register!(rl_a, a);
make_rl_register!(rl_b, b);
make_rl_register!(rl_c, c);
make_rl_register!(rl_d, d);
make_rl_register!(rl_e, e);
make_rl_register!(rl_h, h);
make_rl_register!(rl_l, l);
make_rr_register!(rr_a, a);
make_rr_register!(rr_b, b);
make_rr_register!(rr_c, c);
make_rr_register!(rr_d, d);
make_rr_register!(rr_e, e);
make_rr_register!(rr_h, h);
make_rr_register!(rr_l, l);
make_push_combined_register!(push_bc, b, c);
make_push_combined_register!(push_de, d, e);
make_push_combined_register!(push_hl, h, l);
make_push_combined_register!(push_af, a, f);
make_pop_combined_register!(pop_bc, b, c);
make_pop_combined_register!(pop_de, d, e);
make_pop_combined_register!(pop_hl, h, l);
make_bit_register!(bit_0_b, 0, b);
make_bit_register!(bit_0_c, 0, c);
make_bit_register!(bit_0_d, 0, d);
make_bit_register!(bit_0_e, 0, e);
make_bit_register!(bit_0_h, 0, h);
make_bit_register!(bit_0_l, 0, l);
make_bit_register!(bit_0_a, 0, a);
make_bit_register!(bit_1_b, 1, b);
make_bit_register!(bit_1_c, 1, c);
make_bit_register!(bit_1_d, 1, d);
make_bit_register!(bit_1_e, 1, e);
make_bit_register!(bit_1_h, 1, h);
make_bit_register!(bit_1_l, 1, l);
make_bit_register!(bit_1_a, 1, a);
make_bit_register!(bit_2_b, 2, b);
make_bit_register!(bit_2_c, 2, c);
make_bit_register!(bit_2_d, 2, d);
make_bit_register!(bit_2_e, 2, e);
make_bit_register!(bit_2_h, 2, h);
make_bit_register!(bit_2_l, 2, l);
make_bit_register!(bit_2_a, 2, a);
make_bit_register!(bit_3_b, 3, b);
make_bit_register!(bit_3_c, 3, c);
make_bit_register!(bit_3_d, 3, d);
make_bit_register!(bit_3_e, 3, e);
make_bit_register!(bit_3_h, 3, h);
make_bit_register!(bit_3_l, 3, l);
make_bit_register!(bit_3_a, 3, a);
make_bit_register!(bit_4_b, 4, b);
make_bit_register!(bit_4_c, 4, c);
make_bit_register!(bit_4_d, 4, d);
make_bit_register!(bit_4_e, 4, e);
make_bit_register!(bit_4_h, 4, h);
make_bit_register!(bit_4_l, 4, l);
make_bit_register!(bit_4_a, 4, a);
make_bit_register!(bit_5_b, 5, b);
make_bit_register!(bit_5_c, 5, c);
make_bit_register!(bit_5_d, 5, d);
make_bit_register!(bit_5_e, 5, e);
make_bit_register!(bit_5_h, 5, h);
make_bit_register!(bit_5_l, 5, l);
make_bit_register!(bit_5_a, 5, a);
make_bit_register!(bit_6_b, 6, b);
make_bit_register!(bit_6_c, 6, c);
make_bit_register!(bit_6_d, 6, d);
make_bit_register!(bit_6_e, 6, e);
make_bit_register!(bit_6_h, 6, h);
make_bit_register!(bit_6_l, 6, l);
make_bit_register!(bit_6_a, 6, a);
make_bit_register!(bit_7_b, 7, b);
make_bit_register!(bit_7_c, 7, c);
make_bit_register!(bit_7_d, 7, d);
make_bit_register!(bit_7_e, 7, e);
make_bit_register!(bit_7_h, 7, h);
make_bit_register!(bit_7_l, 7, l);
make_bit_register!(bit_7_a, 7, a);
make_ld_register_memory_combined!(ld_a_memory_bc, a, b, c);
make_ld_register_memory_combined!(ld_a_memory_de, a, d, e);
make_ld_register_memory_combined!(ld_b_memory_hl, b, h, l);
make_ld_register_memory_combined!(ld_c_memory_hl, c, h, l);
make_ld_register_memory_combined!(ld_d_memory_hl, d, h, l);
make_ld_register_memory_combined!(ld_e_memory_hl, e, h, l);
make_ld_register_memory_combined!(ld_h_memory_hl, h, h, l);
make_ld_register_memory_combined!(ld_l_memory_hl, l, h, l);
make_ld_register_memory_combined!(ld_a_memory_hl, a, h, l);
make_ld_memory_hl_register!(ld_memory_hl_a, a);
make_ld_memory_hl_register!(ld_memory_hl_b, b);
make_ld_memory_hl_register!(ld_memory_hl_c, c);
make_ld_memory_hl_register!(ld_memory_hl_d, d);
make_ld_memory_hl_register!(ld_memory_hl_e, e);
make_ld_memory_hl_register!(ld_memory_hl_h, h);
make_ld_memory_hl_register!(ld_memory_hl_l, l);
make_ld_memory_combined_register_a!(ld_memory_bc_a, b, c);
make_ld_memory_combined_register_a!(ld_memory_de_a, d, e);
make_ld_register_register!(ld_a_b, a, b);
make_ld_register_register!(ld_a_c, a, c);
make_ld_register_register!(ld_a_d, a, d);
make_ld_register_register!(ld_a_e, a, e);
make_ld_register_register!(ld_a_h, a, h);
make_ld_register_register!(ld_a_l, a, l);
make_ld_register_register_self!(ld_a_a, a, a);
make_ld_register_register!(ld_b_a, b, a);
make_ld_register_register_self!(ld_b_b, b, b);
make_ld_register_register!(ld_b_c, b, c);
make_ld_register_register!(ld_b_d, b, d);
make_ld_register_register!(ld_b_e, b, e);
make_ld_register_register!(ld_b_h, b, h);
make_ld_register_register!(ld_b_l, b, l);
make_ld_register_register!(ld_c_a, c, a);
make_ld_register_register!(ld_c_b, c, b);
make_ld_register_register_self!(ld_c_c, c, c);
make_ld_register_register!(ld_c_d, c, d);
make_ld_register_register!(ld_c_e, c, e);
make_ld_register_register!(ld_c_h, c, h);
make_ld_register_register!(ld_c_l, c, l);
make_ld_register_register!(ld_d_a, d, a);
make_ld_register_register!(ld_d_b, d, b);
make_ld_register_register!(ld_d_c, d, c);
make_ld_register_register_self!(ld_d_d, d, d);
make_ld_register_register!(ld_d_e, d, e);
make_ld_register_register!(ld_d_h, d, h);
make_ld_register_register!(ld_d_l, d, l);
make_ld_register_register!(ld_e_a, e, a);
make_ld_register_register!(ld_e_b, e, b);
make_ld_register_register!(ld_e_c, e, c);
make_ld_register_register!(ld_e_d, e, d);
make_ld_register_register_self!(ld_e_e, e, e);
make_ld_register_register!(ld_e_h, e, h);
make_ld_register_register!(ld_e_l, e, l);
make_ld_register_register!(ld_h_a, h, a);
make_ld_register_register!(ld_h_b, h, b);
make_ld_register_register!(ld_h_c, h, c);
make_ld_register_register!(ld_h_d, h, d);
make_ld_register_register!(ld_h_e, h, e);
make_ld_register_register_self!(ld_h_h, h, h);
make_ld_register_register!(ld_h_l, h, l);
make_ld_register_register!(ld_l_a, l, a);
make_ld_register_register!(ld_l_b, l, b);
make_ld_register_register!(ld_l_c, l, c);
make_ld_register_register!(ld_l_d, l, d);
make_ld_register_register!(ld_l_e, l, e);
make_ld_register_register!(ld_l_h, l, h);
make_ld_register_register_self!(ld_l_l, l, l);
make_inc_register!(inc_a, a);
make_inc_register!(inc_b, b);
make_inc_register!(inc_c, c);
make_inc_register!(inc_d, d);
make_inc_register!(inc_e, e);
make_inc_register!(inc_h, h);
make_inc_register!(inc_l, l);
make_dec_register!(dec_a, a);
make_dec_register!(dec_b, b);
make_dec_register!(dec_c, c);
make_dec_register!(dec_d, d);
make_dec_register!(dec_e, e);
make_dec_register!(dec_h, h);
make_dec_register!(dec_l, l);
make_inc_combined_register!(inc_bc, b, c);
make_inc_combined_register!(inc_de, d, e);
make_inc_combined_register!(inc_hl, h, l);
make_dec_combined_register!(dec_bc, b, c);
make_dec_combined_register!(dec_de, d, e);
make_dec_combined_register!(dec_hl, h, l);
make_ld_register_imm!(ld_a_imm, a);
make_ld_register_imm!(ld_b_imm, b);
make_ld_register_imm!(ld_c_imm, c);
make_ld_register_imm!(ld_d_imm, d);
make_ld_register_imm!(ld_e_imm, e);
make_ld_register_imm!(ld_h_imm, h);
make_ld_register_imm!(ld_l_imm, l);
make_inc_memory!(inc_memory_hl, h, l);
make_alu_and_register!(and_a, a);
make_alu_and_register!(and_b, b);
make_alu_and_register!(and_c, c);
make_alu_and_register!(and_d, d);
make_alu_and_register!(and_e, e);
make_alu_and_register!(and_h, h);
make_alu_and_register!(and_l, l);
make_alu_or_register!(or_a, |, a);
make_alu_or_register!(or_b, |, b);
make_alu_or_register!(or_c, |, c);
make_alu_or_register!(or_d, |, d);
make_alu_or_register!(or_e, |, e);
make_alu_or_register!(or_h, |, h);
make_alu_or_register!(or_l, |, l);
make_alu_or_register!(xor_a, ^, a);
make_alu_or_register!(xor_b, ^, b);
make_alu_or_register!(xor_c, ^, c);
make_alu_or_register!(xor_d, ^, d);
make_alu_or_register!(xor_e, ^, e);
make_alu_or_register!(xor_h, ^, h);
make_alu_or_register!(xor_l, ^, l);
make_alu_cp_register!(cp_a, a);
make_alu_cp_register!(cp_b, b);
make_alu_cp_register!(cp_c, c);
make_alu_cp_register!(cp_d, d);
make_alu_cp_register!(cp_e, e);
make_alu_cp_register!(cp_h, h);
make_alu_cp_register!(cp_l, l);
make_alu_adc_register!(adc_a, a);
make_alu_adc_register!(adc_b, b);
make_alu_adc_register!(adc_c, c);
make_alu_adc_register!(adc_d, d);
make_alu_adc_register!(adc_e, e);
make_alu_adc_register!(adc_h, h);
make_alu_adc_register!(adc_l, l);
make_alu_add_register!(add_a, a);
make_alu_add_register!(add_b, b);
make_alu_add_register!(add_c, c);
make_alu_add_register!(add_d, d);
make_alu_add_register!(add_e, e);
make_alu_add_register!(add_h, h);
make_alu_add_register!(add_l, l);
make_alu_sub_register!(sub_a, a);
make_alu_sub_register!(sub_b, b);
make_alu_sub_register!(sub_c, c);
make_alu_sub_register!(sub_d, d);
make_alu_sub_register!(sub_e, e);
make_alu_sub_register!(sub_h, h);
make_alu_sub_register!(sub_l, l);
make_alu_and_mem_hl!(and_memory_hl);
make_alu_or_mem_hl!(or_memory_hl, |);
make_alu_or_mem_hl!(xor_memory_hl, ^);
make_alu_add_mem_hl!(add_memory_hl);
make_alu_sub_mem_hl!(sub_memory_hl);
make_ld_16_bit_imm!(ld_bc_imm, b, c);
make_ld_16_bit_imm!(ld_de_imm, d, e);
make_ld_16_bit_imm!(ld_hl_imm, h, l);
make_ret_cond!(ret_nz, |cpu: &cpu::SM83| !cpu.registers.get_flag(registers::Flag::Zero));
make_ret_cond!(ret_z, |cpu: &cpu::SM83| cpu.registers.get_flag(registers::Flag::Zero));
make_ret_cond!(ret_nc, |cpu: &cpu::SM83| !cpu.registers.get_flag(registers::Flag::Carry));
make_ret_cond!(ret_c, |cpu: &cpu::SM83| cpu.registers.get_flag(registers::Flag::Carry));
make_jr_cond!(jr_nz_imm, |cpu: &cpu::SM83| !cpu.registers.get_flag(registers::Flag::Zero));
make_jr_cond!(jr_z_imm, |cpu: &cpu::SM83| cpu.registers.get_flag(registers::Flag::Zero));
make_jr_cond!(jr_nc_imm, |cpu: &cpu::SM83| !cpu.registers.get_flag(registers::Flag::Carry));
make_jr_cond!(jr_c_imm, |cpu: &cpu::SM83| cpu.registers.get_flag(registers::Flag::Carry));
make_swap_register!(swap_a, a);
make_swap_register!(swap_b, b);
make_swap_register!(swap_c, c);
make_swap_register!(swap_d, d);
make_swap_register!(swap_e, e);
make_swap_register!(swap_h, h);
make_swap_register!(swap_l, l);
make_rst!(rst_00, 0x00);
make_rst!(rst_08, 0x08);
make_rst!(rst_10, 0x10);
make_rst!(rst_18, 0x18);
make_rst!(rst_20, 0x20);
make_rst!(rst_28, 0x28);
make_rst!(rst_30, 0x30);
make_rst!(rst_38, 0x38);
make_add_hl_combined_register!(add_hl_bc, b, c);
make_add_hl_combined_register!(add_hl_de, d, e);
make_add_hl_combined_register!(add_hl_hl, h, l);
make_res_bit_register!(res_0_b, 0, b);
make_res_bit_register!(res_0_c, 0, c);
make_res_bit_register!(res_0_d, 0, d);
make_res_bit_register!(res_0_e, 0, e);
make_res_bit_register!(res_0_h, 0, h);
make_res_bit_register!(res_0_l, 0, l);
make_res_bit_register!(res_0_a, 0, a);
make_res_bit_register!(res_1_b, 1, b);
make_res_bit_register!(res_1_c, 1, c);
make_res_bit_register!(res_1_d, 1, d);
make_res_bit_register!(res_1_e, 1, e);
make_res_bit_register!(res_1_h, 1, h);
make_res_bit_register!(res_1_l, 1, l);
make_res_bit_register!(res_1_a, 1, a);
make_res_bit_register!(res_2_b, 2, b);
make_res_bit_register!(res_2_c, 2, c);
make_res_bit_register!(res_2_d, 2, d);
make_res_bit_register!(res_2_e, 2, e);
make_res_bit_register!(res_2_h, 2, h);
make_res_bit_register!(res_2_l, 2, l);
make_res_bit_register!(res_2_a, 2, a);
make_res_bit_register!(res_3_b, 3, b);
make_res_bit_register!(res_3_c, 3, c);
make_res_bit_register!(res_3_d, 3, d);
make_res_bit_register!(res_3_e, 3, e);
make_res_bit_register!(res_3_h, 3, h);
make_res_bit_register!(res_3_l, 3, l);
make_res_bit_register!(res_3_a, 3, a);
make_res_bit_register!(res_4_b, 4, b);
make_res_bit_register!(res_4_c, 4, c);
make_res_bit_register!(res_4_d, 4, d);
make_res_bit_register!(res_4_e, 4, e);
make_res_bit_register!(res_4_h, 4, h);
make_res_bit_register!(res_4_l, 4, l);
make_res_bit_register!(res_4_a, 4, a);
make_res_bit_register!(res_5_b, 5, b);
make_res_bit_register!(res_5_c, 5, c);
make_res_bit_register!(res_5_d, 5, d);
make_res_bit_register!(res_5_e, 5, e);
make_res_bit_register!(res_5_h, 5, h);
make_res_bit_register!(res_5_l, 5, l);
make_res_bit_register!(res_5_a, 5, a);
make_res_bit_register!(res_6_b, 6, b);
make_res_bit_register!(res_6_c, 6, c);
make_res_bit_register!(res_6_d, 6, d);
make_res_bit_register!(res_6_e, 6, e);
make_res_bit_register!(res_6_h, 6, h);
make_res_bit_register!(res_6_l, 6, l);
make_res_bit_register!(res_6_a, 6, a);
make_res_bit_register!(res_7_b, 7, b);
make_res_bit_register!(res_7_c, 7, c);
make_res_bit_register!(res_7_d, 7, d);
make_res_bit_register!(res_7_e, 7, e);
make_res_bit_register!(res_7_h, 7, h);
make_res_bit_register!(res_7_l, 7, l);
make_res_bit_register!(res_7_a, 7, a);
make_set_bit_register!(set_0_b, 0, b);
make_set_bit_register!(set_0_c, 0, c);
make_set_bit_register!(set_0_d, 0, d);
make_set_bit_register!(set_0_e, 0, e);
make_set_bit_register!(set_0_h, 0, h);
make_set_bit_register!(set_0_l, 0, l);
make_set_bit_register!(set_0_a, 0, a);
make_set_bit_register!(set_1_b, 1, b);
make_set_bit_register!(set_1_c, 1, c);
make_set_bit_register!(set_1_d, 1, d);
make_set_bit_register!(set_1_e, 1, e);
make_set_bit_register!(set_1_h, 1, h);
make_set_bit_register!(set_1_l, 1, l);
make_set_bit_register!(set_1_a, 1, a);
make_set_bit_register!(set_2_b, 2, b);
make_set_bit_register!(set_2_c, 2, c);
make_set_bit_register!(set_2_d, 2, d);
make_set_bit_register!(set_2_e, 2, e);
make_set_bit_register!(set_2_h, 2, h);
make_set_bit_register!(set_2_l, 2, l);
make_set_bit_register!(set_2_a, 2, a);
make_set_bit_register!(set_3_b, 3, b);
make_set_bit_register!(set_3_c, 3, c);
make_set_bit_register!(set_3_d, 3, d);
make_set_bit_register!(set_3_e, 3, e);
make_set_bit_register!(set_3_h, 3, h);
make_set_bit_register!(set_3_l, 3, l);
make_set_bit_register!(set_3_a, 3, a);
make_set_bit_register!(set_4_b, 4, b);
make_set_bit_register!(set_4_c, 4, c);
make_set_bit_register!(set_4_d, 4, d);
make_set_bit_register!(set_4_e, 4, e);
make_set_bit_register!(set_4_h, 4, h);
make_set_bit_register!(set_4_l, 4, l);
make_set_bit_register!(set_4_a, 4, a);
make_set_bit_register!(set_5_b, 5, b);
make_set_bit_register!(set_5_c, 5, c);
make_set_bit_register!(set_5_d, 5, d);
make_set_bit_register!(set_5_e, 5, e);
make_set_bit_register!(set_5_h, 5, h);
make_set_bit_register!(set_5_l, 5, l);
make_set_bit_register!(set_5_a, 5, a);
make_set_bit_register!(set_6_b, 6, b);
make_set_bit_register!(set_6_c, 6, c);
make_set_bit_register!(set_6_d, 6, d);
make_set_bit_register!(set_6_e, 6, e);
make_set_bit_register!(set_6_h, 6, h);
make_set_bit_register!(set_6_l, 6, l);
make_set_bit_register!(set_6_a, 6, a);
make_set_bit_register!(set_7_b, 7, b);
make_set_bit_register!(set_7_c, 7, c);
make_set_bit_register!(set_7_d, 7, d);
make_set_bit_register!(set_7_e, 7, e);
make_set_bit_register!(set_7_h, 7, h);
make_set_bit_register!(set_7_l, 7, l);
make_set_bit_register!(set_7_a, 7, a);
make_reset_bit_memory_hl!(res_7_hl, 7);
make_reset_bit_memory_hl!(res_6_hl, 6);
make_reset_bit_memory_hl!(res_5_hl, 5);
make_reset_bit_memory_hl!(res_4_hl, 4);
make_reset_bit_memory_hl!(res_3_hl, 3);
make_reset_bit_memory_hl!(res_2_hl, 2);
make_reset_bit_memory_hl!(res_1_hl, 1);
make_reset_bit_memory_hl!(res_0_hl, 0);
make_set_bit_memory_hl!(set_7_hl, 7);
make_set_bit_memory_hl!(set_6_hl, 6);
make_set_bit_memory_hl!(set_5_hl, 5);
make_set_bit_memory_hl!(set_4_hl, 4);
make_set_bit_memory_hl!(set_3_hl, 3);
make_set_bit_memory_hl!(set_2_hl, 2);
make_set_bit_memory_hl!(set_1_hl, 1);
make_set_bit_memory_hl!(set_0_hl, 0);
make_bit_memory_hl!(bit_7_hl, 7);
make_bit_memory_hl!(bit_6_hl, 6);
make_bit_memory_hl!(bit_5_hl, 5);
make_bit_memory_hl!(bit_4_hl, 4);
make_bit_memory_hl!(bit_3_hl, 3);
make_bit_memory_hl!(bit_2_hl, 2);
make_bit_memory_hl!(bit_1_hl, 1);
make_bit_memory_hl!(bit_0_hl, 0);
make_jp_cond!(jp_nz_imm, |cpu: &cpu::SM83| !cpu.registers.get_flag(registers::Flag::Zero));
make_jp_cond!(jp_z_imm, |cpu: &cpu::SM83| cpu.registers.get_flag(registers::Flag::Zero));
make_jp_cond!(jp_nc_imm, |cpu: &cpu::SM83| !cpu.registers.get_flag(registers::Flag::Carry));
make_jp_cond!(jp_c_imm, |cpu: &cpu::SM83| cpu.registers.get_flag(registers::Flag::Carry));
make_sla_register!(sla_a, a);
make_sla_register!(sla_b, b);
make_sla_register!(sla_c, c);
make_sla_register!(sla_d, d);
make_sla_register!(sla_e, e);
make_sla_register!(sla_h, h);
make_sla_register!(sla_l, l);
make_sra_register!(sra_a, a);
make_sra_register!(sra_b, b);
make_sra_register!(sra_c, c);
make_sra_register!(sra_d, d);
make_sra_register!(sra_e, e);
make_sra_register!(sra_h, h);
make_sra_register!(sra_l, l);
make_srl_register!(srl_a, a);
make_srl_register!(srl_b, b);
make_srl_register!(srl_c, c);
make_srl_register!(srl_d, d);
make_srl_register!(srl_e, e);
make_srl_register!(srl_h, h);
make_srl_register!(srl_l, l);
make_rlc_register!(rlc_a, a);
make_rlc_register!(rlc_b, b);
make_rlc_register!(rlc_c, c);
make_rlc_register!(rlc_d, d);
make_rlc_register!(rlc_e, e);
make_rlc_register!(rlc_h, h);
make_rlc_register!(rlc_l, l);
make_rrc_register!(rrc_a, a);
make_rrc_register!(rrc_b, b);
make_rrc_register!(rrc_c, c);
make_rrc_register!(rrc_d, d);
make_rrc_register!(rrc_e, e);
make_rrc_register!(rrc_h, h);
make_rrc_register!(rrc_l, l);
make_call_cond!(call_nz_imm, |cpu: &cpu::SM83| !cpu.registers.get_flag(registers::Flag::Zero));
make_call_cond!(call_z_imm, |cpu: &cpu::SM83| cpu.registers.get_flag(registers::Flag::Zero));
make_call_cond!(call_nc_imm, |cpu: &cpu::SM83| !cpu.registers.get_flag(registers::Flag::Carry));
make_call_cond!(call_c_imm, |cpu: &cpu::SM83| cpu.registers.get_flag(registers::Flag::Carry));
make_sbc_a_register!(sbc_a_a, a);
make_sbc_a_register!(sbc_a_b, b);
make_sbc_a_register!(sbc_a_c, c);
make_sbc_a_register!(sbc_a_d, d);
make_sbc_a_register!(sbc_a_e, e);
make_sbc_a_register!(sbc_a_h, h);
make_sbc_a_register!(sbc_a_l, l);