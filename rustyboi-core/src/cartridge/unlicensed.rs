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
        // Whole-image hash, the key for the rules that must not false-positive
        // on a cart sharing a signature window (Zelda DX betas, the VF001-behind
        // -GGB81 pair).
        let rom_crc32 = crate::checksum::crc32(data);

        // "Pokemon Jade / Diamond" board (Telefang bootlegs): an
        // MBC3+TIMER+RAM+BATTERY header ($10) plus the D/E/F challenge
        // handshake (hhugboy `MbcUnlPokeJadeDia`, mGBA `_GBPKJD`). The $0184
        // bytes are executable code, so a 48-byte CRC32 (not a byte-sum) is the
        // safe discriminator; the header-type $10 guard plus the collision-free
        // CRC32 window means no licensed cart can match. (M161 also spoofs $10
        // but is gated on the "TETRIS SET" title + 256KB above, so the two
        // never cross-detect.)
        if data[CARTRIDGE_TYPE_OFFSET] == 0x10 && logo_crc32 == super::POKEJADE_LOGO_CRC32 {
            return UnlMapper::PokeJadeDia(PokeJadeState::default());
        }

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

        // Zelda no Densetsu - Yume o Miru Shima DX (Japan) prototype betas (the
        // 1998-06-15 pair + 1998-07-14T181500): the header declares MBC1+RAM+
        // BATTERY ($03) but the game already drives MBC5-style banking, writing
        // full-width bank numbers (>$1F) to $2000-$3FFF (verified: it writes $20
        // to $2100 and never touches the MBC1 BANK2/mode registers). A 5-bit
        // MBC1 mask folds those to bank 0/1 and the boot hangs; electrically
        // these dumps are MBC5. Keyed on the exact whole-ROM CRC32 so only these
        // three prototype dumps are re-mapped -- zero false positives.
        if super::ZELDA_DX_BETA_ROM_CRC32.contains(&rom_crc32) {
            return UnlMapper::ForceMbc5;
        }

        // Mythri (Proto 1 + Proto 2) and Tyrannosaurus Tex (Proto): publisher
        // demos whose MBC5+RAM+BATTERY ($1B) header is a lie -- they write a
        // literal 0 to the ROM-bank register and keep executing from the
        // switchable window, which only survives on a mapper that remaps a zero
        // bank to 1 (MBC3). See `UnlMapper::ForceMbc3` for the two boot traces.
        // Keyed on the exact whole-ROM CRC32 of the three prototype dumps.
        if super::MBC5_HEADER_MBC3_PROTO_ROM_CRC32.contains(&crate::checksum::crc32(data)) {
            return UnlMapper::ForceMbc3;
        }

        // Chongwu Xiao Jingling - Jiejin Ta Zhi Wang (Taiwan) ("DIGIMON", 1 MiB):
        // a plain MBC5+RAM+BATTERY apart from one boot-time protection read. Its
        // $0184 secondary logo is the LiCheng/Niutoude one, so a bare logo gate
        // would route it to `UnlMapper::LiCheng` (where it hangs at $00D4 on the
        // failed check). Keyed on the exact whole-ROM CRC32 — and ordered before
        // the LiCheng arm — because the protection response the board returns is a
        // single OBSERVED latch byte, not a derived transform: gating on the whole
        // image confines the observed constant to this one file. See
        // `UnlMapper::Chongwu`.
        if rom_crc32 == super::CHONGWU_ROM_CRC32 {
            return UnlMapper::Chongwu(ChongwuState::default());
        }

        let claimed_size = 0x8000usize.checked_shl(u32::from(rom_size_code)).unwrap_or(0);
        if LICHENG_LOGO_CRC32.contains(&logo_crc32)
            && (data[CARTRIDGE_TYPE_OFFSET] == 0x01 || data.len() != claimed_size)
        {
            return UnlMapper::LiCheng;
        }

        // The Makon Soft carts run on the same NT "new" split-window board as
        // Capcom vs SNK: they unlock it with the byte sequence below and then
        // arm the split window with the same $1400 <- $55 / $2000 / $2400
        // protocol. Keyed on that unlock sequence wherever it appears (its offset
        // varies per cart, so a fixed-offset hash cannot see it), plus the
        // MBC1/MBC5-family header these carts declare. Deliberately ordered AFTER
        // the LiCheng arm: two carts carry both this unlock and the Niutoude
        // $0184 logo, and both of those boot correctly as LiCheng and white-screen
        // on this board, so LiCheng must keep them. See `NTNEW_MAKON_UNLOCK`.
        if matches!(data[CARTRIDGE_TYPE_OFFSET], 0x00..=0x03 | 0x19..=0x1E)
            && data
                .windows(super::NTNEW_MAKON_UNLOCK.len())
                .any(|w| w == super::NTNEW_MAKON_UNLOCK)
        {
            return UnlMapper::NtNew(NtNewState::default());
        }

        // The unlock-less Makon NT "new" carts: same split-window board, but the
        // boot arms it with the bare $1400 <- $55 write (no $7000/$B200/$B600
        // strobe) and computes the $55 at run time, so neither the unlock scan
        // above nor a bare-arm byte scan can see them. Their shared $0184 Makon
        // logos are NOT a safe gate (a sibling that hangs under the board carries
        // one of the same four logos), so this keys on the exact whole-ROM CRC32
        // of the eight verified dumps. See `NTNEW_MAKON_ROM_CRC32`.
        if super::NTNEW_MAKON_ROM_CRC32.contains(&rom_crc32) {
            return UnlMapper::NtNew(NtNewState::default());
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

        // Vast Fame "operand adder" protection board (Dragon Ball - Final
        // Bout). The image is a decrypted BBD-family dump, so the BBD arm above
        // correctly declines it ($7FFF==$01), but its protection chip is still
        // live: detection keys on the CRC32 of the 40-byte protection thunk at
        // $3F50, which is protection-specific code no other cart carries, plus
        // the MBC5-family header the board really is. See `UnlMapper::VfAdder`.
        if data.len() > super::VFADDER_STUB_OFFSET + super::VFADDER_STUB_LEN
            && crate::checksum::crc32(
                &data[super::VFADDER_STUB_OFFSET
                    ..super::VFADDER_STUB_OFFSET + super::VFADDER_STUB_LEN],
            ) == super::VFADDER_STUB_CRC32
            && matches!(data[CARTRIDGE_TYPE_OFFSET], 0x19..=0x1E)
        {
            return UnlMapper::VfAdder(VfAdderState::default());
        }

        // VF001 protection board behind a GGB81 secondary logo: two dumps whose
        // boot speaks the VF001 $7000 config protocol, keyed on the exact
        // whole-ROM CRC32 because both the $0184 logo CRC32 and the title are
        // shared with GGB81 carts that must keep the GGB81 board (see
        // `VF001G_OVER_GGB81_ROM_CRC32`). Ordered before the GGB81 arm below.
        if super::VF001G_OVER_GGB81_ROM_CRC32.contains(&rom_crc32) {
            return UnlMapper::Vf001Gen(Box::default());
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
        // pair) all declare a truthful MBC5 header ($19-$1E). Zook Z shares the
        // byte-identical "V.fame" logo behind a spoofed MBC1 header ($01) but
        // speaks the *challenge-response* dialect of the board, handled by the
        // `Vf001Zook` arm below.
        // NT "new" split-window board (taizou's hhugboy `MbcUnlNtNew`, CC0),
        // which hhugboy only offers as a manual menu pick. Keyed on the CRC32
        // of the board driver the cart runs out of the interrupt-vector dead
        // space (see `NTNEW_STUB_CRC32` for why the arming write alone is not a
        // safe gate), the MBC5-family header the board declares truthfully, and
        // a self-consistent image: the board's page numbers address 8 KiB
        // windows of the real ROM, so an image whose size disagrees with its own
        // header cannot be mapped through it. That guard also excludes the one
        // other image carrying this driver -- Yingxiong Tianxia, a 2x-inflated
        // dump (128 banks, 64 distinct, every odd bank a byte-copy of the even
        // one before it, declaring 1 MiB in a 2 MiB file) which was verified NOT
        // to run under the board, de-duplicated or not.
        if data.len() > super::NTNEW_STUB_OFFSET + super::NTNEW_STUB_LEN
            && crate::checksum::crc32(
                &data[super::NTNEW_STUB_OFFSET..super::NTNEW_STUB_OFFSET + super::NTNEW_STUB_LEN],
            ) == super::NTNEW_STUB_CRC32
            && matches!(data[CARTRIDGE_TYPE_OFFSET], 0x19..=0x1E)
            && data.len() == claimed_size
        {
            return UnlMapper::NtNew(NtNewState::default());
        }

        // Gedou Jian Shen KF: same board and same "SOUL" $0184 logo as the Soul
        // Falchion trio, but wired for the VF001A config seed ($10). Keyed on the
        // exact dump because the logo CRC32 and the title are shared with the
        // three Soul Falchion images, which must keep the $00 seed. Ordered
        // before the logo arm below, which would otherwise claim it.
        if rom_crc32 == super::VF001A_SEED_OVER_SOUL_ROM_CRC32 {
            return UnlMapper::Vf001Gen(Box::new(Vf001gState {
                config_seed: super::VF001A_CONFIG_SEED,
                ..Default::default()
            }));
        }

        if super::VF001G_LOGO_CRC32.contains(&logo_crc32)
            && matches!(data[CARTRIDGE_TYPE_OFFSET], 0x19..=0x1E)
        {
            return UnlMapper::Vf001Gen(Box::default());
        }

        // Same board, "VF001A" wiring (Sanguozhi - Aoshi Tianxia): the config
        // accumulator restarts from $10 instead of $00. This image carries no
        // "V.fame" logo -- its $0184 is the `jp $0200` entry trampoline -- so
        // the arm above cannot see it; detection keys on the CRC32 of the
        // 46-byte config driver at $0257, which is protection-specific code no
        // other cart in the library carries, plus the MBC5-family header the
        // board truthfully declares. Without the $10 seed the cart's config
        // stream decodes to a zero-length injection and a disabled bank-0
        // replacement, and the boot dead-ends at $0121 with the LCD off.
        if data.len() > super::VF001A_STUB_OFFSET + super::VF001A_STUB_LEN
            && crate::checksum::crc32(
                &data[super::VF001A_STUB_OFFSET
                    ..super::VF001A_STUB_OFFSET + super::VF001A_STUB_LEN],
            ) == super::VF001A_STUB_CRC32
            && matches!(data[CARTRIDGE_TYPE_OFFSET], 0x19..=0x1E)
        {
            return UnlMapper::Vf001Gen(Box::new(Vf001gState {
                config_seed: super::VF001A_CONFIG_SEED,
                ..Default::default()
            }));
        }

        // Vast Fame VF001 challenge-response dialect (Zook Z). Same "V.fame"
        // $0184 logo as the Vf001Gen arm above, but the cart drives the board
        // through the two protection thunks at $3ED9/$3EF5 -- `ld hl,$7081`,
        // four `ld (hl),a` stream writes, then `ld a,($7FFF)` to read back the
        // bank the board selected -- instead of hhugboy's $7000 config-register
        // protocol. Gated on the 48-byte logo CRC32 *and* that thunk's exact
        // opcode bytes, so no licensed cart and no other VF001 cart can match.
        if super::VF001G_LOGO_CRC32.contains(&logo_crc32)
            && data.len() > super::VF001Z_THUNK_OFFSET + super::VF001Z_THUNK.len()
            && data[super::VF001Z_THUNK_OFFSET
                ..super::VF001Z_THUNK_OFFSET + super::VF001Z_THUNK.len()]
                == super::VF001Z_THUNK
        {
            return UnlMapper::Vf001Zook(Box::default());
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

        // Gowin "Story of Lasama" (GS-04) protection board. hhugboy has no
        // auto-detect for the raw cart, so this keys on the CRC32 of the 48
        // bytes at $0184 (executable code on this cart, so a plain sum is
        // unsafe) AND the exact 30-byte boot-protection stub at $02D7 that
        // drives the $6000 outer-bank handshake. Both together make a licensed-
        // or wrong-cart match impossible. Electrically MBC1.
        if crate::checksum::crc32(&data[0x184..0x1B4]) == GOWIN_LASAMA_LOGO_CRC32
            && data.len() > GOWIN_STUB_OFFSET + GOWIN_STUB.len()
            && data[GOWIN_STUB_OFFSET..GOWIN_STUB_OFFSET + GOWIN_STUB.len()] == GOWIN_STUB
        {
            return UnlMapper::Gowin(GowinState::default());
        }

        // Datel "Action Replay V4" cheat device (GameShark Online, Action
        // Replay Online, Action Replay Xtreme). Keyed on the CRC32 of the 48
        // bytes at $0104: on a pass-through cart the boot ROM's logo check is
        // satisfied by the game in the slot, so Datel reused the logo block for
        // ASCII menu strings. No licensed cart can hold anything but the logo
        // there, and the header-type gate keeps this to bankless headers.
        if crate::checksum::crc32(&data[0x104..0x134]) == super::ARV4_HEADER_CRC32
            && data[CARTRIDGE_TYPE_OFFSET] == 0x00
        {
            return UnlMapper::ActionReplayV4(ArV4State::default());
        }

        // Future Console Design "Xploder GB". Same argument as the Action
        // Replay above: a pass-through cheat cart carries no logo of its own,
        // so the 48 bytes at $0104 hold an ASCII credit line instead. The
        // header type gate is the board's garbage $69 (nothing in the Pan Docs
        // table), which no licensed cart can have.
        if crate::checksum::crc32(&data[0x104..0x134]) == super::XPLODER_HEADER_CRC32
            && data[CARTRIDGE_TYPE_OFFSET] == 0x69
        {
            return UnlMapper::XploderGb(XploderState::default());
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
            st.running_value = st.config_seed;
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

    /// Zook Z protection-port write ($6000-$7FFF). The port behaves as a shift
    /// register: every byte written anywhere in the range shifts into a rolling
    /// window, and nothing ever clears it. In particular the $31 the cart writes
    /// to close each sequence is just another byte -- it cannot be a reset,
    /// because three of the cart's own bank-select challenges contain $31 inside
    /// their key bytes ($0A,$31,$18 / $0C,$31,$10 / $30,$93,$31) and those
    /// selects do work on hardware.
    ///
    /// A bank select is four consecutive writes to $7081 -- exactly what the
    /// cart's two thunks emit (`ld hl,$7081` / `ld de,$7081`, four stores, then
    /// `ld a,($7FFF)` to read the resulting bank back). That framing is
    /// load-bearing: 17 byte triples inside the longer challenge streams are
    /// also valid bank keys, so matching an unframed sliding window would switch
    /// banks in the middle of a challenge.
    pub(super) fn vf001z_write(&mut self, addr: u16, value: u8) {
        let UnlMapper::Vf001Zook(ref mut st) = self.unl_mapper else {
            return;
        };
        let len = usize::from(st.len);
        let len = if len == VF001Z_STREAM_MAX {
            st.stream.copy_within(1.., 0);
            len - 1
        } else {
            len
        };
        st.stream[len] = value;
        st.len = (len + 1) as u8;

        if addr != VF001Z_BANK_PORT {
            st.port_run = 0;
            return;
        }
        st.port_run += 1;
        if st.port_run < 4 {
            return;
        }
        st.port_run = 0;
        // The newest byte sits at `len`; the key is the first three of the four.
        let bank = Self::vf001z_bank_response(&st.stream[len - 3..len]);
        // The payload borrow ends here, so the bank register can be driven.
        let Some(bank) = bank else { return };
        if let Mapper::Vf001(m) = &mut self.mapper {
            m.regs.rom_bank_low = bank;
            m.regs.rom_bank_high = 0;
        }
    }

    /// Zook Z protection readback ($A000-$BFFF). Answers with the response the
    /// board gives for the longest known challenge that the port's shift window
    /// currently ends in; `None` (falling through to normal cart RAM) when the
    /// window ends in no known challenge.
    pub(super) fn vf001z_read(st: &Vf001zState, addr: u16) -> Option<u8> {
        let window = &st.stream[..usize::from(st.len)];
        let reg = u8::try_from((addr >> 8) & 0x0F).unwrap_or(0);
        VF001Z_CHALLENGE_RESPONSES
            .iter()
            .filter(|(r, s, _)| *r == reg && window.ends_with(s))
            .max_by_key(|(_, s, _)| s.len())
            .map(|(_, _, v)| *v)
    }

    /// Binary-search `VF001Z_BANK_RESPONSES` for a three-byte bank-select
    /// challenge.
    fn vf001z_bank_response(key: &[u8]) -> Option<u8> {
        VF001Z_BANK_RESPONSES
            .binary_search_by(|e| e[..3].cmp(key))
            .ok()
            .map(|i| VF001Z_BANK_RESPONSES[i][3])
    }

    /// Xploder GB cart-window write ($0000-$7FFF). Only the two bank registers
    /// in the first bytes of the window do anything; the rest of the board's
    /// register file has no effect this model needs, and the ROM behind them is
    /// not writable.
    pub(super) fn xploder_write(&mut self, addr: u16, value: u8) {
        if let UnlMapper::XploderGb(ref mut st) = self.unl_mapper {
            match addr {
                XPLODER_REG_ROM_BANK => st.rom_bank = value,
                XPLODER_REG_RAM_BANK => st.ram_bank = value,
                _ => {}
            }
        }
    }

    /// Byte index into `ram_data` for an Xploder GB cart-RAM access, banked by
    /// its $0007 register. `None` when the array is empty.
    pub(super) fn xploder_ram_offset(&self, addr: u16) -> Option<usize> {
        if self.ram_data.is_empty() {
            return None;
        }
        let UnlMapper::XploderGb(st) = &self.unl_mapper else { return None };
        let base = (st.ram_bank as usize % XPLODER_RAM_BANKS) * RAM_BANK_SIZE;
        Some((base + (addr as usize - 0xA000)) % self.ram_data.len())
    }

    /// Xploder GB read for the switchable ROM window and the cart-RAM window.
    /// `None` for anything else so the caller falls through.
    pub(super) fn xploder_read(&self, st: XploderState, addr: u16) -> Option<u8> {
        if (0x4000..0x8000).contains(&addr) {
            let banks = (self.rom_data.len() / 0x4000).max(1);
            let base = (st.rom_bank as usize % banks) * 0x4000;
            return Some(self.rom_byte(base + (addr as usize - 0x4000)));
        }
        if (EXTERNAL_RAM_START..=EXTERNAL_RAM_END).contains(&addr) {
            return Some(self.xploder_ram_offset(addr).map_or(0xFF, |o| self.ram_data[o]));
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

    /// NT "new" board register write (taizou's hhugboy `MbcUnlNtNew`, CC0).
    /// Only reached for the writes `NtNewState::claims` accepts: $1400 with the
    /// magic $55 arms the split window, and once armed $2000/$2400 program the
    /// two 8 KiB pages. Every other write in the block goes to the MBC5 under
    /// the board.
    pub(super) fn ntnew_write(&mut self, addr: u16, value: u8) {
        if let UnlMapper::NtNew(ref mut st) = self.unl_mapper {
            match addr & 0xFF00 {
                super::NTNEW_ARM_PORT => st.split = true,
                super::NTNEW_LOW_PORT => st.low = value,
                _ => st.high = value,
            }
        }
    }

    /// NT "new" split-window ROM read ($4000-$7FFF), only reached while the
    /// board is armed. Each half of the window is an independent 8 KiB page:
    /// the page number is taken at 8 KiB granularity, wrapped to the ROM, and —
    /// mirroring MBC5's "bank 0 reads as bank 1" — a result that lands inside
    /// the first 16 KiB is pushed up by 16 KiB, so pages 0 and 1 present pages
    /// 2 and 3.
    pub(super) fn ntnew_read(&self, st: NtNewState, addr: u16) -> u8 {
        let page = if addr < 0x6000 { st.low } else { st.high };
        let pages = (self.rom_data.len() / NTNEW_PAGE).max(1);
        let mut base = (page as usize % pages) * NTNEW_PAGE;
        if base < 0x4000 {
            base += 0x4000;
        }
        self.rom_data.get(base + (addr as usize & (NTNEW_PAGE - 1))).copied().unwrap_or(0xFF)
    }

    /// Action Replay V4 cart-window write ($0000-$7FFF). $6000-$7FFF is the
    /// board's SRAM; the two bank registers sit at the top of that window and
    /// are written through to SRAM as well, so the firmware's occasional
    /// read-back of $7FE2 (a register this model does not implement) still
    /// returns what it last wrote. Everything below $6000 is ROM with no MBC
    /// behind it, so those writes are dropped.
    pub(super) fn arv4_write(&mut self, addr: u16, value: u8) {
        if addr < 0x6000 {
            return;
        }
        if (ARV4_REG_START..=ARV4_REG_END).contains(&addr) {
            if addr == ARV4_REG_ROM_PAGE
                && let UnlMapper::ActionReplayV4(ref mut st) = self.unl_mapper
            {
                st.rom_page = value;
            }
            return;
        }
        if let Some(offset) = self.arv4_ram_offset(addr) {
            self.ram_data[offset] = value;
        }
    }

    /// Byte index into `ram_data` for an Action Replay V4 access in its
    /// $6000-$7FFF SRAM window. `None` when the array is empty.
    fn arv4_ram_offset(&self, addr: u16) -> Option<usize> {
        if self.ram_data.is_empty() {
            return None;
        }
        Some((addr as usize - 0x6000) % self.ram_data.len())
    }

    /// Action Replay V4 switchable-window read ($4000-$7FFF): an 8 KiB ROM
    /// page selected by $7FE1 in the low half, the SRAM bank selected by $7FE0
    /// in the high half. Out-of-range pages wrap to the image like every other
    /// board here.
    pub(super) fn arv4_read(&self, st: ArV4State, addr: u16) -> u8 {
        if (ARV4_REG_START..=ARV4_REG_END).contains(&addr) {
            return 0x00;
        }
        if addr >= 0x6000 {
            return self.arv4_ram_offset(addr).map_or(0xFF, |o| self.ram_data[o]);
        }
        let pages = (self.rom_data.len() / ARV4_PAGE).max(1);
        let base = (st.rom_page as usize % pages) * ARV4_PAGE;
        self.rom_data.get(base + (addr as usize & (ARV4_PAGE - 1))).copied().unwrap_or(0xFF)
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

    /// Is the adder protection engaged? The board's enable is the MBC5 ROM-bank
    /// register holding a bank the cart does not physically have — the thunk
    /// parks it at $C0/$80 on a 64-bank (1 MiB) image. The same "out-of-ROM bank
    /// number means protection chip, not ROM" convention as the "New GB Color"
    /// HK PCB, and it is what makes the window unambiguous: every ordinary bank
    /// the game selects is in range, so normal ROM reads and RAM-bank writes are
    /// untouched.
    pub(super) fn vfadder_armed(&self) -> bool {
        let Mapper::Mbc5(m) = &self.mapper else { return false };
        let bank = usize::from(m.regs.rom_bank_low) | (usize::from(m.regs.rom_bank_high) << 8);
        bank >= self.rom_banks.max(1)
    }

    /// Adder protection readback for $4000-$5FFF. The board presents bits 8..1
    /// of `X + (Y << 1)`, i.e. `(X >> 1) + Y`. `None` when the protection is not
    /// engaged, so the caller falls through to a normal ROM read.
    pub(super) fn vfadder_read(&self, st: VfAdderState, addr: u16) -> Option<u8> {
        (addr < 0x6000 && self.vfadder_armed())
            .then(|| (st.x >> 1).wrapping_add(st.y))
    }

    /// PKJD ("Pokemon Jade / Diamond") protection read for $A000-$BFFF, a port
    /// of taizou's hhugboy `MbcUnlPokeJadeDia::readMemory`. Returns the derived
    /// value when the active selector addresses a protection register (or an
    /// unpopulated RTC register), else `None` so the caller falls through to a
    /// normal MBC3 SRAM read. The board is electrically MBC3, so RAM-enable
    /// lives on the `Mapper::Mbc3` board and the D/E state in the UnlMapper
    /// payload.
    pub(super) fn pokejade_read(&self, _addr: u16) -> Option<u8> {
        let ram_enabled = matches!(&self.mapper, Mapper::Mbc3(m) if m.ram_enabled);
        let UnlMapper::PokeJadeDia(st) = &self.unl_mapper else {
            return None;
        };
        if !ram_enabled {
            // RAM not enabled: the window reads back open bus.
            return Some(0xFF);
        }
        match st.sel {
            0x0D => Some(st.reg_d),
            0x0E => Some(st.reg_e),
            0x0F => Some(0), // F is write-only
            // The real RTC registers are unpopulated on this cart, so they read
            // back 0 (hhugboy: `return 0` for selects $08-$0C when !RTC).
            0x08..=0x0C => Some(0),
            // Any other selector ($00-$07) is a plain MBC3 SRAM bank.
            _ => None,
        }
    }

    /// PKJD protection write for $A000-$BFFF, a port of taizou's hhugboy
    /// `MbcUnlPokeJadeDia::writeMemory`. Writes are ignored while RAM is
    /// disabled; selector $0D/$0E set registers D/E, $0F is the command port
    /// that mutates D and E, and every other selector is an ordinary MBC3
    /// SRAM/RTC write.
    pub(super) fn pokejade_write(&mut self, addr: u16, value: u8) {
        let (ram_enabled, ram_bank) = match &self.mapper {
            Mapper::Mbc3(m) => (m.ram_enabled, m.ram_bank),
            _ => return,
        };
        if !ram_enabled {
            return;
        }
        let sel = match &self.unl_mapper {
            UnlMapper::PokeJadeDia(st) => st.sel,
            _ => return,
        };
        match sel {
            0x0D => {
                if let UnlMapper::PokeJadeDia(ref mut st) = self.unl_mapper {
                    st.reg_d = value;
                }
            }
            0x0E => {
                if let UnlMapper::PokeJadeDia(ref mut st) = self.unl_mapper {
                    st.reg_e = value;
                }
            }
            0x0F => {
                // The weak protection scheme: certain writes to F manipulate D
                // and E. Copied byte-for-byte from hhugboy (note $52 decrements
                // E, matching the reference).
                if let UnlMapper::PokeJadeDia(ref mut st) = self.unl_mapper {
                    match value {
                        0x11 => st.reg_d = st.reg_d.wrapping_sub(1),
                        0x12 => st.reg_e = st.reg_e.wrapping_sub(1),
                        0x41 => st.reg_d = st.reg_d.wrapping_add(st.reg_e),
                        0x42 => st.reg_e = st.reg_e.wrapping_add(st.reg_d),
                        0x51 => st.reg_d = st.reg_d.wrapping_add(1),
                        0x52 => st.reg_e = st.reg_e.wrapping_sub(1),
                        _ => {}
                    }
                }
            }
            // Ordinary MBC3 SRAM / RTC write (selector = the RAM/RTC-bank).
            _ => {
                let ram_select_max = if self.is_mbc30() { 0x07 } else { 0x03 };
                if ram_bank <= ram_select_max {
                    if let Some(offset) = self.banked_ram_offset(addr) {
                        let _ = self.write_ram_byte(offset, value);
                    }
                } else if (0x08..=0x0C).contains(&ram_bank) {
                    self.write_rtc_register(ram_bank, value);
                }
            }
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
    /// Gowin ($6000-$7FFF) outer-bank handshake. The port takes a two-write
    /// command: the first write latches a parameter byte, the second (a commit
    /// strobe, whose value is not otherwise used) sets the outer ROM base to
    /// `parameter << 1` 16 KiB banks — a 32 KiB-granular offset applied to both
    /// the fixed and switchable ROM windows (see `rom_bank_bases`). The one
    /// observed transaction is $02 then $BE, selecting bank 4 (the game half of
    /// the 128 KiB dump); the decoy bank 0 is left mapped until then. The base
    /// is masked to the ROM size so it can never point past the image.
    pub(super) fn gowin_write(&mut self, value: u8) {
        let rom_banks = self.rom_banks.max(1);
        if let UnlMapper::Gowin(ref mut st) = self.unl_mapper {
            match st.pending.take() {
                None => st.pending = Some(value),
                Some(param) => st.base = ((usize::from(param) << 1) % rom_banks) as u8,
            }
        }
    }
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
        if self.outer_open_bus(g) {
            return g.rom_banks; // out-of-ROM outer bank -> open bus (0xFF)
        }
        (((self.state.bank & !self.state.mask) | (self.state.base & self.state.mask)) as usize)
            % g.rom_banks
    }
    fn rom_bank0(&self, g: Geom) -> usize {
        if self.outer_open_bus(g) {
            return g.rom_banks; // out-of-ROM outer bank -> open bus (0xFF)
        }
        ((self.state.base & self.state.mask) as usize) % g.rom_banks
    }
    fn ram_bank(&self, _g: Geom) -> usize {
        0
    }
}

impl Sachen {
    /// The multicart outer bank (`base & mask`) addresses ROM beyond the
    /// physical chip. On the real board the solder pads for a cart's ROM size
    /// leave those upper outer-address lines open, so the whole window reads
    /// back as open bus ($FF) rather than wrapping (hhugboy `MbcUnlSachenMMC2`'s
    /// `mbcConfig` open-bus check, derived here from the physical ROM size).
    /// The 4-in-1 menu's size probe depends on this: it reads $0143 under outer
    /// banks $00/$20/$40 and loops forever unless the two out-of-ROM banks read
    /// back different (open-bus) bytes than the in-ROM one. Returning a bank
    /// index of `rom_banks` lands the read past the ROM image, where the read
    /// path already yields $FF.
    fn outer_open_bus(&self, g: Geom) -> bool {
        (self.state.base & self.state.mask) as usize >= g.rom_banks
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

// ---------------------------------------------------------------------------
// Vast Fame VF001, challenge-response dialect (Zook Z)
// ---------------------------------------------------------------------------

/// The one port a bank-select transaction is issued to.
const VF001Z_BANK_PORT: u16 = 0x7081;

/// Observed bank-select responses of the Vast Fame VF001 board in Zook Z, one
/// `[byte0, byte1, byte2, bank]` per entry, sorted for binary search.
///
/// PROVENANCE: these are OBSERVED responses, not a derived chip model. Rockman
/// DX8 (China) (En) (Unl) is a 99.7%-byte-identical, de-protected build of the
/// same game: every one of Zook Z's 97 inline `rst $20` sites and 131 entries
/// across its ten `rst $30` bank tables is replaced there by a plain
/// `ld a,bank / call $3EF1` (`ld ($2000),a`), which names the bank the board
/// must select for that challenge. Diffing the two images therefore yields the
/// full response set with no guesswork -- and, because every challenge in the
/// cart is static ROM data, the set is complete for this game.
///
/// The board's actual decode function is NOT solved. The fourth byte of each
/// challenge is provably ignored (13 sites program the same first three bytes
/// with a different fourth and get the same bank), so the key is three bytes
/// wide; beyond that the following model classes were each exhaustively
/// eliminated against all 228 observations: affine GF(2) over the input bits
/// (Gaussian elimination -- inconsistent for every output bit), every linear
/// combination mod 256 with an arbitrary output table (all 2^24), all 65536
/// CRC-16 polynomials x seeds x byte orders x output extractions, every
/// multiply-accumulate `s = s*k op b` (2^16 parameterisations), rotate/xor
/// folds, and multiply-shift hashes. A minimum hitting-set analysis shows the
/// function must mix at least 11 of the 24 key bits, ruling out a small
/// bit-selection circuit. Replace this table with the closed form if it is
/// ever recovered.
const VF001Z_BANK_RESPONSES: [[u8; 4]; 213] = [
    [0x00, 0x1B, 0x0F, 0x17], [0x00, 0x50, 0x06, 0x30], [0x01, 0x1A, 0x11, 0x14], [0x04, 0x1F, 0x30, 0x13],
    [0x04, 0x32, 0x09, 0x2A], [0x04, 0x32, 0x0C, 0x2D], [0x0A, 0x31, 0x18, 0x0B], [0x0B, 0x15, 0x39, 0x07],
    [0x0C, 0x31, 0x10, 0x0C], [0x0F, 0x32, 0x1E, 0x0B], [0x10, 0x24, 0x10, 0x09], [0x10, 0x24, 0x19, 0x0C],
    [0x10, 0x24, 0x26, 0x06], [0x10, 0x4B, 0x10, 0x12], [0x10, 0x4B, 0x15, 0x11], [0x10, 0x4B, 0x1E, 0x1C],
    [0x11, 0x00, 0xA0, 0x39], [0x12, 0x51, 0x2B, 0x24], [0x13, 0x50, 0x1C, 0x2B], [0x14, 0x20, 0x30, 0x02],
    [0x16, 0x22, 0x16, 0x0A], [0x18, 0x43, 0x1C, 0x1E], [0x19, 0x22, 0x1E, 0x08], [0x19, 0xFA, 0x99, 0x3D],
    [0x1A, 0x21, 0x1B, 0x05], [0x1A, 0x51, 0x2E, 0x28], [0x1C, 0x21, 0x2E, 0x01], [0x1C, 0x50, 0x2F, 0x2B],
    [0x1C, 0x7A, 0x2C, 0x34], [0x1D, 0x46, 0x3C, 0x17], [0x1F, 0x44, 0x3E, 0x18], [0x20, 0x70, 0x43, 0x31],
    [0x21, 0x3A, 0x29, 0x19], [0x22, 0x3C, 0x30, 0x03], [0x22, 0x3C, 0x4D, 0x01], [0x22, 0x64, 0x25, 0x1B],
    [0x23, 0x3D, 0x4D, 0x01], [0x24, 0x3F, 0x2F, 0x14], [0x24, 0x62, 0x27, 0x1D], [0x26, 0x50, 0x51, 0x27],
    [0x27, 0x39, 0x4D, 0x01], [0x28, 0x78, 0x2C, 0x30], [0x2A, 0x5C, 0x3B, 0x25], [0x2B, 0x35, 0x3E, 0x05],
    [0x2B, 0x6D, 0x38, 0x12], [0x2C, 0x5A, 0x2D, 0x29], [0x2C, 0x6A, 0x33, 0x10], [0x30, 0x44, 0x34, 0x0B],
    [0x30, 0x6B, 0x36, 0x1C], [0x30, 0x6B, 0x49, 0x16], [0x30, 0x93, 0x31, 0x36], [0x32, 0x71, 0x32, 0x29],
    [0x34, 0x40, 0x5A, 0x03], [0x38, 0x63, 0x39, 0x1A], [0x38, 0x63, 0x3E, 0x18], [0x3A, 0x41, 0x3F, 0x02],
    [0x3C, 0x47, 0x5E, 0x08], [0x3E, 0x65, 0x49, 0x16], [0x3F, 0x42, 0x52, 0x03], [0x40, 0x5E, 0x55, 0x02],
    [0x40, 0x76, 0x55, 0x2B], [0x42, 0x84, 0x47, 0x18], [0x43, 0x5D, 0x4A, 0x07], [0x44, 0x5F, 0x57, 0x19],
    [0x46, 0x58, 0x54, 0x04], [0x46, 0x70, 0x48, 0x22], [0x47, 0x71, 0x4B, 0x25], [0x48, 0x7E, 0x4C, 0x2D],
    [0x48, 0x8E, 0x4C, 0x14], [0x48, 0x8E, 0x56, 0x1D], [0x4B, 0x55, 0x54, 0x04], [0x4C, 0x8A, 0x50, 0x10],
    [0x4D, 0x53, 0x51, 0x06], [0x4D, 0x8B, 0x4E, 0x18], [0x4E, 0x50, 0x50, 0x01], [0x50, 0x8B, 0x67, 0x10],
    [0x52, 0x91, 0x59, 0x28], [0x54, 0x60, 0x59, 0x0C], [0x57, 0x94, 0x61, 0x2A], [0x58, 0x63, 0x5F, 0x09],
    [0x5A, 0x61, 0x62, 0x03], [0x5A, 0x61, 0x70, 0x02], [0x5A, 0x8A, 0x63, 0x30], [0x5C, 0x61, 0x5D, 0x05],
    [0x5D, 0x86, 0x6F, 0x14], [0x5E, 0x92, 0x6B, 0x27], [0x60, 0xA6, 0x6B, 0x1C], [0x63, 0x7D, 0x65, 0x06],
    [0x64, 0x92, 0x66, 0x2D], [0x67, 0xA1, 0x74, 0x19], [0x68, 0x76, 0x7E, 0x05], [0x68, 0xAE, 0x6C, 0x1E],
    [0x68, 0xB8, 0x77, 0x37], [0x6C, 0x91, 0x71, 0x09], [0x6C, 0x9A, 0x72, 0x22], [0x6C, 0x9A, 0x89, 0x2A],
    [0x6C, 0x9A, 0x8C, 0x2D], [0x6C, 0xAA, 0x6E, 0x14], [0x6D, 0xAB, 0x73, 0x10], [0x6F, 0xFA, 0x8B, 0x0F],
    [0x70, 0xB3, 0x7C, 0x2A], [0x72, 0x86, 0x77, 0x02], [0x72, 0x86, 0x7D, 0x04], [0x73, 0xB0, 0x7B, 0x25],
    [0x76, 0x82, 0x76, 0x06], [0x76, 0xB5, 0x78, 0x26], [0x78, 0x83, 0x84, 0x0A], [0x78, 0x8C, 0xA3, 0x0F],
    [0x78, 0xA3, 0x82, 0x13], [0x78, 0xB3, 0x95, 0x2F], [0x7A, 0xA1, 0x88, 0x11], [0x7C, 0x87, 0x7D, 0x09],
    [0x7D, 0x89, 0x82, 0x0C], [0x7E, 0x83, 0x7E, 0x02], [0x7E, 0x83, 0x80, 0x05], [0x7E, 0x85, 0x80, 0x06],
    [0x81, 0x9F, 0x95, 0x02], [0x88, 0xCE, 0x8E, 0x18], [0x8B, 0xD1, 0xCE, 0x27], [0x8C, 0x92, 0x8D, 0x01],
    [0x8C, 0xBA, 0x8F, 0x2E], [0x8C, 0xCA, 0xAC, 0x1E], [0x90, 0xCB, 0x91, 0x14], [0x90, 0xD3, 0x93, 0x2D],
    [0x91, 0xCA, 0xBC, 0x17], [0x95, 0xCE, 0x97, 0x19], [0x96, 0xCD, 0xA4, 0x1A], [0x96, 0xD5, 0x9F, 0x2D],
    [0x98, 0xC3, 0x99, 0x16], [0x98, 0xC3, 0xA1, 0x1D], [0x9A, 0xD1, 0xAB, 0x25], [0x9A, 0xFF, 0x77, 0x06],
    [0x9B, 0xD0, 0xAF, 0x29], [0x9C, 0xD0, 0xA8, 0x29], [0x9C, 0xFA, 0xBE, 0x33], [0x9D, 0xC6, 0xA1, 0x1D],
    [0xA0, 0xD6, 0xAD, 0x29], [0xA1, 0xBA, 0xB0, 0x13], [0xA4, 0xBA, 0xA8, 0x0C], [0xA4, 0xBA, 0xD5, 0x02],
    [0xA7, 0xB9, 0xAC, 0x09], [0xA9, 0xDF, 0xBC, 0x24], [0xAA, 0xDC, 0xB6, 0x2B], [0xAA, 0xDC, 0xE1, 0x2F],
    [0xAB, 0xFA, 0xAF, 0x32], [0xAC, 0xB2, 0xCC, 0x06], [0xAC, 0xEA, 0xC8, 0x11], [0xAF, 0xE9, 0xB3, 0x10],
    [0xB0, 0xC4, 0xBA, 0x01], [0xB0, 0xEB, 0xB8, 0x1B], [0xB0, 0xF3, 0xBA, 0x2E], [0xB4, 0xF7, 0xB5, 0x2C],
    [0xB8, 0xE3, 0xBC, 0x17], [0xB8, 0xE3, 0xBF, 0x19], [0xBC, 0xE7, 0xC2, 0x13], [0xBE, 0xF2, 0xD4, 0x23],
    [0xC0, 0xDB, 0xC2, 0x13], [0xC0, 0xDE, 0xD4, 0x04], [0xC0, 0xF6, 0xC4, 0x29], [0xC1, 0xF7, 0xCB, 0x25],
    [0xC3, 0xDD, 0xCF, 0x0C], [0xC6, 0x01, 0xEA, 0x2C], [0xC7, 0xD9, 0xCD, 0x01], [0xC8, 0x0E, 0xD3, 0x1A],
    [0xC8, 0x0E, 0xD7, 0x1C], [0xC8, 0x18, 0xDD, 0x35], [0xC8, 0xFE, 0xCD, 0x26], [0xCB, 0xFD, 0xD0, 0x2A],
    [0xCC, 0x0A, 0xD5, 0x11], [0xCC, 0xD2, 0xD3, 0x05], [0xCC, 0xFA, 0xD5, 0x2B], [0xCC, 0xFA, 0xDA, 0x24],
    [0xD0, 0x0B, 0xD3, 0x18], [0xD1, 0xA7, 0xC0, 0x2C], [0xD1, 0xFE, 0x10, 0x3A], [0xD3, 0x10, 0xE9, 0x2E],
    [0xD3, 0xE7, 0xE9, 0x08], [0xD4, 0x17, 0xDC, 0x2B], [0xD4, 0x17, 0xE9, 0x2E], [0xD4, 0xE0, 0xE7, 0x01],
    [0xD5, 0x0E, 0xD9, 0x16], [0xD6, 0x0D, 0xE7, 0x10], [0xD8, 0xE3, 0xE9, 0x06], [0xD9, 0x02, 0xDA, 0x18],
    [0xDB, 0x00, 0xDD, 0x19], [0xDB, 0xE0, 0xE2, 0x03], [0xDC, 0x07, 0xFC, 0x17], [0xDC, 0x0C, 0xE3, 0x30],
    [0xDE, 0x05, 0x08, 0x11], [0xE2, 0xFC, 0x15, 0x02], [0xE5, 0xFB, 0xE6, 0x05], [0xE6, 0x10, 0xE7, 0x2C],
    [0xE7, 0xFC, 0xF0, 0x13], [0xE8, 0x2E, 0xF1, 0x16], [0xE8, 0x2E, 0xFB, 0x15], [0xEC, 0x11, 0xF1, 0x09],
    [0xEC, 0xF2, 0x0C, 0x06], [0xEF, 0x19, 0xFC, 0x24], [0xF2, 0x06, 0xF9, 0x05], [0xF4, 0x12, 0xFB, 0x38],
    [0xF6, 0x02, 0xF7, 0x02], [0xF6, 0x02, 0xFC, 0x08], [0xF6, 0x10, 0x13, 0x30], [0xF8, 0x03, 0xFB, 0x0B],
    [0xFB, 0x00, 0x28, 0x01], [0xFB, 0x20, 0x1C, 0x1E], [0xFC, 0x01, 0x12, 0x03], [0xFC, 0x01, 0x2E, 0x01],
    [0xFC, 0x0A, 0x09, 0x2A], [0xFC, 0x1A, 0x28, 0x31], [0xFC, 0x27, 0xFE, 0x18], [0xFE, 0x25, 0xFF, 0x19],
    [0xFF, 0x02, 0xFF, 0x06],
];

/// Observed challenge-response answers of the Vast Fame VF001 board in Zook Z:
/// `(readback register, byte stream, value)`. The register is the low nibble of
/// the high address byte of the $A000-$BFFF read the cart uses ($A080 -> 0,
/// $A180 -> 1, ... $A880 -> 8); the stream is every byte written to the
/// protection port since the last $31 close.
///
/// PROVENANCE: as for `VF001Z_BANK_RESPONSES` -- observed, not derived. Each of
/// Zook Z's 56 protection readbacks is de-protected in Rockman DX8 into a
/// literal `ld a,value` followed by the same arithmetic and a jump past the
/// sequence, so the value the board must return is stated outright; the
/// `cp n`-style sites additionally carry it in Zook Z's own compare immediate.
/// The streams are the static bytes the cart programs at each site. NOT a chip
/// model -- replace when the board's response function is solved.
const VF001Z_CHALLENGE_RESPONSES: &[(u8, &[u8], u8)] = &[
    (0, &[0x04, 0x0A, 0x76, 0xC4, 0x70, 0x66, 0x20, 0xB0, 0x7C], 0x52),
    (0, &[0x10, 0xA0, 0x83, 0x65, 0x77, 0x33, 0x23, 0x9E, 0x21], 0xF3),
    (0, &[0x11, 0x81, 0x70, 0xF7, 0x2A, 0x66, 0x6F, 0xE9, 0x90, 0xD3, 0x93, 0x56, 0x5D, 0x71, 0x50, 0x43, 0xBC], 0x19),
    (0, &[0x14, 0x15, 0x16, 0x17, 0xA8, 0x8F], 0x93),
    (0, &[0x20, 0x05, 0x3E, 0x90, 0xE0, 0x8B, 0xC9, 0x3E, 0x88], 0xA7),
    (0, &[0x20, 0x12, 0x30, 0x12, 0x56, 0x8D], 0x6B),
    (0, &[0x20, 0x12, 0x56, 0x88], 0xC6),
    (0, &[0x20, 0x12, 0xB8, 0x12, 0x8B, 0xA0], 0x11),
    (0, &[0x20, 0x96], 0x19),
    (0, &[0x21, 0x66, 0x97, 0x85, 0x60, 0x21, 0x25, 0xBD, 0x55], 0x72),
    (0, &[0x30, 0x22, 0x11, 0x20, 0x15, 0x42, 0x23, 0xB4, 0x1C], 0xBF),
    (0, &[0x33, 0x80, 0x60, 0x95, 0x85], 0x5D),
    (0, &[0x33, 0x80, 0x60, 0x95, 0x85, 0xFA, 0x3C, 0xC0, 0xB7], 0xF2),
    (0, &[0x33, 0xB0, 0x95, 0xB8, 0x45, 0x99, 0x16, 0xB4, 0x01], 0xD9),
    (0, &[0x36, 0x31, 0xFB, 0xFE, 0x82], 0x64),
    (0, &[0x36, 0x31, 0xFB, 0xFE, 0xA0], 0x13),
    (0, &[0x55, 0x12, 0x56, 0x30, 0x32, 0x12, 0x21, 0x87, 0x72], 0x74),
    (0, &[0x65, 0x88, 0x46, 0x12, 0x12, 0xA8, 0x32, 0x9E, 0x05], 0x04),
    (0, &[0x75, 0x12, 0x33, 0x9A, 0x12, 0x64, 0x98, 0x9C, 0x20], 0x2D),
    (0, &[0x76, 0x33, 0xA5, 0xB5, 0x88, 0xC0, 0x1F, 0xAD, 0x05], 0xD7),
    (0, &[0x77, 0x45, 0x03, 0x96], 0x11),
    (0, &[0x9A, 0x98, 0x12, 0x55, 0x70, 0x35, 0x9B, 0xBE, 0x2B], 0xA6),
    (0, &[0x9B, 0x12, 0xA9, 0x88], 0xC6),
    (0, &[0xA8, 0x12, 0xA9, 0x9C], 0x85),
    (0, &[0xA8, 0x8C], 0x57),
    (0, &[0xA8, 0xB6], 0x6E),
    (0, &[0xB9, 0x12, 0x12, 0xB4], 0x22),
    (0, &[0xB9, 0xAA], 0x14),
    (0, &[0xC4, 0x69, 0x3A, 0xFA, 0x35, 0xC0, 0xE6, 0x20, 0xC8], 0xCD),
    (0, &[0xCD, 0xBB, 0x57, 0xE7, 0xFC, 0x01, 0x12, 0x5F, 0x21, 0xAE], 0x7D),
    (0, &[0xCE, 0x8A], 0x57),
    (0, &[0xF3, 0x21, 0x80, 0x70, 0x36, 0x77, 0x36, 0x45, 0x36, 0x03, 0x36, 0xA2], 0x4C),
    (0, &[0xF3, 0x21, 0x80, 0x70, 0x36, 0x77, 0x36, 0x45, 0x36, 0x03, 0x36, 0xA2, 0x8C], 0xDF),
    (0, &[0xF3, 0x21, 0x80, 0x70, 0x36, 0x9B, 0x36, 0x9E], 0x05),
    (0, &[0xF3, 0x21, 0x80, 0x70, 0x36, 0x9B, 0x36, 0x9E, 0x9C], 0xAF),
    (0, &[0xF3, 0x21, 0x80, 0x70, 0x36, 0xA8, 0x36, 0xB7], 0x3C),
    (0, &[0xF3, 0x21, 0x80, 0x70, 0x36, 0xA8, 0x36, 0xB7, 0x9C], 0xAF),
    (0, &[0xFA, 0x3C, 0xC0, 0xEE, 0x01, 0xEA, 0x3C, 0xC0, 0xB7], 0x96),
    (2, &[0x75, 0x12, 0xB8, 0x9C], 0x85),
    (3, &[0x22, 0x64, 0xBD], 0x58),
    (3, &[0x29, 0x29, 0x09, 0x09, 0x19, 0x11, 0x81, 0x70, 0xF7, 0x2A, 0x66, 0x6F, 0xE9, 0x00, 0x50, 0x06, 0x77, 0x30, 0x68, 0xDC, 0x0C, 0xE3, 0x66, 0x4F, 0x69, 0x94], 0x33),
    (3, &[0x2A, 0x12, 0x2A, 0x12, 0x2A, 0x12, 0x2A, 0x12, 0x2A, 0x12, 0x2A, 0x12, 0x2A, 0x12, 0xFA, 0x80, 0xA0, 0x86, 0x9B], 0x88),
    (3, &[0x2A, 0x12, 0xFA, 0x80, 0xA0, 0x86, 0x8C], 0x75),
    (3, &[0x3E, 0x04, 0xCD, 0x17, 0x3F, 0x8F], 0x93),
    (3, &[0x3E, 0x09, 0xCD, 0x17, 0x3F, 0xE0, 0xA5, 0xAF, 0xE0, 0xA6, 0xE0, 0xAB, 0x3E, 0x01, 0xE0, 0xAA, 0xF0, 0x9B, 0xFE, 0x08, 0xC0, 0x3E, 0x01, 0xEA, 0x7C, 0xD1, 0x3E, 0x02, 0xE0, 0x84], 0x80),
    (3, &[0x5F, 0x16, 0x00, 0x19, 0x11, 0x81, 0x70, 0xF7, 0xC9, 0xE7, 0xE5, 0xFB, 0xE6, 0x05, 0xCD, 0x3F, 0x45, 0xCD, 0xEA, 0x44, 0xCD, 0x00, 0x40, 0xCD, 0x7F, 0x14, 0xFA, 0xF5, 0xB8], 0x0D),
    (3, &[0x87, 0x5F, 0x16, 0x82], 0x66),
    (3, &[0xAF, 0xCD, 0x17, 0xBB], 0x66),
    (3, &[0xE0, 0xA7, 0xE7, 0x34, 0x40, 0x5A, 0x33, 0xFA, 0x19, 0xD0, 0x6F, 0xFA, 0x1A, 0xD0, 0x67, 0xF0, 0xBA], 0xF0),
    (3, &[0xE7, 0x22, 0x3C, 0x30, 0xAC, 0x2A, 0x66, 0x6F, 0x94], 0x99),
    (3, &[0xE7, 0x3A, 0x41, 0x3F, 0x64, 0xCD, 0x00, 0x40, 0xAF, 0xE0, 0xA5, 0xE0, 0xA6, 0x3E, 0x01, 0xE0, 0xAA, 0x3E, 0xE4, 0xE0, 0x47, 0x3E, 0xE1, 0xB4], 0x08),
    (3, &[0xE7, 0x7A, 0xA1, 0x88, 0x02, 0xCD, 0x00, 0x40, 0xF1, 0xEF, 0xC9, 0xA8], 0x3B),
    (3, &[0xE7, 0x7E, 0x83, 0x80, 0x5D, 0xCD, 0x64, 0x5B, 0xCD, 0x2B, 0x57, 0xCD, 0xF6, 0x53, 0xB8], 0x25),
    (3, &[0xE7, 0x7E, 0x85, 0x80, 0x64, 0xCD, 0xA8, 0x75, 0xF0, 0xC2, 0xFE, 0x1E, 0x38, 0xA2], 0x64),
    (3, &[0xE7, 0xA4, 0xBA, 0xD5, 0x44, 0xF0, 0x9B, 0xFE, 0x82], 0x44),
    (3, &[0xE7, 0xAB, 0xFA, 0xAF, 0x3B, 0xCD, 0x00, 0x40, 0xCD, 0xBA, 0x41, 0xE7, 0x78, 0x83, 0x84, 0x99, 0xF0, 0x9B, 0xFE, 0x0C, 0x38, 0x03, 0x3E, 0x1A, 0xEF, 0xCD, 0x00, 0x40, 0x3E, 0x05, 0xEF, 0x80], 0x16),
    (3, &[0xE7, 0xC8, 0x0E, 0xD3, 0x27, 0xCD, 0x4F, 0x99], 0x08),
    (3, &[0xF3, 0x21, 0x80, 0x70, 0x36, 0x20, 0x36, 0x84], 0x08),
    (3, &[0xF3, 0x21, 0x80, 0x70, 0x36, 0xA8, 0x36, 0x8E], 0x46),
    (3, &[0xF3, 0x21, 0x80, 0x70, 0x36, 0xA8, 0x36, 0xAE], 0xD6),
    (3, &[0xF3, 0x21, 0x80, 0x70, 0x36, 0xB9, 0x36, 0x84], 0x08),
    (3, &[0xF3, 0x21, 0x80, 0x70, 0x36, 0xB9, 0x36, 0x9E], 0x05),
    (3, &[0xFA, 0x80, 0xA0, 0xD6, 0x50, 0x36, 0x31, 0xFB, 0xE0, 0x9B, 0x3E, 0x0E, 0xE0, 0xA3, 0xBC], 0x11),
    (3, &[0xFB, 0xE0, 0x40, 0xF3, 0x92], 0x13),
    (6, &[0x09, 0x19, 0x11, 0x81, 0x70, 0xF7, 0x2A, 0x66, 0x6F, 0xCD, 0x75, 0x06, 0xC9, 0x23, 0x3D, 0x4D, 0xA0, 0xF0, 0x40, 0xA7, 0xB9, 0xAC, 0xD8, 0x09, 0x42, 0x99], 0xA0),
    (6, &[0x1A, 0x77, 0x13, 0x1A, 0x9B], 0x80),
    (6, &[0x77, 0x13, 0xB4], 0x22),
    (8, &[0x11, 0x81, 0x70, 0xF7, 0xEA, 0x98], 0xD0),
];
