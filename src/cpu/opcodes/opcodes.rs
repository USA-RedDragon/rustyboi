use crate::{cpu, cpu::registers, memory, memory::Addressable};

type OpcodeFn = fn(&mut cpu::SM83, &mut memory::mmio::MMIO);

pub struct Opcode {
    pub name: &'static str,
    pub length: u8,
    pub cycles: u8,
    pub execute: OpcodeFn,
}

fn nop(_cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) {
}

macro_rules! make_inc_register {
    ($name:ident, $reg:expr) => {
        fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) {
            cpu.registers.increment($reg);
            cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.get($reg) == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (cpu.registers.get($reg) & 0x0F) == 0);
        }
    };
}

macro_rules! make_dec_register {
    ($name:ident, $reg:expr) => {
        fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) {
            cpu.registers.decrement($reg);
            cpu.registers.set_flag(registers::Flag::Zero, cpu.registers.get($reg) == 0);
            cpu.registers.set_flag(registers::Flag::Negative, true);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (cpu.registers.get($reg) & 0x0F) == 0x0F);
        }
    };
}

macro_rules! make_ld_register_imm {
    ($name:ident, $reg:expr) => {
        fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) {
            let val = mmio.read(cpu.registers.get(registers::Register::PC));
            cpu.registers.set($reg, val as u16);
            cpu.registers.increment(registers::Register::PC);
        }
    };
}

macro_rules! make_inc_memory {
    ($name:ident, $reg1:expr, $reg2:expr) => {
        fn $name(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) {
            let addr = ((cpu.registers.get($reg1) as u16) << 8) | (cpu.registers.get($reg2) as u16);
            mmio.write(addr, mmio.read(addr).wrapping_add(1));
            cpu.registers.set_flag(registers::Flag::Zero, mmio.read(addr) == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, (mmio.read(addr) & 0x0F) == 0);
        }
    };
}

macro_rules! make_and_register {
    ($name:ident, $reg:expr) => {
        fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) {
            let result = cpu.registers.get(registers::Register::A) & cpu.registers.get($reg);
            cpu.registers.set(registers::Register::A, result);
            cpu.registers.set_flag(registers::Flag::Zero, result == 0);
            cpu.registers.set_flag(registers::Flag::Negative, false);
            cpu.registers.set_flag(registers::Flag::HalfCarry, true);
            cpu.registers.set_flag(registers::Flag::Carry, false);
        }
    };
}

macro_rules! make_pop_register_pair {
    ($name:ident, $reg1:expr, $reg2:expr) => {
        fn $name(cpu: &mut cpu::SM83, _mmio: &mut memory::mmio::MMIO) {
            let addr = cpu.registers.get(registers::Register::SP);
            let val = (_mmio.read(addr) as u16) | ((_mmio.read(addr.wrapping_add(1)) as u16) << 8);
            cpu.registers.set(registers::Register::SP, cpu.registers.get(registers::Register::SP).wrapping_add(2));
            cpu.registers.set($reg1, val >> 8 as u8);
            cpu.registers.set($reg2, val.into());
        }
    };
}

make_inc_register!(inc_a, registers::Register::A);
make_inc_register!(inc_b, registers::Register::B);
make_inc_register!(inc_c, registers::Register::C);
make_inc_register!(inc_d, registers::Register::D);
make_inc_register!(inc_e, registers::Register::E);
make_inc_register!(inc_h, registers::Register::H);
make_inc_register!(inc_l, registers::Register::L);
make_inc_register!(inc_sp, registers::Register::SP);
make_dec_register!(dec_a, registers::Register::A);
make_dec_register!(dec_b, registers::Register::B);
make_dec_register!(dec_c, registers::Register::C);
make_dec_register!(dec_d, registers::Register::D);
make_dec_register!(dec_e, registers::Register::E);
make_dec_register!(dec_h, registers::Register::H);
make_dec_register!(dec_l, registers::Register::L);
make_dec_register!(dec_sp, registers::Register::SP);
make_ld_register_imm!(ld_a_imm, registers::Register::A);
make_ld_register_imm!(ld_b_imm, registers::Register::B);
make_ld_register_imm!(ld_c_imm, registers::Register::C);
make_ld_register_imm!(ld_d_imm, registers::Register::D);
make_ld_register_imm!(ld_e_imm, registers::Register::E);
make_ld_register_imm!(ld_h_imm, registers::Register::H);
make_ld_register_imm!(ld_l_imm, registers::Register::L);
make_inc_memory!(inc_memory_hl, registers::Register::H, registers::Register::L);
make_and_register!(and_a, registers::Register::A);
make_and_register!(and_b, registers::Register::B);
make_and_register!(and_c, registers::Register::C);
make_and_register!(and_d, registers::Register::D);
make_and_register!(and_e, registers::Register::E);
make_and_register!(and_h, registers::Register::H);
make_and_register!(and_l, registers::Register::L);
make_pop_register_pair!(pop_af, registers::Register::A, registers::Register::F);

