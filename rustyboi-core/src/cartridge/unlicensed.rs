//! Unlicensed/bootleg boards: content detection plus the read-side boot-lock and
//! protection intercepts (Sachen descramble, Rocket logo gate, Vast Fame VF001).

use super::*;
use super::mapper::{Banking, Geom};
use super::mbc5::Mbc5State;
use serde::{Deserialize, Serialize};

impl Cartridge {
    /// The Sachen MMC address descramble for CPU reads in $0100-$01FF (A8
    /// high, A15..A9 low): RA0<=A6, RA1<=A4, RA4<=A1, RA6<=A0 (bit swaps, so
    /// the mapping is an involution).
    pub(super) fn sachen_unscramble(addr: u16) -> u16 {
        (addr & 0xFFAC)
            | ((addr >> 6) & 0x01)
            | ((addr >> 3) & 0x02)
            | ((addr << 3) & 0x10)
            | ((addr << 6) & 0x40)
    }
    /// Bank-line bit swap used by the NT/Makon and related boards: output bit
    /// i = input bit table[i].
    pub(super) fn reorder_bits(input: u8, table: &[u8; 8]) -> u8 {
        let mut out = 0;
        for (newbit, &oldbit) in table.iter().enumerate() {
            out |= ((input >> oldbit) & 1) << newbit;
        }
        out
    }
    /// Detect unlicensed mapper families from ROM content. The heuristics
    /// follow the community reverse-engineering of these boards and are
    /// deliberately narrow so no licensed cart can ever match:
    /// - Sachen/Rocket require the plain Nintendo logo to be ABSENT at $0104
    ///   (every licensed cart has it, or it would not boot on hardware).
    /// - Wisdom Tree requires header type $00 with a >32KB file plus the
    ///   publisher string (a licensed $00 cart is 32KB by definition), or the
    ///   Pan Docs $C0/$D1 header magic.
    /// - The NT/Makon and ForceMbc1 title rules match the exact
    ///   title/licensee/size shapes of the known carts.
    pub(super) fn detect_unl_mapper(data: &[u8]) -> UnlMapper {
        if data.len() < 0x8000 {
            // Smaller than one full 32KB image: nothing here needs (or can
            // safely take) an unlicensed mapper.
            return UnlMapper::None;
        }

        // M161 (Mani 4 in 1): the header spoofs MBC3+RAM+BATTERY ($10), so
        // detection gates on the exact shape of the one known
        // cart -- a 256KB image whose title is "TETRIS SET". The title check
        // is specific enough that no real MBC3 cart can match.
        if data.len() == 16 * 0x4000
            && data[CARTRIDGE_TYPE_OFFSET] == 0x10
            && &data[0x134..0x13E] == b"TETRIS SET"
        {
            return UnlMapper::M161;
        }

        let logo_sum = |base: usize, scrambled: bool| -> u32 {
            (0..0x30)
                .map(|i| {
                    let a = base + i;
                    let a = if scrambled {
                        Self::sachen_unscramble(a as u16) as usize
                    } else {
                        a
                    };
                    data.get(a).copied().unwrap_or(0) as u32
                })
                .sum()
        };
        let plain_0104 = logo_sum(0x104, false);
        let scrambled_0104 = logo_sum(0x104, true);
        let scrambled_0184 = logo_sum(0x184, true);

        if plain_0104 != LOGO_SUM_NINTENDO {
            // Sachen MMC raw dumps: the Nintendo logo only exists at the
            // scrambled addresses (MMC1 at $01xx, MMC2 at the |0x80 copy),
            // with the Sachen logo at the other bank. Match on either logo
            // (either the Sachen sums or the Nintendo bytes suffice).
            let sachen_a = |s: u32| s == LOGO_SUM_SACHEN_A || s == LOGO_SUM_SACHEN_B;
            if scrambled_0104 == LOGO_SUM_NINTENDO || sachen_a(scrambled_0184) {
                return UnlMapper::SachenMmc1;
            }
            if scrambled_0184 == LOGO_SUM_NINTENDO || sachen_a(scrambled_0104) {
                return UnlMapper::SachenMmc2;
            }
            // Rocket Games logo (checksum 2756; all $97/$99 carts).
            if plain_0104 == LOGO_SUM_ROCKET {
                return UnlMapper::Rocket;
            }
        }

        // strcmp semantics on the 15-byte title at $0134-$0142.
        let title = &data[0x134..0x143];
        let title_eq = |s: &[u8]| -> bool {
            s.len() <= title.len()
                && &title[..s.len()] == s
                && title[s.len()..].first().is_none_or(|&b| b == 0)
        };
        let title_contains =
            |s: &[u8]| -> bool { title.windows(s.len()).any(|w| w == s) };
        let newlic_mk = &data[0x144..0x146] == b"MK";
        let rom_size_code = data[ROM_SIZE_OFFSET];

        // NT/Makon older boards:
        // multicarts with the Pocket Bomberman / Trump Boy / Q Billion menus,
        // the NT Rockman 99 single, and the early Makon GBC singles (Makon
        // "MK" licensee + known title + untouched 256KB header).
        if title_eq(b"POKEBOM USA") && data.len() > 512 * 1024 {
            if data[0x102] == 0xE0 {
                return UnlMapper::NtOld2; // 23-in-1 with Mario
            }
            if data[0x102] == 0xC0 {
                return UnlMapper::NtOld1; // 25-in-1 with Rockman
            }
        }
        if (title_eq(b" - TRUMP  BOY -") || title_eq(b"QBILLION")) && data.len() > 512 * 1024 {
            return UnlMapper::NtOld2;
        }
        if title_eq(b"ROCKMAN 99")
            && !newlic_mk
            && data.get(0x8001).is_some_and(|&b| b != 0xB7)
        {
            return UnlMapper::NtOld1;
        }
        if newlic_mk
            && (title_eq(b"SONIC 7")
                || title_eq(b"SUPER MARIO 3")
                || title_eq(b"DONKEY\tKONG 5")
                || title_eq(b"ROCKMAN 99"))
            && rom_size_code == 0x03
        {
            return UnlMapper::NtOld2;
        }

        // Electrically-plain-MBC1 header liars:
        // Sonic 3D Blast 5 / Super Donkey Kong 3 (type $EA is header-overlap
        // garbage), Captain Knick-Knack (Sachen dump wearing a Tetris header;
        // real Tetris is exactly 32KB so the size gate excludes it), and the
        // 256KB Pocket Monsters GO!GO!GO! dumps.
        if title_contains(b"SONIC5") {
            return UnlMapper::ForceMbc1;
        }
        if title_eq(b"TETRIS") && data.len() > 0x8000 && rom_size_code == 0 {
            return UnlMapper::ForceMbc1;
        }
        if title_eq(b"POCKET MONSTER") && rom_size_code == 0x03 {
            return UnlMapper::ForceMbc1;
        }

        // Vast Fame VF001-class (Legend of Heroes): the secondary VF logo at
        // $0184 AND the exact boot protection stub bytes. The stub check makes
        // a licensed cart matching by logo-sum coincidence impossible.
        if data.len() > VF001_STUB_OFFSET + VF001_STUB.len()
            && logo_sum(0x184, false) == LOGO_SUM_VF001_LOH
            && data[VF001_STUB_OFFSET..VF001_STUB_OFFSET + VF001_STUB.len()] == VF001_STUB
        {
            return UnlMapper::Vf001(Vf001State::default());
        }

        // LiCheng / Niutoude (Vast Fame family): keyed on the CRC32 of the
        // 48-byte secondary logo at $0184 (mGBA `_detectUnlMBC`). The guard
        // mirrors mGBA's: an MBC1 header ($01) OR a file size that disagrees
        // with the header's declared size -- both mark a spoofed cart, so a
        // correctly-dumped MBC5 cart of the declared size is never touched.
        // (All known LiCheng dumps carry the $01 header, so this passes on
        // type alone; the size clause is the belt-and-suspenders half.)
        let logo_crc32 = crate::checksum::crc32(&data[0x184..0x1B4]);
        let claimed_size = 0x8000usize.checked_shl(u32::from(rom_size_code)).unwrap_or(0);
        if LICHENG_LOGO_CRC32.contains(&logo_crc32)
            && (data[CARTRIDGE_TYPE_OFFSET] == 0x01 || data.len() != claimed_size)
        {
            return UnlMapper::LiCheng;
        }

        // BBD (Vast Fame family): keyed on the CRC32 of the 48-byte $0184
        // secondary logo (mGBA `_detectUnlMBC`), gated on $7FFF != $01. A
        // matching $7FFF marks a cracked/decrypted dump that already runs as a
        // plain MBC5, so the bit-scrambling must NOT be applied to it (mGBA's
        // guard). data.len() >= 0x8000 is guaranteed above, so $7FFF is in
        // range. The 48-byte CRC32 window makes a licensed-cart match
        // impossible.
        if BBD_LOGO_CRC32.contains(&logo_crc32) && data[0x7FFF] != 0x01 {
            return UnlMapper::Bbd(BbdState::default());
        }

        // GGB81 (Vast Fame family): keyed on the CRC32 of the 48-byte $0184
        // secondary logo (mGBA `_detectUnlMBC`). Unlike LiCheng these carts
        // wear a truthful MBC5-family header ($19/$1B), so there is no
        // header/size guard -- the 48-byte CRC32 window is the whole gate and
        // cannot match a licensed cart. mGBA applies no $7FFF "fixed dump"
        // guard here either.
        if GGB81_LOGO_CRC32.contains(&logo_crc32) {
            return UnlMapper::Ggb81(0);
        }

        // Wisdom Tree: the Pan Docs $C0-type/$D1 magic, or (type $00 with a
        // banked-size file) the publisher string in the ROM. The
        // string+type+size gate already implies the blank-header shape in
        // practice.
        if data[0x147] == 0xC0 && data[0x14A] == 0xD1 {
            return UnlMapper::WisdomTree;
        }
        if data[0x147] == 0x00
            && (data.windows(11).any(|w| w == b"WISDOM TREE")
                || data.windows(11).any(|w| w == b"WISDOM\0TREE"))
        {
            return UnlMapper::WisdomTree;
        }

        UnlMapper::None
    }
    /// Boot-ROM handoff for skip_bios: the Sachen and Rocket boot locks model
    /// the cart's power-on state as seen BY a real boot ROM; when the boot is
    /// skipped they must start unlocked (the lock state is reset without a
    /// bootstrap). No-op for every other mapper.
    pub(crate) fn skip_boot_handoff(&mut self) {
        match &self.mapper {
            Mapper::Sachen(m) => m.state.lock.set(UNL_UNLOCKED),
            Mapper::Rocket(m) => m.state.lock.set(UNL_UNLOCKED),
            _ => {}
        }
    }
    /// The 48 header-logo bytes a DMG boot ROM would have read through the
    /// LOCKED mapper, when they differ from a plain $0104 read. Sachen MMC1
    /// games check the boot-decompressed VRAM tiles for the SACHEN logo as
    /// copy protection, so skip_bios must seed those tiles instead of the
    /// Nintendo ones (the same expansion is poked into $8010 when no
    /// bootstrap is emulated). Locked MMC1 reads force RA7 high and pass
    /// through the $01xx descramble, so the bytes come from
    /// unscramble($0184+i) — bit 7 survives the bit-swap.
    pub(crate) fn boot_logo_override(&self) -> Option<[u8; 48]> {
        if !matches!(self.get_cartridge_type(), CartridgeType::Sachen { mmc2: false }) {
            return None;
        }
        let mut out = [0u8; 48];
        for (i, b) in out.iter_mut().enumerate() {
            let a = Self::sachen_unscramble((0x184 + i) as u16) as usize;
            *b = self.rom_data.get(a).copied().unwrap_or(0xFF);
        }
        Some(out)
    }
    /// Sachen MMC read-side address transform: boot-lock phase counting plus
    /// the $01xx descramble. Interior mutability (Cell) because the lock
    /// transitions are driven by CPU READS (the A15-transition counter on the
    /// real board).
    pub(super) fn sachen_read_addr(&self, mut addr: u16, mmc2: bool) -> u16 {
        let st = match &self.mapper {
            Mapper::Sachen(m) => &m.state,
            _ => return addr,
        };
        let lock = st.lock.get();
        if mmc2 {
            // MMC2: DMG -> CGB -> unlocked, 0x31 transitions each. (The
            // DMG->CGB shortcut on WRAM traffic is not visible from the
            // cart bus here; the counter path below models the read-driven counter.)
            if lock != UNL_UNLOCKED && (addr & 0x8700) == 0x0100 {
                let t = st.transition.get() + 1;
                if t == 0x31 {
                    st.lock.set(lock + 1);
                    st.transition.set(0);
                } else {
                    st.transition.set(t);
                }
            }
            if (addr & 0xFF00) == 0x0100 {
                if st.lock.get() == UNL_LOCKED_CGB {
                    // Locked: RA7 forced high (presents the second header
                    // copy).
                    addr |= 0x80;
                }
                addr = Self::sachen_unscramble(addr);
            }
        } else {
            // MMC1: single locked phase; the 0x31st $01xx read unlocks.
            if lock != UNL_UNLOCKED && (addr & 0xFF00) == 0x0100 {
                let t = st.transition.get() + 1;
                st.transition.set(t);
                if t == 0x31 {
                    st.lock.set(UNL_UNLOCKED);
                } else {
                    addr |= 0x80;
                }
            }
            if (addr & 0xFF00) == 0x0100 {
                addr = Self::sachen_unscramble(addr);
            }
        }
        addr
    }
    /// Provide the boot ROM's Nintendo logo to the Rocket mapper (sourced from
    /// the loaded boot ROM by `Mmio`). Only consulted during the mapper's
    /// locked-CGB phase, so no logo data is embedded in the cartridge itself.
    pub(crate) fn set_rocket_boot_logo(&mut self, logo: [u8; 48]) {
        self.rocket_boot_logo = Some(logo);
    }
    /// Rocket Games read-side lock counter (advanced on every cart read). While
    /// in the locked-CGB phase, $0104-$0133 present the Nintendo logo so a
    /// running boot ROM's logo check passes; the bytes come from the loaded boot
    /// ROM (`rocket_boot_logo`), so `None` (raw ROM read) when no boot ROM is
    /// present — that window is only ever observed while the boot ROM runs.
    /// (Rocket Games lock state machine.)
    pub(super) fn rocket_locked_logo(&self, addr: u16) -> Option<u8> {
        let rk = match &self.mapper {
            Mapper::Rocket(m) => &m.state,
            _ => return None,
        };
        let mode = rk.lock.get();
        if mode != UNL_UNLOCKED {
            let count = rk.unlock_count.get();
            if count == 0x30 {
                if mode == UNL_LOCKED_DMG {
                    rk.lock.set(UNL_LOCKED_CGB);
                    rk.unlock_count.set(0);
                } else {
                    rk.lock.set(UNL_UNLOCKED);
                }
            } else {
                rk.unlock_count.set(count + 1);
            }
        }
        if rk.lock.get() == UNL_LOCKED_CGB && (0x0104..0x0134).contains(&addr) {
            self.rocket_boot_logo.map(|logo| logo[(addr - 0x0104) as usize])
        } else {
            None
        }
    }
    /// VF001 protection register-file write ($6000-$7FFF). A10-A11 select the
    /// port: port 0 accumulates the 3-byte command, ports 1-3 latch the select
    /// byte. The $7E,$29,$79 command drives the MBC5 ROM-bank register to 6 as
    /// a side effect (the boot flow's `jp $60d0` needs the bank-6
    /// continuation; see the UnlMapper::Vf001 doc).
    pub(super) fn vf001_write(&mut self, addr: u16, value: u8) {
        let UnlMapper::Vf001(ref mut st) = self.unl_mapper else {
            return;
        };
        if (addr >> 10) & 3 == 0 {
            st.cmd = [st.cmd[1], st.cmd[2], value];
            let set_bank6 = st.cmd == [0x7E, 0x29, 0x79];
            if set_bank6
                && let Mapper::Vf001(m) = &mut self.mapper {
                    m.regs.rom_bank_low = 6;
                }
        } else {
            st.select = value;
        }
    }
    /// VF001 protection read front-end for $A000-$BFFF. Returns the derived
    /// value when the armed (command, select, port) triple matches one of the
    /// cart's protection sequences; None falls through to normal cart RAM.
    pub(super) fn vf001_protection_read(st: Vf001State, addr: u16) -> Option<u8> {
        let port = (addr >> 10) & 3;
        match (st.cmd, port) {
            // Boot gate ($32FC): hl bytes for `jp (hl)` -> $0CAE.
            ([0x9A, 0xB8, 0xB9], 2) => match st.select {
                0xB9 => Some(0xC1),
                0x83 => Some(0xF8),
                _ => None,
            },
            // Second gate ($0D36): hl bytes for `jp (hl)` -> $08E9.
            ([0x37, 0x52, 0xCD], 2) => match st.select {
                0xBA => Some(0x82),
                0xA9 => Some(0x8F),
                _ => None,
            },
            // Bank-switch command ($0D16): the $AFFF readback is a decoy on
            // the good path (bank-6 $60D0 discards it); serve a constant.
            ([0x7E, 0x29, 0x79], 3) => Some(0x31),
            // TMA seed ($1015): never branched on (timer IRQ vector is a bare
            // reti and IE.2 is never set); TIMA is only polled as an RNG tap.
            ([_, 0xB9, 0x81], 0) => Some(0x00),
            _ => None,
        }
    }
    /// BBD $2000-$3FFF register write (mGBA `_GBBBD`). Matched on `addr &
    /// 0xF0FF`: $2001 latches the data swap mode, $2080 the bank swap mode, and
    /// $2000 reorders the written bank number through the current bank table.
    /// The (possibly reordered) value then latches into the MBC5 ROM-bank
    /// register exactly as `_GBMBC5` would, so mode writes also update the low
    /// bank register -- a faithful mGBA side effect the game overwrites with its
    /// next $2000 write. Swap-mode state rides in the `UnlMapper::Bbd` payload;
    /// the bank register lives on the `Mapper::Bbd` board.
    pub(super) fn bbd_write(&mut self, addr: u16, value: u8) {
        let value = {
            let UnlMapper::Bbd(ref mut st) = self.unl_mapper else {
                return;
            };
            match addr & 0xF0FF {
                0x2000 => {
                    Self::reorder_bits(value, &BBD_BANK_REORDERING[st.bank_swap_mode as usize])
                }
                0x2001 => {
                    st.data_swap_mode = value & 0x07;
                    value
                }
                0x2080 => {
                    st.bank_swap_mode = value & 0x07;
                    value
                }
                _ => value,
            }
        };
        if let Mapper::Bbd(m) = &mut self.mapper {
            if addr <= 0x2FFF {
                m.regs.rom_bank_low = value; // MBC5 low 8 bits (bank 0 allowed)
            } else {
                m.regs.rom_bank_high = value & 0x01; // MBC5 high bit
            }
        }
    }
    /// GGB81 $2000-$3FFF register write (mGBA `_GBGGB81`). A write with
    /// `addr & 0xF0FF == 0x2001` also latches the 3-bit data-swap mode (carried
    /// in the `UnlMapper::Ggb81` payload); the raw value still lands in the MBC5
    /// low/high bank register, exactly as `_GBMBC5` would (mGBA falls through).
    pub(super) fn ggb81_write(&mut self, addr: u16, value: u8) {
        if let UnlMapper::Ggb81(ref mut mode) = self.unl_mapper
            && (addr & 0xF0FF) == 0x2001
        {
            *mode = value & 0x07;
        }
        if let Mapper::Ggb81(m) = &mut self.mapper {
            if addr <= 0x2FFF {
                m.regs.rom_bank_low = value;
            } else {
                m.regs.rom_bank_high = value & 0x01;
            }
        }
    }
}

