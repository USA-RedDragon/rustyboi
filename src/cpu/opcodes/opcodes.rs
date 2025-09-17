use crate::{cpu, cpu::registers, memory, memory::Addressable};

pub fn nop(_cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) -> u8 {
    4
}

pub fn jp_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
    let low = mmio.read(cpu.registers.pc) as u16;
    let high = mmio.read(cpu.registers.pc + 1) as u16;
    let addr = (high << 8) | low;
    cpu.registers.pc = addr;
    16
}

pub fn jr_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
    let offset = mmio.read(cpu.registers.pc) as i8;
    cpu.registers.pc += 1;
    cpu.registers.pc = ((cpu.registers.pc as i16) + (offset as i16)) as u16;
    12
}

pub fn cp_memory_hl(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let value = mmio.read(addr);
    let result = cpu.registers.a.wrapping_sub(value);
    cpu.registers.set_flag(registers::Flag::Zero, result == 0);
    cpu.registers.set_flag(registers::Flag::Negative, true);
    cpu.registers.set_flag(registers::Flag::Carry, cpu.registers.a < value);
    cpu.registers.set_flag(registers::Flag::HalfCarry, (cpu.registers.a & 0x0F) < (value & 0x0F));
    8
}

pub fn ret(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
    cpu.registers.pc = mmio.read(cpu.registers.sp) as u16;
    cpu.registers.pc |= (mmio.read(cpu.registers.sp + 1) as u16) << 8;
    cpu.registers.sp += 2;
    16
}

pub fn cpl(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) -> u8 {
    cpu.registers.a = !cpu.registers.a;
    cpu.registers.set_flag(registers::Flag::Negative, true);
    cpu.registers.set_flag(registers::Flag::HalfCarry, true);
    4
}

pub fn di(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) -> u8 {
    cpu.registers.ime = false;
    4
}

pub fn ei(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) -> u8 {
    cpu.registers.ime = true;
    4
}

pub fn rla(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) -> u8 {
    let old_carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1 } else { 0 };
    let new_carry = (cpu.registers.a & 0x80) >> 7;
    cpu.registers.a = (cpu.registers.a << 1) | old_carry;
    cpu.registers.set_flag(registers::Flag::Zero, false);
    cpu.registers.set_flag(registers::Flag::Negative, false);
    cpu.registers.set_flag(registers::Flag::HalfCarry, false);
    cpu.registers.set_flag(registers::Flag::Carry, new_carry == 1);
    4
}

pub fn ld_memory_hl_inc_a(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    mmio.write(addr, cpu.registers.a);
    let new_addr = addr.wrapping_add(1);
    cpu.registers.h = (new_addr >> 8) as u8;
    cpu.registers.l = (new_addr & 0x00FF) as u8;
    8
}

pub fn ld_memory_hl_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    let value = mmio.read(cpu.registers.pc);
    mmio.write(addr, value);
    cpu.registers.pc += 1;
    12
}

pub fn ld_memory_imm_a_16(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
    let low = mmio.read(cpu.registers.pc) as u16;
    let high = mmio.read(cpu.registers.pc + 1) as u16;
    let addr = (high << 8) | low;
    mmio.write(addr, cpu.registers.a);
    cpu.registers.pc += 2;
    16
}

pub fn ld_sp_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
    let low = mmio.read(cpu.registers.pc) as u16;
    let high = mmio.read(cpu.registers.pc + 1) as u16;
    let value = (high << 8) | low;
    cpu.registers.sp = value;
    cpu.registers.pc += 2;
    12
}

pub fn ld_a_memory_hl_inc(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    cpu.registers.a = mmio.read(addr);
    let new_addr = addr.wrapping_add(1);
    cpu.registers.h = (new_addr >> 8) as u8;
    cpu.registers.l = (new_addr & 0x00FF) as u8;
    8
}

pub fn ld_memory_c_a(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
    let addr = 0xFF00 | (cpu.registers.c as u16);
    mmio.write(addr, cpu.registers.a);
    8
}

pub fn call_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
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

