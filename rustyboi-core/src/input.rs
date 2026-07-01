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

    /// Super Game Boy state. `Some` only on Hardware::SGB/SGB2 (set via
    /// `enable_sgb`); on DMG/CGB this is `None` and the JOYP path is unchanged.
    #[serde(default)]
    sgb: Option<crate::sgb::Sgb>,
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
            sgb: None,
        }
    }

    /// Turn on Super Game Boy JOYP-packet handling. Called once from `GB::new`
    /// for Hardware::SGB/SGB2 only; leaves DMG/CGB behavior untouched.
    pub fn enable_sgb(&mut self) {
        self.sgb = Some(crate::sgb::Sgb::new());
    }

    /// Immutable access to the SGB state (palettes/mask), for the frame output
    /// path. `None` on non-SGB hardware.
    pub fn sgb(&self) -> Option<&crate::sgb::Sgb> {
        self.sgb.as_ref()
    }

    /// Mutable access to the SGB state, for servicing pending VRAM (_TRN)
    /// transfers from the memory unit. `None` on non-SGB hardware.
    pub fn sgb_mut(&mut self) -> Option<&mut crate::sgb::Sgb> {
        self.sgb.as_mut()
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
                // SGB packet reception: feed every JOYP write to the SGB state
                // machine (RESET/bit pulses on P14/P15). This runs BEFORE the
                // normal joypad response and only exists on SGB hardware.
                if let Some(sgb) = self.sgb.as_mut() {
                    sgb.write_p1(value);
                }
                // Bits 4-5 hold exactly the written select lines (not the old
                // ones); the low nibble is the selected group's pressed state.
                let select = value & 0b0011_0000;
                let mut low = if value & JoypadBits::SelectButtons as u8 == 0 {
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
                // SGB MLT_REQ multiplexing: when the game deselects both groups
                // (both P14/P15 high) the low nibble reports the current player
                // ID (0x0F - joypad_index) instead of a plain 0x0F. This is what
                // the MLT_REQ read protocol clocks through to enumerate players.
                if let Some(sgb) = self.sgb.as_ref() {
                    if select == 0b0011_0000 {
                        low &= sgb.joypad_id_nibble();
                    }
                }
                self.joyp = select | (low & 0x0F);
            },
            _ => panic!("Input: Invalid write address {:04X}", addr),
        }
    }
}