// --- board struct + banking ---------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct WisdomTree {
    pub bank: u8, // 6-bit whole-32KB latch
}

impl Banking for WisdomTree {
    fn rom_bankn(&self, g: Geom) -> usize {
        (self.bank as usize * 2 + 1) % g.rom_banks
    }
    fn rom_bank0(&self, g: Geom) -> usize {
        (self.bank as usize * 2) % g.rom_banks
    }
    fn ram_bank(&self, _g: Geom) -> usize {
        0
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Rocket {
    pub state: RocketState,
}

impl Banking for Rocket {
    fn rom_bankn(&self, g: Geom) -> usize {
        (((self.state.outer as usize & 0x0F) << 4) | (self.state.rom_bank as usize)) % g.rom_banks
    }
    fn rom_bank0(&self, g: Geom) -> usize {
        ((self.state.outer as usize & 0x0F) << 4) % g.rom_banks
    }
    fn ram_bank(&self, _g: Geom) -> usize {
        0
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Sachen {
    pub state: SachenState,
    pub mmc2: bool,
}

impl Banking for Sachen {
    fn rom_bankn(&self, g: Geom) -> usize {
        (((self.state.bank & !self.state.mask) | (self.state.base & self.state.mask)) as usize)
            % g.rom_banks
    }
    fn rom_bank0(&self, g: Geom) -> usize {
        ((self.state.base & self.state.mask) as usize) % g.rom_banks
    }
    fn ram_bank(&self, _g: Geom) -> usize {
        0
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct NtOld {
    pub state: NtState,
    pub v2: bool,
    pub ram_enabled: bool, // MBC3-style RAM gate ($0A to $0000-$1FFF)
}

impl Banking for NtOld {
    fn rom_bankn(&self, g: Geom) -> usize {
        // The $5003 bit-swap is combinational on the bank lines; the $5002
        // bank-count mask and the $5001 base (32KB units) apply after it.
        let mut bank = self.state.bank;
        if self.state.swapped {
            let table = if self.v2 { &super::NT_OLD2_REORDER } else { &super::NT_OLD1_REORDER };
            bank = super::Cartridge::reorder_bits(bank, table);
        }
        if self.state.bank_mask != 0 {
            bank &= self.state.bank_mask;
        }
        (bank as usize + self.state.base as usize * 2) % g.rom_banks
    }
    fn rom_bank0(&self, g: Geom) -> usize {
        (self.state.base as usize * 2) % g.rom_banks
    }
    fn ram_bank(&self, _g: Geom) -> usize {
        0
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct M161 {
    pub state: M161State,
}

impl Banking for M161 {
    fn rom_bankn(&self, g: Geom) -> usize {
        ((self.state.bank as usize) | 1) & (g.rom_banks - 1)
    }
    fn rom_bank0(&self, g: Geom) -> usize {
        (self.state.bank as usize) & (g.rom_banks - 2)
    }
    fn ram_bank(&self, _g: Geom) -> usize {
        0
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Vf001 {
    pub ram_enabled: bool,
    pub regs: Mbc5State,
}

impl Banking for Vf001 {
    fn rom_bankn(&self, g: Geom) -> usize {
        let bank =
            (self.regs.rom_bank_low as usize) | ((self.regs.rom_bank_high as usize & 0x01) << 8);
        bank % g.rom_banks
    }
    fn rom_bank0(&self, _g: Geom) -> usize {
        0
    }
    fn ram_bank(&self, g: Geom) -> usize {
        (self.regs.ram_bank & 0x0F) as usize % g.ram_banks.max(1)
    }
}

/// LiCheng / Niutoude: electrically a plain MBC5+RAM. The only deviation is the
/// $2101-$2FFF bank-write ignore, handled in the write dispatch; the bank math
/// is identical to MBC5.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct LiCheng {
    pub ram_enabled: bool,
    pub regs: Mbc5State,
}

impl Banking for LiCheng {
    fn rom_bankn(&self, g: Geom) -> usize {
        let bank =
            (self.regs.rom_bank_low as usize) | ((self.regs.rom_bank_high as usize & 0x01) << 8);
        bank % g.rom_banks
    }
    fn rom_bank0(&self, _g: Geom) -> usize {
        0
    }
    fn ram_bank(&self, g: Geom) -> usize {
        (self.regs.ram_bank & 0x0F) as usize % g.ram_banks.max(1)
    }
}

/// BBD: electrically MBC5+RAM. The $2000-$2FFF bit-scramble protocol
/// (`bbd_write`) and the $4000-$7FFF read reorder are applied around this
/// board; the bank math itself is plain MBC5.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Bbd {
    pub ram_enabled: bool,
    pub regs: Mbc5State,
}

impl Banking for Bbd {
    fn rom_bankn(&self, g: Geom) -> usize {
        let bank =
            (self.regs.rom_bank_low as usize) | ((self.regs.rom_bank_high as usize & 0x01) << 8);
        bank % g.rom_banks
    }
    fn rom_bank0(&self, _g: Geom) -> usize {
        0
    }
    fn ram_bank(&self, g: Geom) -> usize {
        (self.regs.ram_bank & 0x0F) as usize % g.ram_banks.max(1)
    }
}

/// GGB81: electrically MBC5+RAM. The data-swap mode ($2001) and the
/// $4000-$7FFF read reorder are applied around this board; the bank math is
/// plain MBC5.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Ggb81 {
    pub ram_enabled: bool,
    pub regs: Mbc5State,
}

impl Banking for Ggb81 {
    fn rom_bankn(&self, g: Geom) -> usize {
        let bank =
            (self.regs.rom_bank_low as usize) | ((self.regs.rom_bank_high as usize & 0x01) << 8);
        bank % g.rom_banks
    }
    fn rom_bank0(&self, _g: Geom) -> usize {
        0
    }
    fn ram_bank(&self, g: Geom) -> usize {
        (self.regs.ram_bank & 0x0F) as usize % g.ram_banks.max(1)
    }
}


// --- state ---------------------------------------------------------------

/// Rocket Games board state. `lock`/`unlock_count` model the A15-transition
/// boot lock: the cart powers up locked and, while a boot ROM is running,
/// presents the Nintendo logo during the boot ROM's logo check; skip_bios
/// unlocks immediately (no boot ROM ran). Cell: the counter advances on ROM
/// READS, and `Addressable::read` takes `&self`.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct RocketState {
    pub(super) rom_bank: u8,
    pub(super) outer: u8,
    pub(super) lock: Cell<u8>,
    pub(super) unlock_count: Cell<u8>,
}

impl Default for RocketState {
    fn default() -> Self {
        Self { rom_bank: 1, outer: 0, lock: Cell::new(UNL_LOCKED_DMG), unlock_count: Cell::new(0) }
    }
}

/// Sachen MMC1/MMC2 state. `bank` is the raw inner-bank latch ("unmasked
/// bank"); `base`/`mask` writes only latch while (bank & 0x30) == 0x30. Lock
/// phases as for Rocket.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct SachenState {
    pub(super) base: u8,
    pub(super) mask: u8,
    pub(super) bank: u8,
    pub(super) lock: Cell<u8>,
    pub(super) transition: Cell<u8>,
}

impl Default for SachenState {
    fn default() -> Self {
        Self { base: 0, mask: 0, bank: 1, lock: Cell::new(UNL_LOCKED_DMG), transition: Cell::new(0) }
    }
}

/// NT/Makon "old" board state. `bank` holds the raw written bank; the $5003
/// bit-swap is combinational on the bank lines, so it is applied at read time
/// (`get_rom_bank`), keeping swap-mode flips retroactive exactly like the real
/// wiring (a push-model map would instead re-switch on the mode write to
/// emulate the same thing).
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct NtState {
    pub(super) bank: u8,
    pub(super) base: u8,
    pub(super) bank_mask: u8,
    pub(super) swapped: bool,
}

impl Default for NtState {
    fn default() -> Self {
        Self { bank: 1, base: 0, bank_mask: 0, swapped: false }
    }
}

/// M161 one-shot 32KB latch. `bank` is the even 16KB half of the selected 32KB
/// pair ((data & 7) << 1); the odd half is `bank | 1`. `mapped` blocks any
/// further latch until reset.
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub(super) struct M161State {
    pub(super) bank: u8,
    pub(super) mapped: bool,
}