pub fn cp_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
    let value = mmio.read(cpu.registers.pc);
    cpu.registers.pc += 1;
    let result = cpu.registers.a.wrapping_sub(value);
    cpu.registers.set_flag(registers::Flag::Zero, result == 0);
    cpu.registers.set_flag(registers::Flag::Negative, true);
    cpu.registers.set_flag(registers::Flag::Carry, cpu.registers.a < value);
    cpu.registers.set_flag(registers::Flag::HalfCarry, (cpu.registers.a & 0x0F) < (value & 0x0F));
    8
}

pub fn ldh_a_memory_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
    let offset = mmio.read(cpu.registers.pc) as u16;
    let addr = 0xFF00 | offset;
    cpu.registers.a = mmio.read(addr);
    cpu.registers.pc += 1;
    12
}

pub fn ldh_memory_imm_a(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
    let offset = mmio.read(cpu.registers.pc) as u16;
    let addr = 0xFF00 | offset;
    mmio.write(addr, cpu.registers.a);
    cpu.registers.pc += 1;
    12
}

pub fn ld_memory_hl_dec_a(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
    let addr = ((cpu.registers.h as u16) << 8) | (cpu.registers.l as u16);
    mmio.write(addr, cpu.registers.a);
    let new_addr = addr.wrapping_sub(1);
    cpu.registers.h = (new_addr >> 8) as u8;
    cpu.registers.l = (new_addr & 0x00FF) as u8;
    8
}

pub fn sbc_a_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
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

macro_rules! make_inc_register {
    ($name:ident, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) -> u8 {
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
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) -> u8 {
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
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
            let val = mmio.read(cpu.registers.pc);
            cpu.registers.$reg = val as u8;
            cpu.registers.pc += 1;
            8
        }
    };
}

macro_rules! make_inc_memory {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
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

pub fn pop_af(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
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
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) -> u8 {
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

macro_rules! make_alu_sub_register {
    ($name:ident, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) -> u8 {
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
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) -> u8 {
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
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) -> u8 {
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
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
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
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
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
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
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
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
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
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
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
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
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

macro_rules! make_dec_combined_register {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) -> u8 {
            let value = ((cpu.registers.$reg1 as u16) << 8) | (cpu.registers.$reg2 as u16);
            let new_value = value.wrapping_sub(1);
            cpu.registers.$reg1 = (new_value >> 8) as u8;
            cpu.registers.$reg2 = (new_value & 0x00FF) as u8;
            8
        }
    };
}

macro_rules! make_inc_combined_register {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) -> u8 {
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
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) -> u8 {
            cpu.registers.$reg1 = cpu.registers.$reg2;
            4
        }
    };
}

macro_rules! make_ld_memory_combined_register_a {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
            let addr = ((cpu.registers.$reg1 as u16) << 8) | (cpu.registers.$reg2 as u16);
            mmio.write(addr, cpu.registers.a);
            8
        }
    };
}

macro_rules! make_bit_register {
    ($name:ident, $bit:expr, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) -> u8 {
            let bit_set = (cpu.registers.$reg & (1 << $bit)) != 0;
            cpu.registers.set_flag(registers::Flag::Zero, !bit_set);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, true);
            8
        }
    };
}

macro_rules! make_ld_register_memory_combined {
    ($name:ident, $reg1:ident, $reg2:ident, $reg3:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
            let addr = ((cpu.registers.$reg2 as u16) << 8) | (cpu.registers.$reg3 as u16);
            cpu.registers.$reg1 = mmio.read(addr);
            8
        }
    };
}

macro_rules! make_push_combined_register {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
            cpu.registers.sp = cpu.registers.sp.wrapping_sub(2);
            mmio.write(cpu.registers.sp, cpu.registers.$reg2);
            mmio.write(cpu.registers.sp + 1, cpu.registers.$reg1);
            16
        }
    };
}

