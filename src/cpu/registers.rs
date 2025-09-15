pub enum Register {
    A, F, B, C, D, E, H, L, PC, SP
}

pub enum Flag {
    Carry = 1<<4,
    HalfCarry = 1<<5,
    Negative = 1<<6,
    Zero = 1<<7,
}

pub struct Registers {
    a: u8,
    f: u8,
    b: u8,
    c: u8,
    d: u8,
    e: u8,
    h: u8,
    l: u8,
    pc: u16,
    sp: u16,
}

impl Registers {
    pub fn new() -> Self {
        Registers {
            a: 0,
            f: 0,
            b: 0,
            c: 0,
            d: 0,
            e: 0,
            h: 0,
            l: 0,
            pc: 0,
            sp: 0,
        }
    }

    pub fn get(&self, reg: Register) -> u16 {
        match reg {
            Register::A => self.a as u16,
            Register::F => self.f as u16,
            Register::B => self.b as u16,
            Register::C => self.c as u16,
            Register::D => self.d as u16,
            Register::E => self.e as u16,
            Register::H => self.h as u16,
            Register::L => self.l as u16,
            Register::PC => self.pc,
            Register::SP => self.sp,
        }
    }

    pub fn set(&mut self, reg: Register, value: u16) {
        match reg {
            Register::A => self.a = value as u8,
            Register::F => self.f = value as u8,
            Register::B => self.b = value as u8,
            Register::C => self.c = value as u8,
            Register::D => self.d = value as u8,
            Register::E => self.e = value as u8,
            Register::H => self.h = value as u8,
            Register::L => self.l = value as u8,
            Register::PC => self.pc = value,
            Register::SP => self.sp = value,
        }
    }

    pub fn increment(&mut self, reg: Register) {
        match reg {
            Register::A => self.a = self.a.wrapping_add(1),
            Register::F => self.f = self.f.wrapping_add(1),
            Register::B => self.b = self.b.wrapping_add(1),
            Register::C => self.c = self.c.wrapping_add(1),
            Register::D => self.d = self.d.wrapping_add(1),
            Register::E => self.e = self.e.wrapping_add(1),
            Register::H => self.h = self.h.wrapping_add(1),
            Register::L => self.l = self.l.wrapping_add(1),
            Register::PC => self.pc = self.pc.wrapping_add(1),
            Register::SP => self.sp = self.sp.wrapping_add(1),
        }
    }

    pub fn decrement(&mut self, reg: Register) {
        match reg {
            Register::A => self.a = self.a.wrapping_sub(1),
            Register::F => self.f = self.f.wrapping_sub(1),
            Register::B => self.b = self.b.wrapping_sub(1),
            Register::C => self.c = self.c.wrapping_sub(1),
            Register::D => self.d = self.d.wrapping_sub(1),
            Register::E => self.e = self.e.wrapping_sub(1),
            Register::H => self.h = self.h.wrapping_sub(1),
            Register::L => self.l = self.l.wrapping_sub(1),
            Register::PC => self.pc = self.pc.wrapping_sub(1),
            Register::SP => self.sp = self.sp.wrapping_sub(1),
        }
    }

    pub fn set_flag(&mut self, flag: Flag, value: bool) {
        if value {
            self.f |= flag as u8;
        } else {
            self.f &= !(flag as u8);
        }
    }

    pub fn get_flag(&self, flag: Flag) -> bool {
        (self.f & (flag as u8)) != 0
    }
}
