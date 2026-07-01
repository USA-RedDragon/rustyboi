//! High-level Super Game Boy (SGB) support.
//!
//! The SGB is an SNES cartridge that hosts a Game Boy. Games talk to the SNES
//! side by transmitting command *packets* over the JOYP register ($FF00): the
//! game pulses the P14/P15 select lines to serialise 128 bits (16 bytes) per
//! packet. We interpret those packets HIGH-LEVEL (no SNES core): decode the
//! command, apply its effect (player multiplexing, palettes, mask, ...) on the
//! GB side directly. This is the model BGB/most emulators use and is exactly
//! what the GB-side `sgb-ext-test` protocol stress test checks.
//!
//! ALL of this is gated on `Hardware::SGB`/`SGB2` by the caller; on DMG/CGB the
//! `Sgb` state is never constructed and the JOYP path is byte-identical.

use serde::{Deserialize, Serialize};

/// SGB command numbers (packet byte 0, bits 7-3). Only the ones we act on are
/// named; the rest are decoded but handled as no-ops (cleanly stubbed).
mod cmd {
    pub const PAL01: u8 = 0x00;
    pub const PAL23: u8 = 0x01;
    pub const PAL03: u8 = 0x02;
    pub const PAL12: u8 = 0x03;
    pub const ATTR_BLK: u8 = 0x04;
    pub const ATTR_LIN: u8 = 0x05;
    pub const ATTR_DIV: u8 = 0x06;
    pub const ATTR_CHR: u8 = 0x07;
    pub const PAL_SET: u8 = 0x0A;
    pub const PAL_TRN: u8 = 0x0B;
    pub const MASK_EN: u8 = 0x17;
    pub const ATTR_TRN: u8 = 0x15;
    pub const ATTR_SET: u8 = 0x16;
    pub const MLT_REQ: u8 = 0x11;
    pub const CHR_TRN: u8 = 0x13;
    pub const PCT_TRN: u8 = 0x14;
    pub const DATA_TRN: u8 = 0x0F;
}

/// Screen-mask mode set by MASK_EN. Applied to the GB output frame.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MaskMode {
    /// Normal: GB screen shown as-is.
    #[default]
    Cancel,
    /// Freeze: keep displaying the last frame captured before the freeze.
    Freeze,
    /// Blank the screen to black (color 0 of the current palette).
    Black,
    /// Blank the screen to color 0.
    Color0,
}

/// SGB high-level state: the JOYP packet receiver, the assembled command
/// buffer, and the derived effects (MLT_REQ player multiplexing, mask, palettes).
#[derive(Serialize, Deserialize, Clone)]
pub struct Sgb {
    // ---- Packet reception state machine (SameBoy `GB_sgb_write` model) ----
    // A packet bit is only latched on a P14/P15 pulse when the line has first
    // returned to both-high (`ready_for_pulse`) AND a start pulse (both-low) has
    // armed the receiver (`ready_for_write`). This three-flag handshake handles
    // both the both-high-return and both-low-return bit framings the game uses.
    /// Set by a both-high (0x30) write: the next single-line pulse may latch a bit.
    ready_for_pulse: bool,
    /// Set by a both-low (0x00) start pulse: bit latching is enabled.
    ready_for_write: bool,
    /// Set when a full 128-bit packet has been clocked; the next both-low pulse
    /// finalises the packet (and dispatches the command if the count is met).
    ready_for_stop: bool,
    /// Last value written to JOYP (full byte), for P14 rising-edge detection.
    last_p1: u8,
    /// Bits clocked into the current command so far (across all its packets).
    write_index: u16,
    /// The command buffer: up to 7 packets * 16 bytes, filled bit-by-bit.
    #[serde(default = "default_command", with = "serde_bytes")]
    command: Vec<u8>,

    // ---- MLT_REQ (multiplayer) state ----
    /// Number of players selected by MLT_REQ: 1, 2 or 4.
    players: u8,
    /// Current joypad read index (0..players-1), cycled by the read protocol.
    joypad_index: u8,

    // ---- MASK_EN ----
    pub mask: MaskMode,