fn sbc_a_imm(cpu: &mut cpu::SM83, mmio: &mut memory::mmio::MMIO) {
    let value = mmio.read(cpu.registers.get(registers::Register::PC)) as u16;
    cpu.registers.increment(registers::Register::PC);

    let carry = if cpu.registers.get_flag(registers::Flag::Carry) { 1 } else { 0 };
    let result = cpu.registers.get(registers::Register::A)
        .wrapping_sub(value)
        .wrapping_sub(carry) as u16;
    cpu.registers.set(registers::Register::A, result & 0xFF);
    cpu.registers.set_flag(registers::Flag::Zero, result & 0xFF == 0);
    cpu.registers.set_flag(registers::Flag::Negative, true);
    cpu.registers.set_flag(registers::Flag::Carry, result > 0xFF);
    cpu.registers.set_flag(registers::Flag::HalfCarry, (cpu.registers.get(registers::Register::A) & 0x0F) < (value & 0x0F) + carry);
}

pub const OPCODES: [Opcode; 256] = [
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x00
    Opcode { name: "LD BC, nn", length: 3, cycles: 12, execute: nop }, // 0x01
    Opcode { name: "LD (BC), A", length: 1, cycles: 8, execute: nop }, // 0x02
    Opcode { name: "INC BC", length: 1, cycles: 8, execute: nop }, // 0x03
    Opcode { name: "INC B", length: 1, cycles: 4, execute: inc_b }, // 0x04
    Opcode { name: "DEC B", length: 1, cycles: 4, execute: dec_b }, // 0x05
    Opcode { name: "LD B,n", length: 2, cycles: 8, execute: ld_b_imm }, // 0x06
    Opcode { name: "RLCA", length: 1, cycles: 4, execute: nop }, // 0x07
    Opcode { name: "LD (nn),SP", length: 1, cycles: 4, execute: nop }, // 0x08
    Opcode { name: "ADD HL,BC", length: 1, cycles: 4, execute: nop }, // 0x09
    Opcode { name: "LD A,(BC)", length: 1, cycles: 4, execute: nop }, // 0x0A
    Opcode { name: "DEC BC", length: 1, cycles: 4, execute: nop }, // 0x0B
    Opcode { name: "INC C", length: 1, cycles: 4, execute: inc_c }, // 0x0C
    Opcode { name: "DEC C", length: 1, cycles: 4, execute: dec_c }, // 0x0D
    Opcode { name: "LD C,n", length: 2, cycles: 8, execute: ld_c_imm }, // 0x0E
    Opcode { name: "RRCA", length: 1, cycles: 4, execute: nop }, // 0x0F
    Opcode { name: "STOP", length: 1, cycles: 4, execute: nop }, // 0x10
    Opcode { name: "LD DE,nn", length: 1, cycles: 4, execute: nop }, // 0x11
    Opcode { name: "LD (DE),A", length: 1, cycles: 4, execute: nop }, // 0x12
    Opcode { name: "INC DE", length: 1, cycles: 4, execute: nop }, // 0x13
    Opcode { name: "INC D", length: 1, cycles: 4, execute: inc_d }, // 0x14
    Opcode { name: "DEC D", length: 1, cycles: 4, execute: dec_d }, // 0x15
    Opcode { name: "LD D,n", length: 2, cycles: 8, execute: ld_d_imm }, // 0x16
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x17
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x18
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x19
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x1A
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x1B
    Opcode { name: "INC E", length: 1, cycles: 4, execute: inc_e }, // 0x1C
    Opcode { name: "DEC E", length: 1, cycles: 4, execute: dec_e }, // 0x1D
    Opcode { name: "LD E,n", length: 2, cycles: 8, execute: ld_e_imm }, // 0x1E
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x1F
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x20
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x21
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x22
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x23
    Opcode { name: "INC H", length: 1, cycles: 4, execute: inc_h }, // 0x24
    Opcode { name: "DEC H", length: 1, cycles: 4, execute: dec_h }, // 0x25
    Opcode { name: "LD H,n", length: 2, cycles: 8, execute: ld_h_imm }, // 0x26
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x27
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x28
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x29
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x2A
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x2B
    Opcode { name: "INC L", length: 1, cycles: 4, execute: inc_l }, // 0x2C
    Opcode { name: "DEC L", length: 1, cycles: 4, execute: dec_l }, // 0x2D
    Opcode { name: "LD L,n", length: 2, cycles: 8, execute: ld_l_imm }, // 0x2E
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x2F
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x30
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x31
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x32
    Opcode { name: "INC SP", length: 1, cycles: 8, execute: inc_sp }, // 0x33
    Opcode { name: "INC (HL)", length: 1, cycles: 12, execute: inc_memory_hl }, // 0x34
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x35
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x36
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x37
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x38
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x39
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x3A
    Opcode { name: "DEC SP", length: 1, cycles: 8, execute: dec_sp }, // 0x3B
    Opcode { name: "INC A", length: 1, cycles: 4, execute: inc_a }, // 0x3C
    Opcode { name: "DEC A", length: 1, cycles: 4, execute: dec_a }, // 0x3D
    Opcode { name: "LD A,n", length: 2, cycles: 8, execute: ld_a_imm }, // 0x3E
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x3F
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x40
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x41
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x42
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x43
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x44
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x45
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x46
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x47
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x48
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x49
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x4A
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x4B
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x4C
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x4D
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x4E
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x4F
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x50
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x51
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x52
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x53
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x54
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x55
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x56
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x57
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x58
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x59
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x5A
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x5B
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x5C
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x5D
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x5E
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x5F
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x60
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x61
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x62
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x63
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x64
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x65
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x66
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x67
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x68
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x69
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x6A
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x6B
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x6C
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x6D
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x6E
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x6F
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x70
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x71
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x72
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x73
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x74
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x75
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x76
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x77
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x78
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x79
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x7A
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x7B
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x7C
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x7D
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x7E
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x7F
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x80
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x81
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x82
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x83
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x84
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x85
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x86
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x87
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x88
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x89
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x8A
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x8B
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x8C
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x8D
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x8E
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x8F
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x90
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x91
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x92
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x93
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x94
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x95
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x96
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x97
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x98
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x99
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x9A
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x9B
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x9C
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x9D
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x9E
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x9F
    Opcode { name: "AND B", length: 1, cycles: 4, execute: and_b }, // 0xA0
    Opcode { name: "AND C", length: 1, cycles: 4, execute: and_c }, // 0xA1
    Opcode { name: "AND D", length: 1, cycles: 4, execute: and_d }, // 0xA2
    Opcode { name: "AND E", length: 1, cycles: 4, execute: and_e }, // 0xA3
    Opcode { name: "AND H", length: 1, cycles: 4, execute: and_h }, // 0xA4
    Opcode { name: "AND L", length: 1, cycles: 4, execute: and_l }, // 0xA5
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xA6
    Opcode { name: "AND A", length: 1, cycles: 4, execute: and_a }, // 0xA7
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xA8
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xA9
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xAA
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xAB
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xAC
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xAD
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xAE
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xAF
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xB0
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xB1
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xB2
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xB3
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xB4
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xB5
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xB6
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xB7
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xB8
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xB9
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xBA
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xBB
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xBC
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xBD
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xBE
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xBF
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xC0
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xC1
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xC2
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xC3
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xC4
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xC5
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xC6
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xC7
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xC8
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xC9
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xCA
    Opcode { name: "PREFIX CB", length: 1, cycles: 4, execute: nop }, // 0xCB, handled in the fetch stage
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xCC
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xCD
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xCE
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xCF
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xD0
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xD1
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xD2
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xD3
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xD4
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xD5
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xD6
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xD7
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xD8
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xD9
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xDA
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xDB
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xDC
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xDD
    Opcode { name: "SBC A,n", length: 2, cycles: 8, execute: sbc_a_imm }, // 0xDE
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xDF
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xE0
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xE1
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xE2
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xE3
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xE4
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xE5
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xE6
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xE7
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xE8
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xE9
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xEA
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xEB
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xEC
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xED
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xEE
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xEF
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xF0
    Opcode { name: "POP AF", length: 1, cycles: 12, execute: pop_af }, // 0xF1
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xF2
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xF3
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xF4
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xF5
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xF6
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xF7
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xF8
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xF9
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xFA
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xFB
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xFC
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xFD
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xFE
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xFF
];