macro_rules! make_pop_combined_register {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) -> u8 {
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
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) -> u8 {
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
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) -> u8 {
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
make_ld_memory_combined_register_a!(ld_memory_bc_a, b, c);
make_ld_memory_combined_register_a!(ld_memory_de_a, d, e);
make_ld_memory_combined_register_a!(ld_memory_hl_a, h, l);
make_ld_register_register!(ld_a_b, a, b);
make_ld_register_register!(ld_a_c, a, c);
make_ld_register_register!(ld_a_d, a, d);
make_ld_register_register!(ld_a_e, a, e);
make_ld_register_register!(ld_a_h, a, h);
make_ld_register_register!(ld_a_l, a, l);
make_ld_register_register!(ld_a_a, a, a);
make_ld_register_register!(ld_b_a, b, a);
make_ld_register_register!(ld_b_b, b, b);
make_ld_register_register!(ld_b_c, b, c);
make_ld_register_register!(ld_b_d, b, d);
make_ld_register_register!(ld_b_e, b, e);
make_ld_register_register!(ld_b_h, b, h);
make_ld_register_register!(ld_b_l, b, l);
make_ld_register_register!(ld_c_a, c, a);
make_ld_register_register!(ld_c_b, c, b);
make_ld_register_register!(ld_c_c, c, c);
make_ld_register_register!(ld_c_d, c, d);
make_ld_register_register!(ld_c_e, c, e);
make_ld_register_register!(ld_c_h, c, h);
make_ld_register_register!(ld_c_l, c, l);
make_ld_register_register!(ld_d_a, d, a);
make_ld_register_register!(ld_d_b, d, b);
make_ld_register_register!(ld_d_c, d, c);
make_ld_register_register!(ld_d_d, d, d);
make_ld_register_register!(ld_d_e, d, e);
make_ld_register_register!(ld_d_h, d, h);
make_ld_register_register!(ld_d_l, d, l);
make_ld_register_register!(ld_e_a, e, a);
make_ld_register_register!(ld_e_b, e, b);
make_ld_register_register!(ld_e_c, e, c);
make_ld_register_register!(ld_e_d, e, d);
make_ld_register_register!(ld_e_e, e, e);
make_ld_register_register!(ld_e_h, e, h);
make_ld_register_register!(ld_e_l, e, l);
make_ld_register_register!(ld_h_a, h, a);
make_ld_register_register!(ld_h_b, h, b);
make_ld_register_register!(ld_h_c, h, c);
make_ld_register_register!(ld_h_d, h, d);
make_ld_register_register!(ld_h_e, h, e);
make_ld_register_register!(ld_h_h, h, h);
make_ld_register_register!(ld_h_l, h, l);
make_ld_register_register!(ld_l_a, l, a);
make_ld_register_register!(ld_l_b, l, b);
make_ld_register_register!(ld_l_c, l, c);
make_ld_register_register!(ld_l_d, l, d);
make_ld_register_register!(ld_l_e, l, e);
make_ld_register_register!(ld_l_h, l, h);
make_ld_register_register!(ld_l_l, l, l);
make_inc_register!(inc_a, a);
make_inc_register!(inc_b, b);
make_inc_register!(inc_c, c);
make_inc_register!(inc_d, d);
make_inc_register!(inc_e, e);
make_inc_register!(inc_h, h);
make_inc_register!(inc_l, l);
make_inc_register!(inc_sp, sp);
make_dec_register!(dec_a, a);
make_dec_register!(dec_b, b);
make_dec_register!(dec_c, c);
make_dec_register!(dec_d, d);
make_dec_register!(dec_e, e);
make_dec_register!(dec_h, h);
make_dec_register!(dec_l, l);
make_dec_register!(dec_sp, sp);
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
make_jr_cond!(jr_nz_imm, |cpu: &cpu::SM83| !cpu.registers.get_flag(registers::Flag::Zero));
make_jr_cond!(jr_z_imm, |cpu: &cpu::SM83| cpu.registers.get_flag(registers::Flag::Zero));
make_jr_cond!(jr_nc_imm, |cpu: &cpu::SM83| !cpu.registers.get_flag(registers::Flag::Carry));
make_jr_cond!(jr_c_imm, |cpu: &cpu::SM83| cpu.registers.get_flag(registers::Flag::Carry));