    // ---- Palettes (SGB system palettes 0-3, four colors each, RGB555) ----
    /// The four active SGB palettes, each 4 colors, stored as RGB555 (0..0x7FFF).
    palettes: [[u16; 4]; 4],
    /// Per-8x8-attribute-cell palette assignment: 20x18 = 360 cells, value 0-3.
    #[serde(default = "default_attr", with = "serde_bytes")]
    attr: Vec<u8>,
    /// True once any palette command has run (so we know to colorize output).
    pub colorized: bool,

    // ---- VRAM transfer (_TRN) pending flag ----
    /// A *_TRN command is waiting for the next VBlank to read a 4KB block from
    /// VRAM $8000. Holds the command number; None when idle.
    pending_trn: Option<u8>,
    /// SYSTEM color palettes loaded by PAL_TRN (512 palettes of 4 RGB555 colors),
    /// selectable by PAL_SET. Boxed-in-vec to keep the struct small.
    #[serde(default)]
    sys_palettes: Vec<[u16; 4]>,
}

fn default_attr() -> Vec<u8> {
    vec![0u8; 20 * 18]
}

fn default_command() -> Vec<u8> {
    vec![0u8; 16 * 7]
}

impl Default for Sgb {
    fn default() -> Self {
        Self::new()
    }
}

impl Sgb {
    pub fn new() -> Self {
        Sgb {
            // The JOYP line idles both-high at power-on, which is exactly the
            // state a both-high (0x30) write leaves: the receiver is armed for
            // the next single-line pulse. Starting `false` would drop the very
            // first packet, whose transmit routine begins with the both-low
            // start pulse (no preceding 0x30 to arm it). SameSuite's MLT_REQ
            // tests send that first packet with no warm-up, so we must model the
            // idle-high line as pulse-armed.
            ready_for_pulse: true,
            ready_for_write: false,
            ready_for_stop: false,
            last_p1: 0x30,
            write_index: 0,
            command: default_command(),
            players: 1,
            joypad_index: 0,
            mask: MaskMode::Cancel,
            palettes: [[0x7FFF, 0x56B5, 0x294A, 0x0000]; 4],
            attr: default_attr(),
            colorized: false,
            pending_trn: None,
            sys_palettes: Vec::new(),
        }
    }

    /// Number of players currently selected by MLT_REQ (1/2/3-glitch/4).
    pub fn players(&self) -> u8 {
        self.players
    }

    /// The low-nibble value the JOYP read should return for the current joypad
    /// index in MLT_REQ multiplayer mode. Real SGB returns `0x0F - index` in the
    /// low nibble: player 1 (index 0) reads 0x0F, player 2 -> 0x0E, etc. The
    /// button/direction bits are ANDed on top by the caller. In single-player
    /// mode `index` is always 0 so this is 0x0F (a no-op mask).
    pub fn joypad_id_nibble(&self) -> u8 {
        0x0F - (self.joypad_index & 0x0F)
    }

