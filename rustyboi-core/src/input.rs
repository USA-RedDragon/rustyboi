use crate::memory::Addressable;

use serde::{Deserialize, Serialize};

pub const JOYP: u16 = 0xFF00;

#[derive(Debug, Clone, Copy, Default)]
pub struct ButtonState {
    pub a: bool,
    pub b: bool,
    pub start: bool,
    pub select: bool,
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
}

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

    pub fn set_button_state(&mut self, state: ButtonState) {
        self.a = state.a;
        self.b = state.b;
        self.start = state.start;
        self.select = state.select;
        self.up = state.up;
        self.down = state.down;
        self.left = state.left;
        self.right = state.right;
    }
}

impl Addressable for Input {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            // Bits 6-7 are unused and always read back as 1 (open bus). Bits
            // 4-5 reflect the last-written select lines; bits 0-3 the button
            // state of the selected group.
            JOYP => self.joyp | 0xC0,
            _ => panic!("Input: Invalid read address {:04X}", addr),
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            JOYP => {
                // Bits 4-5 hold exactly the written select lines (not the old
                // ones); the low nibble is the selected group's pressed state.
                let select = value & 0b0011_0000;
                let low = if value & JoypadBits::SelectButtons as u8 == 0 {
                    ((!self.start as u8) << 3)
                        | ((!self.select as u8) << 2)
                        | ((!self.b as u8) << 1)
                        | (!self.a as u8)
                } else if value & JoypadBits::SelectDirections as u8 == 0 {
                    ((!self.down as u8) << 3)
                        | ((!self.up as u8) << 2)
                        | ((!self.left as u8) << 1)
                        | (!self.right as u8)
                } else {
                    0b0000_1111
                };
                self.joyp = select | (low & 0x0F);
            },
            _ => panic!("Input: Invalid write address {:04X}", addr),
        }
    }
}
