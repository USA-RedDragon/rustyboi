use crate::memory::mmio;
use crate::memory::Addressable;

pub const JOYP: u16 = 0xFF00;

#[derive(Clone, Default)]
pub struct Input {
    pub a: bool,
    pub b: bool,
    pub select: bool,
    pub start: bool,
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
}

enum JoypadBits {
    SelectButtons = 1<<5,
    SelectDirections = 1<<4,
    StartDown = 1<<3,
    SelectUp = 1<<2,
    BLeftRight = 1<<1,
    ARight = 1<<0
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
        }
    }

    pub fn update(&self, mmio: &mut mmio::MMIO) {
        let mut joyp = mmio.read(JOYP) & 0b00110000;
        if (joyp & JoypadBits::SelectButtons as u8) == 0 {
            if self.start { joyp &= !(JoypadBits::StartDown as u8); } else { joyp |= JoypadBits::StartDown as u8; }
            if self.select { joyp &= !(JoypadBits::SelectUp as u8); } else { joyp |= JoypadBits::SelectUp as u8; }
            if self.b { joyp &= !(JoypadBits::BLeftRight as u8); } else { joyp |= JoypadBits::BLeftRight as u8; }
            if self.a { joyp &= !(JoypadBits::ARight as u8); } else { joyp |= JoypadBits::ARight as u8; }
        } else if (joyp & JoypadBits::SelectDirections as u8) == 0 {
            if self.down { joyp &= !(JoypadBits::StartDown as u8); } else { joyp |= JoypadBits::StartDown as u8; }
            if self.up { joyp &= !(JoypadBits::SelectUp as u8); } else { joyp |= JoypadBits::SelectUp as u8; }
            if self.left { joyp &= !(JoypadBits::BLeftRight as u8); } else { joyp |= JoypadBits::BLeftRight as u8; }
            if self.right { joyp &= !(JoypadBits::ARight as u8); } else { joyp |= JoypadBits::ARight as u8; }
        } else {
            joyp |= 0b00001111;
        }
        mmio.write(JOYP, joyp);
    }
}