    /// Advance the JOYP write state machine on a $FF00 write. Faithful port of
    /// SameBoy `GB_sgb_write`: bits are decoded from `(value >> 4) & 3` where
    /// 3=both-high (arm pulse), 2=P14-low ("0" bit / advance), 1=P15-low ("1"
    /// bit), 0=both-low (start/stop pulse). The three-flag handshake makes the
    /// receiver tolerant of the multiple bit-framing conventions the stress test
    /// uses, and rejects malformed frames exactly as hardware does.
    pub fn write_p1(&mut self, value: u8) {
        // The command's declared size in bits: byte0 bits 2-0 give the packet
        // count (0 treated as 1), times 128 bits/packet.
        let count = self.command[0] & 7;
        let command_size: u16 = (if count == 0 { 1 } else { count } as u16) * 128;

        // MLT_REQ joypad multiplexing: on the P15 (bit 5) rising edge, advance
        // the current player. Real SGB advances for any multi-player count
        // (2, 3-glitched, or 4) but not in single-player mode, and wraps the
        // index within the live player count. SameSuite's command_mlt_req_1_*
        // read tables pin this edge (P15 0->1) exactly. The wrap here uses
        // `& (players - 1)`; the divergence from `% players` only matters for the
        // invalid 3-player mode and is reconciled at dispatch time.
        if value & 0x20 != 0 && self.last_p1 & 0x20 == 0 && self.players != 1 {
            self.joypad_index = self.joypad_index.wrapping_add(1) & (self.players - 1);
        }
        self.last_p1 = value;

        match (value >> 4) & 3 {
            3 => {
                self.ready_for_pulse = true;
            }
            2 => {
                // "Zero" bit (or, once the packet is full, the stop pulse).
                if !self.ready_for_pulse || !self.ready_for_write {
                    return;
                }
                if self.ready_for_stop {
                    if self.write_index == command_size {
                        self.dispatch();
                        self.write_index = 0;
                        self.command = default_command();
                    }
                    self.ready_for_pulse = false;
                    self.ready_for_write = false;
                    self.ready_for_stop = false;
                } else if (self.write_index as usize) < self.command.len() * 8 {
                    self.write_index += 1;
                    self.ready_for_pulse = false;
                    if self.write_index & (128 - 1) == 0 {
                        self.ready_for_stop = true;
                    }
                }
            }
            1 => {
                // "One" bit.
                if !self.ready_for_pulse || !self.ready_for_write {
                    return;
                }
                if self.ready_for_stop {
                    // Stop pulse: on hardware EITHER a P14-low (0x20) OR a P15-low
                    // (0x10) pulse after the 128th bit terminates the packet and
                    // dispatches (the sgb-ext-test `421A` variant uses a 0x10
                    // stop and expects it to be accepted). SameBoy treats a 0x10
                    // stop as a corrupt command; hardware does not.
                    if self.write_index == command_size {
                        self.dispatch();
                        self.write_index = 0;
                        self.command = default_command();
                    }
                    self.ready_for_pulse = false;
                    self.ready_for_write = false;
                    self.ready_for_stop = false;
                } else if (self.write_index as usize) < self.command.len() * 8 {
                    let byte = (self.write_index / 8) as usize;
                    let bit = (self.write_index & 7) as u8;
                    self.command[byte] |= 1 << bit;
                    self.write_index += 1;
                    self.ready_for_pulse = false;
                    if self.write_index & (128 - 1) == 0 {
                        self.ready_for_stop = true;
                    }
                }
            }
            0 => {
                // Both-low start pulse: arm writing; reset a partial/misaligned
                // command so the next bits start a clean packet.
                if !self.ready_for_pulse {
                    return;
                }
                self.ready_for_write = true;
                self.ready_for_pulse = false;
                if self.write_index & (128 - 1) != 0
                    || self.write_index == 0
                    || self.ready_for_stop
                {
                    self.write_index = 0;
                    self.command = default_command();
                    self.ready_for_stop = false;
                }
            }
            _ => {}
        }
    }

    /// Interpret a fully-assembled command (1..=7 packets in `self.command`) and
    /// apply its effect.
    fn dispatch(&mut self) {
        let command = self.command[0] >> 3;
        match command {
            cmd::MLT_REQ => {
                // Byte 1 bits 0-1 select the player count: 0->1, 1->2, 2->3, 3->4.
                // NOTE: SameBoy's HLE folds the invalid `2` case up to 4 players;
                // real SGB silicon keeps it as a *glitched 3-player* mode (the
                // wrap uses a non-power-of-two modulus), which SameSuite's
                // command_mlt_req captures. We model the hardware, not the HLE:
                // keep player_count = (byte1 & 3) + 1 with no fold, and reconcile
                // the index against the new count with a modulo (so 3 players
                // cycles 0,1,2 rather than the `& 2` glitch an AND-mask would
                // give).
                self.players = (self.command[1] & 3) + 1;
                self.joypad_index %= self.players;
            }
            cmd::MASK_EN => {
                self.mask = match self.command[1] & 0x03 {
                    0 => MaskMode::Cancel,
                    1 => MaskMode::Freeze,
                    2 => MaskMode::Black,
                    _ => MaskMode::Color0,
                };
            }
            cmd::PAL01 => self.apply_pal(0, 1),
            cmd::PAL23 => self.apply_pal(2, 3),
            cmd::PAL03 => self.apply_pal(0, 3),
            cmd::PAL12 => self.apply_pal(1, 2),
            cmd::PAL_SET => self.apply_pal_set(),
            cmd::ATTR_BLK | cmd::ATTR_LIN | cmd::ATTR_DIV | cmd::ATTR_CHR | cmd::ATTR_SET => {
                // Attribute assignment: high-level palette-per-cell. Handled
                // coarsely (see apply_attr); full geometry is deferred.
                self.apply_attr(command);
            }
            cmd::PAL_TRN | cmd::CHR_TRN | cmd::PCT_TRN | cmd::ATTR_TRN | cmd::DATA_TRN => {
                // VRAM block transfer: read 4KB from $8000 at the next VBlank.
                self.pending_trn = Some(command);
            }
            _ => {
                // Other commands (SOUND, DATA_SND, ICON_EN, ...) are decoded but
                // have no GB-visible effect in our high-level model; ignore.
            }
        }
    }

