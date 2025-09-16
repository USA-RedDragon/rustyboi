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
            mmio.write(addr, mmio.read(addr).wrapping_add(1));
            cpu.registers.set_flag(registers::Flag::Zero, mmio.read(addr) == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (mmio.read(addr) & 0x0F) == 0);
            12
        }
    };
}

macro_rules! make_and_register {
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

macro_rules! make_pop_register_pair {
    ($name:ident, $reg1:ident, $reg2:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) -> u8 {
            let addr = cpu.registers.sp;
            let val = (_mmio.read(addr) as u16) | ((_mmio.read(addr.wrapping_add(1)) as u16) << 8);
            cpu.registers.sp = cpu.registers.sp.wrapping_add(2);
            cpu.registers.$reg1 = (val >> 8) as u8;
            cpu.registers.$reg2 = (val & 0x00FF) as u8;
            12
        }
    };
}

macro_rules! make_xor_register {
    ($name:ident, $reg:ident) => {
        pub fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) -> u8 {
            let result = cpu.registers.a ^ cpu.registers.$reg;
            cpu.registers.a = result;
            cpu.registers.set_flag(registers::Flag::Zero, result == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, false);
            cpu.registers.set_flag(registers::Flag::Carry, false);
            4
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
make_ld_register_imm!(ld_a_imm, a);
make_ld_register_imm!(ld_b_imm, b);
make_ld_register_imm!(ld_c_imm, c);
make_ld_register_imm!(ld_d_imm, d);
make_ld_register_imm!(ld_e_imm, e);
make_ld_register_imm!(ld_h_imm, h);
make_ld_register_imm!(ld_l_imm, l);
make_inc_memory!(inc_memory_hl, h, l);
make_and_register!(and_a, a);
make_and_register!(and_b, b);
make_and_register!(and_c, c);
make_and_register!(and_d, d);
make_and_register!(and_e, e);
make_and_register!(and_h, h);
make_and_register!(and_l, l);
make_pop_register_pair!(pop_af, a, f);
make_xor_register!(xor_a, a);
make_xor_register!(xor_b, b);
make_xor_register!(xor_c, c);
make_xor_register!(xor_d, d);
make_xor_register!(xor_e, e);
make_xor_register!(xor_h, h);
make_xor_register!(xor_l, l);
make_ld_16_bit_imm!(ld_bc_imm, b, c);
make_ld_16_bit_imm!(ld_de_imm, d, e);
make_ld_16_bit_imm!(ld_hl_imm, h, l);
make_jr_cond!(jr_nz_imm, |cpu: &cpu::SM83| !cpu.registers.get_flag(registers::Flag::Zero));
make_jr_cond!(jr_z_imm, |cpu: &cpu::SM83| cpu.registers.get_flag(registers::Flag::Zero));
make_jr_cond!(jr_nc_imm, |cpu: &cpu::SM83| !cpu.registers.get_flag(registers::Flag::Carry));
make_jr_cond!(jr_c_imm, |cpu: &cpu::SM83| cpu.registers.get_flag(registers::Flag::Carry));

pub fn di(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) -> u8 {
    cpu.registers.ime = false;
    4
}

pub fn ei(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) -> u8 {
    cpu.registers.ime = true;
    4
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
    let value = mmio.read(cpu.registers.pc) as u16;
    cpu.registers.pc += 1;

    let carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1 } else { 0 } as u8;
    let result = (cpu.registers.a as u16)
        .wrapping_sub(value as u16)
        .wrapping_sub(carry as u16);
    cpu.registers.a = (result & 0x00FF) as u8;
    cpu.registers.set_flag(registers::Flag::Zero, result & 0xFF == 0);
    cpu.registers.set_flag(registers::Flag::Negative, true);
    cpu.registers.set_flag(registers::Flag::Carry, result > 0xFF);
    cpu.registers.set_flag(registers::Flag::HalfCarry, (cpu.registers.a & 0x0F) < ((result & 0x000F) as u8) + carry);
    8
}
