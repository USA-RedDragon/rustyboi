pub enum Flag {
    Carry = 1<<4,
    HalfCarry = 1<<5,
    Negative = 1<<6,
    Zero = 1<<7,
}

pub struct Registers {
    pub a: u8,
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    pub pc: u16,
    pub sp: u16,
    pub ime: bool, // Interrupt Master Enable Flag
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
            ime: false,
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

    pub fn reset(&mut self, skip_bios: bool) {
        self.b = 0x00;
        self.d = 0x00;
        self.ime = false;
        if skip_bios {
            self.a = 0x01;
            self.f = Flag::Zero as u8;
            self.c = 0x13;
            self.e = 0xD8;
            self.h = 0x01;
            self.l = 0x4D;
            self.sp = 0xFFFE;
            self.pc = 0x0100;
        } else {
            self.a = 0x0;
            self.f = 0x0;
            self.c = 0x0;
            self.e = 0x0;
            self.h = 0x0;
            self.l = 0x0;
            self.sp = 0x0;
            self.pc = 0x0;
        }
    }
}
