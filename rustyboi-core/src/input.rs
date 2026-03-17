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

impl Default for Input {
    fn default() -> Self {
        Self::new()
    }
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

    /// Apply the SGB header unlock gate (Pan Docs "SGB Unlocking"): a non-SGB
    /// cart's JOYP writes must not be interpreted as SGB packets. Called from
    /// `GB::insert` with the cartridge header verdict; no-op on non-SGB
    /// hardware.
    pub fn set_sgb_unlocked(&mut self, unlocked: bool) {
        if let Some(sgb) = self.sgb.as_mut() {
            sgb.set_locked(!unlocked);
        }
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

    /// Update the pressed-button state and refresh the JOYP low nibble for the
    /// currently-selected line group (the hardware lines are live, not latched
    /// to JOYP writes). Returns true when any selected input line transitioned
    /// high -> low (a newly-pressed button on an active group), which is the
    /// joypad-interrupt condition; the caller raises IF bit 4.
    pub fn set_button_state(&mut self, state: ButtonState) -> bool {
        self.a = state.a;
        self.b = state.b;
        self.start = state.start;
        self.select = state.select;
        self.up = state.up;
        self.down = state.down;
        self.left = state.left;
        self.right = state.right;

        let old_low = self.joyp & 0x0F;
        let select = self.joyp & 0b0011_0000;
        let new_low = self.low_nibble(select);
        self.joyp = select | new_low;
        (old_low & !new_low) != 0
    }

    /// Low-nibble line state for the given select bits (bits 4-5 of JOYP):
    /// pressed buttons of a selected group pull their line low. With BOTH
    /// groups selected the lines are a logical AND of the two groups (any
    /// pressed key pulls its line low) — this is how real hardware wires
    /// P10-P13, and what a listen-for-everything JOYP=0x00 idle relies on.
    /// Both groups deselected reads 1111 (or the SGB MLT_REQ player-ID
    /// nibble).
    fn low_nibble(&self, select: u8) -> u8 {
        let mut low = 0b0000_1111u8;
        if select & JoypadBits::SelectButtons as u8 == 0 {
            low &= ((!self.start as u8) << 3)
                | ((!self.select as u8) << 2)
                | ((!self.b as u8) << 1)
                | (!self.a as u8);
        }
        if select & JoypadBits::SelectDirections as u8 == 0 {
            low &= ((!self.down as u8) << 3)
                | ((!self.up as u8) << 2)
                | ((!self.left as u8) << 1)
                | (!self.right as u8);
        }
        // SGB MLT_REQ multiplexing: when the game deselects both groups
        // (both P14/P15 high) the low nibble reports the current player
        // ID (0x0F - joypad_index) instead of a plain 0x0F. This is what
        // the MLT_REQ read protocol clocks through to enumerate players.
        if let Some(sgb) = self.sgb.as_ref()
            && select == 0b0011_0000 {
                low &= sgb.joypad_id_nibble();
            }
        low & 0x0F
    }

    /// JOYP ($FF00) write: latch the select lines (bits 4-5) and refresh the
    /// low nibble for the newly-selected group. Returns true when any of the
    /// four P10-P13 input lines transitioned high -> low as a result: selecting
    /// a group whose buttons are held pulls those lines low, and the joypad
    /// interrupt (IF bit 4) fires on any such edge exactly as for a fresh key
    /// press (Pan Docs "Joypad Input"). The caller raises IF bit 4.
    pub fn write_joyp(&mut self, value: u8) -> bool {
        // SGB packet reception: feed every JOYP write to the SGB state
        // machine (RESET/bit pulses on P14/P15). This runs BEFORE the
        // normal joypad response and only exists on SGB hardware.
        if let Some(sgb) = self.sgb.as_mut() {
            sgb.write_p1(value);
        }
        // Bits 4-5 hold exactly the written select lines (not the old
        // ones); the low nibble is the selected group's pressed state.
        let old_low = self.joyp & 0x0F;
        let select = value & 0b0011_0000;
        let new_low = self.low_nibble(select);
        self.joyp = select | new_low;
        (old_low & !new_low) != 0
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
            // Edge-blind fallback; the MMIO dispatch uses `write_joyp` so the
            // select-write high->low line edge can raise the joypad interrupt.
            JOYP => {
                self.write_joyp(value);
            }
            _ => panic!("Input: Invalid write address {:04X}", addr),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pan Docs "Joypad Input": the joypad interrupt fires on a high->low
    /// transition of any P10-P13 line. Selecting a group whose buttons are
    /// already held produces exactly such an edge; reselecting the same group,
    /// deselecting (lines return high), or selecting a group with no held
    /// buttons must not.
    #[test]
    fn joyp_select_write_reports_high_to_low_edge() {
        let mut input = Input::new();
        // Deselect both groups, then hold A: lines stay high, no edge.
        input.write_joyp(0x30);
        assert!(!input.set_button_state(ButtonState { a: true, ..Default::default() }));
        // Selecting the button group (P15 low) pulls P10 low: edge.
        assert!(input.write_joyp(0x10));
        // Same select again: lines already low, no new edge.
        assert!(!input.write_joyp(0x10));
        // Deselecting is a low -> high transition: no edge.
        assert!(!input.write_joyp(0x30));
        // Selecting the direction group with only A held: no edge.
        assert!(!input.write_joyp(0x20));
    }
}