pub const CB_OPCODES: [Opcode; 256] = [
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x00
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x01
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x02
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x03
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x04
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x05
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x06
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x07
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x08
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x09
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x0A
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x0B
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x0C
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x0D
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x0E
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x0F
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x10
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x11
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x12
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x13
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x14
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x15
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x16
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x17
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x18
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x19
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x1A
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x1B
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x1C
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x1D
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x1E
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x1F
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x20
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x21
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x22
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x23
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x24
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x25
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x26
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x27
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x28
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x29
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x2A
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x2B
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x2C
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x2D
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x2E
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x2F
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x30
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x31
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x32
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x33
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x34
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x35
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x36
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x37
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x38
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x39
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x3A
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x3B
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x3C
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x3D
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x3E
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x3F
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x40
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x41
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x42
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x43
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x44
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x45
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x46
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x47
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x48
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x49
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x4A
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x4B
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x4C
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x4D
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x4E
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x4F
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x50
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x51
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x52
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x53
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x54
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x55
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x56
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x57
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x58
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x59
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x5A
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x5B
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x5C
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x5D
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x5E
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x5F
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x60
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x61
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x62
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x63
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x64
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x65
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x66
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x67
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x68
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x69
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x6A
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x6B
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x6C
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x6D
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x6E
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x6F
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x70
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x71
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x72
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x73
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x74
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x75
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x76
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x77
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x78
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x79
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x7A
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x7B
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x7C
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x7D
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x7E
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x7F
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x80
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x81
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x82
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x83
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x84
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x85
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x86
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x87
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x88
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x89
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x8A
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x8B
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x8C
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x8D
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x8E
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x8F
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x90
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x91
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x92
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x93
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x94
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x95
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x96
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x97
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x98
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x99
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x9A
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x9B
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x9C
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x9D
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x9E
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0x9F
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xA0
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xA1
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xA2
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xA3
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xA4
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xA5
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xA6
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xA7
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xA8
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xA9
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xAA
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xAB
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xAC
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xAD
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xAE
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xAF
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xB0
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xB1
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xB2
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xB3
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xB4
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xB5
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xB6
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xB7
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xB8
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xB9
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xBA
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xBB
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xBC
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xBD
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xBE
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xBF
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xC0
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xC1
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xC2
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xC3
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xC4
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xC5
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xC6
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xC7
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xC8
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xC9
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xCA
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xCB
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xCC
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xCD
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xCE
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xCF
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xD0
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xD1
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xD2
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xD3
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xD4
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xD5
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xD6
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xD7
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xD8
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xD9
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xDA
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xDB
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xDC
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xDD
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xDE
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xDF
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xE0
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xE1
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xE2
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xE3
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xE4
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xE5
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xE6
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xE7
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xE8
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xE9
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xEA
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xEB
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xEC
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xED
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xEE
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xEF
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xF0
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xF1
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xF2
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xF3
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xF4
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xF5
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xF6
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xF7
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xF8
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xF9
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xFA
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xFB
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xFC
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xFD
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xFE
    Opcode { name: "NOP", length: 1, cycles: 4, execute: nop }, // 0xFF
];