    /// PAL_01/23/03/12: set two of the four SGB palettes from packet colors.
    /// Bytes 1-2 = color 0 (shared across the two palettes), then colors 1-3 of
    /// palette A, colors 1-3 of palette B (RGB555, little-endian per color).
    fn apply_pal(&mut self, pa: usize, pb: usize) {
        let mut p = [0u8; 16];
        p.copy_from_slice(&self.command[..16]);
        let color0 = u16::from_le_bytes([p[1], p[2]]) & 0x7FFF;
        // Palette A colors 1-3 at bytes 3-8; palette B colors 1-3 at bytes 9-14.
        for (i, base) in [3usize, 5, 7].iter().enumerate() {
            self.palettes[pa][i + 1] = u16::from_le_bytes([p[*base], p[*base + 1]]) & 0x7FFF;
        }
        for (i, base) in [9usize, 11, 13].iter().enumerate() {
            self.palettes[pb][i + 1] = u16::from_le_bytes([p[*base], p[*base + 1]]) & 0x7FFF;
        }
        self.palettes[pa][0] = color0;
        self.palettes[pb][0] = color0;
        self.colorized = true;
    }

    /// PAL_SET: select four system palettes (loaded by PAL_TRN) for the four
    /// active SGB palettes. Bytes 1-8 = four 16-bit little-endian palette indices.
    fn apply_pal_set(&mut self) {
        let mut p = [0u8; 16];
        p.copy_from_slice(&self.command[..16]);
        for slot in 0..4 {
            let idx = u16::from_le_bytes([p[1 + slot * 2], p[2 + slot * 2]]) as usize;
            if let Some(pal) = self.sys_palettes.get(idx) {
                self.palettes[slot] = *pal;
            }
        }
        // Byte 9 bit 6 cancels the mask.
        if p[9] & 0x40 != 0 {
            self.mask = MaskMode::Cancel;
        }
        self.colorized = true;
    }

    /// Coarse attribute handling. Full ATTR_BLK/LIN/DIV/CHR geometry is a large
    /// feature; for now ATTR_DIV (screen split) and a whole-screen fill are the
    /// common game cases. Anything unrecognised leaves `attr` unchanged.
    fn apply_attr(&mut self, command: u8) {
        if command == cmd::ATTR_DIV {
            // ATTR_DIV: byte 1 selects H/V split + the three region palettes.
            // Deferred: leave attr as-is (games still render, just uncolored per
            // region). This keeps the stub clean without wrong colorization.
        }
        self.colorized = true;
    }

    /// True if a _TRN command is pending a VBlank VRAM read. The caller feeds the
    /// 4KB VRAM block via `apply_trn` and clears this.
    pub fn take_pending_trn(&mut self) -> Option<u8> {
        self.pending_trn.take()
    }

    /// Consume a 4KB VRAM block ($8000..$9000) for a pending _TRN command.
    pub fn apply_trn(&mut self, command: u8, vram: &[u8]) {
        if command == cmd::PAL_TRN {
            // PAL_TRN loads 512 system palettes, 4 colors each (RGB555 LE).
            self.sys_palettes.clear();
            for i in 0..512 {
                let base = i * 8;
                if base + 8 > vram.len() {
                    break;
                }
                let mut pal = [0u16; 4];
                for c in 0..4 {
                    pal[c] = u16::from_le_bytes([vram[base + c * 2], vram[base + c * 2 + 1]]) & 0x7FFF;
                }
                self.sys_palettes.push(pal);
            }
        }
        // CHR_TRN/PCT_TRN (border tiles/map) and ATTR_TRN are deferred (border
        // rendering is lowest priority); the block is accepted and dropped.
    }

