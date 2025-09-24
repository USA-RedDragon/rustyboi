use crate::memory::Addressable;

use serde::{Deserialize, Serialize};

pub const JOYP: u16 = 0xFF00;

#[derive(Serialize, Deserialize, Clone)]
pub struct Input {
    #[serde(skip, default)]
    pub a: bool,
    #[serde(skip, default)]
    pub b: bool,
    #[serde(skip, default)]
    pub select: bool,
    #[serde(skip, default)]
    pub start: bool,
    #[serde(skip, default)]
    pub up: bool,
    #[serde(skip, default)]
    pub down: bool,
    #[serde(skip, default)]
    pub left: bool,
    #[serde(skip, default)]
    pub right: bool,

    joyp: u8,
}

enum JoypadBits {
    SelectButtons = 1<<5,
    SelectDirections = 1<<4,
}

impl Input {
    pub fn new() -> Self {
        Input {
            a: false,
            b: false,
            select: false,
            start: false,
            up: false,
            down: false,
            left: false,
            right: false,
            joyp: 0b00001111,
        }
    }
}

impl Addressable for Input {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            JOYP => self.joyp,
            _ => panic!("Input: Invalid read address {:04X}", addr),
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            JOYP => {
                if value & JoypadBits::SelectButtons as u8 == 0 {
                    // Select Buttons
                    self.joyp = (self.joyp & 0b00110000)
                        | ((!self.start as u8) << 3)
                        | ((!self.select as u8) << 2)
                        | ((!self.b as u8) << 1)
                        | (!self.a as u8);
                } else if value & JoypadBits::SelectDirections as u8 == 0 {
                    // Select Directions
                    self.joyp = (self.joyp & 0b00110000)
                        | ((!self.down as u8) << 3)
                        | ((!self.up as u8) << 2)
                        | ((!self.left as u8) << 1)
                        | (!self.right as u8);
                } else {
                    // Neither selected
                    self.joyp = 0b00001111;
                }
            },
            _ => panic!("Input: Invalid write address {:04X}", addr),
        }
    }
}
