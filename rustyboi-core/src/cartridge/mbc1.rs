//! MBC1 board: register state + address->bank math.

use super::*;
use super::mapper::{Banking, Geom};
use serde::{Deserialize, Serialize};

// --- board struct + banking ---------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Mbc1 {
    pub ram_enabled: bool,
    pub rom_bank_low: u8, // 5 bits (0x01-0x1F), zero remapped to one at write time
    pub bank2: u8,        // 2 bits (BANK2): RAM bank or ROM bank bits 5-6
    pub mode: u8,         // 0 = ROM banking, 1 = RAM banking
    pub has_ram: bool,
    pub multicart: bool,
}

impl Banking for Mbc1 {
    fn rom_bankn(&self, g: Geom) -> usize {
        // (BANK2 << shift) | BANK1, regardless of mode. BANK1's zero->one remap
        // is applied at write time, so banks 0x20/0x40/0x60 stay inaccessible.
        let bank = if self.multicart {
            ((self.bank2 as usize) << 4) | (self.rom_bank_low as usize & 0x0F)
        } else {
            ((self.bank2 as usize) << 5) | (self.rom_bank_low as usize)
        };
        bank % g.rom_banks
    }
    fn rom_bank0(&self, g: Geom) -> usize {
        if self.mode == 1 {
            let bank = if self.multicart {
                (self.bank2 as usize) << 4
            } else {
                (self.bank2 as usize) << 5
            };
            bank % g.rom_banks
        } else {
            0
        }
    }
    fn ram_bank(&self, g: Geom) -> usize {
        if self.mode == 1 {
            (self.bank2 as usize) % g.ram_banks.max(1)
        } else {
            0
        }
    }
}


// --- container-side board logic -----------------------------------------

impl Cartridge {
    /// Detect an MBC1 multicart. These are 8Mbit (1MB) MBC1 carts whose ROM is
    /// divided into four 256KB games, each carrying its own Nintendo logo at
    /// 0x104. The accepted heuristic (used by mooneye / hardware reference
    /// emulators) is: cartridge type is MBC1, ROM is exactly 64 banks, and the
    /// Nintendo logo appears at the start of two or more of the four 256KB
    /// segments. On a multicart BANK2 supplies bank bits 4-5 (not 5-6) and only
    /// the low 4 bits of BANK1 are wired.
    pub(super) fn detect_mbc1_multicart(cartridge_type: u8, data: &[u8]) -> bool {
        if !matches!(cartridge_type, MBC1 | MBC1_RAM | MBC1_RAM_BATTERY) {
            return false;
        }
        if data.len() != 64 * 0x4000 {
            return false; // multicarts are exactly 8Mbit / 1MB
        }
        let logo = &data[0x0104..0x0134];
        let mut copies = 0;
        for seg in 0..4 {
            let base = seg * 0x40000;
            if data[base + 0x0104..base + 0x0134] == *logo {
                copies += 1;
            }
        }
        copies >= 2
    }
    /// Reconstruct a trimmed MBC1 multicart dump into the physical 8Mbit
    /// image, or `None` when the data is not one (the overwhelmingly common
    /// case). Some dumps of MBC1M carts (e.g. "Mortal Kombat I & II") strip
    /// each 256KB segment's padding banks, collapsing the games to be
    /// contiguous after the menu. The header still declares MBC1 with 64
    /// banks, but the file is short of that, so `detect_mbc1_multicart`
    /// rejects it and plain-MBC1 BANK2 wiring maps the menu's launch writes to
    /// the wrong banks. This re-bases each segment to its 0x40000 slot (0xFF
    /// fill, like the real ROM's padding) so the multicart detection and the
    /// already-correct MBC1M banking see the physical layout.
    ///
    /// The predicate cannot fire on a normal ROM: it requires the MBC1 type,
    /// a header ROM-size byte of exactly 64 banks with a file SHORTER than
    /// that (never true for a well-formed dump), and two to four
    /// checksum-valid headers carrying the base header's logo at 0x4000-bank
    /// boundaries whose segments each fit a 256KB slot.
    pub(super) fn reconstruct_trimmed_mbc1m(data: &[u8]) -> Option<Vec<u8>> {
        const SEGMENT: usize = 0x40000;
        const FULL: usize = 64 * 0x4000;
        if data.len() < 0x150 || data.len() >= FULL {
            return None;
        }
        if !matches!(data[CARTRIDGE_TYPE_OFFSET], MBC1 | MBC1_RAM | MBC1_RAM_BATTERY) {
            return None;
        }
        if data[ROM_SIZE_OFFSET] != 0x05 {
            return None;
        }
        let logo = &data[0x0104..0x0134];
        if logo.iter().all(|&b| b == logo[0]) {
            return None; // uniform filler would self-match anywhere
        }
        let header_ok = |base: usize| {
            data[base + 0x0104..base + 0x0134] == *logo && {
                let sum = data[base + 0x0134..base + 0x014D]
                    .iter()
                    .fold(0u8, |a, &b| a.wrapping_sub(b).wrapping_sub(1));
                sum == data[base + 0x014D]
            }
        };
        let starts: Vec<usize> =
            (0..data.len().saturating_sub(0x14F)).step_by(0x4000).filter(|&o| header_ok(o)).collect();
        if !(2..=4).contains(&starts.len()) || starts[0] != 0 {
            return None;
        }
        let seg_end = |i: usize| starts.get(i + 1).copied().unwrap_or(data.len());
        if (0..starts.len()).any(|i| seg_end(i) - starts[i] > SEGMENT) {
            return None;
        }
        let mut out = vec![0xFF; FULL];
        for (i, &s) in starts.iter().enumerate() {
            let seg = &data[s..seg_end(i)];
            out[i * SEGMENT..i * SEGMENT + seg.len()].copy_from_slice(seg);
        }
        Some(out)
    }
}