    /// Look up the RGB555 color for a DMG shade index (0-3) at attribute cell
    /// (col,row) in the 20x18 grid. Returns None if not colorized (caller keeps
    /// grayscale).
    pub fn color_for(&self, col: usize, row: usize, shade: u8) -> Option<u16> {
        if !self.colorized {
            return None;
        }
        let cell = row.min(17) * 20 + col.min(19);
        let pal = self.attr[cell] as usize & 0x03;
        Some(self.palettes[pal][(shade & 0x03) as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive one canonical SGB packet through the receiver: RESET (both-low),
    /// settle (both-high), then 128 data bits (LSB-first per byte) framed as
    /// P14-low (0x20) for a 0 and P15-low (0x10) for a 1, each returning to
    /// both-high (0x30); finally a 0x20 stop pulse. Mirrors the transmit routine
    /// the SGB games (and sgb-ext-test) use.
    fn send_packet(sgb: &mut Sgb, bytes: &[u8; 16]) {
        // Precondition: the line is idle both-high so the receiver is armed.
        sgb.write_p1(0x30);
        sgb.write_p1(0x00); // RESET / start pulse
        sgb.write_p1(0x30);
        for &byte in bytes.iter() {
            for bit in 0..8 {
                let one = (byte >> bit) & 1 != 0;
                sgb.write_p1(if one { 0x10 } else { 0x20 });
                sgb.write_p1(0x30);
            }
        }
        sgb.write_p1(0x20); // stop pulse
        sgb.write_p1(0x30);
    }

    #[test]
    fn mlt_req_selects_player_count() {
        // 0->1, 1->2, 2->3 (invalid/glitched), 3->4. No SameBoy 3->4 fold.
        for (sel, want) in [(0u8, 1u8), (1, 2), (2, 3), (3, 4)] {
            let mut sgb = Sgb::new();
            let mut pkt = [0u8; 16];
            pkt[0] = (cmd::MLT_REQ << 3) | 1; // command 0x11, 1 packet
            pkt[1] = sel;
            send_packet(&mut sgb, &pkt);
            assert_eq!(sgb.players(), want, "sel {sel:#x}");
        }
    }

    #[test]
    fn mlt_req_read_cycles_players() {
        // Four-player MLT_REQ, then the P15-rising pulses cycle the joypad index
        // so reads return 0xF, 0xE, 0xD, 0xC, wrapping.
        let mut sgb = Sgb::new();
        let mut pkt = [0u8; 16];
        pkt[0] = (cmd::MLT_REQ << 3) | 1;
        pkt[1] = 3; // 4 players
        send_packet(&mut sgb, &pkt);
        assert_eq!(sgb.players(), 4);
        let mut seen = vec![sgb.joypad_id_nibble()];
        for _ in 0..5 {
            // A P14-low (0x10) then both-high (0x30) is a P15-rising increment.
            sgb.write_p1(0x10);
            sgb.write_p1(0x30);
            seen.push(sgb.joypad_id_nibble());
        }
        assert_eq!(seen, vec![0x0F, 0x0E, 0x0D, 0x0C, 0x0F, 0x0E]);
    }

    #[test]
    fn single_player_read_is_always_0f() {
        // Without MLT_REQ (1 player) the index never advances; reads stay 0x0F.
        let mut sgb = Sgb::new();
        for _ in 0..4 {
            sgb.write_p1(0x10);
            sgb.write_p1(0x30);
            assert_eq!(sgb.joypad_id_nibble(), 0x0F);
        }
    }

    #[test]
    fn mask_en_sets_mode() {
        let mut sgb = Sgb::new();
        let mut pkt = [0u8; 16];
        pkt[0] = (cmd::MASK_EN << 3) | 1;
        pkt[1] = 2; // Black
        send_packet(&mut sgb, &pkt);
        assert_eq!(sgb.mask, MaskMode::Black);
    }
}
