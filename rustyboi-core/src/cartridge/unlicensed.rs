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

        // Hong Kong "POCKETMON" Pokemon Red bootleg: an MBC1+RAM+BATTERY header
        // over a game re-linked to MBC5-style full-width banking. Keyed on the
        // 48-byte $0184 CRC32 signature, gated on the MBC1-family header byte so
        // the (impossible-to-collide) CRC alone never touches a licensed cart.
        // Electrically a plain MBC5+RAM+BATTERY with no scramble.
        if logo_crc32 == super::POCKETMON_MBC5_LOGO_CRC32
            && matches!(data[CARTRIDGE_TYPE_OFFSET], 0x01..=0x03)
        {
            return UnlMapper::ForceMbc5;
        }

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

        // Sintax (Vast Fame family): keyed on the CRC32 of the 48-byte $0184
        // secondary logo (mGBA `_detectUnlMBC`), gated by mGBA's "not a fixed
        // dump" guard -- $7FFF != 0x01. A cracked/"fixed" dump already runs as
        // a plain MBC5, so re-applying the scramble would break it; the guard
        // leaves those untouched. The 48-byte CRC32 window cannot collide with
        // a licensed cart. (data.len() >= 0x8000 is guaranteed above.)
        if SINTAX_LOGO_CRC32.contains(&logo_crc32) && data[0x7FFF] != 0x01 {
            return UnlMapper::Sintax(SintaxState::default());
        }

        // HITEK (Vast Fame family): keyed on the CRC32 of the 48-byte $0184
        // secondary logo (mGBA `_detectUnlMBC`). The $7FFF != $01 guard mirrors
        // mGBA/hhugboy: a matching $01 there marks a cracked/decrypted dump that
        // already runs as plain MBC5, so the descrambler must NOT be applied.
        if logo_crc32 == HITEK_LOGO_CRC32 && data[0x7FFF] != 0x01 {
            return UnlMapper::Hitek(HitekState::default());
        }

        // General VF001 protection board (taizou hhugboy `MbcUnlVf001`, CC0):
        // keyed on the CRC32 of the 48-byte $0184 secondary logo. hhugboy applies
        // no $7FFF "fixed dump" guard to this family, and the collision-free
        // 48-byte CRC32 window is the whole gate — no licensed cart can match.
        // The MBC5-family header gate is conservative: the carts proven to boot
        // under this protection model (Nv Wang Gedou 2000, the Soul Falchion
        // pair) all declare a truthful MBC5 header ($19-$1E), while Zook Z shares
        // the byte-identical "V.fame" logo behind a spoofed MBC1 header ($01) and
        // does not yet render, so it is left on its plain-MBC1 path. Widen this
        // gate when the MBC1-header variant is cracked.
        if super::VF001G_LOGO_CRC32.contains(&logo_crc32)
            && matches!(data[CARTRIDGE_TYPE_OFFSET], 0x19..=0x1E)
        {
            return UnlMapper::Vf001Gen(Box::default());
        }

        // Vast Fame 8 KiB dual-window board (Jieba Tianwang 4). There is no
        // secondary logo at $0184 on this cart -- those bytes are game code --
        // so detection keys on the board's own power-on handshake, which the
        // cart executes out of the header itself:
        //   $0100: nop / jp $0134          (entry jumps INTO the title field)
        //   $0134: push af / ld a,$AA / ld ($7000),a / ...
        // A licensed cart cannot match: its $0134-$0143 must hold the ASCII
        // title, and no licensed entry point jumps into the title field. The
        // header-type gate keeps this to the MBC5 family the board really is.
        // (The `Super Color 26-in-1` multicart embeds this engine's far-call
        // thunk but is a plain MBC5 with a $3FF0 entry, so it stays excluded --
        // 8 KiB windows break it.)
        if data[0x100] == 0x00
            && data[0x101] == 0xC3
            && (0x0134..=0x0143).contains(&(u16::from(data[0x102]) | (u16::from(data[0x103]) << 8)))
            && data[0x134..0x134 + VF8K_BOOT_STUB.len()] == VF8K_BOOT_STUB
            && matches!(data[CARTRIDGE_TYPE_OFFSET], 0x19..=0x1E)
        {
            return UnlMapper::Vf8k(Vf8kState::default());
        }

        // "New GB Color" HK PCB (taizou hhugboy `MbcUnlNewGbHk`, CC0): keyed on
        // the CRC32 of the 46-byte protection trampoline at $0091, in the dead
        // space between the interrupt vectors and the header. Those bytes are
        // protection-specific code -- they write the bank register with bit 7
        // forced high and then read the resulting protection window -- so no
        // licensed cart carries them, and a 46-byte CRC32 cannot collide. The
        // MBC5-family header gate is belt-and-suspenders (the known cart
        // declares $1B truthfully).
        if crate::checksum::crc32(
            &data[NEWGBHK_STUB_OFFSET..NEWGBHK_STUB_OFFSET + NEWGBHK_STUB_LEN],
        ) == NEWGBHK_STUB_CRC32
            && matches!(data[CARTRIDGE_TYPE_OFFSET], 0x19..=0x1E)
        {
            return UnlMapper::NewGbHk;
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

    /// General VF001 config-register write ($6000-$7FFF), a faithful port of
    /// taizou's hhugboy `MbcUnlVf001::writeMemory`. The port is decoded
    /// `addr & 0xF00F`. Writing $96 to $7000 opens config mode (seeding the
    /// running accumulator from the per-cart config byte -- 0 for every
    /// auto-detected VF001 cart), $96 to $700F closes it. While open, each known
    /// config write folds the data into a rotate-right-then-XOR running value
    /// latched per port; a $7000 or $7008 write then activates the byte-injection
    /// or bank-0-replacement effect from the latched ports.
    pub(super) fn vf001g_write(&mut self, addr: u16, data: u8) {
        let UnlMapper::Vf001Gen(ref mut st) = self.unl_mapper else {
            return;
        };
        let eff = addr & 0xF00F;
        if eff == 0x7000 && data == 0x96 {
            st.config_mode = true;
            st.running_value = 0; // config seed 0 (0x10 is the VF001A variant)
            return;
        }
        if eff == 0x700F && data == 0x96 {
            st.config_mode = false;
            return;
        }
        if !st.config_mode {
            return;
        }
        // Only $6000 and $7000-$700A are known config ports; ignore the rest
        // (taizou leaves these unverified, so they are inert).
        if eff >= 0x700B || (eff < 0x7000 && eff > 0x6000) {
            return;
        }
        // Running accumulator: rotate right one bit, then XOR the written data.
        st.running_value =
            (if st.running_value & 1 != 0 { 0x80 } else { 0 }) + (st.running_value >> 1);
        st.running_value ^= data;
        if eff >= 0x7000 {
            st.cur700x[(eff & 0xF) as usize] = st.running_value;
        } else if eff == 0x6000 {
            st.cur6000 = st.running_value;
        }
        // Byte-injection activation ($7000): $7001-2 start address, $7003 start
        // bank, $7004-7 the up-to-4 bytes, and $7000's low 3 bits the length
        // (cmd 4->1 .. 7->4 bytes).
        if eff == 0x7000 {
            st.seq_start_bank = st.cur700x[3];
            st.seq_start_addr = (u16::from(st.cur700x[2]) << 8) + u16::from(st.cur700x[1]);
            st.sequence = [st.cur700x[4], st.cur700x[5], st.cur700x[6], st.cur700x[7]];
            st.seq_len = match st.cur700x[0] & 7 {
                4 => 1,
                5 => 2,
                6 => 3,
                7 => 4,
                _ => 0,
            };
        }
        // Bank-0 replacement activation ($7008): $7009-a start address, $6000 the
        // source bank, and $7008's low nibble $F the enable.
        if eff == 0x7008 {
            st.replace_start_addr = (u16::from(st.cur700x[10]) << 8) + u16::from(st.cur700x[9]);
            st.replace_source_bank = st.cur6000;
            st.should_replace = (st.cur700x[8] & 0xF) == 0xF;
        }
    }

    /// General VF001 protection ROM read ($0000-$7FFF), a faithful port of
    /// taizou's hhugboy `MbcUnlVf001::readMemory`. Returns the injected or
    /// replaced byte when a protection effect is live, else `None` so the caller
    /// falls through to a normal ROM read.
    pub(super) fn vf001g_read(&self, st: &Vf001gState, addr: u16) -> Option<u8> {
        // (1) Byte-sequence injection. A read of the configured (bank, address)
        // arms the sequence; the next `seq_len` ROM reads then return the
        // programmed bytes in turn (taizou consumes one per ROM read < $8000, so
        // an injected multi-byte instruction's operand fetches consume it too).
        // The `>= 4000` literal is taizou's own (decimal): for a switchable-bank
        // trigger the CPU address is already in $4000-$7FFF, so it is always
        // satisfied -- the address equality is the real gate.
        if st.seq_len > 0 {
            let bank_matches = (st.seq_start_bank == 0 && addr < 0x3FFF)
                || (u16::from(st.seq_start_bank) == self.get_rom_bank() as u16 && addr >= 4000);
            if bank_matches && addr == st.seq_start_addr && st.seq_bytes_left.get() == 0 {
                st.seq_bytes_left.set(st.seq_len);
            }
        }
        if st.seq_bytes_left.get() > 0 {
            let left = st.seq_bytes_left.get() - 1;
            st.seq_bytes_left.set(left);
            let current = st.seq_len - left; // 1-based index into the sequence
            return Some(st.sequence[usize::from(current - 1)]);
        }
        // (2) Bank-0 partial replacement: reads of bank 0 from the configured
        // address on are served from the configured source bank (overlaying the
        // real entry onto the decoy header region).
        if st.should_replace && addr >= st.replace_start_addr && addr < 0x4000 {
            let base = (usize::from(st.replace_source_bank) << 14)
                & self.rom_data.len().wrapping_sub(1);
            return self.rom_data.get(base + addr as usize).copied().or(Some(0xFF));
        }
        None
    }

    /// Vast Fame 8 KiB dual-window ROM-bank write ($2000-$3FFF). A10 selects
    /// which 8 KiB page of the switchable area the value programs: low picks
    /// the $4000-$5FFF page, high the $6000-$7FFF page. There is no 16 KiB bank
    /// register on this board, so nothing else is latched.
    pub(super) fn vf8k_write(&mut self, addr: u16, value: u8) {
        if let UnlMapper::Vf8k(ref mut st) = self.unl_mapper {
            if addr & 0x0400 == 0 {
                st.low = value;
            } else {
                st.high = value;
            }
        }
    }

    /// Vast Fame 8 KiB dual-window ROM read ($4000-$7FFF). Each half is served
    /// from its own 8 KiB page register; out-of-range pages wrap to the image
    /// like every other board here.
    pub(super) fn vf8k_read(&self, st: Vf8kState, addr: u16) -> u8 {
        let page = if addr < 0x6000 { st.low } else { st.high };
        let pages = (self.rom_data.len() / VF8K_PAGE).max(1);
        let base = (page as usize % pages) * VF8K_PAGE;
        self.rom_data.get(base + (addr as usize & (VF8K_PAGE - 1))).copied().unwrap_or(0xFF)
    }

    /// "New GB Color" HK-PCB protection read (taizou's hhugboy
    /// `MbcUnlNewGbHk`, CC0). While the MBC5 ROM-bank register holds a value of
    /// $80 or more, the switchable window is the protection chip rather than
    /// ROM: $5000-$7FFF reads back $FF, and $4000-$4FFF returns a byte from
    /// the address bits A4-A11 (`digits`) by one of eight transforms, selected
    /// by `digits & 7`. Returns `None` when the protection is not engaged so
    /// the caller falls through to a normal MBC5 ROM read.
    pub(super) fn newgbhk_read(&self, addr: u16) -> Option<u8> {
        let Mapper::Mbc5(m) = &self.mapper else { return None };
        let bank = u16::from(m.regs.rom_bank_low) | (u16::from(m.regs.rom_bank_high) << 8);
        if bank < 0x80 || !(0x4000..0x8000).contains(&addr) {
            return None;
        }
        if addr >= 0x5000 {
            return Some(0xFF);
        }
        let digits = ((addr >> 4) & 0xFF) as u8;
        // Pairs of bits of `digits` folded into a byte twice over: bits 7/5/3/1
        // into both nibbles for `even`, bits 6/4/2/0 for `odd` (taizou's
        // `evenBitsTwice`/`oddBitsTwice` tables, expressed here directly).
        let spread = |first: u32| -> u8 {
            let mut out = 0u8;
            for i in 0..4 {
                let bit = (digits >> (7 - (first + 2 * i))) & 1;
                out |= bit << (7 - i);
                out |= bit << (3 - i);
            }
            out
        };
        Some(match digits & 7 {
            0 => digits,
            1 => digits ^ 0xAA,
            2 => digits ^ 0x55,
            3 => digits.rotate_right(1),
            4 => digits.rotate_left(1),
            5 => digits.reverse_bits(),
            // OR each bit pair into the high nibble, AND it into the low one.
            6 => {
                let (e, o) = (spread(0), spread(1));
                ((e | o) & 0xF0) | (e & o & 0x0F)
            }
            // XNOR each bit pair into the high nibble, XOR it into the low one.
            _ => {
                let (e, o) = (spread(0), spread(1));
                (!(e ^ o) & 0xF0) | ((e ^ o) & 0x0F)
            }
        })
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
    /// Sintax (Vast Fame family) register protocol (mGBA `_GBSintax`). The board
    /// is electrically MBC5; three windows drive the boot-programmed scramble:
    ///   $2000-$2FFF     bank number, bit-reordered through the active mode's
    ///                   table into the MBC5 low-8 bank register; the RAW value's
    ///                   low 2 bits also pick which XOR byte is active.
    ///   $5x1x           set the 4-bit reorder mode, then re-derive the bank
    ///                   register and XOR from the stored bank number (mGBA
    ///                   replays a fake $2000 write with the new mode).
    ///   $7020/30/40/50  program the four per-bank XOR bytes.
    /// Everything else behaves as plain MBC5. The $4000-$7FFF read XOR is applied
    /// in the read path. `st` is a copy of the `Copy` scramble payload, so `self`
    /// stays borrowable; it is written back at the end. The MBC5 bank/RAM
    /// registers live on the `Mapper::Sintax` board.
    pub(super) fn sintax_write(&mut self, addr: u16, value: u8) {
        let UnlMapper::Sintax(mut st) = self.unl_mapper else {
            return;
        };
        match addr {
            RAM_ENABLE_START..=RAM_ENABLE_END => {
                if let Mapper::Sintax(m) = &mut self.mapper {
                    m.ram_enabled = (value & 0x0F) == 0x0A;
                }
            }
            0x2000..=0x2FFF => {
                st.bank_no = value;
                st.rom_bank_xor = st.xor_values[(value & 0x03) as usize];
                let low = Self::reorder_bits(value, &SINTAX_BANK_REORDER[st.mode as usize]);
                if let Mapper::Sintax(m) = &mut self.mapper {
                    m.regs.rom_bank_low = low;
                }
            }
            0x3000..=0x3FFF => {
                // Bit 8 of the ROM bank -- not reordered (plain MBC5).
                if let Mapper::Sintax(m) = &mut self.mapper {
                    m.regs.rom_bank_high = value & 0x01;
                }
            }
            // Mode select ($5x1x): any address matching `5?1?`. Metal Max writes
            // other $5xxx addresses before battles, which must NOT be treated as
            // mode writes -- only 5x1x is recognised (mGBA comment).
            _ if (addr & 0xF0F0) == 0x5010 => {
                st.mode = value & 0x0F;
                st.rom_bank_xor = st.xor_values[(st.bank_no & 0x03) as usize];
                let low = Self::reorder_bits(st.bank_no, &SINTAX_BANK_REORDER[st.mode as usize]);
                if let Mapper::Sintax(m) = &mut self.mapper {
                    m.regs.rom_bank_low = low;
                }
            }
            RAM_BANK_ROM_BANK_HIGH_START..=RAM_BANK_ROM_BANK_HIGH_END => {
                // Non-$5x1x RAM-bank register: plain MBC5.
                if let Mapper::Sintax(m) = &mut self.mapper {
                    m.regs.ram_bank = value;
                }
            }
            0x7000..=0x7FFF => {
                // Nibble 2 selects which XOR byte to program; the applied XOR is
                // then recomputed for the current bank.
                match (addr & 0x00F0) >> 4 {
                    2 => st.xor_values[0] = value,
                    3 => st.xor_values[1] = value,
                    4 => st.xor_values[2] = value,
                    5 => st.xor_values[3] = value,
                    _ => {}
                }
                st.rom_bank_xor = st.xor_values[(st.bank_no & 0x03) as usize];
            }
            _ => {}
        }
        self.unl_mapper = UnlMapper::Sintax(st);
    }
    /// HITEK write dispatch (mGBA `_GBHitek`): the two swap-mode ports layered
    /// over plain MBC5 write semantics. The register is selected by
    /// `addr & 0xF0FF`, so it fires regardless of the middle address nibble the
    /// game happens to spray. HitekState is Copy, so the swap modes are edited
    /// on a local and written back once (no split borrow). mGBA also lists a
    /// `case 0x300`, but `addr & 0xF0FF` can never be 0x300 (bits 8-9 are
    /// cleared by the mask), so that branch is dead on hardware and omitted.
    pub(super) fn hitek_write(&mut self, addr: u16, value: u8) {
        let UnlMapper::Hitek(mut st) = self.unl_mapper else {
            return;
        };
        let mut value = value;
        match addr & 0xF0FF {
            // Bank-select: the written bank number is bit-reordered before it
            // reaches the MBC5 low-bank register.
            0x2000 => {
                value =
                    Self::reorder_bits(value, &HITEK_BANK_REORDERING[(st.bank_swap_mode & 7) as usize])
            }
            // Program the data-swap mode; the raw value still lands in the MBC5
            // low-bank register below, exactly as on hardware/mGBA.
            0x2001 => st.data_swap_mode = value & 7,
            // Program the bank-swap mode (likewise falls through to MBC5).
            0x2080 => st.bank_swap_mode = value & 7,
            _ => {}
        }
        self.unl_mapper = UnlMapper::Hitek(st);
        // Plain MBC5 write semantics for the effective (addr, value). HITEK has
        // no rumble and no banking-mode register, so $6000-$7FFF is inert.
        if let Mapper::Hitek(m) = &mut self.mapper {
            match addr {
                RAM_ENABLE_START..=RAM_ENABLE_END => {
                    m.ram_enabled = (value & 0x0F) == 0x0A
                }
                ROM_BANK_SELECT_START..=ROM_BANK_SELECT_END => {
                    if addr <= 0x2FFF {
                        m.regs.rom_bank_low = value;
                    } else {
                        m.regs.rom_bank_high = value & 0x01;
                    }
                }
                RAM_BANK_ROM_BANK_HIGH_START..=RAM_BANK_ROM_BANK_HIGH_END => {
                    m.regs.ram_bank = value;
                }
                _ => {}
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

/// Sintax: electrically MBC5+RAM. The $0000-$7FFF scramble protocol
/// (`sintax_write`) and the $4000-$7FFF read XOR are applied around this board;
/// the bank math itself is plain MBC5 (the reorder is done before the value
/// reaches these registers).
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Sintax {
    pub ram_enabled: bool,
    pub regs: Mbc5State,
}

impl Banking for Sintax {
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

/// HITEK: electrically MBC5+RAM+BATTERY. The two swap-mode ports ($2001/$2080),
/// the $2000 bank reorder (`hitek_write`), and the $4000-$7FFF read reorder are
/// applied around this board; the bank math is plain MBC5.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Hitek {
    pub ram_enabled: bool,
    pub regs: Mbc5State,
}

impl Banking for Hitek {
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
