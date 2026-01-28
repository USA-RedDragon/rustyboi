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
    // ---- Packet reception state machine ----
    // Derived cell-by-cell from the real-SGB sgb-ext-test reference screenshot
    // (all 27 adversarial framing variants match). The ICD2 receiver:
    //   * arms a bit on a single-line-low pulse after a both-high return
    //     (`ready_for_pulse`) with the receiver started (`ready_for_write`),
    //   * but only COMMITS the bit value on the following both-high (0x30)
    //     write — the data value is sampled on the line's return to idle. A
    //     second single-line pulse before that both-high OVERRIDES the pending
    //     value without advancing the bit counter (last pulse wins: the
    //     $20->$10->$30 variant latches a 1, $10->$20->$30 latches a 0),
    //   * treats both-low (0x00) as an unconditional start/reset — it fires
    //     even when the line never returned to both-high first (the
    //     $10->$00->$30 variant aborts the packet on real hardware).
    /// Set by a both-high (0x30) write: the next single-line pulse may arm a bit.
    ready_for_pulse: bool,
    /// Set by a both-low (0x00) start pulse: bit latching is enabled.
    ready_for_write: bool,
    /// Set when a full 128-bit packet has been clocked; the next single-line
    /// pulse finalises the packet (and dispatches the command if the count is met).
    ready_for_stop: bool,
    /// Bit value armed by a single-line pulse, committed at the next both-high
    /// write. `None` when no pulse is in flight.
    #[serde(default)]
    pending_bit: Option<bool>,
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
            pending_bit: None,
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

    /// Advance the JOYP write state machine on a $FF00 write. Bits are decoded
    /// from `(value >> 4) & 3` where 3=both-high (commit pending bit + re-arm),
    /// 2=P14-low ("0" bit), 1=P15-low ("1" bit), 0=both-low (start/reset).
    /// Data-bit values are sampled at the both-high return (last pulse wins),
    /// which is what the real-SGB sgb-ext-test reference pins for all 27
    /// adversarial framing variants (see the state-machine field docs).
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
                // Both-high: commit the in-flight bit (the ICD2 samples the data
                // value on the line's return to idle-high) and re-arm.
                if let Some(one) = self.pending_bit.take() {
                    if (self.write_index as usize) < self.command.len() * 8 {
                        if one {
                            let byte = (self.write_index / 8) as usize;
                            let bit = (self.write_index & 7) as u8;
                            self.command[byte] |= 1 << bit;
                        }
                        self.write_index += 1;
                        if self.write_index & (128 - 1) == 0 {
                            self.ready_for_stop = true;
                        }
                    }
                }
                self.ready_for_pulse = true;
            }
            line @ (1 | 2) => {
                // Single-line pulse: P15-low (1) = "one" bit, P14-low (2) =
                // "zero" bit; after the 128th bit, the stop pulse.
                let one = line == 1;
                if self.ready_for_pulse && self.ready_for_write {
                    if self.ready_for_stop {
                        // Stop pulse: on hardware EITHER a P14-low (0x20) OR a
                        // P15-low (0x10) pulse after the 128th bit terminates
                        // the packet and dispatches (sgb-ext-test CorruptStop
                        // uses a 0x10 stop and real SGB accepts it; SameBoy
                        // treats it as corrupt).
                        if self.write_index == command_size {
                            self.dispatch();
                            self.write_index = 0;
                            self.command = default_command();
                        }
                        self.ready_for_pulse = false;
                        self.ready_for_write = false;
                        self.ready_for_stop = false;
                    } else {
                        // Arm the bit; it commits at the next both-high write.
                        self.pending_bit = Some(one);
                        self.ready_for_pulse = false;
                    }
                } else if self.pending_bit.is_some() {
                    // Second pulse before the both-high return: the last pulse
                    // wins ($20->$10->$30 latches a 1, $10->$20->$30 a 0 — real
                    // SGB, sgb-ext-test cells 6-11). The bit counter does not
                    // advance.
                    self.pending_bit = Some(one);
                }
            }
            0 => {
                // Both-low start pulse: arm writing; reset a partial/misaligned
                // command so the next bits start a clean packet. This fires
                // regardless of `ready_for_pulse` — a $10->$00 or $20->$00
                // sequence aborts the packet on real SGB (sgb-ext-test cells
                // 18-23) — and always kills an in-flight bit.
                self.pending_bit = None;
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

    /// Send an MLT_REQ packet where the first bit of byte 1 (the player mask)
    /// is transmitted as the write triple (a, b, 0x30) instead of a normal
    /// pulse, mirroring the sgb-ext-test XXToYY framing variants. The actual
    /// bit value of byte 1 bit 0 is discarded, exactly as the test ROM does.
    fn send_packet_first_mask_bit(sgb: &mut Sgb, bytes: &[u8; 16], a: u8, b: u8) {
        sgb.write_p1(0x30);
        sgb.write_p1(0x00);
        sgb.write_p1(0x30);
        for bit in 0..8 {
            let one = (bytes[0] >> bit) & 1 != 0;
            sgb.write_p1(if one { 0x10 } else { 0x20 });
            sgb.write_p1(0x30);
        }
        sgb.write_p1(a);
        sgb.write_p1(b);
        sgb.write_p1(0x30);
        for bit in 1..8 {
            let one = (bytes[1] >> bit) & 1 != 0;
            sgb.write_p1(if one { 0x10 } else { 0x20 });
            sgb.write_p1(0x30);
        }
        for &byte in bytes[2..].iter() {
            for bit in 0..8 {
                let one = (byte >> bit) & 1 != 0;
                sgb.write_p1(if one { 0x10 } else { 0x20 });
                sgb.write_p1(0x30);
            }
        }
        sgb.write_p1(0x20); // stop pulse
        sgb.write_p1(0x30);
    }

    fn mlt_req_packet(mask: u8) -> [u8; 16] {
        let mut pkt = [0u8; 16];
        pkt[0] = (cmd::MLT_REQ << 3) | 1;
        pkt[1] = mask;
        pkt
    }

    /// Real SGB (sgb-ext-test ref cells 6-8): a $20->$10->$30 bit write latches
    /// a 1 — the last pulse before the both-high return wins. A 1P mask ($00)
    /// therefore arrives as $01 and selects 2 players.
    #[test]
    fn pulse_override_20_to_10_latches_one() {
        let mut sgb = Sgb::new();
        send_packet_first_mask_bit(&mut sgb, &mlt_req_packet(0x00), 0x20, 0x10);
        assert_eq!(sgb.players(), 2);
        let mut sgb = Sgb::new();
        send_packet_first_mask_bit(&mut sgb, &mlt_req_packet(0x03), 0x20, 0x10);
        assert_eq!(sgb.players(), 4);
    }

    /// Real SGB (ref cells 9-11): $10->$20->$30 latches a 0. A 2P mask ($01)
    /// arrives as $00 (1 player); a 4P mask ($03) as $02 (glitched 3-player).
    #[test]
    fn pulse_override_10_to_20_latches_zero() {
        let mut sgb = Sgb::new();
        send_packet_first_mask_bit(&mut sgb, &mlt_req_packet(0x01), 0x10, 0x20);
        assert_eq!(sgb.players(), 1);
        let mut sgb = Sgb::new();
        send_packet_first_mask_bit(&mut sgb, &mlt_req_packet(0x03), 0x10, 0x20);
        assert_eq!(sgb.players(), 3);
    }

    /// Real SGB (ref cells 12-23): a both-low mid-packet — whether after a
    /// both-high ($00->$10/$20) or straight after a pulse ($10/$20->$00) —
    /// resets the receiver, so the packet misframes and is never dispatched.
    #[test]
    fn both_low_mid_packet_aborts() {
        for (a, b) in [(0x00, 0x10), (0x00, 0x20), (0x10, 0x00), (0x20, 0x00)] {
            let mut sgb = Sgb::new();
            send_packet_first_mask_bit(&mut sgb, &mlt_req_packet(0x03), a, b);
            assert_eq!(sgb.players(), 1, "variant {a:#04x}->{b:#04x}");
        }
    }

    /// Real SGB (ref cells 24-26): omitting the $30 after the start pulse drops
    /// the first data bit (a pulse before the both-high return never arms), so
    /// the packet misframes and is rejected.
    #[test]
    fn short_start_rejected() {
        let mut sgb = Sgb::new();
        let pkt = mlt_req_packet(0x03);
        sgb.write_p1(0x30);
        sgb.write_p1(0x00); // start with NO $30 after
        for &byte in pkt.iter() {
            for bit in 0..8 {
                let one = (byte >> bit) & 1 != 0;
                sgb.write_p1(if one { 0x10 } else { 0x20 });
                sgb.write_p1(0x30);
            }
        }
        sgb.write_p1(0x20);
        sgb.write_p1(0x30);
        assert_eq!(sgb.players(), 1);
    }

    /// Real SGB (ref cells 2-3): a $10 (P15-low) stop pulse after the 128th bit
    /// is accepted just like the normal $20 stop.
    #[test]
    fn corrupt_stop_10_accepted() {
        let mut sgb = Sgb::new();
        let pkt = mlt_req_packet(0x03);
        sgb.write_p1(0x30);
        sgb.write_p1(0x00);
        sgb.write_p1(0x30);
        for &byte in pkt.iter() {
            for bit in 0..8 {
                let one = (byte >> bit) & 1 != 0;
                sgb.write_p1(if one { 0x10 } else { 0x20 });
                sgb.write_p1(0x30);
            }
        }
        sgb.write_p1(0x10); // corrupt stop
        sgb.write_p1(0x30);
        assert_eq!(sgb.players(), 4);
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
