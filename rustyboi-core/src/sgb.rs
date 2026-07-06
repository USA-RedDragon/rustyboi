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
    /// Packet byte 1 of the pending _TRN command, captured at dispatch (the
    /// command buffer is cleared before the VBlank read). CHR_TRN needs its
    /// bit 0 (low/high tile half).
    #[serde(default)]
    pending_trn_param: u8,
    /// SYSTEM color palettes loaded by PAL_TRN (512 palettes of 4 RGB555 colors),
    /// selectable by PAL_SET. Boxed-in-vec to keep the struct small.
    #[serde(default)]
    sys_palettes: Vec<[u16; 4]>,
    /// Attribute files ATF0-ATF44 loaded by ATTR_TRN: 45 files x 90 bytes
    /// (20x18 cells x 2 bits, MSB-first within each byte, row-major). Empty
    /// until an ATTR_TRN has run.
    #[serde(default, with = "serde_bytes")]
    atf: Vec<u8>,

    // ---- SGB border (CHR_TRN / PCT_TRN) ----
    /// Border tile data: 256 SNES 4bpp tiles x 32 bytes (0x2000). CHR_TRN
    /// fills one 128-tile half per transfer (packet byte 1 bit 0 selects the
    /// high half). Empty until a CHR_TRN has run.
    #[serde(default, with = "serde_bytes")]
    border_tiles: Vec<u8>,
    /// Border tilemap from PCT_TRN offset 0x000: 32x28 LE16 entries used (the
    /// 0x800-byte region holds 32x32). Entry: bits 0-9 tile (only 0-255
    /// drawable), 10-12 palette (4-7), 14 X-flip, 15 Y-flip.
    #[serde(default, with = "serde_bytes")]
    border_map: Vec<u8>,
    /// Border palettes 4-7 from PCT_TRN offset 0x800: 4 x 16 RGB555 colors.
    #[serde(default)]
    border_pals: Vec<u16>,
    /// True once PCT_TRN has delivered a tilemap + palettes (border renderable).
    #[serde(default)]
    border_ready: bool,
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
            pending_trn_param: 0,
            sys_palettes: Vec::new(),
            atf: Vec::new(),
            border_tiles: Vec::new(),
            border_map: Vec::new(),
            border_pals: Vec::new(),
            border_ready: false,
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
                if let Some(one) = self.pending_bit.take()
                    && (self.write_index as usize) < self.command.len() * 8 {
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
                // real SGB silicon keeps it as a *glitched 3-player* mode, which
                // SameSuite's command_mlt_req captures. We model the hardware,
                // not the HLE: player_count = (byte1 & 3) + 1 with no fold.
                //
                // Index reconcile at dispatch: for the valid counts (1/2/4) the
                // index is AND-masked into range. For the invalid count 3 the
                // dispatch additionally clocks the glitched counter ONCE —
                // `(index+1) & 2`, the same broken step a P15 edge performs in
                // that mode. This is pinned by command_mlt_req results 15-18
                // (real SGB): entering mode 2 from 4-player index 0 and from
                // index 1 must BOTH read player 3 (0xD), which no pure
                // mask/modulo reconcile of a shared counter can produce; the
                // extra glitch-step maps 1->2 and 2->2 (results 15/16) while
                // keeping 3->0 and 0->0 (results 17/18). The 648-model sweep
                // over edge/gate/wrap/read/reconcile families has NO other
                // survivors (see f-mlt sweep).
                self.players = (self.command[1] & 3) + 1;
                if self.players == 3 {
                    self.joypad_index = self.joypad_index.wrapping_add(1) & 2;
                } else {
                    self.joypad_index &= self.players - 1;
                }
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
            cmd::ATTR_BLK => self.attr_blk(),
            cmd::ATTR_LIN => self.attr_lin(),
            cmd::ATTR_DIV => self.attr_div(),
            cmd::ATTR_CHR => self.attr_chr(),
            cmd::ATTR_SET => self.attr_set(),
            cmd::PAL_TRN | cmd::CHR_TRN | cmd::PCT_TRN | cmd::ATTR_TRN | cmd::DATA_TRN => {
                // VRAM block transfer: read 4KB from $8000 at the next VBlank.
                // Byte 1 parameterises some transfers (CHR_TRN tile half); the
                // command buffer is recycled before the read, so capture it now.
                self.pending_trn = Some(command);
                self.pending_trn_param = self.command[1];
            }
            _ => {
                // Other commands (SOUND, DATA_SND, ICON_EN, ...) are decoded but
                // have no GB-visible effect in our high-level model; ignore.
            }
        }
    }

    /// PAL_01/23/03/12: set two of the four SGB palettes from packet colors.
    /// Bytes 1-2 = color 0, then colors 1-3 of palette A (bytes 3-8), colors
    /// 1-3 of palette B (bytes 9-14); RGB555 little-endian per color. Color 0
    /// is a single shared SNES CGRAM entry (the backdrop), so it applies to
    /// ALL FOUR palettes (Pan Docs: "The value transferred as color 0 will be
    /// applied for all four palettes").
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
        for pal in self.palettes.iter_mut() {
            pal[0] = color0;
        }
        self.colorized = true;
    }

    /// PAL_SET: select four system palettes (loaded by PAL_TRN) for the four
    /// active SGB palettes. Bytes 1-8 = four little-endian palette indices;
    /// only 9 bits are decoded (0-511, SameBoy `cmd[n] + (cmd[n+1] & 1) * 0x100`).
    /// Byte 9: bit 7 = apply the attribute file selected by bits 0-5, bit 6 =
    /// cancel mask. Color 0 stays a shared backdrop: all palettes take
    /// palette 0's color 0.
    fn apply_pal_set(&mut self) {
        let mut p = [0u8; 16];
        p.copy_from_slice(&self.command[..16]);
        for slot in 0..4 {
            let idx = (u16::from_le_bytes([p[1 + slot * 2], p[2 + slot * 2]]) & 0x1FF) as usize;
            if let Some(pal) = self.sys_palettes.get(idx) {
                self.palettes[slot] = *pal;
            }
        }
        let color0 = self.palettes[0][0];
        for pal in self.palettes.iter_mut() {
            pal[0] = color0;
        }
        if p[9] & 0x80 != 0 {
            self.load_atf((p[9] & 0x3F) as usize);
        }
        // Byte 9 bit 6 cancels the mask.
        if p[9] & 0x40 != 0 {
            self.mask = MaskMode::Cancel;
        }
        self.colorized = true;
    }

    /// ATTR_BLK: paint rectangular regions of the 20x18 attribute map. Byte 1 =
    /// data-set count (1-18), then 6 bytes per set: control (bit 0 inside,
    /// bit 1 boundary line, bit 2 outside), palettes (bits 0-1 inside, 2-3
    /// line, 4-5 outside), and the rect corners in cell coordinates (x1,y1,
    /// x2,y2, masked to 5 bits). Pan Docs exception: painting ONLY the inside
    /// or ONLY the outside implicitly paints the boundary line with that same
    /// palette.
    fn attr_blk(&mut self) {
        self.colorized = true;
        let count = self.command[1] as usize;
        if count > 0x12 {
            return;
        }
        for i in 0..count {
            let d: [u8; 6] = self.command[2 + i * 6..8 + i * 6].try_into().unwrap();
            let inside = d[0] & 1 != 0;
            let mut line = d[0] & 2 != 0;
            let outside = d[0] & 4 != 0;
            let inside_pal = d[1] & 3;
            let mut line_pal = (d[1] >> 2) & 3;
            let outside_pal = (d[1] >> 4) & 3;
            if inside && !line && !outside {
                line = true;
                line_pal = inside_pal;
            } else if outside && !line && !inside {
                line = true;
                line_pal = outside_pal;
            }
            let (x1, y1, x2, y2) = (d[2] & 0x1F, d[3] & 0x1F, d[4] & 0x1F, d[5] & 0x1F);
            for y in 0..18u8 {
                for x in 0..20u8 {
                    let cell = &mut self.attr[y as usize * 20 + x as usize];
                    if x < x1 || x > x2 || y < y1 || y > y2 {
                        if outside {
                            *cell = outside_pal;
                        }
                    } else if x > x1 && x < x2 && y > y1 && y < y2 {
                        if inside {
                            *cell = inside_pal;
                        }
                    } else if line {
                        *cell = line_pal;
                    }
                }
            }
        }
    }

    /// ATTR_LIN: paint full rows/columns of the attribute map. Byte 1 = data-set
    /// count, then one byte per set: bits 0-4 = line number, bits 5-6 =
    /// palette, bit 7 = 1 for a horizontal line (row), 0 for vertical (column).
    fn attr_lin(&mut self) {
        self.colorized = true;
        let count = self.command[1] as usize;
        if count > self.command.len() - 2 {
            return;
        }
        for i in 0..count {
            let d = self.command[2 + i];
            let line = (d & 0x1F) as usize;
            let pal = (d >> 5) & 3;
            if d & 0x80 != 0 {
                if line < 18 {
                    self.attr[line * 20..line * 20 + 20].fill(pal);
                }
            } else if line < 20 {
                for y in 0..18 {
                    self.attr[y * 20 + line] = pal;
                }
            }
        }
    }

    /// ATTR_DIV: split the screen along a row/column. Byte 1: bits 0-1 =
    /// palette below/right of the divider, bits 2-3 = above/left, bits 4-5 =
    /// the divider line itself, bit 6 = 1 for a horizontal divider (compare
    /// rows) / 0 for vertical (compare columns). Byte 2 = the divider
    /// coordinate in cells.
    fn attr_div(&mut self) {
        self.colorized = true;
        let b = self.command[1];
        let below_right = b & 3;
        let above_left = (b >> 2) & 3;
        let on_line = (b >> 4) & 3;
        let horizontal = b & 0x40 != 0;
        let pos = (self.command[2] & 0x1F) as usize;
        for y in 0..18 {
            for x in 0..20 {
                let c = if horizontal { y } else { x };
                self.attr[y * 20 + x] = match c.cmp(&pos) {
                    core::cmp::Ordering::Less => above_left,
                    core::cmp::Ordering::Equal => on_line,
                    core::cmp::Ordering::Greater => below_right,
                };
            }
        }
    }

    /// ATTR_CHR: paint individual cells starting at (byte 1, byte 2), walking
    /// left-to-right (byte 5 = 0) or top-to-bottom (byte 5 = 1), wrapping at
    /// the screen edge and stopping at the opposite corner. Bytes 3-4 (LE) =
    /// cell count; palette numbers are packed 4-per-byte MSB-first from
    /// byte 6, continuing across packets (up to 6).
    fn attr_chr(&mut self) {
        self.colorized = true;
        let mut x = self.command[1] as usize;
        let mut y = self.command[2] as usize;
        let count = u16::from_le_bytes([self.command[3], self.command[4]]) as usize;
        let vertical = self.command[5] != 0;
        if x >= 20 || y >= 18 {
            return;
        }
        for i in 0..count {
            let Some(&byte) = self.command.get(6 + i / 4) else {
                break;
            };
            self.attr[y * 20 + x] = (byte >> (2 * (3 - (i & 3)))) & 3;
            if vertical {
                y += 1;
                if y == 18 {
                    y = 0;
                    x += 1;
                    if x == 20 {
                        break;
                    }
                }
            } else {
                x += 1;
                if x == 20 {
                    x = 0;
                    y += 1;
                    if y == 18 {
                        break;
                    }
                }
            }
        }
    }

    /// ATTR_SET: load attribute file byte1 bits 0-5 (0-44) into the attribute
    /// map; bit 6 additionally cancels the screen mask.
    fn attr_set(&mut self) {
        self.colorized = true;
        let b = self.command[1];
        self.load_atf((b & 0x3F) as usize);
        if b & 0x40 != 0 {
            self.mask = MaskMode::Cancel;
        }
    }

    /// Expand attribute file `index` (0-44, 90 bytes of 2-bit cells, MSB-first)
    /// from the ATTR_TRN store into the live 20x18 attribute map. Out-of-range
    /// indices and a never-transferred store are ignored (hardware reads
    /// whatever SNES RAM holds; we model "no change").
    fn load_atf(&mut self, index: usize) {
        if index > 0x2C {
            return;
        }
        let base = index * 90;
        if self.atf.len() < base + 90 {
            return;
        }
        for (i, cell) in self.attr.iter_mut().enumerate() {
            let byte = self.atf[base + i / 4];
            *cell = (byte >> (2 * (3 - (i & 3)))) & 3;
        }
    }

    /// True if a _TRN command is pending a VBlank VRAM read. The caller feeds the
    /// 4KB VRAM block via `apply_trn` and clears this.
    pub fn take_pending_trn(&mut self) -> Option<u8> {
        self.pending_trn.take()
    }

    /// Consume a 4KB VRAM block ($8000..$9000) for a pending _TRN command.
    pub fn apply_trn(&mut self, command: u8, vram: &[u8]) {
        match command {
            cmd::PAL_TRN => {
                // PAL_TRN loads 512 system palettes, 4 colors each (RGB555 LE).
                self.sys_palettes.clear();
                for i in 0..512 {
                    let base = i * 8;
                    if base + 8 > vram.len() {
                        break;
                    }
                    let mut pal = [0u16; 4];
                    for c in 0..4 {
                        pal[c] =
                            u16::from_le_bytes([vram[base + c * 2], vram[base + c * 2 + 1]]) & 0x7FFF;
                    }
                    self.sys_palettes.push(pal);
                }
            }
            cmd::ATTR_TRN => {
                // ATTR_TRN loads all 45 attribute files (4050 bytes; the tail
                // of the 4KB block is unused).
                let len = vram.len().min(45 * 90);
                self.atf = vram[..len].to_vec();
            }
            cmd::CHR_TRN => {
                // CHR_TRN: 128 border tiles (SNES 4bpp, 32 bytes each) into
                // the half selected by packet byte 1 bit 0 (0 = tiles 0-127,
                // 1 = tiles 128-255). Bit 1 (BG/OBJ tile type) is irrelevant
                // to border rendering.
                if self.border_tiles.len() != 0x2000 {
                    self.border_tiles = vec![0u8; 0x2000];
                }
                let off = if self.pending_trn_param & 1 != 0 { 0x1000 } else { 0 };
                let len = vram.len().min(0x1000);
                self.border_tiles[off..off + len].copy_from_slice(&vram[..len]);
            }
            cmd::PCT_TRN => {
                // PCT_TRN: border tilemap (offset 0x000, 32x28 LE16 entries)
                // + border palettes 4-7 (offset 0x800, 4 x 16 RGB555 colors).
                let len = vram.len().min(0x800);
                self.border_map = vram[..len].to_vec();
                self.border_pals = (0..64)
                    .map(|i| {
                        let b = 0x800 + i * 2;
                        if b + 2 <= vram.len() {
                            u16::from_le_bytes([vram[b], vram[b + 1]]) & 0x7FFF
                        } else {
                            0
                        }
                    })
                    .collect();
                self.border_ready = true;
            }
            _ => {
                // DATA_TRN writes SNES RAM: no GB-visible effect in our
                // high-level model; the block is dropped.
            }
        }
    }

    /// Border data for the compositor: (4bpp tiles, tilemap bytes, palettes
    /// 4-7). None until BOTH a CHR_TRN (tiles) and a PCT_TRN (map+palettes)
    /// have run — games send CHR_TRN first and PCT_TRN last, so gating on
    /// both avoids flashing a half-loaded border. (Without a CHR_TRN, real
    /// hardware would show the boot-ROM's built-in border tiles, which our
    /// HLE has no copy of.)
    pub fn border(&self) -> Option<(&[u8], &[u8], &[u16])> {
        if !self.border_ready || self.border_map.len() < 0x700 || self.border_pals.len() < 64 {
            return None;
        }
        if self.border_tiles.len() != 0x2000 {
            return None;
        }
        Some((&self.border_tiles, &self.border_map, &self.border_pals))
    }

    /// The shared backdrop color (SNES CGRAM entry 0 = every palette's color
    /// 0): what transparent border pixels outside the GB window show.
    pub fn backdrop(&self) -> u16 {
        self.palettes[0][0]
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

    #[test]
    fn attr_blk_inside_line_outside() {
        let mut sgb = Sgb::new();
        let mut pkt = [0u8; 16];
        pkt[0] = (cmd::ATTR_BLK << 3) | 1;
        pkt[1] = 1; // one data set
        pkt[2] = 0x07; // inside + line + outside
        pkt[3] = 0b11_10_01; // inside=1, line=2, outside=3
        pkt[4] = 2; // x1
        pkt[5] = 3; // y1
        pkt[6] = 5; // x2
        pkt[7] = 6; // y2
        send_packet(&mut sgb, &pkt);
        assert!(sgb.colorized);
        assert_eq!(sgb.attr[0], 3, "corner is outside");
        assert_eq!(sgb.attr[3 * 20 + 2], 2, "rect corner is on the line");
        assert_eq!(sgb.attr[4 * 20 + 3], 1, "strict interior");
        assert_eq!(sgb.attr[6 * 20 + 5], 2, "far corner on the line");
        assert_eq!(sgb.attr[17 * 20 + 19], 3, "far screen corner outside");
    }

    #[test]
    fn attr_blk_inside_only_paints_line_too() {
        // Pan Docs: control=001 (inside only) implicitly paints the boundary
        // line with the inside palette; the outside is untouched.
        let mut sgb = Sgb::new();
        let mut pkt = [0u8; 16];
        pkt[0] = (cmd::ATTR_BLK << 3) | 1;
        pkt[1] = 1;
        pkt[2] = 0x01; // inside only
        pkt[3] = 0b00_00_10; // inside palette 2
        pkt[4] = 4;
        pkt[5] = 4;
        pkt[6] = 8;
        pkt[7] = 8;
        send_packet(&mut sgb, &pkt);
        assert_eq!(sgb.attr[4 * 20 + 4], 2, "boundary painted with inside pal");
        assert_eq!(sgb.attr[5 * 20 + 5], 2, "interior painted");
        assert_eq!(sgb.attr[0], 0, "outside untouched");
    }

    #[test]
    fn attr_lin_row_and_column() {
        let mut sgb = Sgb::new();
        let mut pkt = [0u8; 16];
        pkt[0] = (cmd::ATTR_LIN << 3) | 1;
        pkt[1] = 2; // two data sets
        pkt[2] = 0x80 | (2 << 5) | 5; // horizontal row 5, palette 2
        pkt[3] = (1 << 5) | 7; // vertical column 7, palette 1
        send_packet(&mut sgb, &pkt);
        assert_eq!(sgb.attr[5 * 20 + 0], 2);
        assert_eq!(sgb.attr[5 * 20 + 19], 2);
        assert_eq!(sgb.attr[0 * 20 + 7], 1);
        // The column was painted after the row: the crossing cell is 1.
        assert_eq!(sgb.attr[5 * 20 + 7], 1);
        assert_eq!(sgb.attr[0], 0, "untouched elsewhere");
    }

    #[test]
    fn attr_div_horizontal() {
        let mut sgb = Sgb::new();
        let mut pkt = [0u8; 16];
        pkt[0] = (cmd::ATTR_DIV << 3) | 1;
        // bits 0-1 below=1, bits 2-3 above=2, bits 4-5 line=3, bit 6 horizontal
        pkt[1] = 0x40 | (3 << 4) | (2 << 2) | 1;
        pkt[2] = 6; // divider at row 6
        send_packet(&mut sgb, &pkt);
        assert_eq!(sgb.attr[0], 2, "above the divider");
        assert_eq!(sgb.attr[6 * 20], 3, "on the divider");
        assert_eq!(sgb.attr[17 * 20], 1, "below the divider");
    }

    #[test]
    fn attr_chr_walks_and_wraps() {
        let mut sgb = Sgb::new();
        let mut pkt = [0u8; 16];
        pkt[0] = (cmd::ATTR_CHR << 3) | 1;
        pkt[1] = 18; // start x
        pkt[2] = 0; // start y
        pkt[3] = 4; // count lo (4 cells)
        pkt[4] = 0; // count hi
        pkt[5] = 0; // left-to-right
        pkt[6] = 0b01_10_11_00; // palettes 1,2,3,0 MSB-first
        send_packet(&mut sgb, &pkt);
        assert_eq!(sgb.attr[18], 1);
        assert_eq!(sgb.attr[19], 2);
        assert_eq!(sgb.attr[20], 3, "wrapped to the next row");
        assert_eq!(sgb.attr[21], 0);
    }

    /// Feed ATTR_TRN a synthetic 4KB block, then ATTR_SET each file.
    #[test]
    fn attr_trn_then_attr_set() {
        let mut sgb = Sgb::new();
        // ATF5 = all cells palette 3 (0xFF bytes), ATF6 = palette 1 (0x55).
        let mut block = vec![0u8; 0x1000];
        block[5 * 90..6 * 90].fill(0xFF);
        block[6 * 90..7 * 90].fill(0x55);
        let mut pkt = [0u8; 16];
        pkt[0] = (cmd::ATTR_TRN << 3) | 1;
        send_packet(&mut sgb, &pkt);
        let pending = sgb.take_pending_trn().expect("ATTR_TRN pends a VRAM read");
        assert_eq!(pending, cmd::ATTR_TRN);
        sgb.apply_trn(pending, &block);

        let mut set = [0u8; 16];
        set[0] = (cmd::ATTR_SET << 3) | 1;
        set[1] = 5;
        send_packet(&mut sgb, &set);
        assert!(sgb.attr.iter().all(|&c| c == 3));

        set[1] = 6 | 0x40; // ATF6 + cancel mask
        sgb.mask = MaskMode::Freeze;
        send_packet(&mut sgb, &set);
        assert!(sgb.attr.iter().all(|&c| c == 1));
        assert_eq!(sgb.mask, MaskMode::Cancel);

        // Out-of-range file numbers are ignored.
        set[1] = 45;
        send_packet(&mut sgb, &set);
        assert!(sgb.attr.iter().all(|&c| c == 1));
    }

    #[test]
    fn pal_set_applies_atf_and_shares_color0() {
        let mut sgb = Sgb::new();
        // PAL_TRN: system palette 7 = [0x111, 0x222, 0x333, 0x444],
        // palette 300 = [0x555, ...].
        let mut block = vec![0u8; 0x1000];
        for (i, &(idx, c0)) in [(7usize, 0x111u16), (300, 0x555)].iter().enumerate() {
            let _ = i;
            for c in 0..4 {
                let v = c0 + 0x1111 * c as u16;
                block[idx * 8 + c * 2..idx * 8 + c * 2 + 2].copy_from_slice(&v.to_le_bytes());
            }
        }
        let mut pkt = [0u8; 16];
        pkt[0] = (cmd::PAL_TRN << 3) | 1;
        send_packet(&mut sgb, &pkt);
        let pending = sgb.take_pending_trn().unwrap();
        sgb.apply_trn(pending, &block);

        // ATTR_TRN: ATF0 all-palette-2 (0xAA).
        let mut ablock = vec![0u8; 0x1000];
        ablock[..90].fill(0xAA);
        pkt[0] = (cmd::ATTR_TRN << 3) | 1;
        send_packet(&mut sgb, &pkt);
        let pending = sgb.take_pending_trn().unwrap();
        sgb.apply_trn(pending, &ablock);

        // PAL_SET: slots [7, 300, 7, 300]; byte 9 = apply ATF0 + cancel mask.
        let mut set = [0u8; 16];
        set[0] = (cmd::PAL_SET << 3) | 1;
        set[1..3].copy_from_slice(&7u16.to_le_bytes());
        set[3..5].copy_from_slice(&300u16.to_le_bytes());
        set[5..7].copy_from_slice(&7u16.to_le_bytes());
        set[7..9].copy_from_slice(&300u16.to_le_bytes());
        set[9] = 0x80; // apply ATF 0
        sgb.mask = MaskMode::Freeze;
        send_packet(&mut sgb, &set);
        assert!(sgb.attr.iter().all(|&c| c == 2), "ATF0 applied");
        assert_eq!(sgb.mask, MaskMode::Freeze, "bit 6 clear: mask kept");
        assert_eq!(sgb.palettes[1][1], 0x555 + 0x1111);
        // Shared color 0: every palette's color 0 = palette 0's.
        for pal in &sgb.palettes {
            assert_eq!(pal[0], 0x111);
        }
        // color_for consults the attribute map (palette 2 = system palette 7).
        assert_eq!(sgb.color_for(0, 0, 1), Some(0x111 + 0x1111));
    }

    #[test]
    fn pal01_shares_color0_across_all_palettes() {
        let mut sgb = Sgb::new();
        let mut pkt = [0u8; 16];
        pkt[0] = (cmd::PAL01 << 3) | 1;
        pkt[1..3].copy_from_slice(&0x0123u16.to_le_bytes());
        send_packet(&mut sgb, &pkt);
        for pal in &sgb.palettes {
            assert_eq!(pal[0], 0x0123);
        }
    }

    /// CHR_TRN fills the selected 128-tile half; PCT_TRN delivers map +
    /// palettes and makes `border()` available.
    #[test]
    fn chr_pct_trn_store_border() {
        let mut sgb = Sgb::new();
        assert!(sgb.border().is_none());

        // CHR_TRN low half: tile 0 row 0 plane 0 = 0xFF.
        let mut low = vec![0u8; 0x1000];
        low[0] = 0xFF;
        let mut pkt = [0u8; 16];
        pkt[0] = (cmd::CHR_TRN << 3) | 1;
        pkt[1] = 0;
        send_packet(&mut sgb, &pkt);
        let c = sgb.take_pending_trn().unwrap();
        sgb.apply_trn(c, &low);
        assert!(sgb.border().is_none(), "no border until PCT_TRN");

        // CHR_TRN high half: tile 128 row 0 plane 1 = 0xFF.
        let mut high = vec![0u8; 0x1000];
        high[1] = 0xFF;
        pkt[1] = 1;
        send_packet(&mut sgb, &pkt);
        let c = sgb.take_pending_trn().unwrap();
        sgb.apply_trn(c, &high);

        // PCT_TRN: map entry 0 = tile 0, palette 4; 64 palette colors at 0x800.
        let mut pct = vec![0u8; 0x1000];
        pct[..2].copy_from_slice(&(4u16 << 10).to_le_bytes());
        for i in 0..64u16 {
            let v = 0x100 + i;
            pct[0x800 + i as usize * 2..0x802 + i as usize * 2]
                .copy_from_slice(&v.to_le_bytes());
        }
        pkt[0] = (cmd::PCT_TRN << 3) | 1;
        pkt[1] = 0;
        send_packet(&mut sgb, &pkt);
        let c = sgb.take_pending_trn().unwrap();
        sgb.apply_trn(c, &pct);

        let (tiles, map, pals) = sgb.border().expect("border ready");
        assert_eq!(tiles[0], 0xFF, "low-half tile data");
        assert_eq!(tiles[0x1000 + 1], 0xFF, "high-half tile data");
        assert_eq!((u16::from_le_bytes([map[0], map[1]]) >> 10) & 7, 4);
        assert_eq!(pals[0], 0x100);
        assert_eq!(pals[63], 0x13F);
    }

    /// ATTR_CHR data continues across packet boundaries: 3 packets carry
    /// 42 data bytes = enough for a 160-cell run.
    #[test]
    fn attr_chr_multi_packet() {
        let mut sgb = Sgb::new();
        let mut data = [0u8; 48];
        data[0] = (cmd::ATTR_CHR << 3) | 3; // 3 packets
        data[1] = 0; // x
        data[2] = 0; // y
        data[3..5].copy_from_slice(&160u16.to_le_bytes());
        data[5] = 0; // left-to-right
        data[6..46].fill(0xFF); // 160 cells of palette 3
        for chunk in data.chunks(16) {
            let pkt: [u8; 16] = chunk.try_into().unwrap();
            send_packet(&mut sgb, &pkt);
        }
        assert!(sgb.attr[..160].iter().all(|&c| c == 3));
        assert!(sgb.attr[160..].iter().all(|&c| c == 0));
    }
}
