use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub enum Flag {
    Carry = 1<<4,
    HalfCarry = 1<<5,
    Negative = 1<<6,
    Zero = 1<<7,
}

#[derive(Serialize, Deserialize)]
pub enum InterruptFlag {
    Joypad = 1<<4,
    Serial = 1<<3,
    Timer = 1<<2,
    Lcd = 1<<1,
    VBlank = 1<<0,
}

pub const INTERRUPT_FLAG: u16 = 0xFF0F;
pub const INTERRUPT_ENABLE: u16 = 0xFFFF;

#[derive(Serialize, Deserialize, Clone)]
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
}
