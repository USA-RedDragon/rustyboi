//! Cartridge subsystem: the `Cartridge` container (ROM/RAM buffers, battery
//! persistence, header decode, RTC) plus the per-board mappers alongside it.
//!
//! Header decode lives in [`header`]. The mapper behavior is being migrated
//! out into per-board modules behind a `Mapper` enum (enum-dispatched, no
//! `dyn`, so serde savestates and the hot read/write path are preserved).

use crate::memory;
use crate::memory::mmio;
use serde::{Deserialize, Serialize};

use std::cell::Cell;
use std::fs;
use std::fs::{File, OpenOptions};
use std::io;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use zip::ZipArchive;

mod header;
mod mapper;
pub use self::header::{find_logo_in_boot_rom, CgbSupport, Destination};
use self::header::*;
mod rtc;
mod mbc7;
mod huc3;
mod camera;
mod unlicensed;
mod mbc1;
mod mbc2;
mod mbc3;
mod mbc5;
mod mbc6;
mod huc1;
mod nombc;
mod tama5;
use self::mapper::*;
use self::rtc::{HuC3Rtc, Mbc3Rtc};
// ---------------------------------------------------------------------------
// Unlicensed / bootleg mappers. These boards spoof the header type byte
// ($00/$01, or use out-of-spec values like $97/$99/$EA), so they are detected
// from ROM content (logo checksums / publisher strings / title+size shapes),
// not from $0147. References: the community reverse-engineering of these
// boards, Pan Docs "Other MBCs"
// (https://gbdev.io/pandocs/othermbc.html), and the gbdev forum thread
// "Cartridges with Rare Mappers" (https://gbdev.gg8.se/forums/viewtopic.php?id=948).
// ---------------------------------------------------------------------------

/// Byte sums of the two Sachen logo variants.
const LOGO_SUM_SACHEN_A: u32 = 5542;
const LOGO_SUM_SACHEN_B: u32 = 7484;
/// Byte sum of the Rocket Games logo (2756). Rocket carts never
/// contain the Nintendo logo in the dump; while a boot ROM runs, the mapper
/// presents the logo (sourced from the boot ROM) during its locked-CGB phase so
/// the boot ROM's logo check passes.
const LOGO_SUM_ROCKET: u32 = 2756;
/// Byte sum of the secondary Vast Fame logo at $0184 on the VF001-class
/// Legend of Heroes board. Not one of hhugboy's known VF001 sums
/// (4844/6127/4406) — this cart speaks a different, earlier register-file
/// protocol (see `UnlMapper::Vf001`), so it gets its own detection.
const LOGO_SUM_VF001_LOH: u32 = 4593;
/// File offset and first bytes of the Legend of Heroes boot protection stub
/// (`ld de,$7080; ld a,$9a; ld (de),a; ...`). Required together with the
/// $0184 logo sum so a licensed cart whose header area happens to sum to
/// 4593 can never match.
const VF001_STUB_OFFSET: usize = 0x32FC;
const VF001_STUB: [u8; 6] = [0x11, 0x80, 0x70, 0x3E, 0x9A, 0x12];

/// Page size of the `UnlMapper::Vf8k` switchable windows (8 KiB, half of a
/// normal ROM bank).
const VF8K_PAGE: usize = 0x2000;
/// The `UnlMapper::Vf8k` power-on handshake, executed straight out of the
/// header at $0134: `push af; ld a,$AA; ld ($7000),a`. Detection requires it
/// together with an entry point that jumps into the title field, so no
/// licensed cart (whose $0134-$0143 is the ASCII title) can match.
const VF8K_BOOT_STUB: [u8; 6] = [0xF5, 0x3E, 0xAA, 0xEA, 0x00, 0x70];

/// Page size of the `UnlMapper::ActionReplayV4` switchable ROM window and of
/// its SRAM banks (8 KiB each).
const ARV4_PAGE: usize = 0x2000;
/// The Action Replay V4 register block, at the top of its $6000-$7FFF window.
/// $7FE1 is the only register whose effect is visible without a cartridge in
/// the pass-through slot; the rest describe the slot cart ($7FE5 selects which
/// of the two the window shows, $7FE2-$7FE4 its MBC, $7FE0 its bank).
const ARV4_REG_START: u16 = 0x7FE0;
const ARV4_REG_END: u16 = 0x7FEF;
/// $7FE1: the 8 KiB ROM page mapped at $4000-$5FFF.
const ARV4_REG_ROM_PAGE: u16 = 0x7FE1;

/// Xploder GB ROM-bank register ($4000-$7FFF, 16 KiB banks).
const XPLODER_REG_ROM_BANK: u16 = 0x0006;
/// Xploder GB RAM-bank register ($A000-$BFFF, 8 KiB banks).
const XPLODER_REG_RAM_BANK: u16 = 0x0007;
/// RAM banks on the Xploder GB board: the firmware masks its bank parameter to
/// 4 bits and uses bank $0E, so the array is sixteen 8 KiB banks (128 KiB).
const XPLODER_RAM_BANKS: usize = 16;
/// CRC32 (reflected IEEE) of the 48 bytes at $0104 on the Xploder GB boards.
/// A pass-through cheat cart needs no logo of its own, so FCD reused the block
/// for an ASCII credit line; identical on the Europe and Germany images, and
/// no licensed cart can carry anything but the logo there.
const XPLODER_HEADER_CRC32: u32 = 0xF13F_FA9A;
/// SRAM on the Action Replay V4 board: one flat 8 KiB bank filling $6000-$7FFF
/// (a stock 6264). Nothing in the firmware banks it — $7FE0, the only other
/// register it programs with small numbers, drives the *slot* cartridge (it is
/// swept 4, 5, then $FF as a deselect, and the values it takes index blank
/// pages of the device's own ROM), not this array.
const ARV4_RAM_BANKS: usize = 1;
/// CRC32 (reflected IEEE) of the 48 bytes at $0104 on the Datel Action Replay
/// V4 boards. A pass-through cheat cart needs no logo of its own (the game in
/// the slot supplies the one the boot ROM checks), so Datel reused the block
/// for the ASCII menu strings `Upload Snapshot: `/`Download Snapshot: ` — with
/// a $44 marker at $0104 that the firmware itself compares against to tell its
/// own ROM from the cart in the slot. Identical on all three known images
/// (GameShark Online (USA), Action Replay Online (Europe), Action Replay
/// Xtreme), and no licensed cart can carry anything but the logo there.
const ARV4_HEADER_CRC32: u32 = 0xA69E_896B;

/// CRC32 (reflected IEEE) of the 48 bytes at $0184 on the Hong Kong
/// "POCKETMON" Pokemon Red bootleg: a re-linked Pokemon that the bootlegger
/// converted from MBC1 to MBC5-style linear banking (a full-width bank number
/// written to $2000-$2FFF) while leaving an MBC1+RAM+BATTERY header ($03) in
/// place. On a real 5-bit MBC1 the game's `ld a,$21 / ld ($2000),a` folds bank
/// 33 down to bank 1, whose relocated code is illegal (0xF4 at $4004) and the
/// cart dies; presenting the full byte selects the intended bank (physical
/// bank 33 byte-matches Pocket Monsters Aka bank 1 $4672, only the relinked
/// jump targets differ). A 48-byte CRC32 window plus the MBC1 header guard
/// cannot collide with a licensed cart. See the GBAtemp "Patch a game from
/// MBC1 to MBC5" bootleg-conversion technique.
const POCKETMON_MBC5_LOGO_CRC32: u32 = 0x0864_AF13;

/// Whole-ROM CRC32 (reflected IEEE) of the three Zelda no Densetsu - Yume o
/// Miru Shima DX (Japan) prototype betas that drive banking MBC5-style behind an
/// MBC1+RAM+BATTERY header: 1998-06-15 and its Alt, and 1998-07-14T181500. The
/// header type is a beta placeholder; matching the exact dump file means only
/// these three prototypes are re-mapped, with zero risk to any other cart.
const ZELDA_DX_BETA_ROM_CRC32: [u32; 3] = [0x9367_653B, 0x3FD6_C8FC, 0x2F88_4286];

/// Whole-ROM CRC32 (reflected IEEE) of the three publisher-demo prototypes that
/// need MBC3's zero-bank remap behind an MBC5+RAM+BATTERY ($1B) header: Mythri
/// (USA) (Proto 1) (2000-08-02), Mythri (USA) (Proto 2) (2001-03-31) and
/// Tyrannosaurus Tex (USA) (Proto). See `UnlMapper::ForceMbc3`. Keyed on the
/// exact dump so no other cart -- including the 2019 Piko Interactive
/// Tyrannosaurus Tex release, which is a genuine MBC5 build -- is re-mapped.
const MBC5_HEADER_MBC3_PROTO_ROM_CRC32: [u32; 3] = [0x6648_82AC, 0xD414_9106, 0x1BD4_E588];

/// Window and CRC32 (reflected IEEE) of the "New GB Color" HK-PCB protection
/// trampoline (`UnlMapper::NewGbHk`). It lives in the dead space between the
/// interrupt vectors and the header, and is 46 bytes of protection-specific
/// code: it forces bit 7 of the bank register with `or $80`, reads two bytes
/// out of the resulting protection window as a pointer, dereferences it to
/// pick the bank holding the cart's CGB init, calls that, then reads two more
/// protection bytes to restore the caller's bank. A licensed cart has no
/// reason to contain any of it, and a 46-byte CRC32 window cannot collide with
/// one — the same detection basis as the $0184 secondary-logo hashes below.
/// Keyed on the hash rather than the bytes so the first-party regression ROM
/// can carry a clean-room block instead of the cart's own code.
const NEWGBHK_STUB_OFFSET: usize = 0x0091;
const NEWGBHK_STUB_LEN: usize = 46;
const NEWGBHK_STUB_CRC32: u32 = 0x53C0_8E9D;

/// CRC32 (reflected IEEE) of the 48 bytes at $0184 on the "Pokemon Jade /
/// Diamond" board — the Telefang bootlegs (Pokemon Jade, Koudai Guaishou Da
/// Jihe), which spoof an MBC3+TIMER+RAM+BATTERY header ($10) but embed a
/// three-register challenge handshake (taizou's hhugboy `MbcUnlPokeJadeDia`,
/// CC0; mGBA `_GBPKJD`). Those $0184 bytes are executable code, not a logo, so
/// keying on the 48-byte CRC32 (as mGBA does) rather than a byte-sum gives a
/// collision-free gate; combined with the header-type $10 guard no licensed
/// cart can match. See `UnlMapper::PokeJadeDia`.
const POKEJADE_LOGO_CRC32: u32 = 0x65BB_F1FC;

/// CRC32 (reflected IEEE) of the 48-byte Vast Fame secondary logo at $0184 on
/// the general VF001 protection board (`UnlMapper::Vf001Gen`, taizou's hhugboy
/// `MbcUnlVf001`). hhugboy auto-detects the family by the $0184 byte-sum:
/// 4844 = "V.fame" (Nv Wang Gedou 2000 and the MBC1-header Zook Z, same logo),
/// 6127 = "SOUL" (the Gedou Jian Shen / Soul Falchion pair). Keyed on the CRC32
/// of the same 48-byte window (mGBA-style, strictly more selective than the
/// sum) so the collision-free hash alone gates detection — no licensed cart can
/// match a 48-byte CRC32. The same two constants also gate `UnlMapper::Vf001Zook`
/// (Zook Z), which wears the byte-identical "V.fame" logo behind a spoofed MBC1
/// header but speaks a different protocol: the two arms are told apart by the
/// header (MBC5-family $19-$1E here) plus, for Zook Z, its bank-select thunk.
const VF001G_LOGO_CRC32: [u32; 2] = [
    0x42B7_73B8, // "V.fame" — Nv Wang Gedou 2000 (shared with Zook Z)
    0x906C_2263, // "SOUL"   — Gedou Jian Shen (Soul Falchion) + KF
];

/// File offset and opcode bytes of Zook Z's VF001 bank-select thunk
/// (`ld hl,$7081; ld a,(de); ld (hl),a` x4; `ld a,($7FFF)`). Required together
/// with the "V.fame" $0184 logo CRC32 to select `UnlMapper::Vf001Zook`, so the
/// challenge-response dialect can never be applied to a cart that speaks
/// hhugboy's $7000 config-register dialect (nor to any licensed cart).
const VF001Z_THUNK_OFFSET: usize = 0x3EF5;
const VF001Z_THUNK: [u8; 17] = [
    0x21, 0x81, 0x70, 0x1A, 0x77, 0x13, 0x1A, 0x77, 0x13, 0x1A, 0x77, 0x13, 0x1A, 0x77, 0xFA, 0xFF,
    0x7F,
];

/// CRC32 (reflected IEEE) of the 48-byte Vast Fame secondary logo at $0184 on
/// LiCheng / Niutoude boards. mGBA's `_detectUnlMBC` keys on these values and
/// has shipped them with no licensed-cart false positives; a 48-byte CRC32
/// window cannot collide with licensed header code in practice.
const LICHENG_LOGO_CRC32: [u32; 2] = [0xD2B5_7657, 0x20D0_92E2];

/// CRC32 (reflected IEEE) of the 48-byte Vast Fame secondary logo at $0184 on
/// BBD boards. Same detection basis as LiCheng (mGBA `_detectUnlMBC`). The third
/// entry is King of Fighters R2, whose $0184 block is the "S-GBC" logo (byte-sum
/// 3334) that hhugboy `CartDetection` also routes to BBD; it has a distinct
/// CRC32 from the two mGBA-documented logos so it needs its own whitelist entry.
const BBD_LOGO_CRC32: [u32; 3] = [0x6D1E_A662, 0xC7D8_C1DF, 0xEA3E_443A];

/// BBD data-line reorder tables (mGBA `_bbdDataReordering`), indexed by the
/// current data swap mode; applied to every $4000-$7FFF ROM read. output bit
/// i = input bit table[i]. Only modes 0/4/5/7 are documented on real carts
/// (Garou/Harry/Digimon); the rest are the identity permutation.
const BBD_DATA_REORDERING: [[u8; 8]; 8] = [
    [0, 1, 2, 3, 4, 5, 6, 7], // 00 - normal
    [0, 1, 2, 3, 4, 5, 6, 7], // 01 - unknown
    [0, 1, 2, 3, 4, 5, 6, 7], // 02 - unknown
    [0, 1, 2, 3, 4, 5, 6, 7], // 03 - unknown
    [0, 5, 1, 3, 4, 2, 6, 7], // 04 - Garou
    [0, 4, 2, 3, 1, 5, 6, 7], // 05 - Harry
    [0, 1, 2, 3, 4, 5, 6, 7], // 06 - unknown
    [0, 1, 5, 3, 4, 2, 6, 7], // 07 - Digimon
];

/// BBD bank-line reorder tables (mGBA `_bbdBankReordering`), indexed by the
/// current bank swap mode; applied to the bank number written to $2000 before
/// it latches into the MBC5 low-8 ROM-bank register. output bit i = input bit
/// table[i].
const BBD_BANK_REORDERING: [[u8; 8]; 8] = [
    [0, 1, 2, 3, 4, 5, 6, 7], // 00 - normal
    [0, 1, 2, 3, 4, 5, 6, 7], // 01 - unknown
    [0, 1, 2, 3, 4, 5, 6, 7], // 02 - unknown
    [3, 4, 2, 0, 1, 5, 6, 7], // 03 - Digimon/Garou
    [0, 1, 2, 3, 4, 5, 6, 7], // 04 - unknown
    [1, 2, 3, 4, 0, 5, 6, 7], // 05 - Harry
    [0, 1, 2, 3, 4, 5, 6, 7], // 06 - unknown
    [0, 1, 2, 3, 4, 5, 6, 7], // 07 - unknown
];

/// Window and CRC32 (reflected IEEE) of the VF001A config stub in Sanguozhi -
/// Aoshi Tianxia (Taiwan). Unlike every other VF001 cart this image has no
/// "V.fame" secondary logo at $0184 -- those bytes are the entry trampoline
/// (`jp $0200`) -- so there is nothing for the `VF001G_LOGO_CRC32` arm to
/// match on. These 46 bytes at $0257 are the board's config driver and nothing
/// else: they synthesise $7000 from `$7A and $F0`, open config mode with
/// `ld (hl),$96`, walk $700A..$7000 downwards writing the config stream with
/// `dec l / ld (hl),n`, dip to $6000 via `sub $10` on H, then activate at
/// $7008 and close at $700F. No other cart in the library carries them.
/// Keyed on the hash rather than the bytes so a first-party regression ROM can
/// carry a clean-room block, the same basis as `NEWGBHK_STUB_CRC32`.
const VF001A_STUB_OFFSET: usize = 0x0257;
const VF001A_STUB_LEN: usize = 46;
const VF001A_STUB_CRC32: u32 = 0xCEC2_1544;

/// The VF001A board's config-accumulator seed (hhugboy `mbcConfig[0]` for
/// `UNL_VF001A`, which it only offers as a manual menu choice). Derived
/// independently here: of all 256 possible seeds, $10 is the ONLY one under
/// which the cart's own config stream both activates its two effects AND is
/// self-consistent -- the 3 bytes it programs for injection decode to
/// `ld de,$358C`, and $358C is exactly the bank-0 replacement start address the
/// same stream computes. Every other seed that enables both effects programs an
/// injection pointing somewhere else.
const VF001A_CONFIG_SEED: u8 = 0x10;

/// Whole-ROM CRC32 (reflected IEEE) of Gedou Jian Shen KF (Taiwan) (En) (Unl),
/// the one cart that carries the "SOUL" $0184 logo (`VF001G_LOGO_CRC32[1]`,
/// shared with the three Soul Falchion dumps) but is wired for the VF001A
/// config seed. Its boot config stream is byte-identical to Soul Falchion's
/// except for the first data byte ($2D instead of $25), and $2D XOR $25 = $08 =
/// `ror($10)` -- i.e. the cart pre-compensates for exactly the $10 seed, so
/// under it the accumulator lands on the SAME protection config Soul Falchion
/// programs (inject `01 B3 44` at bank-0 $0225, over the cart's own `01 00 40`
/// -- both images hold those identical three bytes there). Under the $00 seed
/// it instead programs bank $02 / $062D, an address no ROM byte matches and
/// that the board's own bank gate can never trigger, so the injection never
/// fires and the boot dead-ends. Keyed on the exact dump because the logo CRC32
/// and the title are shared with the Soul Falchion trio, which must keep the
/// $00 seed.
const VF001A_SEED_OVER_SOUL_ROM_CRC32: u32 = 0x1907_02B3;

/// Window and CRC32 (reflected IEEE) of the Dragon Ball - Final Bout protection
/// thunk (`UnlMapper::VfAdder`). Those 40 bytes at $3F50 are the entire
/// protocol — park the ROM-bank register out of range at $C0/$80, push the two
/// operands at $4000/$4002, read the answer back out of $4000, restore the
/// caller's bank from the $7FFF bank stamp — so no other cart can carry them
/// (a scan of the whole 1200-ROM library finds them in this one image only).
/// Keyed on the hash rather than the bytes so a first-party regression ROM can
/// carry a clean-room block, the same basis as `NEWGBHK_STUB_CRC32`.
const VFADDER_STUB_OFFSET: usize = 0x3F50;
const VFADDER_STUB_LEN: usize = 40;
const VFADDER_STUB_CRC32: u32 = 0x02A3_6288;

/// CRC32 (reflected IEEE) of the 48-byte secondary logo at $0184 on GGB81
/// boards (Vast Fame family; the "DATA." and "TD-SOFT" runs). mGBA's
/// `_detectUnlMBC` keys on exactly these values.
const GGB81_LOGO_CRC32: [u32; 2] = [0x79F3_4594, 0x7E8C_539B];

/// Whole-ROM CRC32 (reflected IEEE) of the two dumps that wear the GGB81
/// secondary logo but are electrically VF001 protection boards: "Fellowship of
/// the Rings (China) (En)" and "Zhihuan Wang 2 (China) (Zh)" (the same engine —
/// the two images are byte-identical from $12BF to the end). Their boot drives
/// the VF001 config-register protocol instead of GGB81's data-line reorder: it
/// opens config mode with $7000=$96, programs $7001-$7006, activates at $7000
/// and closes with $700F=$96, which arms a 3-byte injection that overlays
/// `call $12BD` on the `call $07F9` at $01FB in bank 0. On a GGB81 board those
/// writes are inert, the injection never happens, and the boot dead-ends with
/// the LCD still off.
///
/// The key has to be the whole-ROM CRC32: the $0184 logo CRC32 ($7E8C_539B) is
/// shared with genuine GGB81 carts, and so is the "MOON ACT V1.0" title — "Emo
/// Dao (Taiwan)" and "Mojie Chuanshuo (Taiwan)" carry BOTH, never touch $7000,
/// and boot correctly as GGB81. Anything looser than an exact image hash would
/// break them.
const VF001G_OVER_GGB81_ROM_CRC32: [u32; 2] = [
    0x759F_07BD, // Fellowship of the Rings (China) (En) (Unl)
    0xE674_8D1F, // Zhihuan Wang 2 (China) (Zh) (Unl)
];

/// GGB81 data-line bit-reorder tables (mGBA `_ggb81DataReordering[8][8]`). The
/// board is electrically MBC5; a write with `addr & 0xF0FF == 0x2001` latches a
/// 3-bit swap mode, and every read from the $4000-$7FFF bank window returns the
/// ROM byte with its data lines permuted through `table[mode]` (output bit i =
/// input bit table[i]). Mode 0 is the identity, so reads are unscrambled until
/// the boot code selects a mode.
const GGB81_DATA_REORDERING: [[u8; 8]; 8] = [
    [0, 1, 2, 3, 4, 5, 6, 7],
    [0, 2, 1, 3, 4, 6, 5, 7],
    [0, 6, 5, 3, 4, 2, 1, 7],
    [0, 5, 1, 3, 4, 2, 6, 7],
    [0, 5, 2, 3, 4, 1, 6, 7],
    [0, 2, 6, 3, 4, 5, 1, 7],
    [0, 1, 6, 3, 4, 2, 5, 7],
    [0, 2, 5, 3, 4, 6, 1, 7],
];

/// CRC32 (reflected IEEE) of the 48-byte $0184 secondary logo on Sintax boards
/// (mGBA `_detectUnlMBC`).
const SINTAX_LOGO_CRC32: [u32; 2] = [0x6C1D_CF2D, 0x99E3_449D];

/// Sintax ROM-bank bit-reorder tables, indexed by the 4-bit mode the game
/// programs via a $5x1x register write (mGBA `_sintaxReordering`). A bank
/// number written to $2xxx is permuted `out bit i = in bit table[i]` before it
/// reaches the MBC5 low-8 bank register, so the boot code writes pre-permuted
/// bank numbers that land on the intended bank. The identity rows are modes
/// mGBA never observed a real game select; mode $F (identity) is the power-on
/// default, so an un-programmed cart banks like a plain MBC5.
const SINTAX_BANK_REORDER: [[u8; 8]; 16] = [
    [2, 1, 4, 3, 6, 5, 0, 7],
    [3, 2, 5, 4, 7, 6, 1, 0],
    [0, 1, 2, 3, 4, 5, 6, 7], // unobserved
    [0, 1, 2, 3, 4, 5, 6, 7], // unobserved
    [0, 1, 2, 3, 4, 5, 6, 7], // unobserved
    [4, 5, 2, 3, 0, 1, 6, 7],
    [0, 1, 2, 3, 4, 5, 6, 7], // unobserved
    [6, 7, 4, 5, 1, 3, 0, 2],
    [0, 1, 2, 3, 4, 5, 6, 7], // unobserved
    [7, 6, 1, 0, 3, 2, 5, 4],
    [0, 1, 2, 3, 4, 5, 6, 7], // unobserved
    [5, 4, 7, 6, 1, 0, 3, 2],
    [0, 1, 2, 3, 4, 5, 6, 7], // unobserved
    [2, 3, 4, 5, 6, 7, 0, 1],
    [0, 1, 2, 3, 4, 5, 6, 7], // unobserved
    [0, 1, 2, 3, 4, 5, 6, 7],
];

/// CRC32 (reflected IEEE) of the 48-byte $0184 secondary logo on HITEK boards
/// (Terrifying 911, Shuihu Zhuan). mGBA's `_detectUnlMBC` keys on this value.
const HITEK_LOGO_CRC32: u32 = 0x4FDA_B691;

/// Gowin "Story of Lasama" (GS-04) protection board. hhugboy has no auto-detect
/// for it (its recommended dump is a hand-fixed plain-MBC1 rebuild); the raw
/// cart is keyed here on the CRC32 of the 48 bytes at $0184 — which on this cart
/// are executable code, not a logo, so a plain byte-sum would be unsafe — AND on
/// the exact 30-byte boot-protection stub the cart runs from $02D7. That stub is
/// the whole tell: it copies a tiny thunk to HRAM that writes the parameter byte
/// then the commit strobe to the MBC1 banking-mode port ($6000) and restarts at
/// $0100, swinging the fixed low bank to the real game half. Requiring both the
/// CRC32 and the stub bytes makes a licensed-cart or wrong-cart match impossible.
const GOWIN_LASAMA_LOGO_CRC32: u32 = 0xDD11_65F1;
const GOWIN_STUB_OFFSET: usize = 0x02D7;
const GOWIN_STUB: [u8; 30] = [
    0x11, 0xF4, 0x02, 0x0E, 0x20, 0x21, 0xFF, 0xDF, 0x1A, 0x32, 0x1D, 0x0D, 0x20, 0xFA, 0xC3, 0xF3,
    0xDF, 0x3E, 0x02, 0xEA, 0x00, 0x60, 0x3E, 0xBE, 0xEA, 0x00, 0x60, 0xC3, 0x00, 0x01,
];

/// HITEK data-bit reordering tables, selected by the data-swap mode the game
/// programs at boot (write to $2001). A read of a switchable ROM bank
/// ($4000-$7FFF) returns `reorder_bits(rom_byte, table)`; bank 0 is unmodified.
/// Verbatim from mGBA `_hitekDataReordering`.
const HITEK_DATA_REORDERING: [[u8; 8]; 8] = [
    [0, 1, 2, 3, 4, 5, 6, 7],
    [0, 6, 5, 3, 4, 1, 2, 7],
    [0, 5, 6, 3, 4, 2, 1, 7],
    [0, 6, 2, 3, 4, 5, 1, 7],
    [0, 6, 1, 3, 4, 5, 2, 7],
    [0, 1, 6, 3, 4, 5, 2, 7],
    [0, 2, 6, 3, 4, 1, 5, 7],
    [0, 6, 2, 3, 4, 1, 5, 7],
];

/// HITEK bank-number reordering tables, selected by the bank-swap mode the game
/// programs at boot (write to $2080). A bank-select write to $2000 stores
/// `reorder_bits(value, table)` as the low ROM-bank byte. Verbatim from mGBA
/// `_hitekBankReordering`.
const HITEK_BANK_REORDERING: [[u8; 8]; 8] = [
    [0, 1, 2, 3, 4, 5, 6, 7],
    [3, 2, 1, 0, 4, 5, 6, 7],
    [2, 1, 0, 3, 4, 5, 6, 7],
    [1, 0, 3, 2, 4, 5, 6, 7],
    [0, 3, 2, 1, 4, 5, 6, 7],
    [2, 3, 0, 1, 4, 5, 6, 7],
    [3, 0, 1, 2, 4, 5, 6, 7],
    [2, 0, 3, 1, 4, 5, 6, 7],
];

// Lock-phase values shared by the Sachen and Rocket boot state machines
// (the board powers up locked and unlocks in DMG -> CGB -> unlocked phases).
const UNL_LOCKED_DMG: u8 = 0;
const UNL_LOCKED_CGB: u8 = 1;
const UNL_UNLOCKED: u8 = 2;

/// NT/Makon "old" bank-line swap tables for the $5003 bit-4 mode, applied to
/// the ROM bank number: output bit i = input bit table[i].
const NT_OLD1_REORDER: [u8; 8] = [0, 2, 1, 4, 3, 5, 6, 7];
const NT_OLD2_REORDER: [u8; 8] = [1, 2, 0, 3, 4, 5, 6, 7];

/// Unlicensed mapper families detected from ROM content at load time. The
/// header type byte is unreliable on these boards, so this override wins over
/// `cartridge_type` in `get_cartridge_type`.
///
/// Not `Copy`: `Vf001Gen` carries a boxed register file (the one payload too
/// large to copy on every bus access), so the dispatch matches borrow
/// `self.unl_mapper` instead of copying it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UnlMapper {
    #[default]
    None,
    /// Wisdom Tree one-latch board: a write anywhere in $0000-$3FFF selects a
    /// whole-$0000-$7FFF 32KB bank from the low 6 bits of the ADDRESS (data
    /// ignored). Pan Docs "Other MBCs".
    WisdomTree,
    /// Rocket Games ($97 singles / $99 2-in-1s): 16KB inner bank at exactly
    /// $3F00 (0 maps to 1), 256KB outer bank at exactly $3FC0, plus the
    /// A15-transition unlock counter with the logo XOR window. gbdev forum
    /// id=948; MiSTer unlicensed thread.
    Rocket,
    /// Sachen MMC1: base/mask outer banking + the $01xx address descramble +
    /// the DMG lock phase (RA7 forced high).
    SachenMmc1,
    /// Sachen MMC2: MMC1 plus a DMG->CGB->unlocked 3-phase lock (the CGB
    /// phase presents the Nintendo logo copy at $0184).
    SachenMmc2,
    /// NT/Makon older board, MBC1-style 5-bit bank register.
    NtOld1,
    /// NT/Makon older board, MBC3-style 8-bit bank register (+ rumble on the
    /// multicarts).
    NtOld2,
    /// Header liars that are electrically plain MBC1 with no RAM: Sonic 3D
    /// Blast 5 (type $EA, code overlapping the header area), Captain
    /// Knick-Knack (Sachen dump with a Tetris header), Pocket Monsters
    /// GO!GO!GO! 256KB dumps. Routed as MBC1 with no RAM.
    ForceMbc1,
    /// Header-liar that is electrically a plain MBC5+RAM+BATTERY behind an
    /// MBC1+RAM+BATTERY header: the Hong Kong "POCKETMON" Pokemon Red bootleg,
    /// re-linked from MBC1 to MBC5-style linear banking (the game writes a
    /// full-width bank number to $2000-$2FFF; a 5-bit MBC1 mask would fold it
    /// and crash). No scramble/protection — just wider bank lines. Detected by
    /// the $0184 CRC32 signature plus the MBC1 header guard.
    ForceMbc5,
    /// M161 (Mani 4 in 1, DMG-601): a one-shot latch that maps one of eight
    /// whole-32KB banks. The header spoofs MBC3+RAM+BAT ($10), so it is
    /// content-detected (256KB + title "TETRIS SET").
    M161,
    /// Vast Fame VF001-class protection board (Legend of Heroes). Electrically
    /// a normal MBC5+RAM+BATTERY plus a 4-port protection register file
    /// decoded from A10-A11: writes at $7080/$7480/$7880/$7C80, value
    /// readback through the cart-RAM window at $A000/$A400/$A800/$AC00.
    /// Port 0 is a command port (last three bytes form the command); writes
    /// to ports 1-3 select which derived value the next protection read
    /// returns. Reverse-engineered protocol of the one known cart (all four
    /// sequences in the ROM; static RE of the required `jp (hl)` targets):
    ///
    ///   cmd $9A,$B8,$B9 (boot gate, $32FC): reads of port 2 ($A800) return
    ///       $C1 after select $B9 and $F8 after select $83; the stub decodes
    ///       hl = ($0C, $AE) via swap/offset and `jp (hl)` -> $0CAE (init).
    ///   cmd $7E,$29,$79 (gate at $0D16): side effect — the device drives the
    ///       MBC5 ROM-bank register to 6 (the following `jp $60d0` needs the
    ///       bank-6 continuation; bank 1 holds a decoy that decompresses
    ///       garbage). The $AFFF read that follows is a decoy (discarded).
    ///   cmd $37,$52,$CD (gate at $0D36): reads of port 2 return $82 after
    ///       select $BA and $8F after select $A9 -> `jp (hl)` -> $08E9
    ///       (title/graphics setup).
    ///   cmd ...,$B9,$81 ($1015): read of port 0 ($A000) supplies the TMA
    ///       seed (timer IRQs are never taken; value is not branched on).
    ///
    /// A trailing command write of $31 closes each sequence. Reads that match
    /// no armed command fall through to normal cart RAM, so saves work.
    Vf001(Vf001State),
    /// LiCheng / Niutoude (Vast Fame family): electrically a plain MBC5 wearing
    /// an MBC1 header ($01), with no data or address scrambling. The one
    /// deviation from MBC5 is that the board ignores bank-register writes in
    /// $2101-$2FFF: the games spray garbage there that would otherwise corrupt
    /// MBC5's low-8 ROM-bank register (mGBA `_GBLiCheng`). Detected from the
    /// $0184 secondary-logo CRC32; kept last so existing variant indices (and
    /// thus every other cart's savestate layout) stay unchanged.
    LiCheng,
    /// BBD (Vast Fame family): electrically an MBC5 whose $2000-$2FFF register
    /// block also carries a bit-scrambling protocol (mGBA `_GBBBD`). A write to
    /// $2001 latches the data swap mode, $2080 the bank swap mode; the bank
    /// number written to $2000 is reordered through `BBD_BANK_REORDERING`
    /// before latching, and every $4000-$7FFF ROM read is reordered through
    /// `BBD_DATA_REORDERING`. Detected from the $0184 secondary-logo CRC32
    /// (gated on $7FFF != $01, which marks a cracked dump that runs plain).
    Bbd(BbdState),
    /// GGB81 (Vast Fame family): electrically a plain MBC5 with a truthful
    /// MBC5-family header, plus data-line scrambling. A write with
    /// `addr & 0xF0FF == 0x2001` latches the 3-bit swap mode carried here;
    /// reads from the $4000-$7FFF bank window return the ROM byte permuted
    /// through `GGB81_DATA_REORDERING[mode]` (mGBA `_GBGGB81`). Detected from
    /// the $0184 secondary-logo CRC32. The mode is volatile logic normalized
    /// to 0 on power-on; it lives in the payload (not a `Cartridge` field) so
    /// every other cart's bincode savestate layout stays byte-identical.
    Ggb81(u8),
    /// Sintax (Vast Fame family): electrically MBC5 plus a boot-programmed data
    /// scramble. The board is driven by three register windows (mGBA
    /// `_GBSintax`): $5x1x selects a 4-bit bank-reorder mode, $2xxx bank writes
    /// are bit-permuted through that mode's table (and their low 2 bits pick one
    /// of four XOR bytes programmed via $7020/$7030/$7040/$7050), and reads of
    /// the switchable $4000-$7FFF window are XORed with the active byte. Bank 0
    /// is never scrambled. Detected from the $0184 secondary-logo CRC32; carries
    /// the small scramble state in the enum payload (as `Vf001` does) so every
    /// other cart's bincode savestate layout stays byte-identical.
    Sintax(SintaxState),
    /// HITEK (Vast Fame family): electrically a plain MBC5+RAM+BATTERY with two
    /// added protections (mGBA `_GBHitek`). A boot-programmed data-swap mode
    /// bit-reorders every switchable-bank ($4000-$7FFF) ROM read, and a
    /// bank-swap mode bit-reorders each bank-select value written to $2000. The
    /// two swap modes are the board's volatile state, carried in the variant
    /// payload so no other cart's savestate layout shifts. Detected from the
    /// $0184 secondary-logo CRC32.
    Hitek(HitekState),
    /// General Vast Fame VF001 protection board (taizou's hhugboy
    /// `MbcUnlVf001`, CC0). Electrically MBC5; the $6000-$7FFF config register
    /// file (decoded `addr & 0xF00F`) is driven by a rotate-right + XOR running
    /// accumulator the boot code seeds by writing $96 to $7000. Two protection
    /// effects hang off the latched config: (1) a byte-sequence injection — a
    /// read of a configured (bank,address) makes the next 1-4 ROM reads return
    /// programmed bytes (an in-place code patch); (2) a bank-0 partial
    /// replacement — reads of bank 0 from a configured address on are served
    /// from a configured high bank (overlaying the real entry onto the decoy
    /// header region). This is a distinct, later protocol from the
    /// `UnlMapper::Vf001` Legend-of-Heroes board above, so it is a separate
    /// variant. The config state rides in the payload (boxed: it is an order of
    /// magnitude larger than every other payload, and boxing keeps the enum —
    /// matched on every bus access — pointer-sized), so no other cart pays for
    /// it in memory or in the bincode savestate. Detected by the $0184 CRC32.
    /// Cracks Nv Wang Gedou 2000 and the Soul Falchion pair.
    Vf001Gen(Box<Vf001gState>),
    /// Vast Fame 8 KiB dual-window board (Jieba Tianwang 4). Electrically an
    /// MBC5+RUMBLE except that the switchable ROM area is banked in 8 KiB
    /// halves through two independent registers instead of one 16 KiB bank:
    ///
    ///   $2000-$3FFF with A10 low  -> 8 KiB bank mapped at $4000-$5FFF
    ///   $2000-$3FFF with A10 high -> 8 KiB bank mapped at $6000-$7FFF
    ///
    /// $0000-$3FFF is fixed to bank 0 and every other register ($0000-$1FFF RAM
    /// enable, $4000-$5FFF RAM bank / rumble) is plain MBC5.
    ///
    /// The game's far-call thunk makes the geometry explicit — it holds the
    /// 16 KiB bank number in a variable, doubles it, and programs the pair:
    ///     LD A,n / LD ($C242),A / SLA A / LD ($2000),A x2 / INC A
    ///     / LD ($2400),A x2 / CALL $400c
    /// so `$2000` gets 2n (the low half of 16 KiB bank n) and `$2400` gets 2n+1
    /// (its high half). Treating those as one 16 KiB register leaves the last
    /// value (2n+1) selecting 16 KiB bank (2n+1)&mask, which lands on one of the
    /// ROM's 34 decoy banks — each is filled with `JP $0000`, so the cart
    /// resets forever. The split is proven by the code that switches banks
    /// *while executing from the switchable area*: at bank 1 $44F8 the routine
    /// runs `LD A,$0A / LD ($242D),A / LD A,$00 / ...` from the $4000-$5FFF
    /// half; $242D has A10 set, so only the $6000-$7FFF half moves and the next
    /// instruction fetch is still the real continuation. A single 16 KiB
    /// register would swap the code out from under the program counter.
    Vf8k(Vf8kState),
    /// "New GB Color" HK-PCB protection board (taizou's hhugboy
    /// `MbcUnlNewGbHk`, CC0), used by the HK0701/HK0819 cartridges — Monster
    /// Go! Go! II (a CGB colourisation hack of Kirby's Dream Land 2 wearing a
    /// `KIRBY2` header) and Pokemon Action Chapter. Electrically a plain
    /// MBC5+RAM+BATTERY, plus one read-side protection: while the ROM-bank
    /// register holds a value of $80 or more, the whole switchable window
    /// stops being ROM and becomes the protection chip — $4000-$4FFF returns a
    /// value derived from address bits A4-A11 by one of eight bit-manipulations
    /// (selected by the low 3 bits of those same address bits), and
    /// $5000-$7FFF returns $FF. The game's boot trampoline sets bit 7 with
    /// `or $80`, reads two of those derived bytes as a pointer, and
    /// dereferences it to get the bank holding its CGB init code; a plain MBC5
    /// masks bit 7 away, serves ordinary ROM, and the cart lands on a `stop`
    /// opcode. Stateless — the protection is a pure function of the bank
    /// register the MBC5 board already stores, so the variant carries no
    /// payload and no other cart's savestate layout shifts.
    NewGbHk,
    /// Vast Fame VF001, challenge-response dialect (Zook Z). The board wears
    /// the same "V.fame" $0184 logo as `Vf001Gen` but is driven completely
    /// differently: instead of hhugboy's $7000 config-register file, the cart
    /// streams bytes into a single protection port in $7000-$7FFF and reads the
    /// board's answer back out. Two transactions exist:
    ///
    ///   * bank select — four bytes to $7081 (thunks at $3ED9/$3EF5, reached by
    ///     `rst $20` with the bytes inline after the call, and by `rst $30`
    ///     walking 4-byte table entries). The board decodes them to a ROM bank
    ///     and switches to it; the cart then reads $7FFF, whose last byte in
    ///     every bank is that bank's own number, to shadow the result at $D300.
    ///   * challenge-response — a byte stream to $7080/$7081 (up to 32 bytes,
    ///     inline `ld (hl),n` or copied from a bank-0 table), then a read of
    ///     $A080/$A180/$A280/$A380/$A680/$A880 whose value the cart compares or
    ///     folds into a pointer. A trailing $31 closes the transaction.
    ///
    /// Electrically MBC5 otherwise: `rst $28` is a plain `ld ($2000),a` bank
    /// write, so the two paths coexist. The port's shift window rides in the
    /// payload, boxed for the same reason `Vf001Gen` is: at 34 bytes it would
    /// otherwise set the size of an enum that is matched on every bus access,
    /// and no other cart should pay for it in memory or in the savestate.
    Vf001Zook(Box<Vf001zState>),
    /// "Pokemon Jade / Diamond" board (the Telefang bootlegs: Pokemon Jade and
    /// Koudai Guaishou Da Jihe). Electrically an MBC3+TIMER+RAM+BATTERY (the
    /// header type $10 is truthful, RTC left unpopulated), plus a weak
    /// challenge handshake layered over MBC3's unused RTC-register-select
    /// register (taizou's hhugboy `MbcUnlPokeJadeDia`, CC0; mGBA `_GBPKJD`):
    ///
    ///   * A write to $4000-$5FFF latches a "register selector" (`sel`) from
    ///     the value — the same write also drives MBC3's own RAM/RTC-bank
    ///     register, so plain SRAM/RTC banking is unaffected.
    ///   * In the $A000-$BFFF window (only while RAM is enabled): sel $0D reads
    ///     back / writes register D, sel $0E register E, and sel $0F is a
    ///     write-only command port whose value mutates D and E ($11 D--, $12
    ///     E--, $41 D+=E, $42 E+=D, $51 D++, $52 E--). Reads of the real (but
    ///     unpopulated) RTC registers $08-$0C return 0; every other selector
    ///     ($00-$07) is ordinary MBC3 SRAM.
    ///
    /// The boot code programs D/E through this port and branches on the derived
    /// values; a plain MBC3 returns open-bus there and the game white-screens.
    /// The three-byte protection state ($sel, D, E) rides in the variant
    /// payload (like `Vf001`/`Bbd`) so no other cart's bincode savestate layout
    /// shifts. Detected by the header type $10 plus the $0184 CRC32 signature.
    PokeJadeDia(PokeJadeState),
    /// Gowin "Story of Lasama" (GS-04) protection board (published by Gowin,
    /// developed by people connected to Vast Fame). Electrically a plain MBC1:
    /// the RAM-enable, 5-bit ROM-bank ($2000-$3FFF) and mode registers behave
    /// normally. The one addition is that the MBC1 banking-mode port
    /// ($6000-$7FFF) is repurposed as a two-write outer-bank handshake — the
    /// first write latches a parameter, the second (a commit strobe) sets an
    /// outer ROM base of `parameter << 1` 16 KiB banks (32 KiB granular). That
    /// base is added to BOTH the fixed $0000-$3FFF window and the switchable
    /// $4000-$7FFF window, so the whole selected 64 KiB half of the ROM is
    /// presented as a stock MBC1 cart. Power-on base 0 runs the decoy bank 0,
    /// which carries the real Nintendo logo (so the boot ROM's check passes) and
    /// a stub that performs the handshake ($6000<-$02, $6000<-$BE) then restarts
    /// at $0100 in the game half — a plain MBC1 leaves the low bank fixed at the
    /// decoy and spins at the logo forever. Detected by the $0184 CRC32 plus the
    /// exact boot stub. The tiny outer-bank state rides in the payload so no
    /// other cart's bincode savestate layout shifts.
    Gowin(GowinState),
    /// Header liars that are electrically MBC3+RAM+BATTERY behind an
    /// MBC5+RAM+BATTERY ($1B) header: the Mythri (Team XKalibur, 2000/2001) and
    /// Tyrannosaurus Tex (Slitherine/Eidos) publisher-demo prototypes.
    ///
    /// MBC5 is the only mapper that lets ROM bank 0 into the $4000-$7FFF
    /// window; MBC1/MBC2/MBC3 all remap a zero bank register to bank 1. All
    /// three dumps write a literal 0 to the bank register and then keep running
    /// code out of the switchable window, so a truthful MBC5 pulls the code out
    /// from under the program counter:
    ///
    ///   * Mythri: the boot sets its far-call bank tracker ($C9A4) to 1 and
    ///     then calls bank 1 $7713, which clears $C000-$C9FF -- including the
    ///     tracker it just set. The first far-call epilogue (bank 0 $3A06)
    ///     therefore restores bank 0 instead of bank 1 and `ret`s to $77C1,
    ///     which in bank 0 is the middle of `ld hl,$4EDD` -- the $DD there is an
    ///     illegal opcode and the CPU hard-locks (white screen). With bank 0
    ///     read as bank 1, $77C1 is the caller's own `ld a,$06` and the game
    ///     boots. Team XKalibur's own release notes say the ROM "will only play
    ///     on the no$gmb emulator ... the cartridge I have still plays on any
    ///     GBC or GBA", and the community fix for it changes exactly one
    ///     functional byte: header type $1B -> $13 (MBC3+RAM+BATTERY).
    ///   * Tyrannosaurus Tex: the loader runs from the switchable window while
    ///     bank 0 is selected there (a mirror of the fixed bank), and at $42EF
    ///     does `ld a,$68 / ld [$2100],a`. Under MBC5 the next fetch at $42F2
    ///     lands in bank $68's tile data, walks it as opcodes into an $FF
    ///     (`rst $38`), and the reset vector's `jp $0150` restarts the boot --
    ///     a ~7-frame reboot loop that flashes a screen of half-uploaded font
    ///     tiles. Under MBC3 the window still holds bank 1 and the loader
    ///     survives its own bank write.
    ///
    /// Nothing else in these dumps needs MBC5: the widest bank they program is
    /// $68 (7 bits), $3000 is only ever written 0, and $4000 is used as a plain
    /// RAM-bank select with no MBC1 mode register in sight. Keyed on the exact
    /// whole-ROM CRC32 of the three prototype dumps, so no other cart moves.
    ForceMbc3,
    /// Datel "Action Replay V4" cheat-device board (GameShark Online (USA),
    /// Action Replay Online (Europe), Action Replay Xtreme). A pass-through
    /// cart: the game plugs into the top of the device, so the device ROM
    /// carries no Nintendo logo of its own — $0104 instead holds the ASCII
    /// menu strings `Upload Snapshot:`/`Download Snapshot:` with a $44 marker
    /// byte the firmware itself tests to tell "my own ROM" from "the cart in
    /// the slot" (see the $FF80 HRAM routine each of these ROMs installs).
    ///
    /// The address map is nothing like an MBC, which is why a header-inferred
    /// MBC1 white-screens: the firmware sets `SP` to $7FC0 and keeps every
    /// variable in the $7xxx region, so on a plain MBC1 its own stack is ROM.
    ///
    ///   * $0000-$3FFF — ROM 8 KiB pages 0 and 1, fixed.
    ///   * $4000-$5FFF — ROM 8 KiB page selected by $7FE1 (the firmware uses
    ///     pages 2/3/8/9; `ld a,$09 / ld ($7FE1),a / call $4100` is the boot's
    ///     first far call, and only 8 KiB granularity puts a real subroutine
    ///     at $4100 — 16 KiB granularity lands mid-data and derails).
    ///   * $6000-$7FFF — 8 KiB battery-backed SRAM, bank selected by $7FE0.
    ///     Not ROM: the firmware writes $7FE0 with 1/2/4/5/6/7, and pages 6
    ///     and 7 of the 80 KiB GameShark Online image are 100% $00, so those
    ///     values cannot be indexing the ROM. The device stores cheat lists
    ///     and game "snapshots" here, hence the large array.
    ///   * $7FE0-$7FE5 — the register file, at the top of the SRAM window.
    ///     $7FE5 is the pass-through window select ($10 = show the cartridge
    ///     in the slot, $00 = show the device's own ROM); $7FE2-$7FE4 carry
    ///     the slot cart's MBC description. Only the two bank registers are
    ///     modelled — with no second cartridge to pass through to, the window
    ///     select has nothing to switch to and the firmware's own menu never
    ///     needs it.
    ///
    /// Detected by the CRC32 of the 48-byte block at $0104 (identical on all
    /// three ROMs, and ASCII no licensed cart can carry there) plus the
    /// bankless header type. The two bank registers ride in the payload so no
    /// other cart's bincode savestate layout shifts.
    ActionReplayV4(ArV4State),
    /// Future Console Design "Xploder GB" cheat device (sold by Blaze; the
    /// Europe v1.2.3E and Germany v1.2.2G images). Another pass-through cart,
    /// so like the Action Replay it carries no Nintendo logo — $0104 holds the
    /// ASCII credit `Future Console Design! * P.J.H. _ I.N. _ W.H.B. ` — and
    /// its header is garbage throughout (type $69, ROM-size $67, RAM-size $6E
    /// are all outside the Pan Docs tables).
    ///
    /// Its register file sits in the first bytes of the ROM window rather than
    /// in an MBC's $0000-$7FFF blocks, so a header-inferred MBC1 reads them as
    /// RAM-enable and leaves the cart with no reachable banks and no RAM:
    ///
    ///   * $0006 — ROM bank at $4000-$7FFF (16 KiB, MBC5-style: no bank-0
    ///     remap). The firmware's own "return to the menu" epilogue writes $01
    ///     here, exactly where a stock cart's power-on bank sits.
    ///   * $0007 — RAM bank at $A000-$BFFF. The firmware's parameter is masked
    ///     to 4 bits before the store (against the constant $000F at $3EAF),
    ///     and it selects bank $0E to copy 8 KiB out to WRAM, so the array is
    ///     sixteen 8 KiB banks.
    ///   * $0000, $0002-$0005 — further board registers, written alongside the
    ///     two above but with no effect this model needs; there is no RAM
    ///     enable gate (the boot writes $0007 and then immediately stores to
    ///     $B000, which a gated board would drop).
    ///
    /// Detected by the CRC32 of the 48-byte block at $0104 (identical on both
    /// images) plus the garbage header type. The two bank registers ride in
    /// the payload so no other cart's bincode savestate layout shifts.
    XploderGb(XploderState),
    /// Vast Fame family "operand adder" protection board — Dragon Ball - Final
    /// Bout (Taiwan). The dump is a *decrypted* BBD-family image (the last byte
    /// of every bank equals that bank's own number, so $7FFF==$01 / $BFFF==$02
    /// and both hhugboy and mGBA correctly decline to re-apply the BBD
    /// scramble), but the cart's protection chip is still live and unpatched,
    /// so on a plain MBC5 the boot computes a garbage jump target and dies in
    /// WRAM.
    ///
    /// The whole protocol is one thunk at $3F50:
    ///     ld a,($7FFF) / push af,de,hl / ld de,$4000 / ld hl,$2300
    ///     ld a,$C0 / ld (hl),a        ; bank register := $C0
    ///     ld a,b   / ld (de),a        ; $4000 <- operand X
    ///     ld a,$80 / ld (hl),a        ; bank register := $80
    ///     ld a,c   / inc de / inc de / ld (de),a   ; $4002 <- operand Y
    ///     dec de / dec de / ld a,(de) ; read $4000 -> answer
    ///     ld b,a / xor a / ld (de),a  ; clear
    ///     pop hl,de,af / ld ($2000),a ; restore the caller's bank
    /// The cart is 1 MiB (64 banks), so bank $C0/$80 address nothing — the
    /// out-of-range bank register is the protection enable, exactly as on the
    /// "New GB Color" HK PCB. While it is set, a write to $4000-$5FFF latches
    /// an operand (A1 picks which) and a read of that window returns
    /// `(X >> 1) + Y` (equivalently: the board sums X with Y shifted up one and
    /// presents bits 8..1 of the result).
    ///
    /// That function is derived, not guessed. The boot at $0200 queries
    /// (X=$00,Y=$02) and (X=$2A,Y=$08), builds `hl` from the two answers and
    /// does `jp (hl)`; $021D — `(0>>1)+2 = $02`, `($2A>>1)+8 = $1D` — is the
    /// only instruction boundary there and continues `di / call $1628 / ...`.
    /// Independently, $3F30 builds a pointer from the answers to (X=$00,Y=$02)
    /// and (X=$00,Y=$00), folds 29 bytes at it into a checksum and compares
    /// that against the answer to (X=$08,Y=$82): the formula puts the pointer
    /// at $0200, the real ROM bytes at $0200-$021C fold to $86, and
    /// `($08>>1)+$82 = $86`. A wrong formula has a 1-in-256 chance of passing
    /// that, and the cart boots.
    VfAdder(VfAdderState),
    /// NT "new" board (taizou's hhugboy `MbcUnlNtNew`, CC0) — the split-window
    /// successor to `NtOld1`/`NtOld2`. Electrically MBC5 until the cart arms it
    /// by writing $55 to $1400 (decoded `addr & $FF00`, inside MBC5's
    /// RAM-enable block, where $55 is a no-op for a real MBC5). From then on
    /// the switchable area is two INDEPENDENT 8 KiB pages instead of one 16 KiB
    /// bank: a write to $2000 (`addr & $FF00`) selects the page at $4000-$5FFF
    /// and one to $2400 the page at $6000-$7FFF. Each page number is taken at
    /// 8 KiB granularity (`page << 13`), wrapped to the ROM, and — mirroring
    /// MBC5's "bank 0 reads as bank 1" — a result inside the first 16 KiB is
    /// pushed up by 16 KiB, so pages 0 and 1 present pages 2 and 3.
    ///
    /// hhugboy implements this board but never auto-detects it (it is a manual
    /// menu pick), so the detection here is our own; see
    /// `NTNEW_SPLIT_STUB_CRC32`.
    NtNew(NtNewState),
}

/// Split-window state of `UnlMapper::NtNew`, carried inside the enum variant
/// (not as `Cartridge` fields) so every other cart's bincode savestate layout
/// stays byte-identical. Power-on is un-armed and MBC5-compatible; the two page
/// registers only matter once the cart writes the $1400 arming value, and it
/// programs both before it reads either.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NtNewState {
    /// Set by `$1400 <- $55`. While clear the board is a plain MBC5.
    split: bool,
    /// 8 KiB page mapped at $4000-$5FFF (written to $2000).
    low: u8,
    /// 8 KiB page mapped at $6000-$7FFF (written to $2400).
    high: u8,
}

impl NtNewState {
    /// Whether a $0000-$3FFF write belongs to the board rather than to the
    /// MBC5 underneath it. Pure, so it can gate the write-path match arm.
    fn claims(self, addr: u16, value: u8) -> bool {
        let port = addr & 0xFF00;
        (port == NTNEW_ARM_PORT && value == NTNEW_ARM_VALUE)
            || (self.split && (port == NTNEW_LOW_PORT || port == NTNEW_HIGH_PORT))
    }
}

/// NT "new" board ports, all decoded `addr & $FF00` (taizou's hhugboy
/// `MbcUnlNtNew`): $1400 arms the split window when it is written the magic
/// $55, after which $2000 and $2400 are the two 8 KiB page registers.
const NTNEW_ARM_PORT: u16 = 0x1400;
const NTNEW_ARM_VALUE: u8 = 0x55;
const NTNEW_LOW_PORT: u16 = 0x2000;
const NTNEW_HIGH_PORT: u16 = 0x2400;
/// Page size of the `UnlMapper::NtNew` split windows (8 KiB, half of a normal
/// ROM bank).
const NTNEW_PAGE: usize = 0x2000;

/// Window and CRC32 (reflected IEEE) of the NT "new" board driver, in the dead
/// space between the interrupt vectors and the header. hhugboy ships the board
/// but never auto-detects it (it is a manual menu pick), so this gate is ours.
/// These 37 bytes are the cart's whole board sequence — `ld a,$55 /
/// ld ($1400),a` to arm the split window, then the CGB double-speed / WRAM-bank
/// init around the far-call into the split half — and are byte-identical in the
/// two library images that carry them (Capcom vs SNK - Millennium Fight 2001
/// and Yingxiong Tianxia, one engine). A library-wide scan of all 3973 GB/GBC
/// images finds this hash in exactly those two and nothing else; a scan for the
/// bare arming write finds two more that must NOT take the board (Jieba
/// Tianwang 4 arms it but also speaks the `Vf8k` $7000 handshake and already
/// boots there; Super Color 26-in-1 merely embeds Jieba's engine inside a
/// plain-MBC5 multicart that 8 KiB windows would break), which is why the gate
/// is the driver hash and not the arming write. Keyed on the hash rather than
/// the bytes so the first-party regression ROM can carry a clean-room block,
/// the same basis as `NEWGBHK_STUB_CRC32`.
const NTNEW_STUB_OFFSET: usize = 0x00B9;
const NTNEW_STUB_LEN: usize = 37;
const NTNEW_STUB_CRC32: u32 = 0x24FD_EE7B;

/// Bank registers of `UnlMapper::XploderGb`, carried inside the enum variant
/// (not as `Cartridge` fields) so every other cart's bincode savestate layout
/// stays byte-identical. Power-on bank 1 / RAM bank 0 matches a stock cart;
/// the firmware programs both before it uses either.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct XploderState {
    /// 16 KiB ROM bank mapped at $4000-$7FFF (written to $0006).
    rom_bank: u8,
    /// 8 KiB RAM bank mapped at $A000-$BFFF (written to $0007).
    ram_bank: u8,
}

impl Default for XploderState {
    fn default() -> Self {
        Self { rom_bank: 1, ram_bank: 0 }
    }
}

/// Page register of `UnlMapper::ActionReplayV4`, carried inside the enum
/// variant (not as a `Cartridge` field) so every other cart's bincode
/// savestate layout stays byte-identical. Power-on page 2 puts the same bytes
/// at $4000-$5FFF that a stock cart's power-on bank 1 would; the firmware
/// programs it before its first far call regardless.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArV4State {
    /// 8 KiB ROM page mapped at $4000-$5FFF (written to $7FE1).
    rom_page: u8,
}

impl Default for ArV4State {
    fn default() -> Self {
        Self { rom_page: 2 }
    }
}

/// The two protection operand latches of `UnlMapper::VfAdder`, carried inside
/// the enum variant so no other cart's bincode savestate layout shifts. Both
/// power up at 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VfAdderState {
    /// Operand written to the even port ($4000).
    x: u8,
    /// Operand written to the odd port ($4002).
    y: u8,
}

/// Protection register state for `UnlMapper::PokeJadeDia`. Carried inside the
/// enum variant (not as `Cartridge` fields) so the bincode savestate layout of
/// every other cart stays byte-identical. All three registers power up at 0
/// (hhugboy `resetVars`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PokeJadeState {
    /// The register selector latched from the last $4000-$5FFF write (hhugboy
    /// `notRtcRegister`). $0D/$0E/$0F address the protection registers; every
    /// other value is a plain MBC3 RAM/RTC-bank select.
    sel: u8,
    /// Protection register D.
    reg_d: u8,
    /// Protection register E.
    reg_e: u8,
}

/// The two 8 KiB ROM-window registers of `UnlMapper::Vf8k`, carried inside the
/// enum variant (not as `Cartridge` fields) so every other cart's bincode
/// savestate layout stays byte-identical. Power-on selects 8 KiB banks 2 and 3,
/// i.e. exactly MBC5's power-on 16 KiB bank 1 across $4000-$7FFF.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vf8kState {
    /// 8 KiB bank mapped at $4000-$5FFF (written to $2000-$3FFF with A10 low).
    low: u8,
    /// 8 KiB bank mapped at $6000-$7FFF (written to $2000-$3FFF with A10 high).
    high: u8,
}

impl Default for Vf8kState {
    fn default() -> Self {
        Self { low: 2, high: 3 }
    }
}

/// Outer-bank handshake state for `UnlMapper::Gowin`, carried inside the enum
/// variant (not as `Cartridge` fields) so every other cart's bincode savestate
/// layout stays byte-identical. Power-on base 0 (the decoy/logo bank), no
/// parameter pending — matching a cold cart before the boot stub runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct GowinState {
    /// Outer ROM base in 16 KiB banks, added to both the fixed and switchable
    /// windows. Set by the $6000 commit strobe to `parameter << 1`.
    base: u8,
    /// The parameter byte latched by the first $6000 write, awaiting the commit
    /// strobe. `None` = waiting for the parameter; `Some` = waiting for commit.
    pending: Option<u8>,
}

/// Swap-mode latches for `UnlMapper::Hitek`, carried inside the enum variant
/// (not as `Cartridge` fields) so every other cart's bincode savestate layout
/// stays byte-identical. Both power up at 0 (the identity permutation).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HitekState {
    /// Selects `HITEK_DATA_REORDERING`; set by a write to $2001 (`value & 7`).
    data_swap_mode: u8,
    /// Selects `HITEK_BANK_REORDERING`; set by a write to $2080 (`value & 7`).
    bank_swap_mode: u8,
}

/// Boot-programmed data-scramble state for `UnlMapper::Sintax`. Held inside the
/// enum variant (not as a `Cartridge` field) so the bincode savestate layout of
/// every other cart stays byte-identical. Field roles mirror mGBA's
/// `GBSintaxState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SintaxState {
    /// Active 4-bit bank-reorder mode (index into `SINTAX_BANK_REORDER`).
    mode: u8,
    /// The four XOR bytes programmed via $7020/$7030/$7040/$7050.
    xor_values: [u8; 4],
    /// Raw (un-permuted) value of the last $2xxx bank write; its low 2 bits
    /// select which of `xor_values` is active, and it is replayed when the mode
    /// changes.
    bank_no: u8,
    /// The active XOR byte (`xor_values[bank_no & 3]`) applied to $4000-$7FFF
    /// reads.
    rom_bank_xor: u8,
}

impl Default for SintaxState {
    fn default() -> Self {
        // Power-on: mode $F is the identity reorder and the XOR is 0, so a
        // Sintax cart banks and reads exactly like a plain MBC5 until the boot
        // code programs the protection (mGBA `GBMBCInit` seeds `mode = 0xF`).
        Self { mode: 0x0F, xor_values: [0; 4], bank_no: 0, rom_bank_xor: 0 }
    }
}

/// Bit-scramble mode state for `UnlMapper::Bbd`. Carried inside the enum
/// variant (like `Vf001State`) so every other cart's bincode savestate layout
/// stays byte-identical. Both modes power up at 0 (identity) and are
/// re-programmed by the game's boot code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BbdState {
    /// Selects `BBD_DATA_REORDERING` for $4000-$7FFF reads (set via $2001).
    data_swap_mode: u8,
    /// Selects `BBD_BANK_REORDERING` for the $2000 bank write (set via $2080).
    bank_swap_mode: u8,
}

/// Protection register-file state for `UnlMapper::Vf001`. Carried inside the
/// enum variant (not as a `Cartridge` field) so the bincode savestate layout
/// of every other cart stays byte-identical.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Vf001State {
    /// Last three bytes written to the command port (port 0), oldest first.
    cmd: [u8; 3],
    /// Most recent byte written to any select port (ports 1-3).
    select: u8,
}

/// Config register file + protection state for `UnlMapper::Vf001Gen` (taizou's
/// hhugboy `MbcUnlVf001`). Carried boxed inside the enum variant (not as a
/// `Cartridge` field) so the bincode savestate layout of every other cart stays
/// byte-identical — this is the largest of the unlicensed payloads, so it is
/// the one that must not be paid for by non-users. The byte-injection counter
/// advances on immutable ROM reads, hence `Cell`. All fields are volatile
/// logic: `power_on` rebuilds this to `default()`, so a `reset` powers the
/// protection up clean exactly like a fresh load.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Vf001gState {
    /// Config mode gate: opened by writing $96 to $7000, closed by $96 to $700F.
    config_mode: bool,
    /// Rotate-right-then-XOR running accumulator applied to every known config
    /// write while `config_mode`. Seeded from `config_seed` on each enable.
    running_value: u8,
    /// The board's per-cart config byte, the seed the accumulator restarts from
    /// (hhugboy `mbcConfig[0]`). $00 for the plain VF001 boards; $10 selects
    /// taizou's `UNL_VF001A` variant, which hhugboy only exposes as a manual
    /// menu choice. Part of the cart's identity rather than volatile state, so
    /// it survives `power_on` -- it is a wiring option on the board, not a
    /// register.
    config_seed: u8,
    /// Latched accumulator for the $6000 port (bank-0 replacement source bank).
    cur6000: u8,
    /// Latched accumulator per $700x port ($7000-$700E).
    cur700x: [u8; 15],
    /// Byte-injection: trigger (bank, address), length (1-4), and the up-to-4
    /// bytes returned once triggered.
    seq_start_bank: u8,
    seq_start_addr: u16,
    seq_len: u8,
    sequence: [u8; 4],
    /// Bytes remaining to inject; advances on ROM reads, so `Cell` (not part of
    /// the persisted set — a savestate mid-injection is not a real scenario).
    #[serde(skip, default)]
    seq_bytes_left: std::cell::Cell<u8>,
    /// Bank-0 partial replacement: when enabled, reads of bank 0 from
    /// `replace_start_addr` on come from `replace_source_bank`.
    should_replace: bool,
    replace_start_addr: u16,
    replace_source_bank: u8,
}

/// Maximum protection-stream length the `UnlMapper::Vf001Zook` board buffers.
/// The longest stream any Zook Z site programs is 31 table bytes plus the
/// trailing command byte.
const VF001Z_STREAM_MAX: usize = 32;

/// Protection stream buffer for `UnlMapper::Vf001Zook`. Carried boxed inside
/// the enum variant (not as a `Cartridge` field) so every other cart's bincode
/// savestate layout stays byte-identical and the enum stays pointer-sized.
/// Pure volatile logic: `power_on` rebuilds it, so `reset` powers up clean.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vf001zState {
    /// The protection port's shift window, oldest byte first. Never cleared;
    /// once `VF001Z_STREAM_MAX` bytes are held, each further byte shifts the
    /// window along.
    stream: [u8; VF001Z_STREAM_MAX],
    /// Bytes currently held in `stream`.
    len: u8,
    /// Consecutive writes to the bank-select port, which frames a bank-select
    /// challenge as "four in a row"; any write elsewhere in $6000-$7FFF resets
    /// it.
    port_run: u8,
}

impl Default for Vf001zState {
    fn default() -> Self {
        Vf001zState { stream: [0; VF001Z_STREAM_MAX], len: 0, port_run: 0 }
    }
}

// MBC1 register ranges
const RAM_ENABLE_START: u16 = 0x0000;
const RAM_ENABLE_END: u16 = 0x1FFF;
const ROM_BANK_SELECT_START: u16 = 0x2000;
const ROM_BANK_SELECT_END: u16 = 0x3FFF;
const RAM_BANK_ROM_BANK_HIGH_START: u16 = 0x4000;
const RAM_BANK_ROM_BANK_HIGH_END: u16 = 0x5FFF;
const BANKING_MODE_START: u16 = 0x6000;
const BANKING_MODE_END: u16 = 0x7FFF;

// External RAM area
const EXTERNAL_RAM_START: u16 = 0xA000;
const EXTERNAL_RAM_END: u16 = 0xBFFF;
/// One external-RAM bank as seen through the $A000-$BFFF window.
const RAM_BANK_SIZE: usize = 0x2000;

// MBC2 specific ranges
const MBC2_RAM_SIZE: usize = 512; // 512 x 4 bits
const MBC2_RAM_START: u16 = 0xA000;

 // 3584
 // 120

#[derive(Clone, Copy, Debug)]
pub(crate) enum CartridgeType {
    NoMBC { battery: bool },
    MBC1 { ram: bool, battery: bool },
    MBC2 { battery: bool },
    MBC3 { ram: bool, battery: bool, timer: bool },
    MBC5 { ram: bool, battery: bool, rumble: bool },
    /// MBC6 ($20): split 8 KiB ROM / 4 KiB RAM windows plus an on-cart flash
    /// chip (implies RAM+BATTERY). Only "Net de Get - Minigame @ 100" uses it.
    MBC6,
    MBC7,
    HuC1,
    HuC3,
    PocketCamera,
    /// Bandai TAMA5 ($FD): the Tamagotchi board (implies RAM+BATTERY+RTC).
    Tama5,
    // Unlicensed boards (selected via UnlMapper content detection, never via
    // the header type byte alone).
    WisdomTree,
    Rocket,
    Sachen { mmc2: bool },
    NtOld { v2: bool },
    /// Mani 4 in 1 one-shot 32KB bank-latch (M161 board).
    M161,
}

#[derive(Serialize, Deserialize)]
pub struct Cartridge {
    // ROM data - all banks. Read-only (never mutated after construction) and
    // potentially multi-MB, so it is kept OUT of savestates: serializing it into
    // every rewind-ring snapshot would be fatal. The frontend re-attaches it via
    // `attach_rom` after a state load from the already-resident ROM bytes; every
    // field that derives from it (`rom_banks`, `cartridge_type`, `mbc1_multicart`,
    // `unl_mapper`, `cgb_support`) DOES serialize, so bank math survives the load.
    // Held behind an `Arc` so `Cartridge::clone` (and thus `GB::clone`, used by
    // the offloaded rewind capture every few frames) shares this multi-MB buffer
    // by refcount instead of deep-copying it. The sole mutation — a Game Genie
    // patch in `apply_rom_patch` — uses `Arc::make_mut` for copy-on-write, so a
    // live clone is never disturbed.
    #[serde(skip, default)]
    rom_data: Arc<[u8]>,
    // Cached (bank0_base, bankN_base) ROM byte offsets for the licensed-mapper
    // read fast path, so a ROM read is an add + bounds check instead of the
    // full mapper-type + bank-register derivation per access. Invalidated by
    // every `write` (the only mutation path for licensed bank registers);
    // never used for unlicensed boards (their lock state can advance on
    // reads). `serde(skip)` deserializes to None = recompute.
    #[serde(skip, default)]
    rom_bank_cache: Cell<Option<(usize, usize)>>,
    // Cached decode of (`unl_mapper`, `cartridge_type`) -> CartridgeType, which
    // every external-RAM access derived two or three times over (the mapper
    // match, then again inside `get_ram_bank`, then `is_mbc30` on MBC3). Both
    // inputs are fixed at construction: nothing assigns `cartridge_type`, and
    // every runtime write to `unl_mapper` mutates a variant PAYLOAD, never the
    // variant itself — and the decode ignores all payloads.
    // So unlike `rom_bank_cache` this never needs invalidating. `serde(skip)`
    // deserializes to None = recompute, so it is correct even if consulted
    // before `attach_rom`.
    #[serde(skip, default)]
    cartridge_type_cache: Cell<Option<CartridgeType>>,
    // External RAM data - all banks
    ram_data: Vec<u8>,
    // Cartridge type
    cartridge_type: u8,
    // Number of ROM and RAM banks
    rom_banks: usize,
    ram_banks: usize,
    // ROM file path (for determining .sav file location)
    #[serde(skip)]
    rom_path: Option<String>,
    // Open file handle for save file (for battery-backed cartridges)
    #[serde(skip)]
    save_file: Option<File>,

    // The live mapper: each board's volatile registers, enum-dispatched (see
    // cartridge/mapper.rs). The battery/persistent domain (RAM, RTC) and the
    // peripheral engines stay on Cartridge below.
    mapper: Mapper,
    // MBC1 multicart: the BANK2 register supplies ROM-bank bits 4-5 and only the
    // low 4 bits of BANK1 are wired, so the combined bank is 6 bits. Detected
    // from the Nintendo-logo-per-segment header layout (see is_mbc1_multicart).
    #[serde(default)]
    mbc1_multicart: bool,

    // MBC2 state (MBC2 has built-in 512x4 RAM)
    mbc2_ram: Vec<u8>, // MBC2 built-in RAM (512 x 4 bits, stored as full bytes)

    // Live MBC3 RTC counters, and the CPU-visible shadows a $6000-$7FFF write
    // latches them into. Same register shape, so they share a type.
    rtc: Mbc3Rtc,
    rtc_latched: Mbc3Rtc,

    // Sub-second cycle accumulator for the cycle-derived RTC. One RTC second is
    // 4_194_304 T-cycles (the 4.194304 MHz master/dot clock). The RTC crystal is
    // independent of CPU speed, so this is driven off the master `abs_cc` dot
    // clock (constant across single/double speed), NOT host wall-clock — keeping
    // RTC advancement fully deterministic and test-reproducible.
    #[serde(default)]
    rtc_cycle_accum: u64,

    // Live sensor input in g, fed by the frontend via `set_accelerometer`.
    // Not persisted (transient hardware input, like buttons), and survives
    // `reset` -- power-cycling the console does not null gravity -- so it sits
    // outside `Mbc7State`, which resets wholesale.
    #[serde(skip, default)]
    mbc7_sensor_x: f32,
    #[serde(skip, default)]
    mbc7_sensor_y: f32,

    // Battery-fed HuC-3 clock: survives `reset` while the mailbox registers
    // above do not, so it is a separate struct.
    #[serde(default)]
    huc3_rtc: HuC3Rtc,


    // Live 128x112 8-bit grayscale sensor input, fed by the frontend via
    // `set_camera_image`. Empty => the built-in deterministic test pattern.
    // Not persisted (transient hardware input, like buttons).
    #[serde(skip, default)]
    cam_image: Vec<u8>,

    // Detected unlicensed mapper family (content heuristics; overrides the
    // header type byte in get_cartridge_type).
    #[serde(default)]
    unl_mapper: UnlMapper,

    // Nintendo logo bytes the Rocket mapper presents at $0104-$0133 during its
    // locked-CGB phase, sourced at RUNTIME from the loaded boot ROM (which
    // contains the logo it checks against) so no logo data is embedded here.
    // None unless a boot ROM is present; only ever observed by a running boot
    // ROM. Not persisted (re-seeded from the boot ROM on load).
    #[serde(skip, default)]
    rocket_boot_logo: Option<[u8; 48]>,

    // CGB support information
    cgb_support: CgbSupport, // CGB compatibility from cartridge header

    // Scratch buffer backing the libretro `RETRO_MEMORY_RTC` view. Filled on
    // demand from the discrete RTC registers; not part of the save state.
    #[serde(skip, default)]
    rtc_memory: Vec<u8>,

    // Copy of the bytes last synced into `rtc_memory`, used to detect the
    // frontend writing externally-loaded RTC data into the RETRO_MEMORY_RTC
    // region (RetroArch memcpys its `.rtc` file straight into our buffer
    // after `retro_load_game`; there is no load callback).
    #[serde(skip, default)]
    rtc_memory_synced: Vec<u8>,

    // Open handle for the `.rtc` sidecar on RTC carts (MBC3 timer / HuC-3),
    // attached only on the disk-load path. None => RTC persistence disabled
    // (in-memory test-runner/WASM loads, host-managed frontends), which also
    // guarantees the cycle-derived RTC stays byte-deterministic: no sidecar
    // I/O and no host-clock reads ever happen without this handle.
    #[serde(skip)]
    rtc_file: Option<File>,

    // When true the cartridge will not open or write sidecar `.sav`/`.rtc`
    // files; the host (e.g. RetroArch) owns persistence of the in-memory RAM.
    #[serde(skip, default)]
    host_managed_saves: bool,
    // Physical SRAM chip-select decode of the emulated board for OAM-DMA
    // E000-FFFF sources (gb-ctr: the DMA asserts the external-RAM CS there and
    // "the resulting behaviour depends on the connected cartridge"). Strict
    // boards (default; the srcE000_readFE00 cgb04c hwtest capture
    // reads 0xFF with RAMG on) exclude E000-FDFF, so the bus floats.
    // Lazy boards decode /CS & A13 only (AntonioND's gbc-hw-tests flashcart)
    // and drive SRAM[src & 0x1FFF] there. Set per test fixture via the
    // manifest `cart=lazy_sram_cs` token; not a savestate property.
    #[serde(skip, default)]
    sram_cs_lazy: bool,
}

/// The ROM-derived identity of a cartridge: the expanded/padded image plus
/// every field computed from it at load time (header decode + content
/// heuristics). Immutable after construction, so `reset` carries it from the
/// live cart instead of re-running the detection predicates (which were
/// designed for the original file bytes, not the padded image). Consumed by
/// `power_on`, the single construction site for a fresh cart.
struct RomIdentity {
    rom_data: Arc<[u8]>,
    cartridge_type: u8,
    rom_banks: usize,
    ram_banks: usize,
    unl_mapper: UnlMapper,
    cgb_support: CgbSupport,
    mbc1_multicart: bool,
}

/// Which per-dot RTC advance a cartridge needs. Cached by the MMIO so the hot
/// `tick_rtc` path avoids recomputing `get_cartridge_type()` every dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum RtcTickKind {
    #[default]
    None,
    Mbc3,
    HuC3,
}

impl Clone for Cartridge {
    fn clone(&self) -> Self {
        Cartridge {
            rom_data: self.rom_data.clone(),
            rom_bank_cache: self.rom_bank_cache.clone(),
            cartridge_type_cache: self.cartridge_type_cache.clone(),
            ram_data: self.ram_data.clone(),
            cartridge_type: self.cartridge_type,
            rom_banks: self.rom_banks,
            ram_banks: self.ram_banks,
            rom_path: self.rom_path.clone(),
            save_file: None, // Don't clone file handles
            mapper: self.mapper.clone(),
            mbc1_multicart: self.mbc1_multicart,
            sram_cs_lazy: self.sram_cs_lazy,
            mbc2_ram: self.mbc2_ram.clone(),
            rtc: self.rtc,
            rtc_latched: self.rtc_latched,
            rtc_cycle_accum: self.rtc_cycle_accum,
            mbc7_sensor_x: self.mbc7_sensor_x,
            mbc7_sensor_y: self.mbc7_sensor_y,
            huc3_rtc: self.huc3_rtc.clone(),
            cam_image: self.cam_image.clone(),
            unl_mapper: self.unl_mapper.clone(),
            rocket_boot_logo: self.rocket_boot_logo,
            cgb_support: self.cgb_support.clone(),
            rtc_memory: self.rtc_memory.clone(),
            rtc_memory_synced: self.rtc_memory_synced.clone(),
            rtc_file: None, // Don't clone file handles
            host_managed_saves: self.host_managed_saves,
        }
    }
}

impl Cartridge {
    /// Detect CGB support from cartridge header byte 0x0143. Sachen MMC1/MMC2
    /// carts scramble the whole $0100-$01FF header page (the CPU reads it back
    /// through the mapper's RA0<-A6/RA1<-A4/RA4<-A1/RA6<-A0 bit-swap), so the
    /// CGB flag the boot ROM actually sees comes from the descrambled offset,
    /// not the raw byte at $0143. A CGB-compatible Sachen multicart (Rocman X
    /// Gold: descrambled $0143 = $80) would otherwise be forced into DMG mode
    /// and hang in its hardware-probe boot loop.
    fn detect_cgb_support(data: &[u8], unl_mapper: &UnlMapper) -> CgbSupport {
        let offset = match unl_mapper {
            UnlMapper::SachenMmc1 | UnlMapper::SachenMmc2 => {
                Self::sachen_unscramble(CGB_FLAG_OFFSET as u16) as usize
            }
            _ => CGB_FLAG_OFFSET,
        };
        match data.get(offset) {
            Some(&CGB_COMPATIBLE) => CgbSupport::Compatible,
            Some(&CGB_ONLY) => CgbSupport::Only,
            _ => CgbSupport::None,
        }
    }



    /// Determine the number of 16KB ROM banks. The cartridge header byte at
    /// 0x0148 is the nominal size, but it is only metadata: the physical ROM
    /// chip determines how many banks the MBC can actually address. Some test
    /// ROMs (e.g. gbmicrotest) ship a deliberately wrong header (claims 32KB
    /// but is 2MB), so when the real file is larger we trust the file size,
    /// rounding up to the next power-of-two bank count (banking masks are
    /// bit-based: bank index is taken modulo this count).
    fn compute_rom_banks(rom_size_code: u8, data_len: usize) -> Result<usize, io::Error> {
        let header_banks = match rom_size_code {
            0x00 => 2,   // 32KB = 2 banks of 16KB
            0x01 => 4,   // 64KB = 4 banks of 16KB
            0x02 => 8,   // 128KB = 8 banks of 16KB
            0x03 => 16,  // 256KB = 16 banks of 16KB
            0x04 => 32,  // 512KB = 32 banks of 16KB
            0x05 => 64,  // 1MB = 64 banks of 16KB
            0x06 => 128, // 2MB = 128 banks of 16KB
            0x07 => 256, // 4MB = 256 banks of 16KB
            0x08 => 512, // 8MB = 512 banks of 16KB (MBC5 64Mbit)
            // Out-of-spec size byte: the physical chip is what matters, so
            // size purely from the file. Unlicensed carts routinely have
            // garbage here (raw Sachen dumps keep the whole header scrambled;
            // Makon games overlap code with the header), so the loader likewise
            // falls back to the file size.
            _ => 0,
        };
        // Number of whole 16KB banks present in the actual file, rounded up to a
        // power of two so the bank-number modulo mask matches the wired address
        // lines.
        let file_banks = data_len.div_ceil(0x4000).next_power_of_two().max(2);
        Ok(header_banks.max(file_banks))
    }

    /// Number of 8KB RAM banks from the header RAM-size byte. Out-of-spec
    /// values are treated as "no RAM" rather than a load failure: unlicensed
    /// carts routinely carry garbage here (Sonic 3D Blast 5 has $20 because
    /// game code overlaps the header), matching reference decoders (RAM size
    /// stays 0 for values > 5).
    ///
    /// One exception, the same "the physical chip is what matters" argument
    /// `compute_rom_banks` makes: when the type byte names a board that carries
    /// an external RAM chip, that chip exists no matter what the size byte
    /// says, so a garbage size byte yields the smallest real part (one 8KB
    /// bank) instead of none. Pro Action Replay (Europe) is the case in point -
    /// its whole header is erased to $FF, and with zero RAM banks its
    /// disclaimer/menu code reads back $FF from its own scratch RAM and never
    /// draws a screen.
    fn compute_ram_banks(ram_size_code: u8, cartridge_type: u8) -> usize {
        match ram_size_code {
            0x00 => 0,  // No RAM
            0x01 => 1,  // 2KB (partial bank)
            0x02 => 1,  // 8KB = 1 bank
            0x03 => 4,  // 32KB = 4 banks of 8KB
            0x04 => 16, // 128KB = 16 banks of 8KB
            0x05 => 8,  // 64KB = 8 banks of 8KB
            _ => usize::from(header_type_has_external_ram(cartridge_type)),
        }
    }

    /// Physical external-RAM byte size. Header RAM-size $01 is a 2KB chip (a
    /// partial 8KB bank): it decodes only A0-A10, so the chip mirrors 4x across
    /// the $A000-$BFFF window (Pan Docs "No MBC" / the RAM-size table). Sizing
    /// the buffer to the true 2KB makes the existing `offset % ram_data.len()`
    /// in every RAM read/write reproduce that mirror. All other codes are a
    /// whole number of 8KB banks.
    fn compute_ram_len(ram_size_code: u8, ram_banks: usize) -> usize {
        if ram_banks > 0 && ram_size_code == 0x01 {
            0x800
        } else {
            ram_banks * 0x2000
        }
    }




    /// The detected unlicensed mapper family (None for licensed carts).
    pub fn unl_mapper(&self) -> &UnlMapper {
        &self.unl_mapper
    }

    pub fn load(path: &str) -> Result<Self, io::Error> {
        let data = if path.to_lowercase().ends_with(".zip") {
            Self::extract_rom_from_zip_bytes(&fs::read(path)?)?
        } else {
            fs::read(path)?
        };

        let mut cartridge = Self::from_rom_image(data)?;
        cartridge.rom_path = Some(path.to_string());

        // Try to load existing save file or create new one (only for battery-backed RAM)
        cartridge.load_or_create_save_file()?;
        // Restore the persisted RTC (with wall-clock catch-up) and attach the
        // `.rtc` sidecar. Disk-load path only; in-memory loads skip this.
        cartridge.attach_rtc_sidecar()?;

        Ok(cartridge)
    }

    /// Shared constructor core: derive everything from an already-unzipped ROM
    /// file image and hand it to `power_on`. `load` and `from_bytes` differ
    /// only in how they obtain the bytes and in sidecar/save-file attachment.
    fn from_rom_image(data: Vec<u8>) -> Result<Self, io::Error> {
        if data.len() < 0x0150 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "ROM too small"));
        }

        // Re-expand trimmed MBC1 multicart dumps before any derived fields.
        let data = Self::reconstruct_trimmed_mbc1m(&data).unwrap_or(data);

        // Read cartridge header information
        let cartridge_type = data[CARTRIDGE_TYPE_OFFSET];
        let rom_size_code = data[ROM_SIZE_OFFSET];
        let ram_size_code = data[RAM_SIZE_OFFSET];

        // Calculate number of ROM banks (header size, widened to the real file).
        let rom_banks = Self::compute_rom_banks(rom_size_code, data.len())?;

        // Calculate number of RAM banks
        let ram_banks = Self::compute_ram_banks(ram_size_code, cartridge_type);

        // Detect unlicensed mapper families (header-spoofing boards) from ROM
        // content. Must run on the raw file image, before padding.
        let unl_mapper = Self::detect_unl_mapper(&data);

        // Detect CGB support (Sachen carts read the flag through the header
        // scramble, so the mapper must be known first).
        let cgb_support = Self::detect_cgb_support(&data, &unl_mapper);

        // Detect MBC1 multicart wiring from the per-segment logo layout.
        let mbc1_multicart = Self::detect_mbc1_multicart(cartridge_type, &data);

        // Initialize RAM data. MBC7 carts declare RAM size 0x00 in the header;
        // their "save RAM" is the 93LC56 EEPROM: 256 bytes = 128 little-endian
        // 16-bit words, erased state 0xFF. Routing it through ram_data reuses
        // the whole battery-save path (LE word order matches the de-facto
        // `.sav` files). ForceMbc1 header-liars carry garbage RAM-size bytes;
        // RAM is forced off for them.
        let ram_banks = if unl_mapper == UnlMapper::ForceMbc1 { 0 } else { ram_banks };
        // Action Replay V4: the header claims ROM ONLY, but the board's whole
        // $6000-$7FFF window is SRAM banked by $7FE0. The firmware selects
        // banks up to 7, so the array is eight 8 KiB banks.
        let ram_banks = if matches!(unl_mapper, UnlMapper::ActionReplayV4(_)) {
            ARV4_RAM_BANKS
        } else {
            ram_banks
        };
        // Xploder GB: the header RAM-size byte is garbage ($6E); the board's
        // own bank register is 4 bits wide, so the array is 16 banks.
        let ram_banks = if matches!(unl_mapper, UnlMapper::XploderGb(_)) {
            XPLODER_RAM_BANKS
        } else {
            ram_banks
        };
        let ram_data = if matches!(unl_mapper, UnlMapper::ActionReplayV4(_)) {
            vec![0xFF; ARV4_RAM_BANKS * RAM_BANK_SIZE]
        } else if matches!(unl_mapper, UnlMapper::XploderGb(_)) {
            vec![0xFF; XPLODER_RAM_BANKS * RAM_BANK_SIZE]
        } else if cartridge_type == MBC7_SENSOR_RUMBLE_RAM_BATTERY {
            vec![0xFF; 256]
        } else if cartridge_type == TAMA5 {
            // TAMA5 declares RAM size $00 too: its save RAM is the 32 bytes the
            // register file can address (see `tama5.rs`), allocated from the
            // type byte so the battery-save path covers it.
            vec![0xFF; tama5::TAMA5_RAM_SIZE]
        } else {
            vec![0xFF; Self::compute_ram_len(ram_size_code, ram_banks)]
        };

        // Copy ROM data. `Arc::from(&slice)` copies exactly once — going
        // through an intermediate `Vec` and then `.into()` would copy twice
        // and leave a ROM-sized transient for the allocator to retain.
        let expected_rom_size = rom_banks * 0x4000; // 16KB per bank
        let rom_data: Arc<[u8]> = if data.len() >= expected_rom_size {
            Arc::from(&data[..expected_rom_size])
        } else {
            // Pad with 0xFF if ROM is smaller than expected
            let mut padded_rom = data;
            padded_rom.resize(expected_rom_size, 0xFF);
            padded_rom.into()
        };

        let identity = RomIdentity {
            rom_data,
            cartridge_type,
            rom_banks,
            ram_banks,
            unl_mapper,
            cgb_support,
            mbc1_multicart,
        };
        Ok(Self::power_on(identity, ram_data))
    }

    /// Build a cartridge in its power-on state: every volatile mapper latch at
    /// its documented initial value (bank registers homed, enable gates
    /// closed, boot locks armed, no in-flight peripheral activity), RAM/RTC as
    /// given/empty. This is the ONLY full `Cartridge` construction site, the
    /// single source of truth for power-on values: `from_rom_image` builds new
    /// carts through it and `reset` rebuilds the volatile domain from it, so a
    /// new field added here is automatically volatile across `reset` unless
    /// explicitly carried in reset's persist list.
    fn power_on(identity: RomIdentity, ram_data: Vec<u8>) -> Self {
        let RomIdentity {
            rom_data,
            cartridge_type,
            rom_banks,
            ram_banks,
            unl_mapper,
            cgb_support,
            mbc1_multicart,
        } = identity;
        // VF001's protection register file is volatile logic; normalize it to
        // its power-on state so reset() (which carries the possibly-mutated
        // identity in) always powers up clean, exactly like a fresh load.
        let unl_mapper = match unl_mapper {
            UnlMapper::Vf001(_) => UnlMapper::Vf001(Vf001State::default()),
            // BBD's swap-mode registers are volatile logic; power up at the
            // identity permutation so reset() matches a fresh load.
            UnlMapper::Bbd(_) => UnlMapper::Bbd(BbdState::default()),
            // GGB81's data-swap mode is volatile; power up with the identity
            // mode selected, exactly like a fresh load.
            UnlMapper::Ggb81(_) => UnlMapper::Ggb81(0),
            // Sintax's scramble state is volatile; power up at the mode-$F
            // identity + zero XOR so reset() matches a fresh load.
            UnlMapper::Sintax(_) => UnlMapper::Sintax(SintaxState::default()),
            // HITEK's swap modes are volatile board logic; power up at 0.
            UnlMapper::Hitek(_) => UnlMapper::Hitek(HitekState::default()),
            // The 8 KiB window registers are volatile; power up on bank 1.
            UnlMapper::Vf8k(_) => UnlMapper::Vf8k(Vf8kState::default()),
            // General VF001's config register file + latched protection
            // effects are volatile; power up with the config gate closed. The
            // config seed is board wiring, not state, so it is carried over.
            UnlMapper::Vf001Gen(st) => UnlMapper::Vf001Gen(Box::new(Vf001gState {
                config_seed: st.config_seed,
                ..Default::default()
            })),
            // Zook Z's protection port is a shift register; power up empty.
            UnlMapper::Vf001Zook(_) => UnlMapper::Vf001Zook(Box::default()),
            // PKJD's three protection registers are volatile; power up at 0.
            UnlMapper::PokeJadeDia(_) => UnlMapper::PokeJadeDia(PokeJadeState::default()),
            // Gowin's outer-bank latch is volatile board logic; power up at the
            // decoy base 0 with no parameter pending, exactly like a cold cart.
            UnlMapper::Gowin(_) => UnlMapper::Gowin(GowinState::default()),
            // The Action Replay's two bank registers are volatile board logic.
            UnlMapper::ActionReplayV4(_) => UnlMapper::ActionReplayV4(ArV4State::default()),
            // The Xploder's two bank registers are volatile board logic.
            UnlMapper::XploderGb(_) => UnlMapper::XploderGb(XploderState::default()),
            // NT "new": the split latch and both page registers are volatile;
            // power up un-armed, i.e. as a plain MBC5.
            UnlMapper::NtNew(_) => UnlMapper::NtNew(NtNewState::default()),
            other => other,
        };
        Cartridge {
            rom_data,
            rom_bank_cache: Cell::new(None),
            cartridge_type_cache: Cell::new(None),
            ram_data,
            cartridge_type,
            rom_banks,
            ram_banks,
            rom_path: None,
            save_file: None,
            mapper: Mapper::from_header(&unl_mapper, cartridge_type, mbc1_multicart, rom_banks, ram_banks),
            mbc1_multicart,
            sram_cs_lazy: false,
            mbc2_ram: vec![0xFF; MBC2_RAM_SIZE],
            rtc: Mbc3Rtc::default(),
            rtc_latched: Mbc3Rtc::default(),
            rtc_cycle_accum: 0,
            mbc7_sensor_x: 0.0,
            mbc7_sensor_y: 0.0,
            huc3_rtc: HuC3Rtc {
                mem: if cartridge_type == HUC3 { vec![0; 256] } else { Vec::new() },
                accum: 0,
            },
            cam_image: Vec::new(),
            unl_mapper,
            rocket_boot_logo: None,
            cgb_support,
            rtc_memory: Vec::new(),
            rtc_memory_synced: Vec::new(),
            rtc_file: None,
            host_managed_saves: false,
        }
    }

    /// Extract the ROM image from an in-memory zip container: prefer a member
    /// with a Game Boy extension, else the largest non-directory member.
    /// `load` reads the file in and comes through here too, so the path and
    /// byte entry points cannot drift apart.
    fn extract_rom_from_zip_bytes(data: &[u8]) -> Result<Vec<u8>, io::Error> {
        use std::io::Cursor;

        let cursor = Cursor::new(data);
        let mut archive = ZipArchive::new(cursor)?;

        // Common Game Boy ROM extensions
        let rom_extensions = [".gb", ".gbc", ".sgb"];

        // First, try to find a file with a ROM extension
        for i in 0..archive.len() {
            let mut file = archive.by_index(i)?;
            let name = file.name().to_lowercase();

            if rom_extensions.iter().any(|ext| name.ends_with(ext)) {
                let mut rom_data = Vec::with_capacity(file.size() as usize);
                file.read_to_end(&mut rom_data)?;
                return Ok(rom_data);
            }
        }

        // If no ROM extension found, look for the largest file
        let mut largest_file_index = 0;
        let mut largest_size = 0;

        for i in 0..archive.len() {
            let file = archive.by_index(i)?;
            if !file.is_dir() && file.size() > largest_size {
                largest_size = file.size();
                largest_file_index = i;
            }
        }

        if largest_size > 0 {
            let mut file = archive.by_index(largest_file_index)?;
            let mut rom_data = Vec::with_capacity(file.size() as usize);
            file.read_to_end(&mut rom_data)?;
            return Ok(rom_data);
        }

        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "No suitable ROM file found in zip archive"
        ))
    }

    /// Decompress `data` to the raw ROM bytes: unzips a `PK\x03\x04` container
    /// (the same extraction `from_bytes` does), else returns the bytes as-is.
    /// Useful when a caller needs the actual ROM image — e.g. to hash it for a
    /// No-Intro CRC32 lookup rather than hashing the zip container.
    pub fn extract_rom_bytes(data: &[u8]) -> Result<Vec<u8>, io::Error> {
        if data.len() >= 4 && &data[0..4] == b"PK\x03\x04" {
            Self::extract_rom_from_zip_bytes(data)
        } else {
            Ok(data.to_vec())
        }
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, io::Error> {
        // Try to detect if this is a zip file by checking the magic bytes
        let actual_data = Self::extract_rom_bytes(data)?;

        // In-memory loading intentionally skips save files so test runners and
        // WASM callers do not create sidecar files. This also skips the `.rtc`
        // sidecar + wall-clock catch-up: the RTC starts at zero and advances
        // only on the deterministic cycle-derived tick.
        Self::from_rom_image(actual_data)
    }

    /// Clone the raw ROM image (all banks, already padded to `rom_banks`) out of
    /// a live cartridge so it can be re-attached to a savestate-restored one. The
    /// ROM is `#[serde(skip)]`, so this is how a load path carries the ROM across.
    pub fn detach_rom(&self) -> Vec<u8> {
        self.rom_data.to_vec()
    }

    /// Re-attach the ROM image after a savestate load (where `rom_data` was
    /// skipped). Pads/truncates to the serialized `rom_banks * 0x4000` exactly as
    /// the constructors do, so the already-restored bank registers index the same
    /// bytes. All other runtime state (RAM, bank regs, RTC) is already present
    /// from deserialize; this only refills the read-only ROM.
    pub(crate) fn attach_rom(&mut self, rom: Vec<u8>) {
        // A caller may re-attach the ORIGINAL file bytes (not a `detach_rom`
        // image), so apply the same trimmed-MBC1M expansion as the
        // constructors; the serialized bank registers assume the physical
        // layout. Already-expanded images never match the predicate.
        let rom = Self::reconstruct_trimmed_mbc1m(&rom).unwrap_or(rom);
        let expected = self.rom_banks * 0x4000;
        self.rom_data = if rom.len() >= expected {
            Arc::from(&rom[..expected])
        } else {
            let mut padded = rom;
            padded.resize(expected, 0xFF);
            padded.into()
        };
    }

    /// Whether the ROM image is currently attached (present after construction or
    /// `attach_rom`; empty right after a savestate deserialize).
    pub fn has_rom(&self) -> bool {
        !self.rom_data.is_empty()
    }

    /// Decoded mapper for this board. Hot: the external-RAM read/write arms
    /// hit it two to three times per access, so the pure
    /// (`unl_mapper`, `cartridge_type`) -> `CartridgeType` decode below is
    /// memoized (see `cartridge_type_cache` for why it never goes stale).
    #[inline]
    fn get_cartridge_type(&self) -> CartridgeType {
        if let Some(ty) = self.cartridge_type_cache.get() {
            return ty;
        }
        let ty = self.decode_cartridge_type();
        self.cartridge_type_cache.set(Some(ty));
        ty
    }

    fn decode_cartridge_type(&self) -> CartridgeType {
        // Content-detected unlicensed boards override the (spoofed) header
        // type byte.
        match &self.unl_mapper {
            UnlMapper::None => {}
            UnlMapper::WisdomTree => return CartridgeType::WisdomTree,
            UnlMapper::Rocket => return CartridgeType::Rocket,
            UnlMapper::SachenMmc1 => return CartridgeType::Sachen { mmc2: false },
            UnlMapper::SachenMmc2 => return CartridgeType::Sachen { mmc2: true },
            UnlMapper::NtOld1 => return CartridgeType::NtOld { v2: false },
            UnlMapper::NtOld2 => return CartridgeType::NtOld { v2: true },
            UnlMapper::ForceMbc1 => {
                return CartridgeType::MBC1 { ram: false, battery: false }
            }
            // Mythri / Tyrannosaurus Tex prototypes: MBC3+RAM+BATTERY behind an
            // MBC5 header. The declared RAM size is kept for saves.
            UnlMapper::ForceMbc3 => {
                return CartridgeType::MBC3 { ram: true, battery: true, timer: false }
            }
            // POCKETMON bootleg: electrically MBC5+RAM+BATTERY behind the MBC1
            // header. The declared RAM (header $03 = 32KB) is kept for saves.
            UnlMapper::ForceMbc5 => {
                return CartridgeType::MBC5 { ram: true, battery: true, rumble: false }
            }
            UnlMapper::M161 => return CartridgeType::M161,
            // VF001 is electrically a normal MBC5+RAM+BATTERY (header $1B is
            // truthful); only the $6000-$7FFF write / $A000-$BFFF read
            // intercepts differ, so fall through to the header type.
            UnlMapper::Vf001(_) => {}
            // LiCheng's MBC1 header ($01) is a lie: the board is electrically
            // MBC5+RAM+BATTERY. Only the $2101-$2FFF bank-write ignore (in the
            // write dispatch) differs; reads and bank math are plain MBC5.
            UnlMapper::LiCheng => {
                return CartridgeType::MBC5 { ram: true, battery: true, rumble: false }
            }
            // BBD is electrically MBC5+RAM+BATTERY; only the $2000-$2FFF write
            // protocol and the $4000-$7FFF read bit-reorder differ.
            UnlMapper::Bbd(_) => {
                return CartridgeType::MBC5 { ram: true, battery: true, rumble: false }
            }
            // GGB81 wears a truthful MBC5-family header ($19 = MBC5, $1B =
            // MBC5+RAM+BATTERY); only the $2001 mode write and the reorder on
            // bank-window reads differ, so fall through to the header type.
            UnlMapper::Ggb81(_) => {}
            // Sintax is electrically MBC5+RAM+BATTERY (the real carts even
            // declare $1B truthfully); the bank reorder / read XOR are applied
            // in the write and read paths. The decode ignores the payload.
            UnlMapper::Sintax(_) => {
                return CartridgeType::MBC5 { ram: true, battery: true, rumble: false }
            }
            // HITEK is electrically MBC5+RAM+BATTERY; decode it explicitly so
            // the mapper is independent of the header byte.
            UnlMapper::Hitek(_) => {
                return CartridgeType::MBC5 { ram: true, battery: true, rumble: false }
            }
            // Zook Z's MBC1 header ($01, "no RAM") is a lie: the board is
            // electrically a bare MBC5. It declares no RAM and has no battery.
            UnlMapper::Vf001Zook(_) => {
                return CartridgeType::MBC5 { ram: false, battery: false, rumble: false }
            }
            // General VF001 is electrically MBC5, and every cart on this arm
            // declares a truthful MBC5-family header ($19-$1E) — the detection
            // gates on exactly that. Derive RAM/battery/rumble from it so saves
            // and rumble match the real board.
            UnlMapper::Vf001Gen(_) => {
                return match self.cartridge_type {
                    MBC5_RAM => CartridgeType::MBC5 { ram: true, battery: false, rumble: false },
                    MBC5_RAM_BATTERY => {
                        CartridgeType::MBC5 { ram: true, battery: true, rumble: false }
                    }
                    MBC5_RUMBLE => CartridgeType::MBC5 { ram: false, battery: false, rumble: true },
                    MBC5_RUMBLE_RAM => {
                        CartridgeType::MBC5 { ram: true, battery: false, rumble: true }
                    }
                    MBC5_RUMBLE_RAM_BATTERY => {
                        CartridgeType::MBC5 { ram: true, battery: true, rumble: true }
                    }
                    _ => CartridgeType::MBC5 { ram: false, battery: false, rumble: false },
                };
            }
            // The 8 KiB dual-window board is otherwise a plain MBC5; the one
            // known cart declares $1C (MBC5+RUMBLE) truthfully, so derive the
            // RAM/battery/rumble flags from the header.
            UnlMapper::Vf8k(_) => {}
            // "New GB Color" HK PCB: electrically a stock MBC5 and its header
            // type ($1B) is truthful, so the header decode below is correct.
            UnlMapper::NewGbHk => {}
            // The adder-protection board is electrically a stock
            // MBC5+RAM+BATTERY and its header type ($1B) is truthful.
            UnlMapper::VfAdder(_) => {}
            // NT "new": electrically MBC5, and the one cart detected here
            // declares $19 truthfully, so the header decode below is correct.
            // (hhugboy forces 32 KiB of battery RAM onto everything it routes
            // to this board because some of them under-declare it; that is a
            // save-support guess, not a boot requirement, so it is not copied.)
            UnlMapper::NtNew(_) => {}
            // PKJD is electrically MBC3+TIMER+RAM+BATTERY and its header type
            // ($10) is truthful, so the header decode below is correct; the
            // D/E/F protection is applied in the read/write intercepts.
            UnlMapper::PokeJadeDia(_) => {}
            // Gowin "Story of Lasama": electrically a plain MBC1 with no RAM or
            // battery (the header type byte $38 is title-overrun garbage). The
            // outer-bank handshake is applied in the write path + bank math.
            UnlMapper::Gowin(_) => {
                return CartridgeType::MBC1 { ram: false, battery: false }
            }
            // Action Replay V4: the board's own windows are served entirely by
            // the read/write intercepts, so the only thing the generic decode
            // still has to get right is "has battery-backed RAM" - the device
            // keeps its cheat list and snapshots across power cycles. Its
            // header type byte ($00) claims neither.
            UnlMapper::ActionReplayV4(_) => {
                return CartridgeType::MBC5 { ram: true, battery: true, rumble: false }
            }
            // Xploder GB: same story - the board's windows are served by the
            // intercepts, and the generic decode only still has to say "has
            // battery-backed RAM" (the device keeps cheat lists and backed-up
            // game saves). Its header type byte ($69) names nothing at all.
            UnlMapper::XploderGb(_) => {
                return CartridgeType::MBC5 { ram: true, battery: true, rumble: false }
            }
        }
        match self.cartridge_type {
            MBC1 => CartridgeType::MBC1 { ram: false, battery: false },
            MBC1_RAM => CartridgeType::MBC1 { ram: true, battery: false },
            MBC1_RAM_BATTERY => CartridgeType::MBC1 { ram: true, battery: true },
            MBC2 => CartridgeType::MBC2 { battery: false },
            MBC2_BATTERY => CartridgeType::MBC2 { battery: true },
            MBC3_TIMER_BATTERY => CartridgeType::MBC3 { ram: false, battery: true, timer: true },
            MBC3_TIMER_RAM_BATTERY => CartridgeType::MBC3 { ram: true, battery: true, timer: true },
            MBC3 => CartridgeType::MBC3 { ram: false, battery: false, timer: false },
            MBC3_RAM => CartridgeType::MBC3 { ram: true, battery: false, timer: false },
            MBC3_RAM_BATTERY => CartridgeType::MBC3 { ram: true, battery: true, timer: false },
            MBC5 => CartridgeType::MBC5 { ram: false, battery: false, rumble: false },
            MBC5_RAM => CartridgeType::MBC5 { ram: true, battery: false, rumble: false },
            MBC5_RAM_BATTERY => CartridgeType::MBC5 { ram: true, battery: true, rumble: false },
            MBC5_RUMBLE => CartridgeType::MBC5 { ram: false, battery: false, rumble: true },
            MBC5_RUMBLE_RAM => CartridgeType::MBC5 { ram: true, battery: false, rumble: true },
            MBC5_RUMBLE_RAM_BATTERY => CartridgeType::MBC5 { ram: true, battery: true, rumble: true },
            MBC6 => CartridgeType::MBC6,
            MBC7_SENSOR_RUMBLE_RAM_BATTERY => CartridgeType::MBC7,
            HUC1_RAM_BATTERY => CartridgeType::HuC1,
            HUC3 => CartridgeType::HuC3,
            POCKET_CAMERA => CartridgeType::PocketCamera,
            TAMA5 => CartridgeType::Tama5,
            // Bankless carts: RAM presence comes from the header RAM-size
            // byte, so $00 ROM ONLY and $08 ROM+RAM decode identically; $09
            // adds the battery. But a bankless header on a >32KB ROM is
            // physically impossible - the upper banks are unreachable without an
            // MBC - so the header simply wasn't finalized (DMG-era betas/protos).
            // Infer the era-standard MBC1, keeping the header's RAM/battery bits.
            ROM_ONLY | ROM_RAM | ROM_RAM_BATTERY if self.rom_banks > 2 => {
                CartridgeType::MBC1 {
                    ram: self.ram_banks > 0,
                    battery: self.cartridge_type == ROM_RAM_BATTERY,
                }
            }
            ROM_RAM => CartridgeType::NoMBC { battery: false },
            ROM_RAM_BATTERY => CartridgeType::NoMBC { battery: true },
            // A type byte outside the Pan Docs table names no board, so it is
            // not a claim of "bankless" - the field just holds garbage (title
            // overrun, unfinalized header). On a >32KB ROM the same physical
            // argument as above applies, so infer MBC1 rather than leave the
            // upper banks unreachable behind a bankless board.
            t if self.rom_banks > 2 && !is_documented_type(t) => {
                CartridgeType::MBC1 { ram: self.ram_banks > 0, battery: false }
            }
            _ => CartridgeType::NoMBC { battery: false },
        }
    }

    /// True when the board's ROM-BANK register lives in the cart-RAM window
    /// ($A000-$BFFF) rather than $0000-$7FFF. Only TAMA5 does this; the bus
    /// needs to know because a write there must invalidate its cached ROM page
    /// map exactly like a $0000-$7FFF cart-register write.
    pub(crate) fn banks_via_ram_window(&self) -> bool {
        matches!(self.mapper, Mapper::Tama5(_))
    }

    /// Whether external RAM is currently enabled, from whichever board carries
    /// the RAMG gate. Boards without one (HuC1 always-on, HuC3 mode-select,
    /// Sachen/Rocket ungated, bankless) report false, matching the old shared
    /// `ram_enabled` field which those paths never set.
    fn ram_enabled(&self) -> bool {
        match &self.mapper {
            Mapper::Mbc1(m) => m.ram_enabled,
            Mapper::Mbc2(m) => m.ram_enabled,
            Mapper::Mbc3(m) => m.ram_enabled,
            Mapper::Mbc5(m) => m.ram_enabled,
            Mapper::Mbc6(m) => m.state.ram_enabled,
            Mapper::Mbc7(m) => m.ram_enabled,
            Mapper::Camera(m) => m.ram_enabled,
            Mapper::NtOld(m) => m.ram_enabled,
            Mapper::Vf001(m) => m.ram_enabled,
            Mapper::LiCheng(m) => m.ram_enabled,
            Mapper::Bbd(m) => m.ram_enabled,
            Mapper::Ggb81(m) => m.ram_enabled,
            Mapper::Sintax(m) => m.ram_enabled,
            Mapper::Hitek(m) => m.ram_enabled,
            _ => false,
        }
    }

    /// ROM/RAM geometry the mapper's bank math needs.
    fn geom(&self) -> Geom {
        Geom { rom_banks: self.rom_banks, ram_banks: self.ram_banks }
    }

    fn get_rom_bank(&self) -> usize {
        self.mapper.rom_bankn(self.geom())
    }

    fn get_ram_bank(&self) -> usize {
        self.mapper.ram_bank(self.geom())
    }

    /// ROM bank mapped at the 0x0000-0x3FFF region. Normally bank 0, but on
    /// MBC1 in banking mode 1 the BANK2 register is also applied here, so a
    /// large cart sees bank 0x20/0x40/0x60 (or 0x10/0x20/0x30 on a multicart).
    fn get_rom_bank0(&self) -> usize {
        self.mapper.rom_bank0(self.geom())
    }

    /// Cached (bank0, bankN) ROM byte-offset bases for the read fast path.
    /// Whether a content-detected unlicensed mapper is active (their lock
    /// state can advance on reads, so flat-map caches must exclude them).
    #[inline]
    pub fn is_unlicensed(&self) -> bool {
        self.unl_mapper != UnlMapper::None
    }

    /// Public view of the cached (bank0, bankN) ROM byte-offset bases for the
    /// passive-read page table.
    #[inline]
    pub fn rom_bases(&self) -> (usize, usize) {
        self.rom_bank_bases()
    }

    /// Whether $4000-$7FFF is two independently banked 8 KiB halves rather than
    /// one 16 KiB bank. Only MBC6 is built that way among the licensed boards,
    /// and its halves can also be showing flash instead of ROM, so no single
    /// `rom_bases().1` describes the window and the passive-read page table
    /// must not flat-map it.
    #[inline]
    pub fn has_split_rom_window(&self) -> bool {
        matches!(self.mapper, Mapper::Mbc6(_))
    }

    /// Bounds-checked raw ROM byte (open-bus 0xFF past the image), mirroring
    /// the banked read arms.
    #[inline]
    pub(crate) fn rom_byte(&self, offset: usize) -> u8 {
        self.rom_data.get(offset).copied().unwrap_or(0xFF)
    }

    /// Cached (bank0, bankN) ROM byte-offset bases for the read fast path.
    /// Licensed mappers only mutate bank registers through `write`, which
    /// invalidates the cache; unlicensed boards can advance lock state during
    /// reads, so they always recompute (identical to the pre-cache behavior).
    #[inline]
    fn rom_bank_bases(&self) -> (usize, usize) {
        // Gowin adds its outer-bank base (set by the $6000 handshake) to both
        // the fixed and switchable windows, so the whole selected 64 KiB ROM
        // half is presented as a stock MBC1 cart. The underlying board is a
        // plain MBC1, so bank0 = base (mode-0) and bankN = base + inner bank.
        if let UnlMapper::Gowin(st) = &self.unl_mapper {
            let banks = self.rom_banks.max(1);
            let base = st.base as usize;
            let b0 = (base + self.get_rom_bank0()) % banks;
            let bn = (base + self.get_rom_bank()) % banks;
            return (b0 * 0x4000, bn * 0x4000);
        }
        if self.unl_mapper != UnlMapper::None {
            return (self.get_rom_bank0() * 0x4000, self.get_rom_bank() * 0x4000);
        }
        if let Some(bases) = self.rom_bank_cache.get() {
            return bases;
        }
        let bases = (self.get_rom_bank0() * 0x4000, self.get_rom_bank() * 0x4000);
        self.rom_bank_cache.set(Some(bases));
        bases
    }

    /// Byte index into `ram_data` for a banked external-RAM access at `addr`
    /// (which must be inside the $A000-$BFFF window). `None` when the board
    /// carries no RAM array, so callers keep their open-bus/no-op branch. A
    /// chip smaller than the selected window mirrors, hence the modulo.
    #[inline]
    fn banked_ram_offset(&self, addr: u16) -> Option<usize> {
        if self.ram_data.is_empty() {
            return None;
        }
        Some(
            ((addr - EXTERNAL_RAM_START) as usize + self.get_ram_bank() * RAM_BANK_SIZE)
                % self.ram_data.len(),
        )
    }

    /// As `banked_ram_offset`, for boards that wire RAM straight through with
    /// no bank register (NoMBC, Rocket/Sachen, NT/Makon old).
    #[inline]
    fn unbanked_ram_offset(&self, addr: u16) -> Option<usize> {
        if self.ram_data.is_empty() {
            return None;
        }
        Some((addr - EXTERNAL_RAM_START) as usize % self.ram_data.len())
    }

    /// Get the save file path for this cartridge
    fn get_save_file_path(&self) -> Option<String> {
        self.rom_path.as_ref().map(|path| {
            // Replace the extension with .sav
            let mut save_path = path.clone();
            if let Some(dot_pos) = save_path.rfind('.') {
                save_path.truncate(dot_pos);
            }
            save_path.push_str(".sav");
            save_path
        })
    }

    /// Load save file data into RAM if it exists, or create empty save file (only for battery-backed RAM)
    fn load_or_create_save_file(&mut self) -> Result<(), io::Error> {
        if let Some(save_path) = self.get_save_file_path() {
            self.attach_save_file_at(Path::new(&save_path))
        } else {
            Ok(())
        }
    }

    /// Attach a battery-backed save file at an explicit path. Used by
    /// callers (e.g. the Android entry point) that loaded the ROM via
    /// `from_bytes` and therefore have no `rom_path` from which to derive
    /// the default sidecar `.sav` location. Behaviour mirrors
    /// `load_or_create_save_file`: if the file exists its contents are
    /// copied into the cart's RAM, otherwise the current RAM contents
    /// are written out. Either way the file is kept open for streaming
    /// per-byte writes from `write_ram_byte` / `write_mbc2_ram_byte`.
    ///
    /// No-op for cartridges without battery-backed RAM.
    pub fn attach_save_file(&mut self, path: impl AsRef<Path>) -> Result<(), io::Error> {
        self.attach_save_file_at(path.as_ref())
    }

    /// Overwrite the cartridge's battery-backed RAM with the supplied
    /// bytes. Intended for the Android sibling-`.sav` path: SAF hands us
    /// the user's existing save bytes from /sdcard, and we copy them
    /// into the live cart RAM *after* `attach_save_file` has prepared
    /// the internal sidecar so subsequent writes still persist. If a
    /// save file is currently attached, the whole RAM image is flushed
    /// to disk so the internal sidecar matches the loaded state.
    ///
    /// Returns the number of bytes actually copied (truncated to the
    /// cart's RAM size). No-op for non-battery carts.
    pub fn load_sram_bytes(&mut self, bytes: &[u8]) -> Result<usize, io::Error> {
        if !self.has_battery() || self.save_ram().is_empty() {
            return Ok(0);
        }
        let copied = self.load_save_image(bytes);
        // If a save file is attached, flush the current RAM image so the
        // internal sidecar mirrors the freshly-loaded state.
        self.flush_save_image()?;
        Ok(copied)
    }

    /// Copy a save image into the cart's battery-backed buffer — MBC2's
    /// built-in array or the external RAM banks — and report the bytes taken.
    /// The single load policy behind every save-attachment path:
    ///
    /// Only the RAM-sized prefix is taken. An oversized file is legitimate for
    /// the de-facto RTC-carrying `.sav` (an appended footer, read separately by
    /// `read_sav_rtc_footer`), and for the rest it is still the safer of the
    /// options: `attach_save_file_at` opens the file for streaming writes
    /// whether or not it loaded anything, so refusing to load never actually
    /// protected the bytes — it only discarded the user's save as well. Callers
    /// that want a mis-picked file rejected outright go through
    /// `import_save_ram`, which bounds the size before delegating here.
    ///
    /// MBC2 nibble masking is not cosmetic. The built-in RAM is physically
    /// 512 x 4 bits: the upper nibble has no storage cell on the die, which is
    /// why the read path returns `0xF0 | nibble` for the undriven lines. Masking
    /// on load keeps `save_ram()` exports and the streamed sidecar (whose
    /// `write_mbc2_ram_byte` already masks) from carrying bits the silicon
    /// cannot hold.
    fn load_save_image(&mut self, bytes: &[u8]) -> usize {
        let is_mbc2 = matches!(self.get_cartridge_type(), CartridgeType::MBC2 { .. });
        let dst = self.save_ram_mut();
        let n = bytes.len().min(dst.len());
        dst[..n].copy_from_slice(&bytes[..n]);
        if is_mbc2 {
            for b in &mut dst[..n] {
                *b &= 0x0F;
            }
        }
        n
    }

    /// Rewrite the whole attached sidecar from the live save RAM. No-op when
    /// no save file is attached.
    fn flush_save_image(&mut self) -> Result<(), io::Error> {
        let is_mbc2 = matches!(self.get_cartridge_type(), CartridgeType::MBC2 { .. });
        if let Some(ref mut file) = self.save_file {
            file.seek(SeekFrom::Start(0))?;
            // Disjoint field borrows: `save_ram()` would re-borrow all of self.
            let buf: &[u8] = if is_mbc2 { &self.mbc2_ram } else { &self.ram_data };
            file.write_all(buf)?;
            file.flush()?;
        }
        Ok(())
    }

    fn attach_save_file_at(&mut self, save_path: &Path) -> Result<(), io::Error> {
        // Only process save files for cartridges with battery-backed RAM
        if !self.has_battery() || self.host_managed_saves || self.save_ram().is_empty() {
            return Ok(());
        }

        // Ensure the parent directory exists; on Android the save
        // directory is created by `android::save_dir()` but callers may
        // hand us nested paths.
        if let Some(parent) = save_path.parent()
            && !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }

        if save_path.exists() {
            let loaded_data = fs::read(save_path)?;
            self.load_save_image(&loaded_data);
        } else {
            fs::write(save_path, self.save_ram())?;
        }

        // Open file handle for efficient streaming writes
        self.save_file = Some(OpenOptions::new().write(true).open(save_path)?);
        Ok(())
    }

    /// Write a byte to both RAM and save file simultaneously (if battery-backed)
    fn write_ram_byte(&mut self, offset: usize, value: u8) -> Result<(), io::Error> {
        if !self.ram_data.is_empty() {
            // Write to RAM buffer (offset is already wrapped by caller)
            self.ram_data[offset] = value;

            // Also write to save file if we have one open
            if let Some(ref mut file) = self.save_file {
                file.seek(SeekFrom::Start(offset as u64))?;
                file.write_all(&[value])?;
                file.flush()?; // Ensure immediate write
            }
        }
        Ok(())
    }


    /// Check if this cartridge has battery-backed RAM
    pub fn has_battery(&self) -> bool {
        match self.get_cartridge_type() {
            CartridgeType::MBC1 { battery, .. } => battery,
            CartridgeType::MBC2 { battery } => battery,
            CartridgeType::MBC3 { battery, .. } => battery,
            CartridgeType::MBC5 { battery, .. } => battery,
            // MBC7's EEPROM is inherently non-volatile; HuC-3 ($FE) implies
            // RAM+BATTERY+RTC, HuC-1 ($FF) implies RAM+BATTERY, and POCKET
            // CAMERA ($FC) implies RAM+BATTERY (the photo album).
            // TAMA5 ($FD) implies RAM+BATTERY+RTC (the TAMA6 keeps the pet
            // alive with the machine off).
            // MBC6 ($20) carries a battery-backed 32 KiB SRAM alongside the
            // flash (the flash keeps downloads, the SRAM keeps saves).
            CartridgeType::MBC6
            | CartridgeType::MBC7
            | CartridgeType::HuC1
            | CartridgeType::HuC3
            | CartridgeType::PocketCamera
            | CartridgeType::Tama5 => true,
            // No known cart on these unlicensed boards has battery-backed RAM.
            CartridgeType::WisdomTree
            | CartridgeType::Rocket
            | CartridgeType::Sachen { .. }
            | CartridgeType::NtOld { .. }
            // M161's RAM line is permanently disabled; the board also zeroes
            // its header type so it never saves.
            | CartridgeType::M161 => false,
            // $09 ROM+RAM+BATTERY; plain $00/$08 (and unknown fallthroughs)
            // have none.
            CartridgeType::NoMBC { battery } => battery,
        }
    }

    /// Get CGB support information from cartridge header
    pub fn get_cgb_support(&self) -> CgbSupport {
        self.cgb_support.clone()
    }

    /// Check if this cartridge supports CGB features
    pub fn supports_cgb(&self) -> bool {
        matches!(self.cgb_support, CgbSupport::Compatible | CgbSupport::Only)
    }

    /// True when the header declares Super Game Boy support: SGB flag
    /// ($0146) == $03 AND old licensee ($014B) == $33 (Pan Docs "SGB
    /// Unlocking"). The SGB system software only honors command packets from
    /// such carts.
    pub fn supports_sgb(&self) -> bool {
        self.rom_data.get(0x0146).copied() == Some(0x03)
            && self.rom_data.get(0x014B).copied() == Some(0x33)
    }

    // -----------------------------------------------------------------------
    // Header-fact accessors (reporting/tooling; no effect on emulation).
    // -----------------------------------------------------------------------

    /// Human-readable mapper name, e.g. `"MBC5+RAM+Battery"`, `"ROM ONLY"`,
    /// `"HuC1"`. Reflects content-detected unlicensed boards (Sachen, NT, …),
    /// not just the header type byte.
    pub fn mapper_name(&self) -> &'static str {
        use CartridgeType::*;
        match self.get_cartridge_type() {
            // $00 and $08 both decode to NoMBC{battery:false}; the raw type byte
            // is the only thing that tells ROM ONLY from ROM+RAM apart.
            NoMBC { battery: false } => {
                if self.cartridge_type == ROM_RAM { "ROM+RAM" } else { "ROM ONLY" }
            }
            NoMBC { battery: true } => "ROM+RAM+Battery",
            MBC1 { ram: false, .. } => "MBC1",
            MBC1 { ram: true, battery: false } => "MBC1+RAM",
            MBC1 { ram: true, battery: true } => "MBC1+RAM+Battery",
            MBC2 { battery: false } => "MBC2",
            MBC2 { battery: true } => "MBC2+Battery",
            MBC3 { timer: true, ram: false, .. } => "MBC3+RTC+Battery",
            MBC3 { timer: true, ram: true, .. } => "MBC3+RTC+RAM+Battery",
            MBC3 { timer: false, ram: false, battery: false } => "MBC3",
            MBC3 { timer: false, ram: true, battery: false } => "MBC3+RAM",
            MBC3 { timer: false, ram: true, battery: true } => "MBC3+RAM+Battery",
            MBC3 { timer: false, ram: false, battery: true } => "MBC3+Battery",
            MBC5 { rumble: true, ram, battery } => match (ram, battery) {
                (true, true) => "MBC5+Rumble+RAM+Battery",
                (true, false) => "MBC5+Rumble+RAM",
                _ => "MBC5+Rumble",
            },
            MBC5 { rumble: false, ram, battery } => match (ram, battery) {
                (true, true) => "MBC5+RAM+Battery",
                (true, false) => "MBC5+RAM",
                _ => "MBC5",
            },
            MBC6 => "MBC6+RAM+Battery+Flash",
            MBC7 => "MBC7+Sensor+Rumble+RAM+Battery",
            HuC1 => "HuC1+RAM+Battery",
            HuC3 => "HuC3+RTC+RAM+Battery",
            PocketCamera => "Pocket Camera",
            Tama5 => "Bandai TAMA5",
            WisdomTree => "Wisdom Tree",
            Rocket => "Rocket Games",
            Sachen { mmc2: false } => "Sachen MMC1",
            Sachen { mmc2: true } => "Sachen MMC2",
            NtOld { v2: false } => "NT (old, MBC1-style)",
            NtOld { v2: true } => "NT (old, MBC3-style)",
            M161 => "M161",
        }
    }

    /// Total ROM size in bytes (all banks, `rom_banks * 16 KiB`).
    pub fn rom_size_bytes(&self) -> usize {
        self.rom_banks * 0x4000
    }

    /// External save-RAM size in bytes as actually wired (honors the 2 KiB
    /// partial chip and MBC2/MBC7's built-in memory via `ram_data`). 0 = none.
    pub fn ram_size_bytes(&self) -> usize {
        self.ram_data.len()
    }

    /// Destination code ($014A). `None` if the header is unavailable (ROM
    /// detached after a savestate load).
    pub fn destination(&self) -> Option<Destination> {
        self.rom_data.get(0x014A).map(|&b| {
            if b == 0x00 { Destination::Japanese } else { Destination::Overseas }
        })
    }

    /// Publisher name from the licensee code: the new-licensee ASCII pair
    /// ($0144-$0145) when the old code ($014B) is $33, else the old code.
    /// `None` if the header is unavailable or the code is unmapped.
    pub fn licensee(&self) -> Option<&'static str> {
        let old = *self.rom_data.get(0x014B)?;
        if old == 0x33 {
            let a = *self.rom_data.get(0x0144)?;
            let b = *self.rom_data.get(0x0145)?;
            new_licensee(a, b)
        } else {
            old_licensee(old)
        }
    }

    /// Header checksum ($014D) validity — the boot ROM's `x = x - byte - 1`
    /// fold over $0134-$014C. A failing check is what the DMG boot ROM hangs on.
    pub fn header_checksum_valid(&self) -> bool {
        let Some(hdr) = self.rom_data.get(0x0134..=0x014D) else {
            return false;
        };
        let sum = hdr[..0x19].iter().fold(0u8, |a, &b| a.wrapping_sub(b).wrapping_sub(1));
        sum == hdr[0x19]
    }

    /// Global checksum: 16-bit sum of every ROM byte except the two checksum
    /// bytes at $014E-$014F. (Real hardware never verifies it.)
    pub fn global_checksum(&self) -> u16 {
        let mut sum: u16 = 0;
        for (i, &b) in self.rom_data.iter().enumerate() {
            if i != 0x014E && i != 0x014F {
                sum = sum.wrapping_add(b as u16);
            }
        }
        sum
    }

    /// Raw cartridge-type byte ($0147) as stored in the header.
    pub fn cartridge_type_byte(&self) -> u8 {
        self.cartridge_type
    }

    /// Header title ($0134-$0143), printable-ASCII-trimmed. Empty if unreadable.
    pub fn title(&self) -> String {
        let Some(raw) = self.rom_data.get(0x0134..0x0144) else {
            return String::new();
        };
        let end = raw.iter().position(|&b| !(0x20..0x7f).contains(&b)).unwrap_or(raw.len());
        std::str::from_utf8(&raw[..end]).unwrap_or("").trim().to_string()
    }

    /// CRC32 of the whole ROM (the No-Intro key), over the internal buffer with
    /// no copy. `None` if the ROM is detached (post-savestate, before re-attach).
    pub fn rom_crc32(&self) -> Option<u32> {
        (!self.rom_data.is_empty()).then(|| crate::checksum::crc32(&self.rom_data))
    }



















    // --- RTC persistence -------------------------------------------------
    //
    // MBC3 blob: the de-facto community "RTC data" layout, 48 bytes, all fields
    // little-endian. Common tools write this same block as a footer
    // appended to the `.sav`, and libretro cores expose it verbatim as
    // RETRO_MEMORY_RTC, so RetroArch `.rtc` files use it too. We store it in
    // a `.rtc` sidecar next to the `.sav` (the RetroArch convention) and
    // additionally READ it from a `.sav` footer for imported saves.
    //
    //   offset size field
    //   0x00   4    seconds       (live counter)
    //   0x04   4    minutes       (live)
    //   0x08   4    hours         (live)
    //   0x0C   4    days low      (live)
    //   0x10   4    days high     (live; bit0=day bit8, bit6=HALT, bit7=carry)
    //   0x14   4    latched seconds
    //   0x18   4    latched minutes
    //   0x1C   4    latched hours
    //   0x20   4    latched days low
    //   0x24   4    latched days high
    //   0x28   8    u64 UNIX time the state was saved at (the legacy 44-byte
    //               variant stores a u32 here; accepted on read)
    //
    // Layout: the five live registers (seconds..control), then the latched
    // copies, then a union{time_t,u64} timestamp, written raw with a -4 read
    // leeway for the legacy u32 form (32LE fields + 64LE timestamp, read also
    // accepts the sizeof-4 short form).
    //
    // HuC-3 blob: the de-facto 136-byte layout: the RTC
    // MCU's 256 nibbles packed two per byte (nibble N -> byte N/2, even N in
    // the low half) followed by a u64 LE UNIX timestamp. This carries the
    // architected minute-of-day/day-counter nibbles (0x10-0x15) plus the
    // whole MCU memory (event time, tone, scratch I/O).


















    // --- libretro accessors ---

    /// Mark this cartridge as host-managed: it will not open or write any
    /// sidecar `.sav` file. Persistence of the in-memory RAM is the frontend's
    /// responsibility (e.g. RetroArch via `RETRO_MEMORY_SAVE_RAM`).
    pub fn set_host_managed_saves(&mut self, enabled: bool) {
        self.host_managed_saves = enabled;
    }

    /// Mutable view of the battery/save RAM the frontend should persist. For
    /// MBC2 this is the built-in 512x4 RAM; otherwise the external RAM banks.
    /// Returns an empty slice when there is no save RAM.
    pub fn save_ram_mut(&mut self) -> &mut [u8] {
        match self.get_cartridge_type() {
            CartridgeType::MBC2 { .. } => &mut self.mbc2_ram,
            _ => &mut self.ram_data,
        }
    }

    /// Read-only view of the battery/save RAM.
    pub fn save_ram(&self) -> &[u8] {
        match self.get_cartridge_type() {
            CartridgeType::MBC2 { .. } => &self.mbc2_ram,
            _ => &self.ram_data,
        }
    }

    /// Import a battery save image into the cart's RAM (File → Import Battery
    /// Save). Copies `min(src, dst)` bytes so a footer-carrying `.sav` (RTC
    /// footer) or a short file loads its RAM-sized prefix; a wildly-oversized
    /// file (more than double the RAM) is rejected so a mis-picked file can't be
    /// silently accepted. If a sidecar `.sav` is attached (desktop) the freshly
    /// loaded image is flushed straight through it, so the import survives a
    /// reload with no extra host plumbing. No-op for non-battery carts.
    pub fn import_save_ram(&mut self, bytes: &[u8]) -> Result<usize, String> {
        if !self.has_battery() {
            return Err("cartridge has no battery-backed save RAM".into());
        }
        let ram_len = self.save_ram().len();
        if ram_len == 0 {
            return Err("cartridge has no save RAM".into());
        }
        if bytes.len() > ram_len.saturating_mul(2) {
            return Err(format!(
                "save file is {} bytes but this cart's RAM is {ram_len} bytes",
                bytes.len()
            ));
        }
        self.load_sram_bytes(bytes).map_err(|e| e.to_string())
    }

    /// Byte the cartridge RAM chip drives when the OAM-DMA controller asserts
    /// the external-RAM chip select (gb-ctr "OAM DMA address decoding": all
    /// A000-FFFF sources are external-RAM accesses). Bypasses the CPU read
    /// front-end (unlicensed boot locks / descramblers watch CPU ROM fetches,
    /// not the RAM chip select) and models the plain RAMG-gated array: enabled
    /// banked RAM drives its byte, anything else leaves the bus floating
    /// (0xFF, matching the RAM-less srcE000 cgb04c captures).
    pub(crate) fn dma_sram_bus_read(&self, addr: u16) -> u8 {
        if self.sram_cs_lazy && self.ram_enabled() && !self.ram_data.is_empty() {
            // NOT `banked_ram_offset`: `addr` here reaches $E000-$FFFF, and the
            // captures pin the wrapped decode ($E000 -> $A000), which `addr -
            // EXTERNAL_RAM_START` would not produce.
            let offset = ((addr as usize & 0x1FFF) + self.get_ram_bank() * RAM_BANK_SIZE)
                % self.ram_data.len();
            self.ram_data[offset]
        } else {
            0xFF
        }
    }

    /// Select the board's SRAM chip-select decode (see `dma_sram_bus_read`).
    pub(crate) fn set_sram_cs_lazy(&mut self, lazy: bool) {
        self.sram_cs_lazy = lazy;
    }





    // -----------------------------------------------------------------------
    // POCKET CAMERA (MAC-GBD controller + Mitsubishi M64282FP image sensor)
    //
    // References: Pan Docs "Game Boy Camera" (including its published
    // "Sample code for emulators" image pipeline) and the public
    // gbcam-rev-engineer register/timing documentation (v1.1.1).
    //
    // Register file (A000-A035 while a bank with bit 4 set is selected,
    // mirrored every $80):
    //   A000     trigger/status: bit0 start capture / busy flag, bits 1-2
    //            select the M64282FP 1-D filtering set (P/M/X registers).
    //   A001     -> sensor reg 1: N (bit7), VH (bits 5-6), gain (bits 0-4).
    //   A002/03  -> sensor regs 2/3: 16-bit exposure, MSB first.
    //   A004     -> sensor reg 7: E3+edge ratio (bits 4-7), invert (bit 3),
    //            output node bias V (bits 0-2, analog only).
    //   A005     -> sensor reg 0: zero-point calibration (bits 6-7), output
    //            reference voltage O (bits 0-5, analog only).
    //   A006-35  4x4 dither/contrast matrix, 3 threshold bytes per cell.
    // -----------------------------------------------------------------------














    /// True for MBC5 rumble cartridges.
    pub fn has_rumble(&self) -> bool {
        matches!(self.get_cartridge_type(), CartridgeType::MBC5 { rumble: true, .. })
    }

    /// Current state of the rumble motor (bit 3 of the last RAM-bank write on
    /// a rumble cart). Always false for non-rumble carts.
    pub fn rumble_active(&self) -> bool {
        matches!(&self.mapper, Mapper::Mbc5(m) if m.rumble_motor)
    }

    /// Patch a ROM byte (Game Genie). `addr` is a 0x0000-0x7FFF CPU address;
    /// the patch is applied to ROM bank 0 for 0x0000-0x3FFF and to the bank
    /// currently mapped at 0x4000-0x7FFF otherwise. When `compare` is given the
    /// patch only applies if the existing byte matches. Game Genie codes target
    /// bank 0 / early ROM in practice, where the mapping is fixed.
    pub fn apply_rom_patch(&mut self, addr: u16, new: u8, compare: Option<u8>) {
        let offset = if addr < 0x4000 {
            addr as usize
        } else if addr < 0x8000 {
            let bank = self.get_rom_bank();
            (addr as usize - 0x4000) + bank * 0x4000
        } else {
            return;
        };
        if offset >= self.rom_data.len() {
            return;
        }
        if let Some(c) = compare
            && self.rom_data[offset] != c
        {
            return;
        }
        Arc::make_mut(&mut self.rom_data)[offset] = new;
    }

    /// Power-cycle the mapper: rebuild the cartridge in its power-on state
    /// (`power_on`, the same derivation the constructors use) and carry over
    /// ONLY what survives a real power cycle. The battery-fed domain persists
    /// — cartridge RAM, MBC2 built-in RAM, the MBC3 RTC time registers, the
    /// HuC-3 RTC memory, and their sub-second accumulators — just like
    /// pressing the console's reset/power button, which cuts mapper power but
    /// not the cart battery. Transient hardware inputs (accelerometer tilt,
    /// camera image) and host plumbing (file handles, rom_path,
    /// host_managed_saves, sram_cs_lazy, libretro RTC views, boot-logo seed)
    /// persist too. Everything else — bank registers, enable gates, banking
    /// modes, boot locks, in-flight peripheral state — comes from `fresh`, so
    /// a new field is volatile across reset unless added to the carry list.
    ///
    /// Boot locks (Sachen/Rocket) re-arm here; a subsequent `skip_bios` runs
    /// `skip_boot_handoff` to unlock them when no boot ROM will execute.
    pub fn reset(&mut self) {
        let fresh = Self::power_on(
            RomIdentity {
                rom_data: self.rom_data.clone(), // Arc: refcount bump, no copy
                cartridge_type: self.cartridge_type,
                rom_banks: self.rom_banks,
                ram_banks: self.ram_banks,
                unl_mapper: std::mem::take(&mut self.unl_mapper),
                cgb_support: self.cgb_support.clone(),
                mbc1_multicart: self.mbc1_multicart,
            },
            Vec::new(), // discarded: the battery-backed RAM is carried below
        );
        let carried = Cartridge {
            // Battery-fed domain.
            ram_data: std::mem::take(&mut self.ram_data),
            mbc2_ram: std::mem::take(&mut self.mbc2_ram),
            rtc: self.rtc,
            rtc_cycle_accum: self.rtc_cycle_accum,
            huc3_rtc: std::mem::take(&mut self.huc3_rtc),
            // Transient hardware inputs: power cycling the console nulls
            // neither gravity nor the camera scene.
            mbc7_sensor_x: self.mbc7_sensor_x,
            mbc7_sensor_y: self.mbc7_sensor_y,
            cam_image: std::mem::take(&mut self.cam_image),
            // Host plumbing.
            rom_path: self.rom_path.take(),
            save_file: self.save_file.take(),
            rtc_file: self.rtc_file.take(),
            rtc_memory: std::mem::take(&mut self.rtc_memory),
            rtc_memory_synced: std::mem::take(&mut self.rtc_memory_synced),
            rocket_boot_logo: self.rocket_boot_logo,
            host_managed_saves: self.host_managed_saves,
            sram_cs_lazy: self.sram_cs_lazy,
            ..fresh
        };
        *self = carried;
    }



    /// The 48 header-logo bytes the boot ROM would decompress into the VRAM
    /// tiles at $8010: normally the cart's own $0104-$0133, or the locked-mapper
    /// substitution for Sachen MMC1 (`boot_logo_override`). Read straight from
    /// `rom_data` (no bus side effects) so skip_bios never perturbs mapper state.
    pub(crate) fn boot_logo_bytes(&self) -> [u8; 48] {
        if let Some(logo) = self.boot_logo_override() {
            return logo;
        }
        let mut out = [0u8; 48];
        for (i, b) in out.iter_mut().enumerate() {
            *b = self.rom_data.get(0x104 + i).copied().unwrap_or(0xFF);
        }
        out
    }



    /// The 48-byte logo bitmap held in the cartridge header ($0104-$0133). This
    /// is the loaded ROM's own data; the CGB boot ROM copies it through HRAM
    /// while verifying it, so `Mmio` reuses it to reconstruct the post-boot HRAM
    /// residue instead of embedding the bitmap. `None` if the ROM is too short.
    pub(crate) fn header_logo(&self) -> Option<[u8; 48]> {
        let slice = self.rom_data.get(0x0104..0x0134)?;
        let mut logo = [0u8; 48];
        logo.copy_from_slice(slice);
        Some(logo)
    }



}

impl memory::Addressable for Cartridge {
    fn read(&self, addr: u16) -> u8 {
        // Unlicensed-board read front-end: Sachen boot lock + $01xx address
        // descramble, Rocket boot lock + logo window. Licensed carts
        // (UnlMapper::None) skip this entirely.
        let mut addr = addr;
        match &self.unl_mapper {
            UnlMapper::SachenMmc1 if addr < 0x8000 => {
                addr = self.sachen_read_addr(addr, false);
            }
            UnlMapper::SachenMmc2 if addr < 0x8000 => {
                addr = self.sachen_read_addr(addr, true);
            }
            UnlMapper::Rocket if addr < 0x8000 => {
                // Advances the lock counter; presents the boot ROM's logo during
                // the locked-CGB window so the boot ROM's check passes.
                if let Some(byte) = self.rocket_locked_logo(addr) {
                    return byte;
                }
            }
            UnlMapper::Vf001(st)
                if (EXTERNAL_RAM_START..=EXTERNAL_RAM_END).contains(&addr) =>
            {
                // Protection value readback through the cart-RAM window;
                // unmatched reads fall through to normal MBC5 RAM.
                if let Some(byte) = Self::vf001_protection_read(*st, addr) {
                    return byte;
                }
            }
            // General VF001: byte-sequence injection + bank-0 partial
            // replacement, applied to ROM reads ($0000-$7FFF). Returns the
            // protection byte when active, else falls through to a normal read.
            UnlMapper::Vf001Gen(st) if addr < 0x8000 => {
                if let Some(byte) = self.vf001g_read(st, addr) {
                    return byte;
                }
            }
            // Zook Z challenge-response: the board answers through the cart-RAM
            // window. Unrecognised streams fall through to normal MBC5 RAM.
            UnlMapper::Vf001Zook(st)
                if (EXTERNAL_RAM_START..=EXTERNAL_RAM_END).contains(&addr) =>
            {
                if let Some(byte) = Self::vf001z_read(st, addr) {
                    return byte;
                }
            }
            // 8 KiB dual-window banking: the switchable area is two independent
            // 8 KiB pages, so it cannot go through the 16 KiB `rom_bank_bases`
            // path at all.
            UnlMapper::Vf8k(st)
                if (mmio::CARTRIDGE_BANK_START..=mmio::CARTRIDGE_BANK_END).contains(&addr) =>
            {
                return self.vf8k_read(*st, addr);
            }
            // Action Replay V4: the switchable area is an 8 KiB ROM page plus
            // an 8 KiB SRAM bank, so it cannot go through the 16 KiB
            // `rom_bank_bases` path at all.
            UnlMapper::ActionReplayV4(st)
                if (mmio::CARTRIDGE_BANK_START..=mmio::CARTRIDGE_BANK_END).contains(&addr) =>
            {
                return self.arv4_read(*st, addr);
            }
            // Xploder GB: both windows come off the board's own bank registers,
            // and its RAM has no enable gate, so neither can go through the
            // generic MBC arms.
            UnlMapper::XploderGb(st) if addr >= mmio::CARTRIDGE_BANK_START => {
                if let Some(byte) = self.xploder_read(*st, addr) {
                    return byte;
                }
            }
            // "New GB Color" HK PCB: while the bank register holds $80 or more
            // the switchable window is the protection chip, not ROM.
            UnlMapper::NewGbHk
                if (mmio::CARTRIDGE_BANK_START..=mmio::CARTRIDGE_BANK_END).contains(&addr) =>
            {
                if let Some(byte) = self.newgbhk_read(addr) {
                    return byte;
                }
            }
            // NT "new": once armed, the switchable area is two independent
            // 8 KiB pages, so it cannot go through the 16 KiB `rom_bank_bases`
            // path at all. Un-armed it falls through to the plain MBC5 arm.
            UnlMapper::NtNew(st)
                if st.split
                    && (mmio::CARTRIDGE_BANK_START..=mmio::CARTRIDGE_BANK_END).contains(&addr) =>
            {
                return self.ntnew_read(*st, addr);
            }
            // Adder protection: while the bank register is out of ROM range the
            // switchable window answers with the operand sum instead of ROM.
            UnlMapper::VfAdder(st)
                if (mmio::CARTRIDGE_BANK_START..=mmio::CARTRIDGE_BANK_END).contains(&addr) =>
            {
                if let Some(byte) = self.vfadder_read(*st, addr) {
                    return byte;
                }
            }
            // PKJD: the D/E/F protection registers answer through the cart-RAM
            // window; unmatched selectors fall through to normal MBC3 SRAM.
            UnlMapper::PokeJadeDia(_)
                if (EXTERNAL_RAM_START..=EXTERNAL_RAM_END).contains(&addr) =>
            {
                if let Some(byte) = self.pokejade_read(addr) {
                    return byte;
                }
            }
            _ => {}
        }
        match addr {
            // ROM Bank 0 (0x0000-0x3FFF). Fixed to bank 0 except on MBC1 in
            // banking mode 1, where BANK2 also selects this region.
            mmio::CARTRIDGE_START..=mmio::CARTRIDGE_END => {
                let offset = (addr - mmio::CARTRIDGE_START) as usize + self.rom_bank_bases().0;
                if offset < self.rom_data.len() {
                    self.rom_data[offset]
                } else {
                    0xFF
                }
            }
            // ROM Bank 1-N (switchable)
            mmio::CARTRIDGE_BANK_START..=mmio::CARTRIDGE_BANK_END => {
                // MBC6 splits this window into two independently banked 8 KiB
                // halves, either of which can be showing the flash chip, so it
                // cannot go through the single 16 KiB base at all.
                if let Mapper::Mbc6(m) = &self.mapper {
                    return self.mbc6_rom_read(&m.state, addr);
                }
                let offset =
                    (addr - mmio::CARTRIDGE_BANK_START) as usize + self.rom_bank_bases().1;
                let byte = if offset < self.rom_data.len() {
                    self.rom_data[offset]
                } else {
                    0xFF
                };
                // Vast Fame boards descramble the switchable $4000-$7FFF window
                // on read (mGBA `_GB*Read`); bank 0 (the fixed arm above) is
                // never scrambled.
                match &self.unl_mapper {
                    UnlMapper::Bbd(st) => {
                        Self::reorder_bits(byte, &BBD_DATA_REORDERING[st.data_swap_mode as usize])
                    }
                    UnlMapper::Ggb81(mode) => {
                        Self::reorder_bits(byte, &GGB81_DATA_REORDERING[usize::from(*mode)])
                    }
                    // Sintax XORs the switchable window with the active per-bank
                    // key (mGBA `_GBSintaxRead`).
                    UnlMapper::Sintax(st) => byte ^ st.rom_bank_xor,
                    // HITEK bit-reorders the switchable window through the
                    // boot-programmed data-swap mode (mGBA `_GBHitekRead`).
                    UnlMapper::Hitek(st) => {
                        Self::reorder_bits(byte, &HITEK_DATA_REORDERING[(st.data_swap_mode & 7) as usize])
                    }
                    _ => byte,
                }
            }
            // External RAM
            EXTERNAL_RAM_START..=EXTERNAL_RAM_END => {
                match &self.mapper {
                    Mapper::Mbc1(m) if m.has_ram => {
                        if m.ram_enabled
                            && let Some(offset) = self.banked_ram_offset(addr)
                        {
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    Mapper::Mbc2(m) => {
                        // MBC2 has built-in 512x4 RAM. The 512 nibbles echo every
                        // 0x200 bytes across the whole 0xA000-0xBFFF window. Only
                        // the low 4 data bits are stored; the upper 4 read back as
                        // 1s (open data lines), so reads return 0xF0 | nibble.
                        if m.ram_enabled {
                            let offset = (addr - MBC2_RAM_START) as usize % self.mbc2_ram.len();
                            0xF0 | (self.mbc2_ram[offset] & 0x0F)
                        } else {
                            0xFF
                        }
                    }
                    Mapper::Mbc3(m) if m.has_ram => {
                        if m.ram_enabled {
                            // MBC30 wires a third RAM-bank bit: selects 0x00-0x07
                            // are RAM there, 0x00-0x03 on plain MBC3. 0x08-0x0C
                            // are the RTC registers on both.
                            let ram_select_max = if m.is_mbc30(self.geom()) { 0x07 } else { 0x03 };
                            if m.ram_bank <= ram_select_max {
                                // RAM bank access
                                if let Some(offset) = self.banked_ram_offset(addr) {
                                    self.ram_data[offset]
                                } else {
                                    0xFF
                                }
                            } else if (0x08..=0x0C).contains(&m.ram_bank) {
                                // RTC register access
                                self.read_rtc_register(m.ram_bank)
                            } else {
                                0xFF
                            }
                        } else {
                            0xFF
                        }
                    }
                    Mapper::Mbc3(m) if m.timer => {
                        // Timer-only MBC3 (no RAM)
                        if m.ram_enabled && (0x08..=0x0C).contains(&m.ram_bank) {
                            self.read_rtc_register(m.ram_bank)
                        } else {
                            0xFF
                        }
                    }
                    Mapper::Mbc5(m) if m.has_ram => {
                        if m.ram_enabled
                            && let Some(offset) = self.banked_ram_offset(addr)
                        {
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    // Vast Fame VF001 is electrically MBC5+RAM; its protection
                    // reads are served by the front-end above, so a fall-through
                    // read here is plain cart RAM.
                    Mapper::Vf001(m) => {
                        if m.ram_enabled
                            && let Some(offset) = self.banked_ram_offset(addr)
                        {
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    // LiCheng is electrically MBC5+RAM; RAM reads are plain.
                    Mapper::LiCheng(m) => {
                        if m.ram_enabled
                            && let Some(offset) = self.banked_ram_offset(addr)
                        {
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    // BBD is electrically MBC5+RAM; RAM reads are plain (only
                    // the ROM read path is descrambled).
                    Mapper::Bbd(m) => {
                        if m.ram_enabled
                            && let Some(offset) = self.banked_ram_offset(addr)
                        {
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    // GGB81 is electrically MBC5+RAM; RAM reads are plain.
                    Mapper::Ggb81(m) => {
                        if m.ram_enabled
                            && let Some(offset) = self.banked_ram_offset(addr)
                        {
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    // Sintax is electrically MBC5+RAM; RAM reads are plain (only
                    // the ROM read path is XOR-descrambled).
                    Mapper::Sintax(m) => {
                        if m.ram_enabled
                            && let Some(offset) = self.banked_ram_offset(addr)
                        {
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    // HITEK is electrically MBC5+RAM; RAM reads are plain (only
                    // the ROM read path is reordered).
                    Mapper::Hitek(m) => {
                        if m.ram_enabled
                            && let Some(offset) = self.banked_ram_offset(addr)
                        {
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    Mapper::Mbc7(m) => {
                        // MBC7 exposes registers, not RAM. They only respond
                        // when BOTH enable stages are unlocked, and only in
                        // A000-AFFF (B000-BFFF just reads 0xFF). The register
                        // is selected by address bits 4-7; bits 0-3 and 8-11
                        // are ignored.
                        if m.ram_enabled && m.state.ram_enabled2 && addr < 0xB000 {
                            match (addr >> 4) & 0x0F {
                                0x2 => (m.state.accel_x & 0xFF) as u8,
                                0x3 => (m.state.accel_x >> 8) as u8,
                                0x4 => (m.state.accel_y & 0xFF) as u8,
                                0x5 => (m.state.accel_y >> 8) as u8,
                                // Ax6x always reads 0x00 (possibly a reserved
                                // Z axis); Ax7x always 0xFF.
                                0x6 => 0x00,
                                0x8 => m.state.eeprom.pin_state(),
                                // Ax0x/Ax1x are write-only (latch control),
                                // Ax7x and Ax9x-AxFx read 0xFF.
                                _ => 0xFF,
                            }
                        } else {
                            0xFF
                        }
                    }
                    Mapper::HuC1(m) => {
                        if m.state.ir_mode {
                            // IR receiver: 0xC1 = light seen, 0xC0 = no light
                            // (Pan Docs HuC1). No IR transport is modeled, so
                            // this always reads the documented idle 0xC0.
                            0xC0
                        } else if let Some(offset) = self.banked_ram_offset(addr) {
                            // RAM is always enabled (no MBC1-style gate).
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    Mapper::Camera(m) => {
                        if m.state.regs_selected {
                            // Register file, mirrored every $80. Only A000 is
                            // readable: bits 1-2 are the stored 1-D filter
                            // set, bit 0 is the live capture-busy flag; bits
                            // 3-7 read '0'. All other registers read $00.
                            if (addr & 0x7F) == 0 {
                                (m.state.regs[0] & 0x06) | (m.state.running as u8)
                            } else {
                                0x00
                            }
                        } else if m.state.running {
                            // "When the capture process is active all RAM
                            // banks will return 00h when read."
                            0x00
                        } else if let Some(offset) = self.banked_ram_offset(addr) {
                            // No read gate: RAM reads are always enabled.
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    Mapper::HuC3(m) => {
                        match m.state.mode {
                            // 0x0 = RAM read-only, 0xA = RAM read/write; both
                            // read the banked external RAM.
                            0x0 | 0xA => {
                                if let Some(offset) = self.banked_ram_offset(addr) {
                                    self.ram_data[offset]
                                } else {
                                    0xFF
                                }
                            }
                            // RTC command/response: bits 6-4 echo the last
                            // command written to the mailbox, bits 3-0 hold
                            // the result of the last executed command. D7 is
                            // not driven by the chip (open bus, usually
                            // high).
                            0xC => 0x80 | (m.state.rtc_command << 4) | m.state.rtc_result,
                            // RTC semaphore: bit 0 high = MCU ready. Modeled
                            // as always ready (instant execution). Bits 7-1
                            // are not specified; 0 matches observed software
                            // expectations.
                            0xD => 0x01,
                            // IR receiver stub: 0xC0 = no light seen (same
                            // idle value as HuC-1's IR register). Full IR
                            // link emulation is out of scope.
                            0xE => 0xC0,
                            // 0xB is the write-only command mailbox; other
                            // select values are unmapped. Reads are open bus.
                            _ => 0xFF,
                        }
                    }
                    // TAMA5 exposes its whole register file here, not RAM.
                    Mapper::Tama5(m) => self.tama5_read(addr, &m.state),
                    // MBC6 banks the two 4 KiB halves of this window
                    // independently, so it cannot use `banked_ram_offset`.
                    Mapper::Mbc6(m) => self.mbc6_ram_read(&m.state, addr),
                    Mapper::NoMbc(_) => {
                        // Pan Docs "No MBC": optional RAM (up to 8KB) is wired
                        // straight through at A000-BFFF -- no banking, no
                        // enable gate. A smaller chip mirrors across the
                        // window (address modulo its size).
                        if let Some(offset) = self.unbanked_ram_offset(addr) {
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    // Rocket/Sachen boards wire any RAM straight through with
                    // no enable gate (RAM is mapped unconditionally).
                    Mapper::Rocket(_) | Mapper::Sachen(_) => {
                        if let Some(offset) = self.unbanked_ram_offset(addr) {
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    // NT/Makon old boards gate RAM MBC3-style ($0A to
                    // $0000-$1FFF), unbanked.
                    Mapper::NtOld(m) if m.ram_enabled => {
                        if let Some(offset) = self.unbanked_ram_offset(addr) {
                            self.ram_data[offset]
                        } else {
                            0xFF
                        }
                    }
                    _ => 0xFF,
                }
            }
            _ => 0xFF,
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        // Any write can change bank-register state; drop the ROM-base cache
        // unconditionally (recomputing once per write is trivial next read).
        self.rom_bank_cache.set(None);
        match addr {
            // MBC2 register block (0x0000-0x3FFF). MBC2 has a SINGLE register
            // region here, selected by address bit 8: bit8==0 => RAMG (RAM
            // enable), bit8==1 => ROMB (ROM bank, low 4 bits). The 0x2000
            // boundary is irrelevant on MBC2 -- only bit 8 matters -- so handle
            // the whole range here before the generic per-quarter arms.
            RAM_ENABLE_START..=ROM_BANK_SELECT_END if matches!(self.mapper, Mapper::Mbc2(_)) => {
                if let Mapper::Mbc2(m) = &mut self.mapper {
                    if (addr & 0x0100) == 0 {
                        m.ram_enabled = (value & 0x0F) == 0x0A;
                    } else {
                        m.rom_bank_low = (value & 0x0F).max(1);
                    }
                }
            }
            // MBC6: its register file is decoded at 1 KiB granularity and does
            // not line up with any other board's quarters, and $4000-$7FFF is
            // the flash chip's own command bus rather than dead ROM space, so
            // the whole cart window is intercepted before the generic arms.
            RAM_ENABLE_START..=BANKING_MODE_END if matches!(self.mapper, Mapper::Mbc6(_)) => {
                self.mbc6_write(addr, value);
            }
            // Wisdom Tree: a single '377 latch loaded from the ADDRESS lines on
            // any $0000-$3FFF write; the data byte is ignored (bank = addr & 0x3F).
            RAM_ENABLE_START..=ROM_BANK_SELECT_END
                if matches!(self.mapper, Mapper::WisdomTree(_)) =>
            {
                if let Mapper::WisdomTree(m) = &mut self.mapper {
                    m.bank = (addr & 0x3F) as u8;
                }
            }
            // M161: the FIRST write anywhere in the whole $0000-$7FFF ROM area
            // latches the 32KB bank from data bits 0-2; later writes are ignored.
            RAM_ENABLE_START..=BANKING_MODE_END if matches!(self.mapper, Mapper::M161(_)) => {
                if let Mapper::M161(m) = &mut self.mapper
                    && !m.state.mapped {
                        m.state.bank = (value & 7) << 1;
                        m.state.mapped = true;
                    }
            }
            // Sintax (Vast Fame): the entire $0000-$7FFF register protocol is
            // intercepted here -- bank writes are bit-reordered, $5x1x selects
            // the reorder mode, $7xxx programs the XOR bytes, and $0000-$1FFF /
            // $4000-$5FFF behave as plain MBC5 RAM-enable / RAM-bank (mGBA
            // `_GBSintax`). Placed before the generic arms so the reorder is not
            // bypassed; scoped to $0000-$7FFF so cart RAM ($A000-$BFFF) still
            // flows to the external-RAM arm below.
            RAM_ENABLE_START..=BANKING_MODE_END
                if matches!(self.unl_mapper, UnlMapper::Sintax(_)) =>
            {
                self.sintax_write(addr, value);
            }
            // NT "new": the arming write sits inside MBC5's RAM-enable block and
            // the two page registers inside its ROM-bank block. Only the writes
            // the board actually claims are taken here; everything else falls
            // through to the plain MBC5 arms below.
            RAM_ENABLE_START..=ROM_BANK_SELECT_END
                if matches!(self.unl_mapper, UnlMapper::NtNew(st) if st.claims(addr, value)) =>
            {
                self.ntnew_write(addr, value);
            }
            // HITEK (Vast Fame): the whole $0000-$7FFF register space follows
            // mGBA `_GBHitek` (the two swap-mode ports layered over MBC5 write
            // semantics), so it is handled before the generic arms.
            RAM_ENABLE_START..=BANKING_MODE_END
                if matches!(self.unl_mapper, UnlMapper::Hitek(_)) =>
            {
                self.hitek_write(addr, value);
            }
            // Vast Fame 8 KiB dual-window: the whole ROM-bank register block
            // programs the two 8 KiB page registers instead of one 16 KiB bank
            // (A10 picks the page), so it is intercepted before the generic
            // MBC5 arm. Every other window falls through unchanged.
            ROM_BANK_SELECT_START..=ROM_BANK_SELECT_END
                if matches!(self.unl_mapper, UnlMapper::Vf8k(_)) =>
            {
                self.vf8k_write(addr, value);
            }
            // Action Replay V4: the whole cart window is the board's own, not
            // an MBC's. $6000-$7FFF is SRAM (with the two bank registers at the
            // top of it) and everything below is inert, so this is intercepted
            // before every generic arm.
            RAM_ENABLE_START..=BANKING_MODE_END
                if matches!(self.unl_mapper, UnlMapper::ActionReplayV4(_)) =>
            {
                self.arv4_write(addr, value);
            }
            // Xploder GB: its two bank registers live in the first bytes of the
            // ROM window, nowhere near an MBC's register blocks, so the whole
            // cart window is intercepted before every generic arm.
            RAM_ENABLE_START..=BANKING_MODE_END
                if matches!(self.unl_mapper, UnlMapper::XploderGb(_)) =>
            {
                self.xploder_write(addr, value);
            }
            // RAM Enable (0x0000-0x1FFF)
            RAM_ENABLE_START..=RAM_ENABLE_END => match &mut self.mapper {
                Mapper::Mbc1(m) => m.ram_enabled = (value & 0x0F) == 0x0A,
                Mapper::Mbc3(m) => m.ram_enabled = (value & 0x0F) == 0x0A,
                Mapper::Mbc5(m) => m.ram_enabled = (value & 0x0F) == 0x0A,
                Mapper::Vf001(m) => m.ram_enabled = (value & 0x0F) == 0x0A,
                Mapper::LiCheng(m) => m.ram_enabled = (value & 0x0F) == 0x0A,
                Mapper::Bbd(m) => m.ram_enabled = (value & 0x0F) == 0x0A,
                Mapper::Ggb81(m) => m.ram_enabled = (value & 0x0F) == 0x0A,
                // MBC7 stage-1 unlock (stage 2 is 0x40 to 0x4000-0x5FFF).
                Mapper::Mbc7(m) => m.ram_enabled = (value & 0x0F) == 0x0A,
                // HuC1 IR select: $0E maps the IR transceiver, else RAM (no disable).
                Mapper::HuC1(m) => m.state.ir_mode = (value & 0x0F) == 0x0E,
                // HuC3 RAM/RTC/IR select (low nibble).
                Mapper::HuC3(m) => m.state.mode = value & 0x0F,
                // Pocket Camera gates RAM WRITES only.
                Mapper::Camera(m) => m.ram_enabled = (value & 0x0F) == 0x0A,
                // Sachen base ROM bank, latched only while inner bank bits 4-5 are set.
                Mapper::Sachen(m) => {
                    if (m.state.bank & 0x30) == 0x30 {
                        m.state.base = value;
                    }
                }
                Mapper::NtOld(m) => m.ram_enabled = (value & 0x0F) == 0x0A,
                _ => {}
            },
            // LiCheng/Niutoude: the games spray garbage bank numbers across
            // $2101-$2FFF that would corrupt MBC5's low-8 bank register; the
            // board drops them (mGBA `_GBLiCheng`). $2000-$2100 (low bank) and
            // $3000-$3FFF (high bit) still latch through the generic arm below.
            ROM_BANK_SELECT_START..=ROM_BANK_SELECT_END
                if self.unl_mapper == UnlMapper::LiCheng
                    && (0x2101..=0x2FFF).contains(&addr) => {}
            // BBD (Vast Fame): the $2000/$2001/$2080 protocol reorders the
            // written bank number and latches the data/bank swap modes, then
            // behaves as MBC5 for the ROM-bank register (mGBA `_GBBBD`).
            ROM_BANK_SELECT_START..=ROM_BANK_SELECT_END
                if matches!(self.unl_mapper, UnlMapper::Bbd(_)) =>
            {
                self.bbd_write(addr, value);
            }
            // GGB81 (Vast Fame): a write with addr & 0xF0FF == 0x2001 latches
            // the 3-bit data-swap mode, then the raw value latches into the
            // MBC5 low/high bank register (mGBA `_GBGGB81`).
            ROM_BANK_SELECT_START..=ROM_BANK_SELECT_END
                if matches!(self.unl_mapper, UnlMapper::Ggb81(_)) =>
            {
                self.ggb81_write(addr, value);
            }
            // ROM Bank Number (0x2000-0x3FFF)
            ROM_BANK_SELECT_START..=ROM_BANK_SELECT_END => match &mut self.mapper {
                Mapper::Mbc1(m) => m.rom_bank_low = (value & 0x1F).max(1), // 5 bits, min 1
                // 7/8 bits, min 1; stored raw, get_rom_bank applies the wired width.
                Mapper::Mbc3(m) => m.rom_bank_low = value.max(1),
                Mapper::Mbc5(m) => {
                    if addr <= 0x2FFF {
                        m.regs.rom_bank_low = value; // low 8 bits; bank 0 allowed
                    } else {
                        m.regs.rom_bank_high = value & 0x01; // upper 1 bit
                    }
                }
                Mapper::Vf001(m) => {
                    if addr <= 0x2FFF {
                        m.regs.rom_bank_low = value;
                    } else {
                        m.regs.rom_bank_high = value & 0x01;
                    }
                }
                // LiCheng is plain MBC5 here; the $2101-$2FFF garbage-write
                // ignore is a guarded arm above this match.
                Mapper::LiCheng(m) => {
                    if addr <= 0x2FFF {
                        m.regs.rom_bank_low = value;
                    } else {
                        m.regs.rom_bank_high = value & 0x01;
                    }
                }
                Mapper::Mbc7(m) => m.state.rom_bank = value, // like MBC5, bank 0 allowed
                Mapper::HuC1(m) => m.state.rom_bank = value & 0x3F, // 6-bit
                Mapper::HuC3(m) => m.state.rom_bank = value & 0x7F, // 7-bit
                Mapper::Camera(m) => m.state.rom_bank = value & 0x3F, // 6-bit
                // Rocket: two EXACT register addresses; everything else ignored.
                Mapper::Rocket(m) => match addr {
                    0x3F00 => m.state.rom_bank = value.max(1), // inner 16KB bank, 0->1
                    0x3FC0 => m.state.outer = value,           // outer 256KB bank
                    _ => {}
                },
                // Sachen inner ("unmasked") bank register, 0 maps to 1.
                Mapper::Sachen(m) => m.state.bank = value.max(1),
                // NT v1 is 5-bit, v2 8-bit; both remap 0 to 1. $5003 swap applies
                // combinationally in get_rom_bank.
                Mapper::NtOld(m) => {
                    let bank = if m.v2 { value } else { value & 0x1F };
                    m.state.bank = bank.max(1);
                }
                _ => {}
            },
            // PKJD: a $4000-$5FFF write latches the protection register selector
            // AND drives MBC3's own RAM/RTC-bank register (hhugboy sets
            // notRtcRegister, then falls through to MbcNin3), so do both here.
            RAM_BANK_ROM_BANK_HIGH_START..=RAM_BANK_ROM_BANK_HIGH_END
                if matches!(self.unl_mapper, UnlMapper::PokeJadeDia(_)) =>
            {
                if let UnlMapper::PokeJadeDia(ref mut st) = self.unl_mapper {
                    st.sel = value;
                }
                if let Mapper::Mbc3(m) = &mut self.mapper {
                    m.ram_bank = value & 0x0F;
                }
            }
            // Adder protection: while the bank register is out of ROM range a
            // $4000-$5FFF write latches a protection operand instead of the RAM
            // bank (A1 selects which). Otherwise it is a plain MBC5 RAM-bank
            // write, handled by the generic arm below.
            RAM_BANK_ROM_BANK_HIGH_START..=RAM_BANK_ROM_BANK_HIGH_END
                if matches!(self.unl_mapper, UnlMapper::VfAdder(_))
                    && self.vfadder_armed() =>
            {
                if let UnlMapper::VfAdder(ref mut st) = self.unl_mapper {
                    if addr & 0x02 == 0 {
                        st.x = value;
                    } else {
                        st.y = value;
                    }
                }
            }
            // RAM Bank Number / Upper ROM Bank Number (0x4000-0x5FFF)
            RAM_BANK_ROM_BANK_HIGH_START..=RAM_BANK_ROM_BANK_HIGH_END => {
                match &mut self.mapper {
                    Mapper::Mbc1(m) => m.bank2 = value & 0x03, // 2 bits
                    Mapper::Mbc3(m) => m.ram_bank = value & 0x0F, // 4-bit RAM/RTC select
                    Mapper::Mbc5(m) => {
                        if m.rumble {
                            // Bit 3 drives the motor; low 3 bits select the RAM bank.
                            m.rumble_motor = (value & 0x08) != 0;
                        }
                        m.regs.ram_bank = value; // 4 bits used
                    }
                    Mapper::Vf001(m) => m.regs.ram_bank = value,
                    Mapper::LiCheng(m) => m.regs.ram_bank = value,
                    Mapper::Bbd(m) => m.regs.ram_bank = value,
                    Mapper::Ggb81(m) => m.regs.ram_bank = value,
                    // MBC7 stage-2 unlock: exactly 0x40 enables.
                    Mapper::Mbc7(m) => m.state.ram_enabled2 = value == 0x40,
                    Mapper::HuC1(m) => m.state.ram_bank = value,
                    Mapper::HuC3(m) => m.state.ram_bank = value,
                    Mapper::Camera(m) => {
                        // Bit 4 maps the CAM register file; else low 4 bits = RAM bank.
                        if value & 0x10 != 0 {
                            m.state.regs_selected = true;
                        } else {
                            m.state.regs_selected = false;
                            m.state.ram_bank = value & 0x0F;
                        }
                    }
                    // Sachen ROM bank mask, latched only while inner bits 4-5 are set.
                    Mapper::Sachen(m) => {
                        if (m.state.bank & 0x30) == 0x30 {
                            m.state.mask = value;
                        }
                    }
                    // NT/Makon mode registers live in $5000-$5FFF, decoded by A0-A1;
                    // $4000-$4FFF is ignored (v2 rumble data bits aren't wired here).
                    Mapper::NtOld(m) if (addr & 0xF000) == 0x5000 => match addr & 0x03 {
                        0x01 => m.state.base = value & 0x3F, // multicart base, 32KB units
                        0x02 => {
                            // Low nibble selects the sub-game bank-count mask.
                            m.state.bank_mask = match value & 0x0F {
                                0x00 => 31, // 512KB
                                0x08 => 15, // 256KB
                                0x0C => 7,  // 128KB
                                0x0E => 3,  // 64KB
                                0x0F => 1,  // 32KB
                                _ => 31,
                            };
                        }
                        0x03 => m.state.swapped = (value & 0x10) != 0, // bank-line swap (bit 4)
                        _ => {}
                    },
                    _ => {}
                }
                // NT $5002 high nibble $Ex declares 8KB cart RAM (the header says
                // none). Done after the mapper borrow so it can grow ram_data.
                if matches!(self.mapper, Mapper::NtOld(_))
                    && (addr & 0xF000) == 0x5000
                    && (addr & 0x03) == 0x02
                    && (value & 0xF0) == 0xE0
                    && self.ram_data.is_empty()
                {
                    self.ram_data = vec![0xFF; 0x2000];
                    self.ram_banks = 1;
                }
            }
            // VF001 protection register file lives in the (MBC5-unused)
            // $6000-$7FFF range; A10-A11 select the port.
            BANKING_MODE_START..=BANKING_MODE_END
                if matches!(self.unl_mapper, UnlMapper::Vf001(_)) =>
            {
                self.vf001_write(addr, value);
            }
            // General VF001 config register file: the $6000-$7FFF accumulator +
            // sequence/replacement activation (taizou `MbcUnlVf001::writeMemory`).
            BANKING_MODE_START..=BANKING_MODE_END
                if matches!(self.unl_mapper, UnlMapper::Vf001Gen(_)) =>
            {
                self.vf001g_write(addr, value);
            }
            // Zook Z challenge-response port: every $6000-$7FFF write feeds the
            // protection stream (and can complete a bank select).
            BANKING_MODE_START..=BANKING_MODE_END
                if matches!(self.unl_mapper, UnlMapper::Vf001Zook(_)) =>
            {
                self.vf001z_write(addr, value);
            }
            // Gowin: the MBC1 banking-mode port is repurposed as the two-write
            // outer-bank handshake (parameter then commit strobe), so it is
            // intercepted before the generic MBC1 mode-select arm below.
            BANKING_MODE_START..=BANKING_MODE_END
                if matches!(self.unl_mapper, UnlMapper::Gowin(_)) =>
            {
                self.gowin_write(value);
            }
            // Banking Mode Select (0x6000-0x7FFF)
            BANKING_MODE_START..=BANKING_MODE_END => {
                // MBC3 timer carts latch the RTC on ANY write here (no edge
                // needed); MBC1 sets the banking mode bit; others ignore it.
                let latch = match &mut self.mapper {
                    Mapper::Mbc1(m) => {
                        m.mode = value & 0x01;
                        false
                    }
                    Mapper::Mbc3(m) => m.timer,
                    _ => false,
                };
                if latch {
                    self.latch_rtc();
                }
            }
            // PKJD: the $A000-$BFFF window is the D/E/F protection port when a
            // protection selector is active, else plain MBC3 SRAM/RTC.
            EXTERNAL_RAM_START..=EXTERNAL_RAM_END
                if matches!(self.unl_mapper, UnlMapper::PokeJadeDia(_)) =>
            {
                self.pokejade_write(addr, value);
            }
            // Xploder GB: banked by its own $0007 register and never gated.
            EXTERNAL_RAM_START..=EXTERNAL_RAM_END
                if matches!(self.unl_mapper, UnlMapper::XploderGb(_)) =>
            {
                if let Some(offset) = self.xploder_ram_offset(addr) {
                    let _ = self.write_ram_byte(offset, value);
                }
            }
            // External RAM (0xA000-0xBFFF)
            EXTERNAL_RAM_START..=EXTERNAL_RAM_END => {
                // Snapshot the board + its A000-BFFF-relevant registers from an
                // immutable view; the borrow is released before any &mut self
                // engine call (write_ram_byte / write_rtc_register / cam / huc3).
                enum Ext {
                    Banked(bool),
                    Mbc2(bool),
                    Mbc3Ram(bool, u8),
                    Mbc3Rtc(bool, u8),
                    Mbc7(bool),
                    HuC1(bool),
                    Camera(bool, bool),
                    HuC3(u8),
                    Tama5,
                    Mbc6,
                    Unbanked,
                    Nt(bool),
                    None,
                }
                let plan = match &self.mapper {
                    Mapper::Mbc1(m) => Ext::Banked(m.has_ram && m.ram_enabled),
                    Mapper::Mbc2(m) => Ext::Mbc2(m.ram_enabled),
                    Mapper::Mbc3(m) if m.has_ram => Ext::Mbc3Ram(m.ram_enabled, m.ram_bank),
                    Mapper::Mbc3(m) => Ext::Mbc3Rtc(m.ram_enabled && m.timer, m.ram_bank),
                    Mapper::Mbc5(m) => Ext::Banked(m.has_ram && m.ram_enabled),
                    Mapper::Vf001(m) => Ext::Banked(m.ram_enabled),
                    Mapper::LiCheng(m) => Ext::Banked(m.ram_enabled),
                    Mapper::Bbd(m) => Ext::Banked(m.ram_enabled),
                    Mapper::Ggb81(m) => Ext::Banked(m.ram_enabled),
                    Mapper::Sintax(m) => Ext::Banked(m.ram_enabled),
                    Mapper::Hitek(m) => Ext::Banked(m.ram_enabled),
                    Mapper::Mbc7(m) => Ext::Mbc7(m.ram_enabled && m.state.ram_enabled2),
                    Mapper::HuC1(m) => Ext::HuC1(m.state.ir_mode),
                    Mapper::Camera(m) => {
                        Ext::Camera(m.state.regs_selected, m.ram_enabled && !m.state.running)
                    }
                    Mapper::HuC3(m) => Ext::HuC3(m.state.mode),
                    Mapper::Tama5(_) => Ext::Tama5,
                    Mapper::Mbc6(_) => Ext::Mbc6,
                    Mapper::NoMbc(_) | Mapper::Rocket(_) | Mapper::Sachen(_) => Ext::Unbanked,
                    Mapper::NtOld(m) => Ext::Nt(m.ram_enabled),
                    _ => Ext::None,
                };
                match plan {
                    // Banked, RAMG-gated RAM (MBC1/MBC5/VF001). banked_ram_offset
                    // returns None when the board carries no RAM array.
                    Ext::Banked(true) => {
                        if let Some(offset) = self.banked_ram_offset(addr) {
                            let _ = self.write_ram_byte(offset, value);
                        }
                    }
                    // MBC2 built-in 512x4 RAM, echoing every 0x200 bytes.
                    Ext::Mbc2(true) => {
                        let offset = (addr - MBC2_RAM_START) as usize % self.mbc2_ram.len();
                        let _ = self.write_mbc2_ram_byte(offset, value);
                    }
                    Ext::Mbc3Ram(true, rb) => {
                        let ram_select_max = if self.is_mbc30() { 0x07 } else { 0x03 };
                        if rb <= ram_select_max {
                            if let Some(offset) = self.banked_ram_offset(addr) {
                                let _ = self.write_ram_byte(offset, value);
                            }
                        } else if (0x08..=0x0C).contains(&rb) {
                            self.write_rtc_register(rb, value);
                        }
                    }
                    // Timer-only MBC3 (no RAM): RTC registers only.
                    Ext::Mbc3Rtc(true, rb) if (0x08..=0x0C).contains(&rb) => {
                        self.write_rtc_register(rb, value);
                    }
                    // MBC7 registers respond only with both stages unlocked, in A000-AFFF.
                    Ext::Mbc7(true) => match (addr >> 4) & 0x0F {
                        0x0 => {
                            // Erase the accelerometer latch (re-arm re-latching).
                            if value == 0x55
                                && let Mapper::Mbc7(m) = &mut self.mapper {
                                    m.state.accel_x = 0x8000;
                                    m.state.accel_y = 0x8000;
                                    m.state.accel_latched = false;
                                }
                        }
                        0x1 => {
                            // Latch the current sample, only after an erase.
                            if value == 0xAA {
                                let (sx, sy) = (self.mbc7_sensor_x, self.mbc7_sensor_y);
                                if let Mapper::Mbc7(m) = &mut self.mapper
                                    && !m.state.accel_latched {
                                        m.state.accel_x = Self::mbc7_accel_counts(sx);
                                        m.state.accel_y = Self::mbc7_accel_counts(sy);
                                        m.state.accel_latched = true;
                                    }
                            }
                        }
                        0x8 => self.mbc7_eeprom_bus_write(value),
                        _ => {}
                    },
                    Ext::HuC1(ir) => {
                        if ir {
                            // IR transmitter: bit 0 drives the LED (latched, unobserved).
                            if let Mapper::HuC1(m) = &mut self.mapper {
                                m.state.ir_led = value & 0x01 != 0;
                            }
                        } else if let Some(offset) = self.banked_ram_offset(addr) {
                            // RAM is always enabled (no MBC1-style gate).
                            let _ = self.write_ram_byte(offset, value);
                        }
                    }
                    Ext::Camera(regs_selected, ram_ok) => {
                        if regs_selected {
                            // Register writes always enabled, mirrored every $80.
                            self.cam_reg_write(addr & 0x7F, value);
                        } else if ram_ok && let Some(offset) = self.banked_ram_offset(addr) {
                            // RAM writes need the $0A gate and are ignored mid-capture.
                            let _ = self.write_ram_byte(offset, value);
                        }
                    }
                    Ext::HuC3(mode) => match mode {
                        // RAM read/write. Mode 0x0 (read-only) ignores writes.
                        0xA => {
                            if let Some(offset) = self.banked_ram_offset(addr) {
                                let _ = self.write_ram_byte(offset, value);
                            }
                        }
                        // RTC command/argument mailbox (command bits 6-4, arg 3-0).
                        0xB => {
                            if let Mapper::HuC3(m) = &mut self.mapper {
                                m.state.rtc_command = (value >> 4) & 0x07;
                                m.state.rtc_argument = value & 0x0F;
                            }
                        }
                        // Semaphore: bit 0 clear requests the MCU execute the command.
                        0xD if value & 0x01 == 0 => self.huc3_execute_command(),
                        _ => {}
                    },
                    // TAMA5's register-file protocol (the board's only write
                    // port; it has none in $0000-$7FFF).
                    Ext::Tama5 => self.tama5_write(addr, value),
                    Ext::Mbc6 => self.mbc6_ram_write(addr, value),
                    // NoMBC / Rocket / Sachen: straight-through, ungated.
                    Ext::Unbanked => {
                        if let Some(offset) = self.unbanked_ram_offset(addr) {
                            let _ = self.write_ram_byte(offset, value);
                        }
                    }
                    // NT/Makon old: MBC3-style enable gate, unbanked.
                    Ext::Nt(true) => {
                        if let Some(offset) = self.unbanked_ram_offset(addr) {
                            let _ = self.write_ram_byte(offset, value);
                        }
                    }
                    _ => {}
                }
            }
            _ => {
                // Ignore writes to other areas (ROM is read-only)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::camera::{CAM_H, CAM_TILE_BYTES, CAM_W};
    use crate::memory::Addressable;

    /// Minimal in-memory ROM image with the given type/RAM-size header bytes.
    fn make_rom(cartridge_type: u8, ram_size_code: u8) -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[CARTRIDGE_TYPE_OFFSET] = cartridge_type;
        rom[ROM_SIZE_OFFSET] = 0x00;
        rom[RAM_SIZE_OFFSET] = ram_size_code;
        rom
    }

    // Synthetic 48-byte header-logo fixtures. Cartridge/unlicensed-mapper
    // detection keys ONLY on the 48-byte SUM (compared against the LOGO_SUM_*
    // constants), never on the individual bytes, so these stand-ins carry the
    // required sums without embedding any real (copyrighted) logo. Readback
    // assertions in the tests are self-consistent with whatever bytes these hold.
    const fn logo_with_sum(fill: u8, last: u8) -> [u8; 48] {
        let mut a = [fill; 48];
        a[47] = last;
        a
    }
    // Sum == LOGO_SUM_NINTENDO (5446): 47*0x71 + 0x87. Marks a "licensed" header.
    const LICENSED_LOGO: [u8; 48] = logo_with_sum(0x71, 0x87);
    // Sum == LOGO_SUM_ROCKET (2756): 47*0x39 + 0x4D. A Rocket cart's stored logo.
    const ROCKET_LOGO: [u8; 48] = logo_with_sum(0x39, 0x4D);

    /// Sized ROM with a bank-index marker at offset 0x1000 of every 16KB bank.
    fn make_sized_rom(cartridge_type: u8, rom_size_code: u8, size: usize) -> Vec<u8> {
        let mut rom = vec![0u8; size];
        rom[CARTRIDGE_TYPE_OFFSET] = cartridge_type;
        rom[ROM_SIZE_OFFSET] = rom_size_code;
        for bank in 0..(size / 0x4000) {
            rom[bank * 0x4000 + 0x1000] = bank as u8;
        }
        rom
    }

    // Sum == LOGO_SUM_VF001_LOH (4593): 47*0x60 + 0x51. The Vast Fame
    // secondary logo at $0184 on the Legend of Heroes board.
    const VF001_LOGO: [u8; 48] = logo_with_sum(0x60, 0x51);

    /// 1MB MBC5+RAM+BATTERY image carrying the VF001 detection signature: the
    /// secondary VF logo sum at $0184 and the boot protection stub at $32FC.
    /// This is the exact shape `detect_unl_mapper` keys on for Legend of Heroes.
    fn make_vf001_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x100000];
        rom[CARTRIDGE_TYPE_OFFSET] = MBC5_RAM_BATTERY;
        rom[ROM_SIZE_OFFSET] = 0x05; // 1MB / 64 banks
        rom[RAM_SIZE_OFFSET] = 0x02; // 8KB
        rom[0x184..0x1B4].copy_from_slice(&VF001_LOGO);
        rom[VF001_STUB_OFFSET..VF001_STUB_OFFSET + VF001_STUB.len()]
            .copy_from_slice(&VF001_STUB);
        rom
    }

    #[test]
    fn vf001_detects_only_with_logo_and_stub() {
        // Full signature -> VF001.
        let rom = make_vf001_rom();
        assert_eq!(
            Cartridge::detect_unl_mapper(&rom),
            UnlMapper::Vf001(Vf001State::default())
        );
        // Electrically an MBC5+RAM+BATTERY: the header type is truthful.
        let cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(
            cart.get_cartridge_type(),
            CartridgeType::MBC5 { ram: true, battery: true, .. }
        ));
        assert!(cart.has_battery());

        // Correct logo sum but no boot stub -> not VF001 (stays plain MBC5).
        let mut no_stub = make_vf001_rom();
        no_stub[VF001_STUB_OFFSET..VF001_STUB_OFFSET + VF001_STUB.len()].fill(0);
        assert_eq!(Cartridge::detect_unl_mapper(&no_stub), UnlMapper::None);

        // Stub present but the $0184 sum is wrong -> not VF001.
        let mut wrong_logo = make_vf001_rom();
        wrong_logo[0x184] = wrong_logo[0x184].wrapping_add(1);
        assert_eq!(Cartridge::detect_unl_mapper(&wrong_logo), UnlMapper::None);
    }

    // Clean-room 48-byte stand-in for the Vast Fame secondary logo: a plain
    // ASCII banner (44 bytes) plus a 4-byte suffix computed so the block's
    // CRC32 equals LICHENG_LOGO_CRC32[0] (0xD2B5_7657). No copyrighted logo
    // bytes; the same block is embedded in the licheng_banking test ROM.
    const LICHENG_LOGO: [u8; 48] =
        *b"RUSTYBOI LICHENG NIUTOUDE CLEANROOM STANDIN!\xC9\x37\x57\x41";

    /// 1MB image carrying the LiCheng signature: the MBC1 header lie ($01), the
    /// $0184 logo whose CRC32 keys detection, a real Nintendo logo, and bank
    /// markers at banks 1 and 0x21 (the latter only reachable via MBC5's 8-bit
    /// low bank register).
    fn make_licheng_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x100000]; // 64 banks
        rom[CARTRIDGE_TYPE_OFFSET] = 0x01; // MBC1 header (the lie)
        rom[ROM_SIZE_OFFSET] = 0x05; // 1MB
        rom[RAM_SIZE_OFFSET] = 0x01; // 2KB
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        rom[0x184..0x1B4].copy_from_slice(&LICHENG_LOGO);
        rom[0x4000] = 0xB1; // bank 1, offset 0
        rom[0x21 * 0x4000] = 0xA5; // bank 33, offset 0
        rom
    }

    #[test]
    fn licheng_detects_and_ignores_garbage_bank_writes() {
        let rom = make_licheng_rom();
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::LiCheng);

        // Electrically MBC5 despite the MBC1 header byte.
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(cart.get_cartridge_type(), CartridgeType::MBC5 { .. }));

        // MBC5 8-bit bank select: a single write of 0x21 reaches bank 33. A
        // plain MBC1 decode would 5-bit-mask it to bank 1 (0xB1).
        cart.write(0x2000, 0x21);
        assert_eq!(cart.read(0x4000), 0xA5);

        // Garbage bank write inside the ignored $2101-$2FFF window: the board
        // drops it, so bank 33 stays selected (a plain MBC5 would clobber it).
        cart.write(0x2500, 0xC3);
        assert_eq!(cart.read(0x4000), 0xA5);

        // The honored $2000-$2100 window still latches (we didn't over-ignore).
        cart.write(0x2000, 0x01);
        assert_eq!(cart.read(0x4000), 0xB1);

        // A one-byte perturbation of the $0184 block changes its CRC32 -> the
        // rule no longer matches (detection is a 48-byte CRC32, not a sum).
        let mut perturbed = rom.clone();
        perturbed[0x184] ^= 0x01;
        assert_eq!(Cartridge::detect_unl_mapper(&perturbed), UnlMapper::None);
    }

    // Clean-room 48-byte stand-in for the BBD secondary logo at $0184: a plain
    // ASCII banner (44 bytes) plus a 4-byte suffix computed so the block's
    // CRC32 equals BBD_LOGO_CRC32[0] (0x6D1E_A662). No copyrighted logo bytes;
    // the same block is embedded in the bbd_banking test ROM.
    const BBD_LOGO: [u8; 48] =
        *b"RUSTYBOI BBD VASTFAME CLEANROOM STANDIN BBD!\x1B\xF7\x14\xA4";

    /// 256KB (16-bank) MBC5+RAM+BATTERY image carrying the BBD signature: the
    /// $0184 logo whose CRC32 keys detection, the $7FFF!=$01 "still protected"
    /// guard byte, and bank markers at banks 1 and 8.
    fn make_bbd_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x40000]; // 16 banks
        rom[CARTRIDGE_TYPE_OFFSET] = MBC5_RAM_BATTERY;
        rom[ROM_SIZE_OFFSET] = 0x03; // 256KB
        rom[RAM_SIZE_OFFSET] = 0x02; // 8KB
        rom[0x184..0x1B4].copy_from_slice(&BBD_LOGO);
        rom[0x7FFF] = 0x08; // != 0x01: a still-protected dump (as the real cart)
        rom[0x4000] = 0x04; // bank 1, offset 0 (mode-7 reorders 0x04 -> 0x20)
        rom[8 * 0x4000] = 0xA5; // bank 8, offset 0 (mode-3 bank reorder target)
        rom
    }

    #[test]
    fn bbd_detects_and_descrambles() {
        let rom = make_bbd_rom();
        assert_eq!(
            Cartridge::detect_unl_mapper(&rom),
            UnlMapper::Bbd(BbdState::default())
        );

        // Electrically MBC5+RAM+BATTERY.
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(
            cart.get_cartridge_type(),
            CartridgeType::MBC5 { ram: true, battery: true, .. }
        ));

        // --- data reorder (mode 7 = Digimon [0,1,5,3,4,2,6,7]) ---
        // $2001 latches dataSwapMode=7 (and, per mGBA, clobbers the low bank
        // register); re-select bank 1 via $2000. bank1[0]=0x04 reorders to
        // 0x20. A plain MBC5 read would return the raw 0x04.
        cart.write(0x2001, 0x07);
        cart.write(0x2000, 0x01);
        assert_eq!(cart.read(0x4000), 0x20);

        // --- bank reorder (mode 3 = [3,4,2,0,1,5,6,7]) ---
        // $2080 latches bankSwapMode=3; reset dataSwapMode to 0 (identity) to
        // isolate the bank reorder. Writing 0x01 to $2000 reorders bit0 -> bit3
        // = bank 8. A plain MBC5 would select bank 1 (raw 0x04 marker).
        cart.write(0x2080, 0x03);
        cart.write(0x2001, 0x00);
        cart.write(0x2000, 0x01);
        assert_eq!(cart.read(0x4000), 0xA5);

        // Detection is a 48-byte CRC32, not a sum: a one-byte perturbation of
        // the $0184 block no longer matches.
        let mut perturbed = rom.clone();
        perturbed[0x184] ^= 0x01;
        assert_eq!(Cartridge::detect_unl_mapper(&perturbed), UnlMapper::None);

        // $7FFF==0x01 marks a cracked dump that runs plain -> excluded.
        let mut cracked = rom.clone();
        cracked[0x7FFF] = 0x01;
        assert_eq!(Cartridge::detect_unl_mapper(&cracked), UnlMapper::None);
    }

    // Clean-room 48-byte stand-in for the "S-GBC" $0184 block on King of Fighters
    // R2: an ASCII banner (44 bytes) plus a 4-byte suffix computed so the block's
    // CRC32 equals BBD_LOGO_CRC32[2] (0xEA3E_443A). KoF R2's real $0184 sums to
    // 3334, which hhugboy `CartDetection` routes to BBD, but its CRC32 differs
    // from the two mGBA-documented BBD logos, so it needs its own whitelist entry.
    // No copyrighted bytes.
    const BBD_KOF_LOGO: [u8; 48] =
        *b"RUSTYBOI BBD KOF-R2 SGBC CLEANROOM STANDIN!!\xA4\xEB\xB8\x57";

    #[test]
    fn bbd_kof_r2_logo_detects_as_bbd() {
        // 1MB MBC5+RAM+BATTERY image wearing the KoF-R2 "S-GBC" $0184 block, with
        // the same $7FFF!=$01 still-protected guard the real cart carries.
        let mut rom = vec![0u8; 0x100000];
        rom[CARTRIDGE_TYPE_OFFSET] = MBC5_RAM_BATTERY;
        rom[ROM_SIZE_OFFSET] = 0x05; // 1MB
        rom[0x184..0x1B4].copy_from_slice(&BBD_KOF_LOGO);
        rom[0x7FFF] = 0x08; // as the real cart
        assert_eq!(
            Cartridge::detect_unl_mapper(&rom),
            UnlMapper::Bbd(BbdState::default())
        );
        // A cracked dump ($7FFF==$01) still runs plain.
        rom[0x7FFF] = 0x01;
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);
    }

    #[test]
    fn zelda_dx_mbc1_beta_forces_mbc5() {
        // A dump whose whole-ROM CRC32 matches a Zelda LA DX prototype beta is a
        // header-liar driving MBC5-style banking -> ForceMbc5. Build a 32 KiB
        // image whose CRC32 equals the real 1998-06-15 dump (0x9367653B) via a
        // 4-byte forged tail; the mapper keys purely on that whole-ROM CRC.
        let mut rom = vec![0u8; 0x8000];
        rom[0x7FFC..0x8000].copy_from_slice(&[0x1A, 0x6A, 0xF3, 0xE1]);
        assert_eq!(crate::checksum::crc32(&rom), 0x9367_653B);
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::ForceMbc5);

        // Any other ROM (a one-byte change moves the CRC) is untouched.
        rom[0x100] ^= 0x01;
        assert_ne!(crate::checksum::crc32(&rom), 0x9367_653B);
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);
    }

    #[test]
    fn mbc5_header_proto_forces_mbc3() {
        // Mythri Proto 1 / Proto 2 and Tyrannosaurus Tex (Proto) wear an
        // MBC5+RAM+BATTERY header but need MBC3's zero-bank remap. Build 32 KiB
        // images whose CRC32 equals each real dump via a 4-byte forged tail; the
        // rule keys purely on that whole-ROM CRC.
        for (crc, tail) in [
            (0x6648_82ACu32, [0xA4, 0xC0, 0x46, 0xCC]),
            (0xD414_9106, [0xE4, 0x8F, 0x09, 0x7E]),
            (0x1BD4_E588, [0xF6, 0x9D, 0x19, 0xE1]),
        ] {
            let mut rom = vec![0u8; 0x8000];
            rom[0x7FFC..0x8000].copy_from_slice(&tail);
            assert_eq!(crate::checksum::crc32(&rom), crc);
            assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::ForceMbc3);

            // Any other ROM (a one-byte change moves the CRC) is untouched.
            rom[0x100] ^= 0x01;
            assert_ne!(crate::checksum::crc32(&rom), crc);
            assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);
        }

        // The override must land on a real MBC3+RAM board, not the header's
        // MBC5 -- MBC3 is what remaps a zero bank register to bank 1.
        let mapper = Mapper::from_header(&UnlMapper::ForceMbc3, 0x1B, false, 128, 4);
        assert!(matches!(mapper, Mapper::Mbc3(_)));
    }

    // Clean-room 48-byte stand-in for the GGB81 secondary logo at $0184: a
    // plain ASCII banner (44 bytes) plus a 4-byte suffix computed so the
    // block's CRC32 equals GGB81_LOGO_CRC32[0] (0x79F3_4594, the mGBA "DATA."
    // run). No copyrighted logo bytes; the same block is embedded in the
    // ggb81_banking test ROM.
    const GGB81_LOGO: [u8; 48] =
        *b"RUSTYBOI GGB81 DATA CLEANROOM STANDIN SIGGB!\xBC\x5B\x9C\x21";

    /// 512KB image carrying the GGB81 signature: a truthful MBC5+RAM+BATTERY
    /// header ($1B), the $0184 logo whose CRC32 keys detection, and distinctive
    /// bytes at bank 0 and bank 1 offset 0 so the read-path data-line reorder
    /// is observable.
    fn make_ggb81_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x80000]; // 32 banks
        rom[CARTRIDGE_TYPE_OFFSET] = MBC5_RAM_BATTERY; // truthful $1B header
        rom[ROM_SIZE_OFFSET] = 0x04; // 512KB
        rom[RAM_SIZE_OFFSET] = 0x02; // 8KB
        rom[0x184..0x1B4].copy_from_slice(&GGB81_LOGO);
        rom[0x2500] = 0x0F; // bank 0 marker (reads here are never reordered)
        rom[0x4000] = 0x0F; // bank 1 offset 0 (reordered on read)
        rom
    }

    /// Clean-room 46-byte stand-in for the VF001A config stub at $0257: an
    /// ASCII banner (42 bytes) plus a 4-byte suffix computed so the block's
    /// CRC32 equals `VF001A_STUB_CRC32`. No cartridge code is embedded.
    const VF001A_STUB_STANDIN: [u8; 46] = [
        0x52, 0x55, 0x53, 0x54, 0x59, 0x42, 0x4F, 0x49, 0x20, 0x56, 0x46, 0x30, 0x30, 0x31, 0x41,
        0x20, 0x43, 0x4F, 0x4E, 0x46, 0x49, 0x47, 0x20, 0x53, 0x54, 0x55, 0x42, 0x20, 0x43, 0x4C,
        0x45, 0x41, 0x4E, 0x52, 0x4F, 0x4F, 0x4D, 0x20, 0x53, 0x49, 0x47, 0x21, 0x03, 0x12, 0xC5,
        0x48,
    ];

    #[test]
    fn vf001a_stub_selects_the_config_seed() {
        // Sanguozhi - Aoshi Tianxia has no "V.fame" logo at $0184 (those bytes
        // are its `jp $0200` entry trampoline), so the VF001 arm cannot see it;
        // the $0257 config-driver CRC32 is the gate, and it selects the $10
        // accumulator seed rather than the plain board's $00.
        let mut rom = vec![0u8; 0x8000];
        rom[CARTRIDGE_TYPE_OFFSET] = MBC5_RAM_BATTERY;
        rom[VF001A_STUB_OFFSET..VF001A_STUB_OFFSET + VF001A_STUB_LEN]
            .copy_from_slice(&VF001A_STUB_STANDIN);
        assert_eq!(
            crate::checksum::crc32(&rom[VF001A_STUB_OFFSET..VF001A_STUB_OFFSET + VF001A_STUB_LEN]),
            VF001A_STUB_CRC32
        );
        let want = UnlMapper::Vf001Gen(Box::new(Vf001gState {
            config_seed: VF001A_CONFIG_SEED,
            ..Default::default()
        }));
        assert_eq!(Cartridge::detect_unl_mapper(&rom), want);

        // A one-byte perturbation of the stub moves its CRC32 -> no match, and
        // (with no VF001 logo either) the image is left as a plain MBC5.
        rom[VF001A_STUB_OFFSET] ^= 0x01;
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);

        // A plain VF001 cart (matched by its $0184 logo) keeps the $00 seed.
        let mut plain = vec![0u8; 0x8000];
        plain[CARTRIDGE_TYPE_OFFSET] = MBC5_RAM_BATTERY;
        plain[0x184..0x1B4].copy_from_slice(&VF001G_LOGO);
        assert_eq!(Cartridge::detect_unl_mapper(&plain), UnlMapper::Vf001Gen(Box::default()));
    }

    /// Clean-room 48-byte stand-in for the "SOUL" $0184 block shared by Gedou
    /// Jian Shen KF and the three Soul Falchion dumps: an ASCII banner plus a
    /// 4-byte suffix computed so the block's CRC32 equals
    /// `VF001G_LOGO_CRC32[1]`. No cartridge bytes are embedded.
    const VF001G_SOUL_LOGO: [u8; 48] =
        *b"RUSTYBOI VF001 SOUL CLEANROOM STANDIN SIG!!!e\x08\xCBf";

    #[test]
    fn vf001a_seed_applies_to_the_one_soul_logo_cart_that_needs_it() {
        // Gedou Jian Shen KF wears the same "SOUL" $0184 logo as the three Soul
        // Falchion dumps, so only the whole-ROM CRC32 can tell it apart -- and
        // that rule must run BEFORE the logo arm. Build a 32 KiB image with the
        // SOUL logo (which the logo arm would otherwise claim at seed $00) whose
        // whole-ROM CRC32 equals the real KF dump's, via a 4-byte forged tail.
        let build = || {
            let mut rom = vec![0u8; 0x8000];
            rom[CARTRIDGE_TYPE_OFFSET] = MBC5; // KF's truthful $19 header
            rom[ROM_SIZE_OFFSET] = 0x00;
            rom[0x184..0x1B4].copy_from_slice(&VF001G_SOUL_LOGO);
            rom[0x7FFC..0x8000].copy_from_slice(&[0x48, 0xF6, 0x8B, 0xE4]);
            rom
        };
        let rom = build();
        assert_eq!(crate::checksum::crc32(&rom[0x184..0x1B4]), VF001G_LOGO_CRC32[1]);
        assert_eq!(crate::checksum::crc32(&rom), VF001A_SEED_OVER_SOUL_ROM_CRC32);
        assert_eq!(
            Cartridge::detect_unl_mapper(&rom),
            UnlMapper::Vf001Gen(Box::new(Vf001gState {
                config_seed: VF001A_CONFIG_SEED,
                ..Default::default()
            }))
        );
        // The near-miss that makes an image hash necessary: the Soul Falchion
        // dumps carry the same logo and the same "Gals Fighters" title but must
        // keep the plain $00 seed.
        let mut other = build();
        other[0x134..0x141].copy_from_slice(b"Gals Fighters");
        assert_ne!(crate::checksum::crc32(&other), VF001A_SEED_OVER_SOUL_ROM_CRC32);
        assert_eq!(Cartridge::detect_unl_mapper(&other), UnlMapper::Vf001Gen(Box::default()));
    }

    #[test]
    fn vf001_behind_ggb81_logo_routes_to_vf001gen() {
        // The two dumps that wear a GGB81 $0184 logo but drive the VF001 $7000
        // config protocol are keyed on the whole-ROM CRC32, and that rule runs
        // BEFORE the GGB81 arm. Build 32 KiB images carrying the GGB81 logo (so
        // the GGB81 arm would otherwise claim them) whose whole-ROM CRC32 equals
        // each real dump's, via a 4-byte forged tail.
        let build = |tail: [u8; 4]| {
            let mut rom = vec![0u8; 0x8000];
            rom[CARTRIDGE_TYPE_OFFSET] = MBC5_RAM_BATTERY;
            rom[ROM_SIZE_OFFSET] = 0x00;
            rom[0x184..0x1B4].copy_from_slice(&GGB81_LOGO);
            rom[0x7FFC..0x8000].copy_from_slice(&tail);
            rom
        };
        for (tail, crc) in
            [([0x8D, 0x87, 0xEF, 0x0B], 0x759F_07BD), ([0x41, 0xE9, 0x3B, 0x46], 0xE674_8D1F)]
        {
            let rom = build(tail);
            assert_eq!(crate::checksum::crc32(&rom), crc);
            assert_eq!(
                Cartridge::detect_unl_mapper(&rom),
                UnlMapper::Vf001Gen(Box::default())
            );
        }
        // The near-miss that makes an image hash necessary: a cart with the same
        // $0184 logo (Emo Dao / Mojie Chuanshuo also share the "MOON ACT V1.0"
        // title) but a different image keeps the GGB81 board.
        let mut rom = build([0x8D, 0x87, 0xEF, 0x0B]);
        rom[0x134..0x141].copy_from_slice(b"MOON ACT V1.0");
        assert_ne!(crate::checksum::crc32(&rom), 0x759F_07BD);
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::Ggb81(0));
    }

    #[test]
    fn ggb81_detects_and_reorders_data_lines() {
        let rom = make_ggb81_rom();
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::Ggb81(0));

        // Truthful MBC5+RAM+BATTERY header: decode falls through to it.
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(
            cart.get_cartridge_type(),
            CartridgeType::MBC5 { ram: true, battery: true, .. }
        ));

        // Select bank 1; default swap mode 0 is the identity, so the read of
        // bank 1 offset 0 is unscrambled.
        cart.write(0x2000, 0x01);
        assert_eq!(cart.read(0x4000), 0x0F);

        // A write with addr & 0xF0FF == 0x2001 latches mode = value & 7 (and,
        // being an MBC5 low-bank write, also sets the bank to that value); the
        // boot code re-selects the real bank via $2000 without disturbing the
        // mode. Mode 2 reorders 0x0F -> 0x69.
        cart.write(0x2001, 0x02); // mode 2 (also selects bank 2)
        cart.write(0x2000, 0x01); // reselect bank 1; mode stays 2
        assert_eq!(cart.read(0x4000), 0x69);

        // Bank 0 reads ($0000-$3FFF) are never reordered, even with a mode set.
        assert_eq!(cart.read(0x2500), 0x0F);

        // Returning to mode 0 restores the identity read.
        cart.write(0x2001, 0x00); // mode 0 (also selects bank 0)
        cart.write(0x2000, 0x01); // reselect bank 1
        assert_eq!(cart.read(0x4000), 0x0F);

        // A one-byte perturbation of the $0184 block changes its CRC32 -> the
        // rule no longer matches (detection is a 48-byte CRC32, not a sum).
        let mut perturbed = rom.clone();
        perturbed[0x184] ^= 0x01;
        assert_eq!(Cartridge::detect_unl_mapper(&perturbed), UnlMapper::None);
    }

    // Clean-room 48-byte stand-in for the Vast Fame secondary logo on Sintax
    // boards: a plain ASCII banner (44 bytes) plus a 4-byte suffix computed so
    // the block's CRC32 equals the Sintax detection constant 0x6C1DCF2D. No
    // copyrighted logo bytes; the same block is embedded in the
    // sintax_scramble test ROM.
    const SINTAX_LOGO: [u8; 48] =
        *b"RUSTYBOI SINTAX VASTFAME CLEANROOM STANDIN!!\xFE\xDF\x1B\x3C";

    /// 1MB MBC5+RAM+BATTERY image carrying the Sintax signature: the real
    /// Nintendo logo (so the boot ROM's check passes, exactly like the real
    /// carts), the $0184 secondary logo whose CRC32 keys detection, the truthful
    /// $1B header type, and the protected-dump guard byte at $7FFF (!= 0x01). A
    /// distinct marker `0x10 + bank` sits at offset 0 of every bank.
    fn make_sintax_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x100000]; // 64 banks
        rom[CARTRIDGE_TYPE_OFFSET] = MBC5_RAM_BATTERY; // $1B, truthful
        rom[ROM_SIZE_OFFSET] = 0x05; // 1MB
        rom[RAM_SIZE_OFFSET] = 0x03; // 32KB
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        rom[0x184..0x1B4].copy_from_slice(&SINTAX_LOGO);
        rom[0x7FFF] = 0x7B; // protected dump (real dumps read 0x7B here)
        for bank in 0..64usize {
            rom[bank * 0x4000] = (0x10 + bank) as u8;
        }
        rom
    }

    #[test]
    fn sintax_detects_scrambles_and_descrambles() {
        let rom = make_sintax_rom();
        assert_eq!(
            Cartridge::detect_unl_mapper(&rom),
            UnlMapper::Sintax(SintaxState::default())
        );

        // Electrically MBC5+RAM+BATTERY.
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(
            cart.get_cartridge_type(),
            CartridgeType::MBC5 { ram: true, battery: true, .. }
        ));

        // Program reorder mode 1 via a $5x1x write, then write a RAW bank
        // number to $2xxx. The effective bank is the value permuted through
        // the mode-1 table -- a plain MBC5 would select the raw value directly,
        // so this read distinguishes the two.
        let mode = 1u8;
        let raw = 0x04u8;
        cart.write(0x5010, mode);
        cart.write(0x2000, raw);
        let eff = Cartridge::reorder_bits(raw, &SINTAX_BANK_REORDER[mode as usize]) as usize;
        assert_ne!(eff, raw as usize, "mode 1 must actually permute this value");
        // XOR still 0, so the read is the bank marker unchanged.
        assert_eq!(cart.read(0x4000), (0x10 + eff) as u8);

        // Program XOR byte 0 (raw & 3 == 0 selects it); reads of the switchable
        // window are now XORed, bank 0 is not.
        cart.write(0x7020, 0x5A);
        assert_eq!(cart.read(0x4000), (0x10 + eff) as u8 ^ 0x5A);
        assert_eq!(cart.read(0x0000), rom[0], "bank 0 is never scrambled");

        // Changing the mode replays the stored bank number under the new table
        // (mGBA fakes a $2000 write), re-selecting a different physical bank.
        let mode2 = 5u8;
        cart.write(0x5010, mode2);
        let eff2 = Cartridge::reorder_bits(raw, &SINTAX_BANK_REORDER[mode2 as usize]) as usize;
        assert_ne!(eff2, eff);
        assert_eq!(cart.read(0x4000), (0x10 + eff2) as u8 ^ 0x5A);
    }

    #[test]
    fn sintax_detection_guards() {
        // A "fixed"/cracked dump ($7FFF == 0x01) already runs as plain MBC5 and
        // must NOT be re-scrambled (mGBA's guard).
        let mut fixed = make_sintax_rom();
        fixed[0x7FFF] = 0x01;
        assert_eq!(Cartridge::detect_unl_mapper(&fixed), UnlMapper::None);

        // A one-byte perturbation of the $0184 block changes its CRC32 -> the
        // rule no longer matches (detection is a 48-byte CRC32, not a sum).
        let mut perturbed = make_sintax_rom();
        perturbed[0x184] ^= 0x01;
        assert_eq!(Cartridge::detect_unl_mapper(&perturbed), UnlMapper::None);
    }

    // Clean-room 48-byte stand-in for the HITEK secondary logo: a plain ASCII
    // banner (44 bytes) plus a 4-byte suffix computed so the block's CRC32
    // equals HITEK_LOGO_CRC32 (0x4FDA_B691). No copyrighted logo bytes; the
    // same block is embedded in the hitek_banking test ROM.
    const HITEK_LOGO: [u8; 48] =
        *b"RUSTYBOI HITEK VASTFAME CLEANROOM STANDIN!!!\x8C\x13\x02\x75";

    /// 128KB image carrying the HITEK signature: the truthful MBC5+RAM+BATTERY
    /// header ($1B), the $0184 logo whose CRC32 keys detection, a $7FFF byte
    /// that is not the "cracked-dump" $01, a real Nintendo logo, and bank
    /// markers at banks 1, 2 and 4 (the same layout as the hitek_banking ROM).
    fn make_hitek_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x20000]; // 8 banks
        rom[CARTRIDGE_TYPE_OFFSET] = 0x1B; // MBC5+RAM+BATTERY (truthful)
        rom[ROM_SIZE_OFFSET] = 0x02; // 128KB / 8 banks
        rom[RAM_SIZE_OFFSET] = 0x03; // 32KB
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        rom[0x184..0x1B4].copy_from_slice(&HITEK_LOGO);
        rom[0x7FFF] = 0x99; // != 0x01: a protected (non-cracked) dump
        rom[0x4000] = 0xB1; // bank 1, offset 0
        rom[0x2 * 0x4000] = 0xB2; // bank 2 (where a plain MBC5 lands in step 1)
        rom[0x4 * 0x4000] = 0xA4; // bank 4 (the HITEK bank-reorder target)
        rom
    }

    #[test]
    fn hitek_detects_and_descrambles() {
        let rom = make_hitek_rom();
        assert_eq!(
            Cartridge::detect_unl_mapper(&rom),
            UnlMapper::Hitek(HitekState::default())
        );

        // Electrically MBC5+RAM+BATTERY.
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(cart.get_cartridge_type(), CartridgeType::MBC5 { .. }));

        // Bank reorder: bank_swap_mode=1, then a $2000 write of $02 selects bank
        // 4 (reorder of $02 through HITEK_BANK_REORDERING[1]). data_swap_mode is
        // still 0, so the read is the raw bank-4 marker. A plain MBC5 would
        // select bank 2 and read 0xB2.
        cart.write(0x2080, 0x01); // bank_swap_mode = 1
        cart.write(0x2000, 0x02); // -> bank 4
        assert_eq!(cart.read(0x4000), 0xA4);

        // Data reorder: data_swap_mode=1 (the $2001 write also reselects bank 1
        // with the raw value); the bank-1 marker 0xB1 reads back reordered to
        // 0x95. A plain MBC5 would read the raw 0xB1.
        cart.write(0x2001, 0x01); // data_swap_mode = 1, bank_low = 1
        assert_eq!(cart.read(0x4000), 0x95);

        // A one-byte perturbation of the $0184 block changes its CRC32 -> the
        // rule no longer matches (detection is a 48-byte CRC32, not a sum).
        let mut perturbed = rom.clone();
        perturbed[0x184] ^= 0x01;
        assert_eq!(Cartridge::detect_unl_mapper(&perturbed), UnlMapper::None);

        // The $7FFF==$01 guard: a cracked dump that already runs as plain MBC5
        // must NOT be routed to HITEK.
        let mut cracked = rom.clone();
        cracked[0x7FFF] = 0x01;
        assert_eq!(Cartridge::detect_unl_mapper(&cracked), UnlMapper::None);
    }

    /// A Vast Fame 8 KiB dual-window cart (Jieba Tianwang 4's shape): the entry
    /// point jumps into the title field, which holds the board's power-on
    /// handshake. 64KB = 4 banks = 8 pages of 8 KiB, each marked at its start.
    fn make_vf8k_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x10000]; // 4 banks / 8 pages
        rom[0x100] = 0x00;
        rom[0x101] = 0xC3;
        rom[0x102] = 0x34; // jp $0134 -- into the title field
        rom[0x103] = 0x01;
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        rom[0x134..0x13A].copy_from_slice(&VF8K_BOOT_STUB);
        rom[CARTRIDGE_TYPE_OFFSET] = 0x1C; // MBC5+RUMBLE (truthful)
        rom[ROM_SIZE_OFFSET] = 0x01; // 64KB / 4 banks
        rom[RAM_SIZE_OFFSET] = 0x00;
        for page in 0..8usize {
            rom[page * VF8K_PAGE] = 0xF0 | page as u8;
        }
        rom
    }

    // Clean-room 48-byte stand-in for the general-VF001 secondary logo: a plain
    // ASCII banner (44 bytes) plus a 4-byte suffix computed so the block's
    // CRC32 equals VF001G_LOGO_CRC32[0] (0x42B7_73B8). No copyrighted logo
    // bytes.
    const VF001G_LOGO: [u8; 48] =
        *b"RUSTYBOI VF001 GENERAL CLEANROOM STANDIN!!!!\x82\xF7\xF9\x01";

    /// 128KB image carrying the general-VF001 signature: a truthful
    /// MBC5+RAM+BATTERY header ($1B, inside the $19-$1E detection gate), the
    /// $0184 logo whose CRC32 keys detection, a real Nintendo logo, and a
    /// per-bank marker byte at each bank's offset 0 and at offset $2000.
    fn make_vf001g_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x20000]; // 8 banks
        rom[CARTRIDGE_TYPE_OFFSET] = 0x1B; // MBC5+RAM+BATTERY (truthful)
        rom[ROM_SIZE_OFFSET] = 0x02; // 128KB / 8 banks
        rom[RAM_SIZE_OFFSET] = 0x03; // 32KB
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        rom[0x184..0x1B4].copy_from_slice(&VF001G_LOGO);
        for bank in 0..8usize {
            rom[bank * 0x4000] = 0xB0 + bank as u8;
            rom[bank * 0x4000 + 0x2000] = 0xC0 + bank as u8;
        }
        rom
    }

    #[test]
    fn vf8k_detects_and_banks_two_8k_windows() {
        let rom = make_vf8k_rom();
        assert_eq!(
            Cartridge::detect_unl_mapper(&rom),
            UnlMapper::Vf8k(Vf8kState::default())
        );

        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        // Electrically MBC5 (+rumble, from the truthful header byte).
        assert!(matches!(cart.get_cartridge_type(), CartridgeType::MBC5 { rumble: true, .. }));

        // Power-on: pages 2 and 3, i.e. exactly MBC5's power-on 16 KiB bank 1.
        assert_eq!(cart.read(0x4000), 0xF2);
        assert_eq!(cart.read(0x6000), 0xF3);

        // A10 low programs the $4000-$5FFF page, A10 high the $6000-$7FFF page.
        // Pages 5 and 6 are NOT a (2n, 2n+1) pair, so no single 16 KiB bank
        // register can produce this map.
        cart.write(0x2000, 0x05);
        cart.write(0x2400, 0x06);
        assert_eq!(cart.read(0x4000), 0xF5);
        assert_eq!(cart.read(0x6000), 0xF6);

        // The windows are independent: moving the high one leaves the low one.
        cart.write(0x2400, 0x04);
        assert_eq!(cart.read(0x4000), 0xF5);
        assert_eq!(cart.read(0x6000), 0xF4);

        // $0000-$3FFF is always bank 0.
        assert_eq!(cart.read(0x0000), 0xF0);
        assert_eq!(cart.read(0x2000), 0xF1);

        // Detection is the entry-into-header + handshake pair. Break either and
        // the cart falls back to its (truthful) MBC5 header.
        let mut no_stub = rom.clone();
        no_stub[0x136] ^= 0xFF; // corrupt the $AA of `ld a,$AA`
        assert_eq!(Cartridge::detect_unl_mapper(&no_stub), UnlMapper::None);
        let mut no_jump = rom.clone();
        no_jump[0x102] = 0x50; // entry targets $0150, not the title field
        assert_eq!(Cartridge::detect_unl_mapper(&no_jump), UnlMapper::None);
        // ...and the header-family gate keeps it off non-MBC5 carts.
        let mut mbc1 = rom.clone();
        mbc1[CARTRIDGE_TYPE_OFFSET] = 0x01;
        assert_eq!(Cartridge::detect_unl_mapper(&mbc1), UnlMapper::None);
    }

    /// Clean-room stand-in for the "New GB Color" HK-PCB protection trampoline
    /// at $0091. Detection keys ONLY on the 46-byte CRC32, never the bytes, so
    /// this is an ASCII banner plus a 4-byte suffix computed to hit
    /// NEWGBHK_STUB_CRC32 -- the same block the first-party ROM carries, and no
    /// cartridge code.
    const NEWGBHK_SIG: [u8; 46] =
        *b"RUSTYBOI NEWGBHK HK0701 CLEANROOM SIG!!!!!\x38\xCB\x84\x40";

    /// 512KB image carrying the New GB HK signature: a truthful MBC5+RAM+BATTERY
    /// header, the $0091 stub whose CRC32 keys detection, and bank-1 markers at
    /// the addresses the protection also answers, so "ROM below $80, protection
    /// at or above $80" is observable.
    fn make_newgbhk_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x80000]; // 32 banks
        rom[CARTRIDGE_TYPE_OFFSET] = MBC5_RAM_BATTERY;
        rom[ROM_SIZE_OFFSET] = 0x04; // 512KB
        rom[RAM_SIZE_OFFSET] = 0x02; // 8KB
        rom[NEWGBHK_STUB_OFFSET..NEWGBHK_STUB_OFFSET + NEWGBHK_STUB_LEN]
            .copy_from_slice(&NEWGBHK_SIG);
        rom[0x0A00] = 0xC3; // bank-0 marker (never protected)
        rom[0x4000] = 0x11; // bank-1 markers, all != their protection value
        rom[0x4096] = 0x5A;
        rom[0x5000] = 0x3C;
        rom
    }

    /// Clean-room stand-in for the NT "new" board driver at $00B9. Detection
    /// keys ONLY on the 37-byte CRC32, never the bytes, so this is an ASCII
    /// banner plus a 4-byte suffix computed to hit `NTNEW_STUB_CRC32` -- the
    /// same block the first-party ROM carries, and no cartridge code.
    const NTNEW_SIG: [u8; 37] = *b"RUSTYBOI NTNEW SPLIT CLEANROOM!!!\xA6\x85\x5F\xCF";

    /// 128KB image carrying the NT "new" signature: a truthful MBC5 header of
    /// the declared size (the detection requires the two to agree), the $00B9
    /// driver whose CRC32 keys detection, and one marker per 8 KiB page so both
    /// windows are independently observable.
    fn make_ntnew_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x20000]; // 8 banks = 16 pages
        rom[CARTRIDGE_TYPE_OFFSET] = MBC5;
        rom[ROM_SIZE_OFFSET] = 0x02; // 128KB, matching the image
        rom[NTNEW_STUB_OFFSET..NTNEW_STUB_OFFSET + NTNEW_STUB_LEN].copy_from_slice(&NTNEW_SIG);
        for page in 0..16usize {
            rom[page * NTNEW_PAGE] = 0xA0 | page as u8;
        }
        rom
    }

    #[test]
    fn ntnew_detects_and_splits_the_switchable_window() {
        let rom = make_ntnew_rom();
        assert_eq!(
            Cartridge::detect_unl_mapper(&rom),
            UnlMapper::NtNew(NtNewState::default())
        );
        // Truthful MBC5 header: decode falls through to it.
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(
            cart.get_cartridge_type(),
            CartridgeType::MBC5 { ram: false, battery: false, rumble: false }
        ));

        // Un-armed the board is a plain MBC5: one 16 KiB bank register moves
        // BOTH halves of the window together.
        cart.write(0x2000, 0x02);
        assert_eq!(cart.read(0x4000), 0xA4); // bank 2 = pages 4 and 5
        assert_eq!(cart.read(0x6000), 0xA5);
        cart.write(0x2400, 0x03); // still just the same bank register
        assert_eq!(cart.read(0x4000), 0xA6);
        assert_eq!(cart.read(0x6000), 0xA7);
        // Only the exact $55 magic arms it.
        cart.write(0x1400, 0x54);
        cart.write(0x2400, 0x02);
        assert_eq!(cart.read(0x4000), 0xA4);

        // Armed: $2000 and $2400 are two independent 8 KiB page registers, and
        // they can express a pair no 16 KiB bank register can (page 5 is the
        // upper half of bank 2, page 6 the lower half of bank 3).
        cart.write(0x1400, 0x55);
        cart.write(0x2000, 0x05);
        cart.write(0x2400, 0x06);
        assert_eq!(cart.read(0x4000), 0xA5);
        assert_eq!(cart.read(0x6000), 0xA6);
        // Independence: moving one window leaves the other alone.
        cart.write(0x2400, 0x0B);
        assert_eq!(cart.read(0x4000), 0xA5);
        assert_eq!(cart.read(0x6000), 0xAB);
        // Pages 0 and 1 fold up by 16 KiB, mirroring MBC5's bank-0 remap.
        cart.write(0x2000, 0x00);
        cart.write(0x2400, 0x01);
        assert_eq!(cart.read(0x4000), 0xA2);
        assert_eq!(cart.read(0x6000), 0xA3);
        // Page numbers wrap to the image (16 pages here), and the fixed bank is
        // never affected.
        cart.write(0x2000, 0x14); // 0x14 % 16 = 4
        assert_eq!(cart.read(0x4000), 0xA4);
        assert_eq!(cart.read(0x0000), 0xA0);
    }

    #[test]
    fn ntnew_requires_a_self_consistent_image() {
        // The board's page arithmetic addresses 8 KiB windows of the real ROM,
        // so an image whose size disagrees with its own header is not routed
        // here -- the guard that keeps the 2x-inflated Yingxiong Tianxia dump
        // (which does not run under the board) on plain MBC5.
        let mut rom = make_ntnew_rom();
        rom[ROM_SIZE_OFFSET] = 0x03; // declares 256KB in a 128KB image
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);
        // A one-byte perturbation of the driver also moves its CRC32.
        let mut rom = make_ntnew_rom();
        rom[NTNEW_STUB_OFFSET] ^= 0x01;
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);
    }

    #[test]
    fn newgbhk_detects_and_serves_protection_window() {
        let rom = make_newgbhk_rom();
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::NewGbHk);

        // Truthful MBC5+RAM+BATTERY header: decode falls through to it.
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(
            cart.get_cartridge_type(),
            CartridgeType::MBC5 { ram: true, battery: true, .. }
        ));

        // Bank register below $80: the window is ordinary ROM.
        cart.write(0x2000, 0x01);
        assert_eq!(cart.read(0x4096), 0x5A);
        assert_eq!(cart.read(0x5000), 0x3C);

        // At or above $80: the window is the protection chip. One address per
        // `digits & 7` transform, where digits = (addr >> 4) & $FF.
        cart.write(0x2000, 0x81);
        for &(addr, want) in &[
            (0x4000u16, 0x00u8), // digits $00, case 0: identity
            (0x4010, 0xAB),      // digits $01, case 1: XOR $AA
            (0x4020, 0x57),      // digits $02, case 2: XOR $55
            (0x4030, 0x81),      // digits $03, case 3: rotate right 1
            (0x4040, 0x08),      // digits $04, case 4: rotate left 1
            (0x4050, 0xA0),      // digits $05, case 5: bit reversal
            (0x4060, 0x30),      // digits $06, case 6: OR/AND bit pairs
            (0x4070, 0xD2),      // digits $07, case 7: XNOR/XOR bit pairs
            (0x4096, 0xA3),      // the address the real cart reads
            (0x4FF0, 0xF0),      // last protected address
            (0x5000, 0xFF),      // $5000-$7FFF reads back $FF, not a transform
            (0x7FFF, 0xFF),
        ] {
            assert_eq!(cart.read(addr), want, "protection read at ${addr:04X}");
        }
        // Bank 0 is never affected while the protection is engaged.
        assert_eq!(cart.read(0x0A00), 0xC3);

        // The gate is the 9-bit bank value, not bit 7 of the low byte: low $00
        // with the high bit set is $100, still at or above $80.
        cart.write(0x2000, 0x00);
        cart.write(0x3000, 0x01);
        assert_eq!(cart.read(0x4096), 0xA3);

        // Dropping back below $80 restores ordinary ROM reads.
        cart.write(0x3000, 0x00);
        cart.write(0x2000, 0x01);
        assert_eq!(cart.read(0x4096), 0x5A);

        // A one-byte perturbation of the $0091 block changes its CRC32 -> no
        // match, so an unrelated cart can never be routed here.
        let mut perturbed = rom.clone();
        perturbed[NEWGBHK_STUB_OFFSET] ^= 0x01;
        assert_eq!(Cartridge::detect_unl_mapper(&perturbed), UnlMapper::None);

        // The header gate: a non-MBC5-family type is not this board.
        let mut mbc1_header = rom.clone();
        mbc1_header[CARTRIDGE_TYPE_OFFSET] = MBC1_RAM_BATTERY;
        assert_eq!(Cartridge::detect_unl_mapper(&mbc1_header), UnlMapper::None);
    }

    /// Clean-room stand-in for the PKJD $0184 signature: detection keys ONLY on
    /// the 48-byte CRC32 (0x65BBF1FC), never the bytes, so this is an ASCII
    /// banner plus a 4-byte suffix computed to hit it -- the same block the
    /// first-party mooneye ROM carries, and no cartridge code.
    const PKJD_SIG: [u8; 48] =
        *b"RUSTYBOI CLEANROOM PKJD TELEFANG DEF REGS !!\x74\xF4\xF7\x90";

    /// 512KB image wearing a truthful MBC3+TIMER+RAM+BATTERY header ($10) with
    /// the PKJD $0184 signature whose CRC32 keys detection.
    fn make_pkjd_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x80000]; // 32 banks
        rom[CARTRIDGE_TYPE_OFFSET] = MBC3_TIMER_RAM_BATTERY;
        rom[ROM_SIZE_OFFSET] = 0x04; // 512KB
        rom[RAM_SIZE_OFFSET] = 0x03; // 32KB / 4 banks
        rom[0x184..0x1B4].copy_from_slice(&PKJD_SIG);
        rom
    }

    #[test]
    fn pkjd_detects_and_serves_def_registers() {
        let rom = make_pkjd_rom();
        assert_eq!(
            Cartridge::detect_unl_mapper(&rom),
            UnlMapper::PokeJadeDia(PokeJadeState::default())
        );

        // Truthful MBC3+TIMER+RAM+BATTERY header: decode falls through to it.
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(
            cart.get_cartridge_type(),
            CartridgeType::MBC3 { ram: true, battery: true, timer: true }
        ));

        // While RAM is disabled the protection window reads back open bus.
        cart.write(0x4000, 0x0D);
        assert_eq!(cart.read(0xA000), 0xFF);

        cart.write(0x0000, 0x0A); // RAMG enable

        // Register D and E round-trip through the $A000 window.
        cart.write(0x4000, 0x0D);
        cart.write(0xA000, 0x5A);
        assert_eq!(cart.read(0xA000), 0x5A);
        cart.write(0x4000, 0x0E);
        cart.write(0xA000, 0x2D);
        assert_eq!(cart.read(0xA000), 0x2D);
        // D is untouched by the E traffic.
        cart.write(0x4000, 0x0D);
        assert_eq!(cart.read(0xA000), 0x5A);

        // The $0F command port folds D and E (write-only: reads return 0).
        let cmd = |cart: &mut Cartridge, c: u8| {
            cart.write(0x4000, 0x0F);
            cart.write(0xA000, c);
        };
        let reg = |cart: &mut Cartridge, sel: u8| {
            cart.write(0x4000, sel);
            cart.read(0xA000)
        };
        cmd(&mut cart, 0x41); // D += E -> $5A + $2D = $87
        assert_eq!(reg(&mut cart, 0x0D), 0x87);
        cmd(&mut cart, 0x51); // D++ -> $88
        assert_eq!(reg(&mut cart, 0x0D), 0x88);
        cmd(&mut cart, 0x12); // E-- -> $2C
        assert_eq!(reg(&mut cart, 0x0E), 0x2C);
        cmd(&mut cart, 0x42); // E += D -> $2C + $88 = $B4
        assert_eq!(reg(&mut cart, 0x0E), 0xB4);
        cmd(&mut cart, 0x11); // D-- -> $87
        assert_eq!(reg(&mut cart, 0x0D), 0x87);

        // $0F is write-only; the unpopulated RTC registers $08-$0C read 0.
        cart.write(0x4000, 0x0F);
        assert_eq!(cart.read(0xA000), 0x00);
        cart.write(0x4000, 0x08);
        assert_eq!(cart.read(0xA000), 0x00);

        // Selector $00-$07 is ordinary MBC3 SRAM (round-trips through real RAM).
        cart.write(0x4000, 0x00);
        cart.write(0xA000, 0x99);
        assert_eq!(cart.read(0xA000), 0x99);

        // A one-byte perturbation of the $0184 block changes its CRC32, and the
        // header-type gate keeps a non-$10 cart off this board.
        let mut perturbed = rom.clone();
        perturbed[0x184] ^= 0x01;
        assert_eq!(Cartridge::detect_unl_mapper(&perturbed), UnlMapper::None);
        let mut wrong_type = rom.clone();
        wrong_type[CARTRIDGE_TYPE_OFFSET] = MBC3_RAM_BATTERY;
        assert_eq!(Cartridge::detect_unl_mapper(&wrong_type), UnlMapper::None);
    }

    /// Drives the general-VF001 config register file the way the boot code
    /// does: every config write folds its data into a rotate-right-then-XOR
    /// running accumulator that is then latched into the addressed port, so
    /// latching a WANTED byte means writing `ror1(running) ^ wanted`.
    struct Vf001gProgrammer {
        running: u8,
    }

    impl Vf001gProgrammer {
        /// Opens config mode ($96 to $7000), which also zeroes the accumulator.
        fn open(cart: &mut Cartridge) -> Self {
            cart.write(0x7000, 0x96);
            Self { running: 0 }
        }

        /// Latch `want` into the port at `addr` ($6000 or $7000-$700A).
        fn latch(&mut self, cart: &mut Cartridge, addr: u16, want: u8) {
            let rotated = (if self.running & 1 != 0 { 0x80 } else { 0 }) + (self.running >> 1);
            let data = rotated ^ want;
            assert!(
                !(addr & 0xF00F == 0x7000 && data == 0x96),
                "this write would re-open config mode instead of latching"
            );
            self.running = want;
            cart.write(addr, data);
        }
    }

    /// Program the byte-injection effect: a read of (`bank`, `addr`) makes the
    /// next reads return `bytes` in turn. The $7000 write both latches the
    /// command port and activates the sequence, so it goes last.
    fn vf001g_arm_injection(cart: &mut Cartridge, bank: u8, addr: u16, bytes: &[u8; 4], len: u8) {
        let mut p = Vf001gProgrammer::open(cart);
        p.latch(cart, 0x7001, (addr & 0xFF) as u8);
        p.latch(cart, 0x7002, (addr >> 8) as u8);
        p.latch(cart, 0x7003, bank);
        for (i, b) in bytes.iter().enumerate() {
            p.latch(cart, 0x7004 + i as u16, *b);
        }
        p.latch(cart, 0x7000, 0x03 + len); // low 3 bits 4..7 => 1..4 bytes
    }

    /// Program the bank-0 partial replacement: reads of bank 0 from `from` on
    /// are served out of `source_bank`. The $7008 write activates it, so it
    /// goes last.
    fn vf001g_arm_replacement(cart: &mut Cartridge, from: u16, source_bank: u8) {
        let mut p = Vf001gProgrammer::open(cart);
        p.latch(cart, 0x6000, source_bank);
        p.latch(cart, 0x7009, (from & 0xFF) as u8);
        p.latch(cart, 0x700A, (from >> 8) as u8);
        p.latch(cart, 0x7008, 0x0F); // low nibble $F = enable
    }

    #[test]
    fn vf001g_detects_and_serves_both_protection_effects() {
        let rom = make_vf001g_rom();
        assert_eq!(
            Cartridge::detect_unl_mapper(&rom),
            UnlMapper::Vf001Gen(Box::default())
        );

        // Electrically MBC5; the truthful $1B header supplies RAM + battery.
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(
            cart.get_cartridge_type(),
            CartridgeType::MBC5 { ram: true, battery: true, rumble: false }
        ));

        // Unprogrammed, the board is a plain MBC5: bank 0 and the switchable
        // window read their own markers.
        assert_eq!(cart.read(0x0000), 0xB0);
        cart.write(0x2000, 0x03);
        assert_eq!(cart.read(0x4000), 0xB3);

        // (1) Bank-0 partial replacement: reads of bank 0 from $2000 on come
        // from bank 5 ($C5), while lower bank-0 addresses stay real ($B0).
        vf001g_arm_replacement(&mut cart, 0x2000, 5);
        assert_eq!(cart.read(0x2000), 0xC5);
        assert_eq!(cart.read(0x0000), 0xB0);

        // (2) Byte-sequence injection: a read of bank 0 / $0100 returns the
        // four programmed bytes in turn, then falls back to real ROM.
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        vf001g_arm_injection(&mut cart, 0, 0x0100, &[0x11, 0x22, 0x33, 0x44], 4);
        assert_eq!(cart.read(0x0100), 0x11);
        assert_eq!(cart.read(0x0101), 0x22);
        assert_eq!(cart.read(0x0102), 0x33);
        assert_eq!(cart.read(0x0103), 0x44);
        assert_eq!(cart.read(0x0000), 0xB0, "sequence exhausted -> real ROM");
    }

    /// The protection register file lives in the `UnlMapper::Vf001Gen` payload,
    /// so it must survive a savestate round-trip: a restored cart has to serve
    /// the same latched injection and replacement as the live one.
    #[test]
    fn vf001g_protection_state_round_trips_through_a_savestate() {
        let rom = make_vf001g_rom();
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        vf001g_arm_replacement(&mut cart, 0x2000, 5);
        vf001g_arm_injection(&mut cart, 0, 0x0100, &[0x11, 0x22, 0x33, 0x44], 4);
        cart.write(0x2000, 0x03);

        let bytes = bincode::serialize(&cart).unwrap();
        let mut restored: Cartridge = bincode::deserialize(&bytes).unwrap();
        restored.attach_rom(rom.clone());

        assert_eq!(
            restored.unl_mapper(),
            cart.unl_mapper(),
            "the whole VF001 register file must serialize"
        );
        assert_eq!(restored.read(0x0100), 0x11);
        assert_eq!(restored.read(0x0101), 0x22);
        assert_eq!(restored.read(0x0102), 0x33);
        assert_eq!(restored.read(0x0103), 0x44);
        assert_eq!(restored.read(0x2000), 0xC5, "replacement still armed");
        assert_eq!(restored.read(0x4000), 0xB3, "bank register survived too");
    }

    /// No other board may pay for the VF001 register file: it is inside the
    /// variant payload, so every other `UnlMapper` serializes to the bare
    /// variant index.
    #[test]
    fn vf001g_register_file_costs_non_users_nothing() {
        let none = bincode::serialize(&UnlMapper::None).unwrap().len();
        let vf = bincode::serialize(&UnlMapper::Vf001Gen(Box::default())).unwrap().len();
        assert_eq!(
            vf - none,
            31,
            "the 31-byte register file must be paid for by VF001 carts alone"
        );
    }

    /// The register file is volatile board logic: a reset must power the
    /// protection up clean (config gate closed, no effect armed), exactly like
    /// a fresh load — the payload move must not smuggle it into the carried
    /// domain.
    #[test]
    fn vf001g_protection_state_is_volatile_across_reset() {
        let rom = make_vf001g_rom();
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        vf001g_arm_replacement(&mut cart, 0x2000, 5);
        vf001g_arm_injection(&mut cart, 0, 0x0100, &[0x11, 0x22, 0x33, 0x44], 4);
        assert_eq!(cart.read(0x2000), 0xC5);

        cart.reset();
        assert_eq!(cart.unl_mapper(), &UnlMapper::Vf001Gen(Box::default()));
        assert_eq!(cart.read(0x0100), rom[0x0100], "injection disarmed");
        assert_eq!(cart.read(0x2000), 0xC0, "replacement disarmed");
    }

    /// Write the correct boot-ROM header checksum into $014D.
    fn fix_header_checksum(rom: &mut [u8]) {
        let sum = rom[0x0134..0x014D].iter().fold(0u8, |a, &b| a.wrapping_sub(b).wrapping_sub(1));
        rom[0x014D] = sum;
    }

    #[test]
    fn mapper_name_covers_common_types() {
        let cases: &[(u8, &str)] = &[
            (0x00, "ROM ONLY"),
            (MBC1, "MBC1"),
            (MBC1_RAM_BATTERY, "MBC1+RAM+Battery"),
            (MBC2_BATTERY, "MBC2+Battery"),
            (MBC3_TIMER_RAM_BATTERY, "MBC3+RTC+RAM+Battery"),
            (MBC3_RAM_BATTERY, "MBC3+RAM+Battery"),
            (MBC5_RAM_BATTERY, "MBC5+RAM+Battery"),
            (MBC5_RUMBLE_RAM_BATTERY, "MBC5+Rumble+RAM+Battery"),
            (HUC1_RAM_BATTERY, "HuC1+RAM+Battery"),
            (POCKET_CAMERA, "Pocket Camera"),
            (TAMA5, "Bandai TAMA5"),
        ];
        for &(ty, name) in cases {
            let cart = Cartridge::from_bytes(&make_rom(ty, 0x02)).unwrap();
            assert_eq!(cart.mapper_name(), name, "type {ty:#04x}");
        }
    }

    /// TAMA5's save RAM and battery come from the TYPE byte, not the header
    /// RAM-size byte (which the real carts leave at $00) — the same shape as
    /// MBC7's EEPROM. Host-side identity only; the bus protocol is pinned by
    /// test-roms/src/cartridge/tama5_banking.dmg.mooneye.asm.
    #[test]
    fn tama5_allocates_save_ram_from_the_type_byte() {
        let cart = Cartridge::from_bytes(&make_sized_rom(TAMA5, 0x04, 0x80000)).unwrap();
        assert_eq!(cart.ram_size_bytes(), tama5::TAMA5_RAM_SIZE);
        assert!(cart.has_battery());
        // The bus must invalidate its cached ROM page map on cart-RAM-window
        // writes for this board: its bank register lives there.
        assert!(cart.banks_via_ram_window());
    }

    /// The nibble register file: an 8-bit bank assembled from BANK_LO/BANK_HI,
    /// the ACTIVE readiness flag, and a save-RAM byte round-tripping through
    /// the WRITE_LO/WRITE_HI + ADDR_HI/ADDR_LO command path.
    #[test]
    fn tama5_register_file_banks_and_round_trips_ram() {
        let mut cart = Cartridge::from_bytes(&make_sized_rom(TAMA5, 0x04, 0x80000)).unwrap();
        // `put(reg, nibble)`: select on the odd address, payload on the even one.
        let put = |cart: &mut Cartridge, reg: u8, value: u8| {
            cart.write(0xA001, reg);
            cart.write(0xA000, value);
        };
        // ACTIVE reads the readiness flag; the odd half of the window is open bus.
        cart.write(0xA001, 0x0A);
        assert_eq!(cart.read(0xA000), 0xF1);
        assert_eq!(cart.read(0xA001), 0xFF);

        // BANK_HI really is the high nibble: bank $12, not bank $02.
        put(&mut cart, 0x0, 0x2);
        put(&mut cart, 0x1, 0x1);
        assert_eq!(cart.read(0x5000), 0x12, "bank $12 must be mapped at $4000");

        // Write $A9 to save-RAM address $15 (ADDR_HI bit 0 is address bit 4).
        put(&mut cart, 0x4, 0x9);
        put(&mut cart, 0x5, 0xA);
        put(&mut cart, 0x6, 0x1);
        put(&mut cart, 0x7, 0x5);
        assert_eq!(cart.save_ram()[0x15], 0xA9);

        // Read it back as two nibble halves, upper nibble driven high.
        put(&mut cart, 0x6, 0x3); // command 1 (RAM read), address bit 4 set
        put(&mut cart, 0x7, 0x5);
        cart.write(0xA001, 0x0C);
        assert_eq!(cart.read(0xA000), 0xF9);
        cart.write(0xA001, 0x0D);
        assert_eq!(cart.read(0xA000), 0xFA);
    }

    #[test]
    fn oversized_bankless_header_infers_mbc1() {
        // A DMG-era beta/proto whose header type byte was left at $00 ROM ONLY
        // but whose file is 128KB: a bankless header on a >32KB board is
        // physically impossible, so the decode infers MBC1 and its banking must
        // actually reach the upper banks (not just relabel the mapper).
        let mut rom = make_sized_rom(0x00, 0x02, 0x20000); // type $00, 128KB
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);

        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        assert_eq!(cart.mapper_name(), "MBC1");
        assert!(matches!(
            cart.get_cartridge_type(),
            CartridgeType::MBC1 { ram: false, battery: false }
        ));

        // Power-on: fixed bank 0 low, switchable window defaults to bank 1.
        assert_eq!(cart.read(0x1000), 0);
        assert_eq!(cart.read(0x5000), 1);
        // A BANK1 write ($2000-$3FFF) moves the switchable window to a higher
        // bank -- proves banking is live, not merely the label.
        cart.write(0x2000, 0x05);
        assert_eq!(cart.read(0x5000), 5);

        // A genuine 32KB ROM ONLY cart is untouched (no regression).
        let cart = Cartridge::from_bytes(&make_rom(0x00, 0x00)).unwrap();
        assert_eq!(cart.mapper_name(), "ROM ONLY");
        assert!(matches!(cart.get_cartridge_type(), CartridgeType::NoMBC { .. }));
    }

    #[test]
    fn oversized_undocumented_type_infers_mbc1() {
        // Mofa Qiu - Magic Ball: 64KB, $0147 = $30, because its 23-character
        // title overran the 16-byte title field ($30 is the '0' of "930920").
        // $30 names no board, so it is not a bankless claim -- and >32KB rules
        // bankless out physically -- so the decode infers MBC1.
        let mut rom = make_sized_rom(0x30, 0x00, 0x10000); // type $30, 64KB
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);

        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        assert_eq!(cart.mapper_name(), "MBC1");
        assert_eq!(cart.read(0x1000), 0);
        assert_eq!(cart.read(0x5000), 1);
        cart.write(0x2000, 0x03);
        assert_eq!(cart.read(0x5000), 3);

        // A 32KB ROM with the same garbage byte stays bankless: the physical
        // argument that forces the inference does not apply there.
        let cart = Cartridge::from_bytes(&make_rom(0x30, 0x00)).unwrap();
        assert!(matches!(cart.get_cartridge_type(), CartridgeType::NoMBC { .. }));

        // Documented-but-unimplemented types are NOT inferred, however large:
        // $0B names a real board (MMM01), so it is evidence about the cart
        // rather than evidence the header is garbage.
        let mut rom = make_sized_rom(0x0B, 0x00, 0x10000);
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        let cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(
            matches!(cart.get_cartridge_type(), CartridgeType::NoMBC { .. }),
            "MMM01 ($0B) must not be inferred"
        );

        // MBC6 ($20) and TAMA5 ($FD) are documented AND implemented, so they
        // decode to their own boards -- the inference must not divert them to
        // MBC1 either.
        let mut rom = make_sized_rom(MBC6, 0x00, 0x10000);
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        let cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(cart.get_cartridge_type(), CartridgeType::MBC6));
        let mut rom = make_sized_rom(TAMA5, 0x00, 0x10000);
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        let cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(cart.get_cartridge_type(), CartridgeType::Tama5));
    }

    #[test]
    fn rom_and_ram_size_bytes() {
        // 256 KiB MBC5+RAM+BAT with an 8 KiB RAM code.
        let mut rom = make_sized_rom(MBC5_RAM_BATTERY, 0x03, 0x40000);
        rom[RAM_SIZE_OFFSET] = 0x02; // 8 KiB
        let cart = Cartridge::from_bytes(&rom).unwrap();
        assert_eq!(cart.rom_size_bytes(), 0x40000);
        assert_eq!(cart.ram_size_bytes(), 0x2000);

        // ROM ONLY, no RAM.
        let cart = Cartridge::from_bytes(&make_rom(0x00, 0x00)).unwrap();
        assert_eq!(cart.rom_size_bytes(), 0x8000);
        assert_eq!(cart.ram_size_bytes(), 0);
    }

    #[test]
    fn destination_and_licensee() {
        let mut rom = make_rom(MBC1, 0x00);
        rom[0x014A] = 0x00;
        rom[0x014B] = 0x01; // old licensee: Nintendo
        let cart = Cartridge::from_bytes(&rom).unwrap();
        assert_eq!(cart.destination(), Some(Destination::Japanese));
        assert_eq!(cart.licensee(), Some("Nintendo"));

        // Overseas + new-licensee indirection ($014B == $33 -> read $0144-45).
        let mut rom = make_rom(MBC1, 0x00);
        rom[0x014A] = 0x01;
        rom[0x014B] = 0x33;
        rom[0x0144] = b'0';
        rom[0x0145] = b'8'; // "08" -> Capcom
        let cart = Cartridge::from_bytes(&rom).unwrap();
        assert_eq!(cart.destination(), Some(Destination::Overseas));
        assert_eq!(cart.licensee(), Some("Capcom"));
    }

    #[test]
    fn header_and_global_checksum() {
        let mut rom = make_rom(MBC3_RAM_BATTERY, 0x03);
        fix_header_checksum(&mut rom);
        let cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(cart.header_checksum_valid());
        // global_checksum sums every byte except $014E-$014F.
        let expected: u16 = rom
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != 0x014E && i != 0x014F)
            .fold(0u16, |a, (_, &b)| a.wrapping_add(b as u16));
        assert_eq!(cart.global_checksum(), expected);

        // Corrupt a header byte -> checksum no longer matches.
        let mut bad = rom.clone();
        bad[0x0140] = bad[0x0140].wrapping_add(1);
        let cart = Cartridge::from_bytes(&bad).unwrap();
        assert!(!cart.header_checksum_valid());
    }

    #[test]
    fn vf001_serves_protection_transform_table() {
        let mut cart = Cartridge::from_bytes(&make_vf001_rom()).unwrap();

        // Arm each command by writing its three bytes to port 0 ($7080), pick
        // a select port ($7480/$7880), and read the value back through the RAM
        // window. Ports: A10-A11 of the write address; the read port likewise.
        let arm = |cart: &mut Cartridge, bytes: [u8; 3]| {
            for b in bytes {
                cart.write(0x7080, b);
            }
        };

        // Boot gate: cmd $9A,$B8,$B9 -> $A800 returns $C1 (sel $B9) / $F8 (sel $83).
        arm(&mut cart, [0x9A, 0xB8, 0xB9]);
        cart.write(0x7480, 0xB9); // select via port 1
        assert_eq!(cart.read(0xA800), 0xC1);
        cart.write(0x7480, 0x83);
        assert_eq!(cart.read(0xA800), 0xF8);

        // Second gate: cmd $37,$52,$CD -> $A800 returns $82 (sel $BA) / $8F (sel $A9).
        arm(&mut cart, [0x37, 0x52, 0xCD]);
        cart.write(0x7880, 0xBA); // select via port 2
        assert_eq!(cart.read(0xA800), 0x82);
        cart.write(0x7880, 0xA9);
        assert_eq!(cart.read(0xA800), 0x8F);

        // Bank-switch command drives the MBC5 ROM-bank register to 6.
        arm(&mut cart, [0x7E, 0x29, 0x79]);
        assert!(matches!(&cart.mapper, Mapper::Vf001(m) if m.regs.rom_bank_low == 6));
        assert_eq!(cart.read(0xAFFF), 0x31); // port 3 decoy readback

        // An unarmed read falls through to normal cart RAM (saves still work).
        cart.write(0x0000, 0x0A); // RAMG on
        arm(&mut cart, [0x00, 0x00, 0x00]);
        cart.write(0xA400, 0x5A);
        assert_eq!(cart.read(0xA400), 0x5A);
    }

    /// 1MB image carrying the Zook Z signature: the "V.fame" $0184 logo (whose
    /// CRC32 is `VF001G_LOGO_CRC32[0]`, taken from the real cart's own 48 bytes)
    /// plus the bank-select thunk at $3EF5, behind the cart's spoofed MBC1
    /// header.
    fn make_vf001z_rom(logo: &[u8; 48]) -> Vec<u8> {
        let mut rom = vec![0u8; 0x100000];
        rom[CARTRIDGE_TYPE_OFFSET] = 0x01; // MBC1 (the board does not honour it)
        rom[ROM_SIZE_OFFSET] = 0x05; // 1MB / 64 banks
        rom[0x184..0x1B4].copy_from_slice(logo);
        rom[VF001Z_THUNK_OFFSET..VF001Z_THUNK_OFFSET + VF001Z_THUNK.len()]
            .copy_from_slice(&VF001Z_THUNK);
        rom
    }

    /// The 48 bytes whose CRC32 is `VF001G_LOGO_CRC32[0]`: a clean-room block
    /// (ASCII banner + a CRC32-forcing suffix), the same construction the
    /// first-party `vf001zook_protection` ROM uses. Detection keys only on the
    /// CRC32, so no copyrighted logo bytes are needed.
    const VF001Z_LOGO: [u8; 48] = *b"RUSTYBOI VF001 ZOOK CLEANROOM STANDIN SIG!!!\xDF\xD4\x43\xD7";

    #[test]
    fn vf001z_detects_only_with_logo_and_thunk() {
        let rom = make_vf001z_rom(&VF001Z_LOGO);
        assert_eq!(crate::checksum::crc32(&rom[0x184..0x1B4]), VF001G_LOGO_CRC32[0]);
        assert_eq!(
            Cartridge::detect_unl_mapper(&rom),
            UnlMapper::Vf001Zook(Box::default())
        );
        // The MBC1 header is a lie: the board is electrically a bare MBC5.
        let cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(
            cart.get_cartridge_type(),
            CartridgeType::MBC5 { ram: false, battery: false, rumble: false }
        ));

        // Right logo, no thunk -> not this board (the $7000 config-register
        // dialect and this one must never be confused).
        let mut no_thunk = make_vf001z_rom(&VF001Z_LOGO);
        no_thunk[VF001Z_THUNK_OFFSET..VF001Z_THUNK_OFFSET + VF001Z_THUNK.len()].fill(0);
        assert_eq!(Cartridge::detect_unl_mapper(&no_thunk), UnlMapper::None);

        // Right thunk, wrong logo -> no match either.
        let mut no_logo = make_vf001z_rom(&VF001Z_LOGO);
        no_logo[0x184..0x1B4].fill(0);
        assert_eq!(Cartridge::detect_unl_mapper(&no_logo), UnlMapper::None);
    }

    #[test]
    fn vf001z_answers_bank_and_challenge_transactions() {
        let mut cart = Cartridge::from_bytes(&make_vf001z_rom(&VF001Z_LOGO)).unwrap();

        // A bank select is four bytes at $7081; only the first three are keyed.
        let select = |cart: &mut Cartridge, b: [u8; 4]| {
            for x in b {
                cart.write(0x7081, x);
            }
        };
        select(&mut cart, [0x46, 0x58, 0x54, 0x5F]);
        assert_eq!(cart.get_rom_bank(), 4);
        select(&mut cart, [0x34, 0x40, 0x5A, 0x33]);
        assert_eq!(cart.get_rom_bank(), 3);
        // $31 is not a reset: a key containing it still selects.
        select(&mut cart, [0x0A, 0x31, 0x18, 0x57]);
        assert_eq!(cart.get_rom_bank(), 0x0B);
        // The fourth byte is ignored -- same bank from a different one.
        select(&mut cart, [0xA4, 0xBA, 0xD5, 0x44]);
        assert_eq!(cart.get_rom_bank(), 2);
        select(&mut cart, [0x0A, 0x31, 0x18, 0x99]);
        assert_eq!(cart.get_rom_bank(), 0x0B);

        // The same four bytes written anywhere BUT $7081 are just stream bytes,
        // so they must not re-bank.
        select(&mut cart, [0xA4, 0xBA, 0xD5, 0x44]);
        for x in [0x46u8, 0x58, 0x54, 0x5F] {
            cart.write(0x7080, x);
        }
        assert_eq!(cart.get_rom_bank(), 2);

        // Challenge-response: the answer is keyed on the stream AND the
        // register selected by A8-A11 of the read address.
        for x in [0xA8u8, 0xB6] {
            cart.write(0x7080, x);
        }
        assert_eq!(cart.read(0xA080), 0x6E);
        assert_eq!(cart.read(0xA180), 0xFF); // wrong register: no answer
        for x in [0x77u8, 0x13, 0xB4] {
            cart.write(0x7A80, x);
        }
        assert_eq!(cart.read(0xA680), 0x22);
    }

    /// The port's shift window is volatile board logic: a reset must power it
    /// up empty, exactly like a fresh load.
    #[test]
    fn vf001z_port_state_is_volatile_across_reset() {
        let mut cart = Cartridge::from_bytes(&make_vf001z_rom(&VF001Z_LOGO)).unwrap();
        for x in [0xA8u8, 0xB6] {
            cart.write(0x7080, x);
        }
        assert_eq!(cart.read(0xA080), 0x6E);
        cart.reset();
        // The shift register powers up empty, so the same read answers nothing.
        assert_eq!(cart.read(0xA080), 0xFF);
        assert_eq!(cart.unl_mapper(), &UnlMapper::Vf001Zook(Box::default()));
    }

    /// The window lives in the variant payload, so a mid-challenge savestate
    /// has to restore it: the same read must still answer after a round-trip.
    #[test]
    fn vf001z_port_state_round_trips_through_a_savestate() {
        let rom = make_vf001z_rom(&VF001Z_LOGO);
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        for x in [0x46u8, 0x58, 0x54, 0x5F] {
            cart.write(0x7081, x);
        }
        for x in [0xA8u8, 0xB6] {
            cart.write(0x7080, x);
        }

        let bytes = bincode::serialize(&cart).unwrap();
        let mut restored: Cartridge = bincode::deserialize(&bytes).unwrap();
        restored.attach_rom(rom.clone());
        assert_eq!(restored.unl_mapper(), cart.unl_mapper(), "the window must serialize");
        assert_eq!(restored.read(0xA080), 0x6E);
        assert_eq!(restored.get_rom_bank(), 4, "bank register survived too");
    }

    /// No other board may pay for the shift window: it is inside the variant
    /// payload, so every other `UnlMapper` serializes to the bare variant index.
    #[test]
    fn vf001z_shift_window_costs_non_users_nothing() {
        let none = bincode::serialize(&UnlMapper::None).unwrap().len();
        let vfz = bincode::serialize(&UnlMapper::Vf001Zook(Box::default())).unwrap().len();
        assert_eq!(
            vfz - none,
            VF001Z_STREAM_MAX + 2,
            "the 32-byte window plus its two counters must be paid for by Zook Z alone"
        );
    }

    #[test]
    fn vf001_protection_state_is_volatile_across_reset() {
        let mut cart = Cartridge::from_bytes(&make_vf001_rom()).unwrap();
        cart.write(0x7080, 0x9A);
        cart.write(0x7080, 0xB8);
        cart.write(0x7080, 0xB9);
        cart.write(0x7480, 0xB9);
        assert_eq!(cart.read(0xA800), 0xC1); // armed
        cart.reset();
        // After a power cycle the register file is blank: the same read no
        // longer matches any command and falls through to RAM (0xFF, RAMG off).
        assert_eq!(cart.read(0xA800), 0xFF);
        assert_eq!(cart.unl_mapper, UnlMapper::Vf001(Vf001State::default()));
    }

    #[test]
    fn licensed_shapes_are_not_misdetected() {
        // Plain 32KB ROM-only cart with the Nintendo logo (e.g. Tetris).
        let mut rom = make_rom(0x00, 0x00);
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        rom[0x134..0x13A].copy_from_slice(b"TETRIS");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);

        // 128KB MBC1 cart titled GAME (the shape of the owner's descrambled
        // Sachen singles): must stay plain MBC1.
        let mut rom = make_sized_rom(0x01, 0x02, 0x20000);
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        rom[0x134..0x138].copy_from_slice(b"GAME");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);
        let cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(cart.get_cartridge_type(), CartridgeType::MBC1 { .. }));

        // Header claims 32KB but the file is 2MB with a normal logo
        // (gbmicrotest shape, type $03): still MBC1, never Wisdom Tree.
        let mut rom = make_sized_rom(0x03, 0x00, 0x200000);
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        rom[0x134..0x13D].copy_from_slice(b"microtest");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);

        // A real 256KB MBC3+RAM+BATTERY ($10) cart NOT titled "TETRIS SET"
        // must stay MBC3 -- M161 detection is gated on the exact title.
        let mut rom = make_sized_rom(0x10, 0x03, 0x40000);
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        rom[0x134..0x13B].copy_from_slice(b"POKEMON");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);
        assert!(matches!(
            Cartridge::from_bytes(&rom).unwrap().get_cartridge_type(),
            CartridgeType::MBC3 { .. }
        ));

        // A genuine 1MB MBC5+RAM+BATTERY cart with the Nintendo logo must stay
        // MBC5: VF001 needs BOTH the VF secondary logo sum at $0184 AND the
        // boot stub, so a licensed MBC5 can never match.
        let mut rom = make_sized_rom(MBC5_RAM_BATTERY, 0x05, 0x100000);
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        rom[0x134..0x139].copy_from_slice(b"MBC5G");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::None);
    }

    #[test]
    fn m161_latches_a_32kb_bank_once() {
        // Mani 4 in 1 shape: 256KB, header spoofs MBC3+RAM+BAT ($10), title
        // "TETRIS SET" (M161 board).
        let mut rom = make_sized_rom(0x10, 0x03, 0x40000);
        rom[0x134..0x13E].copy_from_slice(b"TETRIS SET");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::M161);

        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(cart.get_cartridge_type(), CartridgeType::M161));
        assert!(!cart.has_battery()); // RAM disabled + zeroed header type

        // Power-on (unmapped): the first 32KB pair -> 16KB banks 0 and 1.
        assert_eq!(cart.read(0x1000), 0);
        assert_eq!(cart.read(0x5000), 1);
        // External RAM line is permanently disabled.
        assert_eq!(cart.read(0xA000), 0xFF);

        // First ROM write anywhere in $0000-$7FFF latches the 32KB bank from
        // data bits 0-2: value 3 -> even/odd 16KB banks 6/7.
        cart.write(0x2000, 0x03);
        assert_eq!(cart.read(0x1000), 6);
        assert_eq!(cart.read(0x5000), 7);

        // Every later write is ignored until reset (one-shot latch).
        cart.write(0x6000, 0x01);
        assert_eq!(cart.read(0x1000), 6);
        assert_eq!(cart.read(0x5000), 7);

        // Bank 7 (data & 7) selects the top 32KB pair (banks 14/15).
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        cart.write(0x0000, 0xFF); // low 3 bits = 7; upper bits ignored
        assert_eq!(cart.read(0x1000), 14);
        assert_eq!(cart.read(0x5000), 15);
    }

    #[test]
    fn wisdom_tree_detects_and_switches_whole_window() {
        // Exodus shape: type $00, header claims 32KB, 128KB file, publisher
        // string in the ROM.
        let mut rom = make_sized_rom(0x00, 0x00, 0x20000);
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        rom[0x300..0x30B].copy_from_slice(b"WISDOM TREE");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::WisdomTree);

        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        // Power-on: 32KB bank 0 across the whole window.
        assert_eq!(cart.read(0x1000), 0);
        assert_eq!(cart.read(0x5000), 1);
        // Bank select = ADDRESS low bits of any $0000-$3FFF write; the data
        // byte is ignored.
        cart.write(0x0003, 0xA5);
        assert_eq!(cart.read(0x1000), 6); // 16KB banks 6/7 = 32KB bank 3
        assert_eq!(cart.read(0x5000), 7);
        // Out-of-range bank wraps on the wired lines (128KB = 4 x 32KB).
        cart.write(0x0005, 0x00);
        assert_eq!(cart.read(0x1000), 2); // bank 5 % 4 = 1 -> 16KB banks 2/3
        assert_eq!(cart.read(0x5000), 3);

        // The Pan Docs $C0/$D1 header magic alone also detects.
        let mut rom = make_sized_rom(0x00, 0x00, 0x10000);
        rom[0x147] = 0xC0;
        rom[0x14A] = 0xD1;
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::WisdomTree);
    }

    #[test]
    fn rocket_games_registers_and_boot_lock() {
        // Rocket carts store their own logo (sums to 2756), which is what the
        // detection keys on.
        let mut rom = make_sized_rom(0x97, 0x04, 0x80000);
        rom[0x104..0x134].copy_from_slice(&ROCKET_LOGO);
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::Rocket);

        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        cart.skip_boot_handoff(); // no boot ROM: start unlocked
        // Unlocked reads return the raw (Rocket) logo.
        assert_eq!(cart.read(0x0104), ROCKET_LOGO[0]);
        // Inner bank at exactly $3F00 (0 -> 1), outer 256KB bank at $3FC0.
        assert_eq!(cart.read(0x5000), 1);
        cart.write(0x3F00, 0x05);
        assert_eq!(cart.read(0x5000), 5);
        cart.write(0x3F00, 0x00);
        assert_eq!(cart.read(0x5000), 1);
        cart.write(0x3FC0, 0x01);
        assert_eq!(cart.read(0x1000), 16); // outer bank alone at $0000
        assert_eq!(cart.read(0x5000), 17); // outer | inner at $4000
        // Writes elsewhere in the region are ignored.
        cart.write(0x2000, 0x07);
        assert_eq!(cart.read(0x5000), 17);

        // Boot lock: a fresh cart is locked; after 0x30 ROM reads it enters the
        // CGB phase where $0104-$0133 present the logo the boot ROM supplied
        // (the boot ROM check), and after 0x30 more it unlocks. The logo is
        // sourced from the boot ROM at runtime; simulate that here.
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        cart.set_rocket_boot_logo(LICENSED_LOGO);
        for _ in 0..0x31 {
            cart.read(0x0000);
        }
        assert_eq!(cart.read(0x0104), LICENSED_LOGO[0]);
        assert_eq!(cart.read(0x0105), LICENSED_LOGO[1]);
        for _ in 0..0x31 {
            cart.read(0x0000);
        }
        // Unlocked again: raw (Rocket) logo.
        assert_eq!(cart.read(0x0104), ROCKET_LOGO[0]);
    }

    #[test]
    fn sachen_mmc1_descramble_lock_and_masked_banking() {
        // Raw-dump shape: the Nintendo logo lives at the DESCRAMBLED
        // positions of $0104 (CPU reads through the $01xx address swizzle),
        // and the Sachen logo (here: marker bytes) at the |0x80 copy.
        let mut rom = make_sized_rom(0x00, 0x00, 0x20000);
        for i in 0..48u16 {
            rom[Cartridge::sachen_unscramble(0x104 + i) as usize] = LICENSED_LOGO[i as usize];
            rom[Cartridge::sachen_unscramble(0x184 + i) as usize] = 0xB0 | (i as u8 & 0x0F);
        }
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::SachenMmc1);

        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        // The boot-logo override presents the locked view (Sachen logo).
        let logo = cart.boot_logo_override().unwrap();
        assert_eq!(logo[0], 0xB0);
        assert_eq!(logo[47], 0xB0 | (47 & 0x0F));

        // Locked (power-on): $01xx reads are forced to the |0x80 copy. The
        // 0x31st such read unlocks.
        for i in 0..0x30u16 {
            assert_eq!(cart.read(0x0104 + i), 0xB0 | (i as u8 & 0x0F));
        }
        // Unlock transition read, then the descrambled Nintendo logo is
        // visible at $0104.
        cart.read(0x0104);
        assert_eq!(cart.read(0x0104), LICENSED_LOGO[0]);
        assert_eq!(cart.read(0x0105), LICENSED_LOGO[1]);
        assert_eq!(cart.read(0x0133), LICENSED_LOGO[47]);

        // Masked outer banking: base/mask only latch while
        // the inner bank has bits 4-5 set; effective switchable bank =
        // base&mask | bank&~mask, base window = base&mask.
        cart.write(0x2000, 0x33); // open the latch gate
        cart.write(0x0000, 0x04); // base
        cart.write(0x4000, 0x04); // mask
        cart.write(0x2000, 0x03); // inner bank (gate now closed)
        cart.write(0x0000, 0x00); // ignored: gate closed
        assert_eq!(cart.read(0x1000), 4); // base & mask
        assert_eq!(cart.read(0x5000), 7); // 4 | 3
        // skip_boot_handoff unlocks immediately (no boot ROM).
        let mut fresh = Cartridge::from_bytes(&rom).unwrap();
        fresh.skip_boot_handoff();
        assert_eq!(fresh.read(0x0104), LICENSED_LOGO[0]);
    }

    // 48 bytes whose CRC32 is the Gowin "Story of Lasama" detection constant
    // (0xDD1165F1): a clean-room ASCII banner + a 4-byte CRC-forcing suffix, so
    // the fixture carries the signature with no copyrighted logo bytes. (Same
    // block as test-roms/src/cartridge/gowin_banking.dmg.mooneye.asm.)
    const GOWIN_LOGO: [u8; 48] = [
        0x52, 0x55, 0x53, 0x54, 0x59, 0x42, 0x4F, 0x49, 0x20, 0x47, 0x4F, 0x57, 0x49, 0x4E, 0x20,
        0x4C, 0x41, 0x53, 0x41, 0x4D, 0x41, 0x20, 0x47, 0x53, 0x30, 0x34, 0x20, 0x43, 0x4C, 0x45,
        0x41, 0x4E, 0x52, 0x4F, 0x4F, 0x4D, 0x20, 0x53, 0x49, 0x47, 0x21, 0x21, 0x21, 0x21, 0xB3,
        0x97, 0xA2, 0x17,
    ];

    /// A 128KB Gowin "Story of Lasama" image: the $0184 CRC32 signature and the
    /// $02D7 boot stub, over `make_sized_rom`'s per-bank markers.
    fn make_gowin_rom() -> Vec<u8> {
        let mut rom = make_sized_rom(0x01, 0x02, 0x20000); // MBC1 header, 128KB
        rom[0x184..0x1B4].copy_from_slice(&GOWIN_LOGO);
        rom[GOWIN_STUB_OFFSET..GOWIN_STUB_OFFSET + GOWIN_STUB.len()].copy_from_slice(&GOWIN_STUB);
        rom
    }

    #[test]
    fn gowin_detects_and_outer_bank_handshake_offsets_both_windows() {
        let rom = make_gowin_rom();
        assert_eq!(
            Cartridge::detect_unl_mapper(&rom),
            UnlMapper::Gowin(GowinState::default())
        );
        // Both halves of the key are load-bearing: perturbing either the CRC
        // block or the boot stub drops detection back to plain MBC1.
        let mut no_crc = rom.clone();
        no_crc[0x184] ^= 0x01;
        assert_eq!(Cartridge::detect_unl_mapper(&no_crc), UnlMapper::None);
        let mut no_stub = rom.clone();
        no_stub[GOWIN_STUB_OFFSET] ^= 0x01;
        assert_eq!(Cartridge::detect_unl_mapper(&no_stub), UnlMapper::None);

        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        // Electrically MBC1, no RAM.
        assert!(matches!(
            cart.get_cartridge_type(),
            CartridgeType::MBC1 { ram: false, battery: false }
        ));
        // Power-on base 0: the fixed window is bank 0, the switchable window is
        // the MBC1 inner bank (default 1).
        assert_eq!(cart.read(0x1000), 0);
        assert_eq!(cart.read(0x5000), 1);

        // Two-write $6000 handshake: parameter $02 then a commit strobe sets the
        // outer base to 2<<1 = 4, added to BOTH windows. A plain MBC1 would read
        // the second write ($BE) as a mode-select and leave the low bank at 0.
        cart.write(0x6000, 0x02);
        cart.write(0x6000, 0xBE);
        assert_eq!(cart.read(0x1000), 4, "fixed window swung to bank 4");
        assert_eq!(cart.read(0x5000), 5, "switchable window = base 4 + inner 1");
        // The inner ($2000) register still indexes within the selected half.
        cart.write(0x2000, 0x02);
        assert_eq!(cart.read(0x5000), 6, "base 4 + inner 2");

        // A parameter-0 handshake restores base 0 (both windows fall back).
        cart.write(0x6000, 0x00);
        cart.write(0x6000, 0xBE);
        assert_eq!(cart.read(0x1000), 0);
        assert_eq!(cart.read(0x5000), 2, "base 0 + inner 2");
    }

    /// A Sachen MMC2 raw dump: the Nintendo logo lives at the DESCRAMBLED $0184
    /// positions (so the scrambled-$0184 sum reads as the Nintendo logo, the
    /// MMC2 tell), over `make_sized_rom`'s per-bank markers.
    fn make_sachen_mmc2_rom(rom_size_code: u8, size: usize) -> Vec<u8> {
        let mut rom = make_sized_rom(0x00, rom_size_code, size);
        for i in 0..48u16 {
            rom[Cartridge::sachen_unscramble(0x184 + i) as usize] = LICENSED_LOGO[i as usize];
        }
        rom
    }

    #[test]
    fn sachen_reads_the_cgb_flag_through_the_header_scramble() {
        // Rocman X Gold shape: a CGB Sachen multicart whose CGB flag the CPU
        // reads at $0143 THROUGH the $01xx address scramble, i.e. from the
        // descrambled offset. The raw $0143 byte is not the flag.
        let mut rom = make_sachen_mmc2_rom(0x04, 0x80000); // 512KB, MMC2
        let scrambled = Cartridge::sachen_unscramble(0x143) as usize;
        assert_ne!(scrambled, 0x143, "the CGB flag is genuinely relocated");
        rom[0x143] = 0x00; // raw byte says "DMG only"
        rom[scrambled] = CGB_COMPATIBLE; // the flag the mapper actually presents
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::SachenMmc2);

        let cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(
            cart.supports_cgb(),
            "the descrambled $0151 CGB flag ($80) must win over the raw $0143"
        );

        // Control: the same descramble must NOT invent CGB support out of a
        // genuinely DMG Sachen cart (both the raw and descrambled bytes DMG).
        let mut dmg = make_sachen_mmc2_rom(0x04, 0x80000);
        dmg[0x143] = 0xC0; // raw looks CGB-only...
        dmg[scrambled] = 0x00; // ...but the descrambled flag is DMG
        assert!(!Cartridge::from_bytes(&dmg).unwrap().supports_cgb());
    }

    #[test]
    fn sachen_multicart_out_of_rom_outer_bank_reads_open_bus() {
        // Rocman X Gold's boot probe reads $0143 under outer banks $00/$20/$40
        // and loops forever unless the two out-of-ROM banks read back a
        // different (open-bus) byte than the in-ROM one. On a 128KB (8-bank)
        // cart both $20 and $40 address beyond the chip, so they must be $FF.
        let mut rom = make_sachen_mmc2_rom(0x02, 0x20000); // 128KB = 8 banks
        rom[0x0000] = 0x5A; // a distinctive byte in the fixed window, bank 0

        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        // In-ROM multicart select: open the latch gate (inner bits 4-5 set),
        // program base/mask, then read the fixed window. base&mask = 4 < 8.
        cart.write(0x2000, 0x33); // gate open
        cart.write(0x0000, 0x04); // base
        cart.write(0x4000, 0x04); // mask
        cart.write(0x2000, 0x03); // inner bank (gate closed)
        assert_eq!(cart.read(0x1000), 4, "in-ROM outer bank selects bank 4");
        assert_eq!(cart.read(0x5000), 4 | 3, "switchable = base | inner");

        // Out-of-ROM outer bank ($40 & $40 = 64 >= 8): the whole window is open
        // bus, matching the real cart's solder-pad wiring. WITHOUT this the
        // wrapped read would return bank-0 data and the menu probe would hang.
        cart.write(0x2000, 0x30); // gate open
        cart.write(0x0000, 0x40); // base past the ROM
        cart.write(0x4000, 0x40); // mask selects that bit
        assert_eq!(cart.read(0x0000), 0xFF, "fixed window is open bus");
        assert_eq!(cart.read(0x1000), 0xFF);
        assert_eq!(cart.read(0x5000), 0xFF, "switchable window is open bus");

        // The $20 (512KB) outer bank is likewise beyond an 8-bank chip.
        cart.write(0x2000, 0x30);
        cart.write(0x0000, 0x20);
        cart.write(0x4000, 0x20);
        assert_eq!(cart.read(0x1000), 0xFF);

        // Back in range ($00): normal ROM, so the probe sees a real byte that
        // differs from the open-bus reads and proceeds.
        cart.write(0x2000, 0x30);
        cart.write(0x0000, 0x00);
        cart.write(0x4000, 0x00);
        assert_eq!(cart.read(0x0000), 0x5A, "outer bank 0 is real ROM again");
    }

    // 48 bytes whose CRC32 is the Action Replay V4 detection constant
    // (0xA69E896B): a clean-room ASCII banner plus a 4-byte CRC-forcing
    // suffix, so the fixture carries the signature without reproducing the
    // device's own header block.
    const ARV4_HEADER: [u8; 48] = [
        0x52, 0x55, 0x53, 0x54, 0x59, 0x42, 0x4F, 0x49, 0x20, 0x41, 0x43, 0x54,
        0x49, 0x4F, 0x4E, 0x20, 0x52, 0x45, 0x50, 0x4C, 0x41, 0x59, 0x20, 0x56,
        0x34, 0x20, 0x43, 0x4C, 0x45, 0x41, 0x4E, 0x52, 0x4F, 0x4F, 0x4D, 0x20,
        0x53, 0x49, 0x47, 0x21, 0x21, 0x21, 0x21, 0x21, 0x1A, 0x14, 0x6A, 0xC0,
    ];
    // Likewise for the Xploder GB constant (0xF13FFA9A).
    const XPLODER_HEADER: [u8; 48] = [
        0x52, 0x55, 0x53, 0x54, 0x59, 0x42, 0x4F, 0x49, 0x20, 0x58, 0x50, 0x4C,
        0x4F, 0x44, 0x45, 0x52, 0x20, 0x47, 0x42, 0x20, 0x46, 0x43, 0x44, 0x20,
        0x43, 0x4C, 0x45, 0x41, 0x4E, 0x52, 0x4F, 0x4F, 0x4D, 0x20, 0x53, 0x49,
        0x47, 0x21, 0x21, 0x21, 0x21, 0x21, 0x21, 0x21, 0x0A, 0xC6, 0xE6, 0xB2,
    ];

    #[test]
    fn action_replay_v4_board() {
        // GameShark Online shape: bankless header type, no logo, 80KB (an
        // exact 10 x 8KB pages - the device's ROM is paged, not banked, so a
        // non-power-of-two image is the natural size, not a truncated dump).
        let mut rom = make_sized_rom(0x00, 0x00, 0x14000);
        rom[0x104..0x134].copy_from_slice(&ARV4_HEADER);
        // Per-8KB-page marker so the page register is observable.
        for page in 0..(rom.len() / 0x2000) {
            rom[page * 0x2000 + 0x100] = 0xA0 | page as u8;
        }
        assert_eq!(
            Cartridge::detect_unl_mapper(&rom),
            UnlMapper::ActionReplayV4(ArV4State::default())
        );

        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        // A bankless header would have left the upper pages unreachable and
        // the whole $6000-$7FFF window read-only.
        assert!(cart.has_battery());
        assert_eq!(cart.ram_data.len(), ARV4_RAM_BANKS * RAM_BANK_SIZE);

        // $7FE1 pages 8KB at $4000-$5FFF; the boot's first far call is
        // `ld a,$09 / ld ($7FE1),a / call $4100`.
        cart.write(0x7FE1, 0x09);
        assert_eq!(cart.read(0x4100), 0xA0 | 9);
        cart.write(0x7FE1, 0x02);
        assert_eq!(cart.read(0x4100), 0xA0 | 2);
        // Out-of-range pages wrap to the image (padded to 8 banks = 16 pages).
        assert_eq!(cart.rom_data.len(), 0x20000);
        cart.write(0x7FE1, 0x12);
        assert_eq!(cart.read(0x4100), 0xA0 | 2);

        // $6000-$7FFF is SRAM: the firmware runs its stack from $7FC0 and
        // copies code to $7800 to execute it, so both must stick.
        cart.write(0x7FBF, 0x5A);
        assert_eq!(cart.read(0x7FBF), 0x5A);
        cart.write(0x7800, 0xC9);
        assert_eq!(cart.read(0x7800), 0xC9);
        cart.write(0x6000, 0x11);
        assert_eq!(cart.read(0x6000), 0x11);

        // The register block is write-only and reads back 0. The firmware
        // tests bits 0 and 4 of $7FEE and only takes its normal boot path when
        // both are clear, so open-bus $FF there sends it down the error path.
        assert_eq!(cart.read(0x7FEE), 0x00);
        assert_eq!(cart.read(0x7FE1), 0x00);

        // Nothing below $6000 is writable - there is no MBC behind it.
        cart.write(0x2000, 0x05);
        assert_eq!(cart.read(0x4100), 0xA0 | 2, "no MBC bank register");
    }

    #[test]
    fn xploder_gb_board() {
        // Xploder GB shape: garbage header throughout ($69/$67/$6E), no logo,
        // 128KB.
        let mut rom = make_sized_rom(0x69, 0x67, 0x20000);
        rom[RAM_SIZE_OFFSET] = 0x6E;
        rom[0x104..0x134].copy_from_slice(&XPLODER_HEADER);
        assert_eq!(
            Cartridge::detect_unl_mapper(&rom),
            UnlMapper::XploderGb(XploderState::default())
        );

        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(cart.has_battery());
        assert_eq!(cart.ram_data.len(), XPLODER_RAM_BANKS * RAM_BANK_SIZE);

        // $0006 is the 16KB ROM bank. An inferred MBC1 would have read this as
        // RAM-enable and left the bank stuck at 1.
        assert_eq!(cart.read(0x5000), 1, "power-on bank 1");
        cart.write(0x0006, 0x06);
        assert_eq!(cart.read(0x5000), 6);
        cart.write(0x0006, 0x03);
        assert_eq!(cart.read(0x5000), 3);
        // MBC5-style: bank 0 is selectable, and out-of-range wraps.
        cart.write(0x0006, 0x00);
        assert_eq!(cart.read(0x5000), 0);
        cart.write(0x0006, 0x09);
        assert_eq!(cart.read(0x5000), 1, "9 % 8 banks");

        // $0007 is the RAM bank, and the RAM has no enable gate: the boot
        // writes $0007 and then stores straight to $B000, which a gated board
        // would drop.
        cart.write(0x0007, 0x00);
        cart.write(0xB000, 0x11);
        cart.write(0x0007, 0x0E); // the bank the firmware copies out to WRAM
        cart.write(0xB000, 0x22);
        assert_eq!(cart.read(0xB000), 0x22);
        cart.write(0x0007, 0x00);
        assert_eq!(cart.read(0xB000), 0x11, "banks are independent");
    }

    #[test]
    fn blank_header_ram_type_still_gets_a_ram_chip() {
        // Pro Action Replay (Europe): the whole header is erased to $FF, so the
        // RAM-size byte is out of spec. The type byte that survives ($FF, HuC1
        // +RAM+BATTERY) still names a board with a RAM chip, and the device's
        // menu keeps its state there - with zero banks it reads back $FF and
        // never draws a screen.
        let mut cart = Cartridge::from_bytes(&make_rom(HUC1_RAM_BATTERY, 0xFF)).unwrap();
        assert_eq!(cart.ram_banks, 1);
        cart.write(0xB000, 0x3C);
        assert_eq!(cart.read(0xB000), 0x3C);

        // A type byte that names no RAM chip keeps the old "garbage size byte
        // means no RAM" answer (Sonic 3D Blast 5 and the Sachen/Makon carts,
        // whose header fields are overrun by game data).
        assert_eq!(Cartridge::from_bytes(&make_rom(MBC1, 0x20)).unwrap().ram_banks, 0);
        assert_eq!(Cartridge::from_bytes(&make_rom(0x30, 0x32)).unwrap().ram_banks, 0);
        // And an in-spec size byte is still honoured verbatim.
        assert_eq!(Cartridge::from_bytes(&make_rom(MBC5_RAM_BATTERY, 0x03)).unwrap().ram_banks, 4);
        assert_eq!(Cartridge::from_bytes(&make_rom(MBC5_RAM_BATTERY, 0x00)).unwrap().ram_banks, 0);
    }

    #[test]
    fn nt_old2_swap_multicart_and_ram_declare() {
        // Super Mario Special 3 shape: MBC1-spoofing header, Makon "MK"
        // licensee, 256KB.
        let mut rom = make_sized_rom(0x01, 0x03, 0x40000);
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        rom[0x134..0x141].copy_from_slice(b"SUPER MARIO 3");
        rom[0x144] = b'M';
        rom[0x145] = b'K';
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::NtOld2);

        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        // MBC3-style 8-bit bank, 0 -> 1.
        cart.write(0x2000, 0x05);
        assert_eq!(cart.read(0x5000), 5);
        // $5003 bit-swap mode: bank lines reorder combinationally
        // (reorder: out0=in1, out1=in2, out2=in0).
        cart.write(0x5003, 0x10);
        assert_eq!(cart.read(0x5000), 6); // reorder(5) = 0b110
        cart.write(0x5003, 0x00);
        assert_eq!(cart.read(0x5000), 5);
        // $5001 multicart base (32KB units) offsets both windows; $5002 low
        // nibble masks the bank window.
        cart.write(0x5001, 0x02);
        cart.write(0x5002, 0x0C); // 128KB window -> mask 7
        cart.write(0x2000, 0x09);
        assert_eq!(cart.read(0x1000), 4); // base bank
        assert_eq!(cart.read(0x5000), 4 + 1); // (9 & 7) + base
        // $5002 high-nibble $Ex declares 8KB RAM on a header that lists none.
        assert!(cart.ram_data.is_empty());
        cart.write(0x5002, 0xE8);
        assert_eq!(cart.ram_data.len(), 0x2000);
        cart.write(0x0000, 0x0A); // MBC3-style enable
        cart.write(0xA123, 0x77);
        assert_eq!(cart.read(0xA123), 0x77);
        cart.write(0x0000, 0x00);
        assert_eq!(cart.read(0xA123), 0xFF);
    }

    #[test]
    fn force_mbc1_header_liars_load_and_bank() {
        // Sonic 3D Blast 5 shape: type $EA and garbage RAM size $20 (game
        // code overlaps the header), 256KB file with a 32KB size byte. Must
        // LOAD (no invalid-RAM error) and bank as plain MBC1 sized from the
        // file.
        let mut rom = make_sized_rom(0xEA, 0x00, 0x40000);
        rom[RAM_SIZE_OFFSET] = 0x20;
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        rom[0x134..0x13A].copy_from_slice(b"SONIC5");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::ForceMbc1);
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(matches!(cart.get_cartridge_type(), CartridgeType::MBC1 { ram: false, battery: false }));
        assert!(cart.ram_data.is_empty());
        cart.write(0x2000, 0x0B);
        assert_eq!(cart.read(0x5000), 11);

        // Captain Knick-Knack: type $00 with a Tetris header on a 128KB file.
        let mut rom = make_sized_rom(0x00, 0x00, 0x20000);
        rom[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        rom[0x134..0x13A].copy_from_slice(b"TETRIS");
        assert_eq!(Cartridge::detect_unl_mapper(&rom), UnlMapper::ForceMbc1);
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        cart.write(0x2000, 0x07);
        assert_eq!(cart.read(0x5000), 7);
    }

    fn mbc3_rtc_cart() -> Cartridge {
        Cartridge::from_bytes(&make_rom(MBC3_TIMER_RAM_BATTERY, 0x03)).unwrap()
    }

    /// Pan Docs MBC5: on rumble carts bit 3 of the $4000-$5FFF RAM-bank write
    /// drives the motor. `rumble_active()` is what the libretro frontend polls
    /// each frame; plain MBC5 carts must never report a motor.
    #[test]
    fn mbc5_rumble_latch_via_bus() {
        let mut cart = Cartridge::from_bytes(&make_rom(MBC5_RUMBLE_RAM, 0x03)).unwrap();
        assert!(cart.has_rumble());
        assert!(!cart.rumble_active());
        cart.write(0x4000, 0x08);
        assert!(cart.rumble_active());
        cart.write(0x5FFF, 0x07); // bank bits only, motor off
        assert!(!cart.rumble_active());

        let mut plain = Cartridge::from_bytes(&make_rom(MBC5_RAM, 0x03)).unwrap();
        assert!(!plain.has_rumble());
        plain.write(0x4000, 0x08);
        assert!(!plain.rumble_active());
    }

    fn huc3_cart() -> Cartridge {
        Cartridge::from_bytes(&make_rom(HUC3, 0x03)).unwrap()
    }

    fn set_mbc3_rtc(cart: &mut Cartridge, regs: (u8, u8, u8, u8, u8)) {
        cart.rtc.seconds = regs.0;
        cart.rtc.minutes = regs.1;
        cart.rtc.hours = regs.2;
        cart.rtc.days_low = regs.3;
        cart.rtc.days_high = regs.4;
    }

    fn mbc3_rtc(cart: &Cartridge) -> (u8, u8, u8, u8, u8) {
        (
            cart.rtc.seconds,
            cart.rtc.minutes,
            cart.rtc.hours,
            cart.rtc.days_low,
            cart.rtc.days_high,
        )
    }

    /// The closed-form catch-up must be bit-exact with iterating the
    /// per-second cascade, including the out-of-range 6/5-bit register bands
    /// (values 60-63 / 24-31 wrap to 0 without a carry) and the day-counter
    /// overflow latch.
    #[test]
    fn mbc3_catch_up_matches_iterative_cascade() {
        let states = [
            (0u8, 0u8, 0u8, 0u8, 0u8),
            (59, 59, 23, 0xFF, 0x01),
            (59, 59, 23, 0xFF, 0x41), // halted flag preserved (advance ignores it)
            (60, 0, 0, 0, 0),         // out-of-range seconds
            (63, 63, 31, 0xFE, 0x01), // everything out-of-range near wrap
            (30, 61, 25, 0x80, 0x80), // carry already latched stays latched
            (1, 2, 3, 4, 0xC1),
        ];
        let ns = [
            0u64, 1, 2, 59, 60, 61, 119, 3599, 3600, 3661, 86399, 86400, 86401, 2 * 86400 + 123,
            1_000_000,
        ];
        for &state in &states {
            for &n in &ns {
                let mut iter_cart = mbc3_rtc_cart();
                set_mbc3_rtc(&mut iter_cart, state);
                for _ in 0..n {
                    iter_cart.advance_rtc_second();
                }

                let mut closed_cart = mbc3_rtc_cart();
                set_mbc3_rtc(&mut closed_cart, state);
                closed_cart.mbc3_rtc_advance_seconds(n);

                assert_eq!(
                    mbc3_rtc(&iter_cart),
                    mbc3_rtc(&closed_cart),
                    "state {state:?} + {n}s"
                );
            }
        }
    }

    #[test]
    fn mbc3_rtc_blob_round_trips() {
        let mut cart = mbc3_rtc_cart();
        set_mbc3_rtc(&mut cart, (61, 5, 17, 0xAB, 0xC1)); // incl. out-of-range seconds
        cart.rtc_latched.seconds = 33;
        cart.rtc_latched.minutes = 44;
        cart.rtc_latched.hours = 12;
        cart.rtc_latched.days_low = 0x12;
        cart.rtc_latched.days_high = 0x81;

        let blob = cart.mbc3_rtc_serialize(0x0102_0304_0506_0708);
        assert_eq!(blob.len(), 48);
        // Spot-check the documented layout: LE u32 fields in the de-facto order.
        assert_eq!(&blob[0..4], &[61, 0, 0, 0]);
        assert_eq!(&blob[16..20], &[0xC1, 0, 0, 0]);
        assert_eq!(&blob[20..24], &[33, 0, 0, 0]);
        assert_eq!(&blob[40..48], &0x0102_0304_0506_0708u64.to_le_bytes());

        let mut restored = mbc3_rtc_cart();
        let ts = restored.mbc3_rtc_deserialize(&blob).unwrap();
        assert_eq!(ts, 0x0102_0304_0506_0708);
        assert_eq!(mbc3_rtc(&restored), (61, 5, 17, 0xAB, 0xC1));
        assert_eq!(restored.rtc_latched.seconds, 33);
        assert_eq!(restored.rtc_latched.days_high, 0x81);
    }

    /// The legacy 44-byte variant (32-bit timestamp, from older tools) must be
    /// accepted, mirroring the de-facto format's -4 / sizeof-4 read leeway.
    #[test]
    fn mbc3_rtc_blob_accepts_legacy_44_bytes() {
        let mut cart = mbc3_rtc_cart();
        set_mbc3_rtc(&mut cart, (10, 20, 15, 0x55, 0x00));
        let mut blob = cart.mbc3_rtc_serialize(0).to_vec();
        blob.truncate(44);
        blob[40..44].copy_from_slice(&0xCAFE_F00Du32.to_le_bytes());

        let mut restored = mbc3_rtc_cart();
        let ts = restored.mbc3_rtc_deserialize(&blob).unwrap();
        assert_eq!(ts, 0xCAFE_F00D);
        assert_eq!(mbc3_rtc(&restored), (10, 20, 15, 0x55, 0x00));
    }

    #[test]
    fn mbc3_catch_up_respects_halt() {
        let mut cart = mbc3_rtc_cart();
        set_mbc3_rtc(&mut cart, (5, 6, 7, 8, 0x40));
        cart.rtc_catch_up(86_400);
        assert_eq!(mbc3_rtc(&cart), (5, 6, 7, 8, 0x40));
    }

    #[test]
    fn huc3_rtc_blob_round_trips_with_nibble_packing() {
        let mut cart = huc3_cart();
        cart.huc3_set_clock(0x2A5, 0x123);
        cart.huc3_rtc.mem[0x58] = 0x7; // event-time nibble
        let blob = cart.huc3_rtc_serialize(0xDEAD_BEEF);
        assert_eq!(blob.len(), 136);
        // Nibble packing: nibble N -> byte N/2, even N in the low half. Minutes
        // 0x2A5 -> nibbles 0x10=0x5, 0x11=0xA, 0x12=0x2; days 0x123 ->
        // 0x13=0x3. Byte 8 = nib 0x10|0x11<<4, byte 9 = nib 0x12|0x13<<4.
        assert_eq!(blob[0x08], 0xA5);
        assert_eq!(blob[0x09], 0x32);
        let mut restored = huc3_cart();
        let ts = restored.huc3_rtc_deserialize(&blob).unwrap();
        assert_eq!(ts, 0xDEAD_BEEF);
        assert_eq!(restored.huc3_clock(), (0x2A5, 0x123));
        assert_eq!(restored.huc3_rtc.mem[0x58], 0x7);
    }

    /// Closed-form HuC-3 minute catch-up == iterating the per-minute tick,
    /// across midnight and 12-bit day-counter wraps.
    #[test]
    fn huc3_catch_up_matches_iterative_tick() {
        let states = [(0u16, 0u16), (1439, 0), (1438, 0xFFF), (720, 0x7FF), (1500, 5)];
        let ns = [0u64, 1, 2, 1439, 1440, 1441, 3 * 1440 + 7, 100_000];
        for &(minutes, days) in &states {
            for &n in &ns {
                let mut iter_cart = huc3_cart();
                iter_cart.huc3_set_clock(minutes, days);
                for _ in 0..n {
                    let (mut m, mut d) = iter_cart.huc3_clock();
                    m += 1;
                    if m >= 1440 {
                        m = 0;
                        d = (d + 1) & 0x0FFF;
                    }
                    iter_cart.huc3_set_clock(m, d);
                }

                let mut closed_cart = huc3_cart();
                closed_cart.huc3_set_clock(minutes, days);
                closed_cart.huc3_rtc_advance_minutes(n);

                assert_eq!(
                    iter_cart.huc3_clock(),
                    closed_cart.huc3_clock(),
                    "clock ({minutes},{days}) + {n}min"
                );
            }
        }
    }

    /// End-to-end sidecar flow on the disk-load path: a fresh load creates
    /// the `.rtc`; a reload after back-dating its timestamp catches the clock
    /// up by the elapsed wall time; a halted clock stays put.
    #[test]
    fn rtc_sidecar_round_trip_with_wall_clock_catch_up() {
        let dir = std::env::temp_dir().join(format!(
            "rustyboi-rtc-test-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir_all(&dir).unwrap();
        let rom_path = dir.join("game.gb");
        fs::write(&rom_path, make_rom(MBC3_TIMER_RAM_BATTERY, 0x03)).unwrap();
        let rom_path_str = rom_path.to_str().unwrap();
        let rtc_path = dir.join("game.rtc");

        {
            let cart = Cartridge::load(rom_path_str).unwrap();
            assert_eq!(mbc3_rtc(&cart), (0, 0, 0, 0, 0));
        }
        assert_eq!(fs::read(&rtc_path).unwrap().len(), 48);

        // Back-date: registers (5,0,0), saved one hour ago.
        let mut cart = mbc3_rtc_cart();
        set_mbc3_rtc(&mut cart, (5, 0, 0, 0, 0));
        let before = Cartridge::unix_now();
        fs::write(&rtc_path, cart.mbc3_rtc_serialize(before - 3600)).unwrap();

        let cart = Cartridge::load(rom_path_str).unwrap();
        let (s, m, h, dl, dh) = mbc3_rtc(&cart);
        let total = s as u64 + m as u64 * 60 + h as u64 * 3600;
        let elapsed_max = 3600 + (Cartridge::unix_now() - before) + 1;
        assert!(
            (3605..=5 + elapsed_max).contains(&total),
            "expected ~1h subsequent catch-up, got {}s ({s} {m} {h})",
            total
        );
        assert_eq!((dl, dh), (0, 0));

        // Halted clock: no catch-up applied.
        let mut cart = mbc3_rtc_cart();
        set_mbc3_rtc(&mut cart, (7, 8, 9, 1, 0x40));
        fs::write(&rtc_path, cart.mbc3_rtc_serialize(Cartridge::unix_now() - 86_400)).unwrap();
        let cart = Cartridge::load(rom_path_str).unwrap();
        assert_eq!(mbc3_rtc(&cart), (7, 8, 9, 1, 0x40));

        fs::remove_dir_all(&dir).unwrap();
    }

    /// A `.sav` with a de-facto RTC footer (RAM + 48 bytes) restores both
    /// the RAM prefix and the clock when no `.rtc` sidecar exists yet.
    #[test]
    fn sav_rtc_footer_import() {
        let dir = std::env::temp_dir().join(format!(
            "rustyboi-footer-test-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir_all(&dir).unwrap();
        let rom_path = dir.join("game.gb");
        fs::write(&rom_path, make_rom(MBC3_TIMER_RAM_BATTERY, 0x03)).unwrap();

        let mut donor = mbc3_rtc_cart();
        set_mbc3_rtc(&mut donor, (11, 22, 13, 0x44, 0x01));
        let mut sav = vec![0x5A; 32 * 1024];
        sav.extend_from_slice(&donor.mbc3_rtc_serialize(Cartridge::unix_now()));
        fs::write(dir.join("game.sav"), &sav).unwrap();

        let mut cart = Cartridge::load(rom_path.to_str().unwrap()).unwrap();
        // RAM prefix loaded (footer not spilled into RAM).
        assert_eq!(cart.ram_data[0], 0x5A);
        assert_eq!(cart.ram_data.len(), 32 * 1024);
        // Clock restored (catch-up window: allow a couple of live seconds).
        let (s, m, h, dl, dh) = mbc3_rtc(&cart);
        assert!((11..=13).contains(&s), "seconds {s}");
        assert_eq!((m, h, dl, dh), (22, 13, 0x44, 0x01));
        // Sidecar was created and now wins over the footer.
        assert!(dir.join("game.rtc").exists());

        // The live RAM write path still streams to the .sav without
        // clobbering the (read-only) footer.
        cart.write(0x0000, 0x0A);
        cart.write(0x4000, 0x00);
        cart.write(0xA000, 0x77);
        let sav_after = fs::read(dir.join("game.sav")).unwrap();
        assert_eq!(sav_after.len(), sav.len());
        assert_eq!(sav_after[0], 0x77);
        assert_eq!(&sav_after[32 * 1024..], &sav[32 * 1024..]);

        fs::remove_dir_all(&dir).unwrap();
    }

    /// The libretro RETRO_MEMORY_RTC region: stable pointer, de-facto-format
    /// content, and external writes are adopted with catch-up on the next
    /// frame sync.
    #[test]
    fn libretro_rtc_memory_sync_adopts_external_writes() {
        let mut cart = mbc3_rtc_cart();
        let ptr_before = cart.rtc_memory_mut().as_ptr();
        assert_eq!(cart.rtc_memory_mut().len(), 48);

        // Simulate RetroArch memcpying a `.rtc` file into the region:
        // registers (9,0,0), saved two minutes ago.
        let mut donor = mbc3_rtc_cart();
        set_mbc3_rtc(&mut donor, (9, 0, 0, 0, 0));
        let before = Cartridge::unix_now();
        let blob = donor.mbc3_rtc_serialize(before - 120);
        // The frontend writes through its cached raw pointer; poking the
        // buffer directly models that (bypassing the refresh in
        // rtc_memory_mut).
        cart.rtc_memory.copy_from_slice(&blob);

        cart.rtc_memory_frame_sync();
        let (s, m, h, _, _) = mbc3_rtc(&cart);
        let total = s as u64 + m as u64 * 60 + h as u64 * 3600;
        let elapsed_max = 120 + (Cartridge::unix_now() - before) + 1;
        assert!(
            (129..=9 + elapsed_max).contains(&total),
            "expected ~2min catch-up, got {total}s"
        );
        assert_eq!(cart.rtc_memory_mut().as_ptr(), ptr_before);

        // Idle frames (no external write) leave the clock alone.
        let regs = mbc3_rtc(&cart);
        cart.rtc_memory_frame_sync();
        assert_eq!(mbc3_rtc(&cart), regs);
    }

    /// HuC-3 carts expose the 136-byte blob through the libretro view.
    #[test]
    fn libretro_rtc_memory_huc3_shape() {
        let mut cart = huc3_cart();
        cart.huc3_set_clock(100, 2);
        let mem = cart.rtc_memory_mut();
        assert_eq!(mem.len(), 136);
        // Non-RTC carts expose nothing.
        let mut plain = Cartridge::from_bytes(&make_rom(MBC1, 0x02)).unwrap();
        assert!(plain.rtc_memory_mut().is_empty());
    }

    /// HuC-1 image shaped like Pokemon Card GB: 1MB ROM (64 banks) with a
    /// marker byte per bank, 32KB RAM (4 banks).
    fn huc1_cart() -> Cartridge {
        let mut rom = vec![0u8; 64 * 0x4000];
        rom[CARTRIDGE_TYPE_OFFSET] = HUC1_RAM_BATTERY;
        rom[ROM_SIZE_OFFSET] = 0x05;
        rom[RAM_SIZE_OFFSET] = 0x03;
        for bank in 0..64 {
            rom[bank * 0x4000 + 0x100] = bank as u8;
        }
        Cartridge::from_bytes(&rom).unwrap()
    }

    #[test]
    fn huc1_rom_banking_is_6_bit_with_bank_0_selectable() {
        let mut cart = huc1_cart();
        assert_eq!(cart.read(0x4100), 1); // power-on default bank 1
        cart.write(0x2000, 0x05);
        assert_eq!(cart.read(0x4100), 5);
        // Bank 0 has no zero->one remap on HuC-1.
        cart.write(0x2000, 0x00);
        assert_eq!(cart.read(0x4100), 0);
        // Only 6 bits are wired: 0x7F decodes as bank 0x3F.
        cart.write(0x2000, 0x7F);
        assert_eq!(cart.read(0x4100), 0x3F);
        // Fixed bank 0 at 0000-3FFF regardless.
        assert_eq!(cart.read(0x0100), 0);
    }

    #[test]
    fn huc1_ram_is_always_enabled_and_banked() {
        let mut cart = huc1_cart();
        // No 0x0A enable write anywhere: RAM must respond immediately.
        cart.write(0xA000, 0x42);
        assert_eq!(cart.read(0xA000), 0x42);
        // Bank switch via 4000-5FFF.
        cart.write(0x4000, 0x01);
        assert_eq!(cart.read(0xA000), 0xFF); // untouched cell in bank 1
        cart.write(0xA000, 0x77);
        assert_eq!(cart.read(0xA000), 0x77);
        cart.write(0x4000, 0x00);
        assert_eq!(cart.read(0xA000), 0x42); // bank 0 cell intact
        assert!(cart.has_battery());
    }

    #[test]
    fn huc1_ir_mode_switches_a000_region() {
        let mut cart = huc1_cart();
        cart.write(0xA000, 0x42);
        // Low nibble 0xE selects IR mode; reads see "no light" and writes
        // drive the LED instead of RAM.
        cart.write(0x0000, 0x0E);
        assert_eq!(cart.read(0xA000), 0xC0);
        cart.write(0xA000, 0x01);
        assert!(matches!(&cart.mapper, Mapper::HuC1(m) if m.state.ir_led));
        cart.write(0xA000, 0x00);
        assert!(matches!(&cart.mapper, Mapper::HuC1(m) if !m.state.ir_led));
        // Anything else selects RAM mode again; RAM was not disturbed.
        cart.write(0x0000, 0x00);
        assert_eq!(cart.read(0xA000), 0x42);
        // 0x0A (a plain MBC RAM-enable value) is RAM mode too, not IR.
        cart.write(0x0000, 0x0A);
        assert_eq!(cart.read(0xA000), 0x42);
    }

    #[test]
    fn nombc_ram_is_wired_straight_through() {
        // $08 ROM+RAM, 8KB: reads/writes hit RAM directly, no enable gate.
        let mut cart = Cartridge::from_bytes(&make_rom(ROM_RAM, 0x02)).unwrap();
        assert!(!cart.has_battery());
        cart.write(0xA000, 0x77);
        assert_eq!(cart.read(0xA000), 0x77);
        cart.write(0xBFFF, 0x12);
        assert_eq!(cart.read(0xBFFF), 0x12);

        // $09 adds the battery.
        let cart = Cartridge::from_bytes(&make_rom(ROM_RAM_BATTERY, 0x02)).unwrap();
        assert!(cart.has_battery());

        // $00 ROM ONLY with no header RAM keeps floating reads.
        let mut cart = Cartridge::from_bytes(&make_rom(0x00, 0x00)).unwrap();
        cart.write(0xA000, 0x77);
        assert_eq!(cart.read(0xA000), 0xFF);
    }

    #[test]
    fn nombc_2kb_ram_mirrors_across_the_window() {
        // $08 ROM+RAM with RAM-size $01 = a 2KB chip: it decodes only A0-A10,
        // so the 2KB repeats 4x across $A000-$BFFF (Pan Docs "No MBC").
        let mut cart = Cartridge::from_bytes(&make_rom(ROM_RAM, 0x01)).unwrap();
        cart.write(0xA000, 0x11);
        cart.write(0xA123, 0x22);
        // Every 2KB-offset alias of $A000 / $A123 reads the same cell.
        for base in [0xA000u16, 0xA800, 0xB000, 0xB800] {
            assert_eq!(cart.read(base), 0x11, "mirror of A000 at {base:04X}");
            assert_eq!(cart.read(base + 0x123), 0x22, "mirror of A123 at {base:04X}");
        }
        // Writing through a high alias lands in the same physical cell.
        cart.write(0xB800, 0x33);
        assert_eq!(cart.read(0xA000), 0x33);

        // Contrast: an 8KB chip ($02) does NOT mirror -- A800 is its own cell.
        let mut cart = Cartridge::from_bytes(&make_rom(ROM_RAM, 0x02)).unwrap();
        cart.write(0xA000, 0x11);
        assert_eq!(cart.read(0xA800), 0xFF);
    }

    /// The completeness-audit repro, end to end through the CPU/bus: a type
    /// $08 micro-ROM stores $77 to $A000, reads it back and parks it in HRAM
    /// ($FF80). Previously the NoMBC arm returned $FF.
    #[test]
    fn nombc_ram_micro_rom_via_cpu() {
        let mut rom = make_rom(ROM_RAM, 0x02);
        // 0x100: nop; jp 0x0150
        rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
        rom[0x150..0x15E].copy_from_slice(&[
            0x3E, 0x77, // ld a, $77
            0xEA, 0x00, 0xA0, // ld ($A000), a
            0x3E, 0x00, // ld a, $00
            0xFA, 0x00, 0xA0, // ld a, ($A000)
            0xE0, 0x80, // ldh ($80), a
            0x18, 0xFE, // jr -2 (spin)
        ]);
        let cart = Cartridge::from_bytes(&rom).unwrap();
        let mut gb = crate::gb::GB::new(crate::gb::Hardware::DMG);
        gb.insert(cart);
        gb.skip_bios();
        // Two frames: skip_bios hands off near the end of a frame, so the
        // first frame_ready() fires after only a handful of instructions.
        gb.run_until_frame(false);
        gb.run_until_frame(false);
        assert_eq!(gb.read_memory(0xFF80), 0x77);
    }

    /// POCKET CAMERA image shaped like the real cart: 1MB ROM (64 banks)
    /// with a marker byte per bank, 128KB RAM (16 banks).
    fn camera_cart() -> Cartridge {
        let mut rom = vec![0u8; 64 * 0x4000];
        rom[CARTRIDGE_TYPE_OFFSET] = POCKET_CAMERA;
        rom[ROM_SIZE_OFFSET] = 0x05;
        rom[RAM_SIZE_OFFSET] = 0x04;
        for bank in 0..64 {
            rom[bank * 0x4000 + 0x100] = bank as u8;
        }
        Cartridge::from_bytes(&rom).unwrap()
    }

    /// Program a usable capture configuration: 2D enhancement (the ROM's
    /// shooting mode), mid exposure, and a flat $80 threshold matrix.
    fn camera_configure(cart: &mut Cartridge) {
        cart.write(0x4000, 0x10); // select CAM registers
        cart.write(0xA001, 0xE8); // N=1 VH=3 gain
        cart.write(0xA002, 0x08); // exposure MSB
        cart.write(0xA003, 0x00); // exposure LSB
        cart.write(0xA004, 0x24); // E3=0 alpha=1.00 I=0 V=4
        cart.write(0xA005, 0x3F); // zero point / Vref (analog only)
        for i in 0..48u16 {
            cart.write(0xA006 + i, 0x80);
        }
    }

    #[test]
    fn pocket_camera_rom_banking_is_6_bit_with_bank_0_selectable() {
        let mut cart = camera_cart();
        assert!(cart.has_camera() && cart.has_battery() && !cart.has_rtc());
        assert_eq!(cart.read(0x4100), 1); // power-on default bank 1
        cart.write(0x2000, 0x3F);
        assert_eq!(cart.read(0x4100), 0x3F);
        // Bank 0 is selectable (no zero->one remap), and only 6 bits wired.
        cart.write(0x2000, 0x40);
        assert_eq!(cart.read(0x4100), 0);
        assert_eq!(cart.read(0x0100), 0); // fixed bank 0 at 0000-3FFF
    }

    #[test]
    fn pocket_camera_ram_banking_and_register_select() {
        let mut cart = camera_cart();
        // RAM WRITES need the $0A gate; reads are always enabled.
        cart.write(0xA000, 0x42);
        assert_eq!(cart.read(0xA000), 0xFF); // write dropped (gate closed)
        cart.write(0x0000, 0x0A);
        cart.write(0xA000, 0x42);
        assert_eq!(cart.read(0xA000), 0x42);
        // 16 RAM banks.
        cart.write(0x4000, 0x0F);
        cart.write(0xA000, 0x77);
        cart.write(0x4000, 0x00);
        assert_eq!(cart.read(0xA000), 0x42);
        cart.write(0x0000, 0x00); // close the gate again
        assert_eq!(cart.read(0xA000), 0x42); // reads still enabled
        // Bit 4 maps the register file; all registers but A000 read $00,
        // and the file mirrors every $80.
        cart.write(0x4000, 0x10);
        assert_eq!(cart.read(0xA000), 0x00); // idle: busy clear
        assert_eq!(cart.read(0xA001), 0x00);
        cart.write(0xA004, 0x55); // write-only, sticks despite closed gate
        assert_eq!(cart.read(0xA004), 0x00);
        cart.write(0xA000 + 0x80, 0x06); // mirror of A000: bits 1-2 stored
        assert_eq!(cart.read(0xA000), 0x06);
        // Back to RAM: bank latch survived the register window.
        cart.write(0x4000, 0x00);
        assert_eq!(cart.read(0xA000), 0x42);
    }

    #[test]
    fn pocket_camera_capture_timing_busy_gate_and_commit() {
        let mut cart = camera_cart();
        cart.write(0x0000, 0x0A);
        camera_configure(&mut cart);
        cart.write(0xA000, 0x03); // trigger, positive 1-D set
        assert_eq!(cart.read(0xA000), 0x03); // busy | stored bits 1-2
        // RAM is unreadable (returns $00) and write-locked while capturing.
        cart.write(0x4000, 0x00);
        assert_eq!(cart.read(0xA000), 0x00);
        assert_eq!(cart.read(0xA100), 0x00);
        cart.write(0xA000, 0x99); // ignored
        // Pan Docs: M-cycles = 32446 + (N?0:512) + 16*exposure; N=1 here.
        let total = 4 * (32446 + 16 * 0x0800u64);
        cart.cam_tick(total - 1);
        cart.write(0x4000, 0x10);
        assert_eq!(cart.read(0xA000), 0x03); // still busy on the last cycle
        cart.cam_tick(1);
        assert_eq!(cart.read(0xA000), 0x02); // busy cleared, bits 1-2 kept
        // RAM readable again; the A000 write during capture was dropped and
        // the processed tile data landed at bank 0 offset $0100.
        cart.write(0x4000, 0x00);
        assert_eq!(cart.read(0xA000), 0xFF); // untouched cell (not 0x99)
        let tiles: Vec<u8> = (0..CAM_TILE_BYTES)
            .map(|i| cart.read(0xA100 + i as u16))
            .collect();
        assert!(tiles.iter().any(|&b| b != tiles[0]), "flat capture output");
    }

    #[test]
    fn pocket_camera_capture_stop_and_resume() {
        let mut cart = camera_cart();
        cart.write(0x0000, 0x0A);
        camera_configure(&mut cart);
        cart.write(0x4000, 0x00);
        cart.write(0xA100, 0xAB); // pre-capture RAM content
        cart.write(0x4000, 0x10);
        cart.write(0xA000, 0x03);
        cart.cam_tick(1000);
        // Stop mid-capture: busy clears, RAM shows the OLD contents again.
        cart.write(0xA000, 0x02);
        assert_eq!(cart.read(0xA000), 0x02);
        cart.write(0x4000, 0x00);
        assert_eq!(cart.read(0xA100), 0xAB);
        cart.cam_tick(1 << 40); // stopped: countdown frozen
        cart.write(0x4000, 0x10);
        assert_eq!(cart.read(0xA000), 0x02);
        // Resume: finishes with the ORIGINAL parameters/image.
        cart.write(0xA000, 0x03);
        assert_eq!(cart.read(0xA000), 0x03);
        cart.cam_tick(4 * (32446 + 16 * 0x0800u64)); // > remaining
        assert_eq!(cart.read(0xA000), 0x02);
        cart.write(0x4000, 0x00);
        assert_ne!(cart.read(0xA100), 0xAB); // committed over the old byte
    }

    #[test]
    fn pocket_camera_sensor_image_feeds_capture() {
        let run_capture = |image: Option<[u8; CAM_W * CAM_H]>| -> Vec<u8> {
            let mut cart = camera_cart();
            cart.write(0x0000, 0x0A);
            if let Some(img) = image {
                cart.set_camera_image(&img);
            }
            camera_configure(&mut cart);
            cart.write(0xA000, 0x01);
            cart.cam_tick(u64::MAX / 2);
            cart.write(0x4000, 0x00);
            (0..CAM_TILE_BYTES)
                .map(|i| cart.read(0xA100 + i as u16))
                .collect()
        };
        let builtin = run_capture(None);
        let dark = run_capture(Some([0u8; CAM_W * CAM_H]));
        let bright = run_capture(Some([255u8; CAM_W * CAM_H]));
        // A flat black input dithers to solid black tiles (both bitplanes
        // set), flat white to solid white; the built-in pattern differs from
        // both.
        assert!(dark.iter().all(|&b| b == 0xFF));
        assert!(bright.iter().all(|&b| b == 0x00));
        assert_ne!(builtin, dark);
        assert_ne!(builtin, bright);
    }

    #[test]
    fn pocket_camera_photo_persists_to_sav() {
        let dir = std::env::temp_dir().join(format!(
            "rustyboi-cam-test-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir_all(&dir).unwrap();
        let sav = dir.join("camera.sav");

        let mut cart = camera_cart();
        cart.attach_save_file(&sav).unwrap();
        cart.write(0x0000, 0x0A);
        camera_configure(&mut cart);
        cart.write(0xA000, 0x01);
        cart.cam_tick(u64::MAX / 2);
        cart.write(0x4000, 0x00);
        let expected: Vec<u8> = (0..CAM_TILE_BYTES)
            .map(|i| cart.read(0xA100 + i as u16))
            .collect();
        drop(cart);

        let bytes = fs::read(&sav).unwrap();
        assert_eq!(bytes.len(), 16 * 0x2000); // full 128KB album RAM
        assert_eq!(&bytes[0x100..0x100 + CAM_TILE_BYTES], &expected[..]);
        fs::remove_dir_all(&dir).ok();
    }

    /// Unique-ish suffix for temp dirs (tests may run in parallel).
    fn unique_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }

    /// Trimmed MBC1M dump shape (Mortal Kombat I & II): menu bank + two
    /// contiguous 256KB games, 33 banks total; header MBC1 with a 64-bank
    /// size byte; checksum-valid headers carrying the base logo at file
    /// offsets 0, 0x4000 and 0x44000.
    fn make_trimmed_mbc1m() -> Vec<u8> {
        let mut rom = make_sized_rom(0x01, 0x05, 33 * 0x4000);
        for base in [0usize, 0x4000, 0x44000] {
            rom[base + 0x104..base + 0x134].copy_from_slice(&LICENSED_LOGO);
            rom[base + 0x147] = 0x01;
            rom[base + 0x148] = if base == 0 { 0x05 } else { 0x03 };
            let sum = rom[base + 0x134..base + 0x14D]
                .iter()
                .fold(0u8, |a, &b| a.wrapping_sub(b).wrapping_sub(1));
            rom[base + 0x14D] = sum;
        }
        rom
    }

    #[test]
    fn trimmed_mbc1m_dump_reconstructs_physical_layout() {
        let rom = make_trimmed_mbc1m();
        let out = Cartridge::reconstruct_trimmed_mbc1m(&rom).unwrap();
        assert_eq!(out.len(), 0x100000);
        // Menu keeps slot 0; the rest of its slot is 0xFF padding.
        assert_eq!(out[0x1000], 0);
        assert!(out[0x4000..0x40000].iter().all(|&b| b == 0xFF));
        // Game 1 re-bases 0x4000 -> 0x40000 (file banks 1..17).
        assert_eq!(out[0x40000 + 0x1000], 1);
        assert_eq!(out[0x7C000 + 0x1000], 16);
        // Game 2 re-bases 0x44000 -> 0x80000 (file banks 17..33).
        assert_eq!(out[0x80000 + 0x1000], 17);
        assert_eq!(out[0xBC000 + 0x1000], 32);
        // Empty slot 3.
        assert!(out[0xC0000..].iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn trimmed_mbc1m_loads_as_multicart_and_banks_physically() {
        let rom = make_trimmed_mbc1m();
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        assert!(cart.mbc1_multicart);
        assert_eq!(cart.rom_banks, 64);
        // The menu's launch sequence: BANK2 = 1 + mode 1 re-homes 0x0000 to
        // game 1's first bank (physical 0x10); BANK1 selects within the game.
        cart.write(0x2000, 0x01);
        cart.write(0x4000, 0x01);
        cart.write(0x6000, 0x01);
        assert_eq!(cart.read(0x1000), 1); // bank0 window = game 1 home bank
        assert_eq!(cart.read(0x5000), 2); // banked window = game 1 bank 1
    }

    /// `Cartridge::reset` = power cycle: after the MBC1M menu's launch
    /// sequence re-homed the bank-0 window to a game, reset must return every
    /// MBC1 latch to its power-on value so the window reads the menu again
    /// (Mortal Kombat I & II: frontend Reset previously restarted into the
    /// last-selected game).
    #[test]
    fn reset_rehomes_mbc1m_to_menu() {
        let rom = make_trimmed_mbc1m();
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        cart.write(0x0000, 0x0A); // RAMG on
        cart.write(0x2000, 0x01); // BANK1
        cart.write(0x4000, 0x01); // BANK2 -> game 1
        cart.write(0x6000, 0x01); // MODE 1 re-homes the 0x0000 window
        assert_eq!(cart.read(0x1000), 1); // game 1 home bank, not the menu

        cart.reset();
        assert_eq!(cart.read(0x1000), 0); // menu bank back in the 0x0000 window
        let Mapper::Mbc1(m) = &cart.mapper else { panic!("expected MBC1") };
        assert!(!m.ram_enabled);
        assert_eq!(m.rom_bank_low, 1);
        assert_eq!(m.bank2, 0);
        assert_eq!(m.mode, 0);
    }

    /// MBC3 reset: the latch registers and bank selects clear, but the RTC
    /// time itself (and cart RAM) is battery-fed and survives.
    #[test]
    fn reset_clears_mbc3_latches_but_keeps_rtc_time() {
        let mut cart = mbc3_rtc_cart();
        set_mbc3_rtc(&mut cart, (12, 34, 5, 0x67, 0x01));
        cart.write(0x0000, 0x0A); // RAMG on
        cart.write(0x2000, 0x15); // ROM bank
        cart.write(0x4000, 0x08); // map RTC seconds
        cart.write(0x6000, 0x00); // latch edge
        cart.write(0x6000, 0x01);
        assert_eq!(cart.rtc_latched.seconds, 12);
        cart.ram_data[0] = 0x5A; // battery RAM must survive

        cart.reset();
        let Mapper::Mbc3(m) = &cart.mapper else { panic!("expected MBC3") };
        assert!(!m.ram_enabled);
        assert_eq!(m.rom_bank_low, 1);
        assert_eq!(m.ram_bank, 0);
        assert_eq!(
            (
                cart.rtc_latched.seconds,
                cart.rtc_latched.minutes,
                cart.rtc_latched.hours,
                cart.rtc_latched.days_low,
                cart.rtc_latched.days_high,
            ),
            (0, 0, 0, 0, 0)
        );
        assert_eq!(mbc3_rtc(&cart), (12, 34, 5, 0x67, 0x01)); // clock kept ticking
        assert_eq!(cart.ram_data[0], 0x5A);
    }

    /// MBC5 reset: bank registers re-home (ROMB0=1, ROMB1=0, RAMB=0) and the
    /// rumble motor line drops.
    #[test]
    fn reset_rehomes_mbc5_banks_and_stops_rumble() {
        let mut cart = Cartridge::from_bytes(&make_rom(MBC5_RUMBLE_RAM, 0x03)).unwrap();
        cart.write(0x0000, 0x0A);
        cart.write(0x2000, 0x42);
        cart.write(0x3000, 0x01);
        cart.write(0x4000, 0x0A); // RAM bank 2 + motor on
        assert!(cart.rumble_active());

        cart.reset();
        let Mapper::Mbc5(m) = &cart.mapper else { panic!("expected MBC5") };
        assert!(!m.ram_enabled);
        assert_eq!(m.regs.rom_bank_low, 1);
        assert_eq!(m.regs.rom_bank_high, 0);
        assert_eq!(m.regs.ram_bank, 0);
        assert!(!cart.rumble_active());
    }

    /// Completeness proof for reset()'s carry list: hammer a cart's mapper
    /// registers and persist domain, reset, and require the serialized bytes
    /// to equal a same-ROM POWER-ON cart with only the persist domain grafted
    /// in. Any serialized field that reset() fails to return to its power-on
    /// value (or wrongly volatilizes) breaks the byte equality, for every
    /// mapper family.
    #[test]
    fn reset_is_power_on_plus_persist_domain() {
        let roms: Vec<Vec<u8>> = vec![
            make_rom(MBC1_RAM_BATTERY, 0x03),
            make_rom(MBC2_BATTERY, 0x00),
            make_rom(MBC3_TIMER_RAM_BATTERY, 0x03),
            make_rom(MBC5_RUMBLE_RAM, 0x03),
            make_rom(MBC7_SENSOR_RUMBLE_RAM_BATTERY, 0x00),
            make_rom(HUC1_RAM_BATTERY, 0x03),
            make_rom(HUC3, 0x03),
            make_rom(POCKET_CAMERA, 0x03),
            make_trimmed_mbc1m(),
            make_vf001_rom(),
            make_vf001g_rom(),
            make_vf001z_rom(&VF001Z_LOGO),
        ];
        for rom in roms {
            let mut cart = Cartridge::from_bytes(&rom).unwrap();
            let ct = cart.cartridge_type;
            // Hammer every mapper-register window (enable gates, bank
            // registers, modes, latches).
            cart.write(0x0000, 0x0A);
            for addr in (0x0000..0x8000u16).step_by(0x100) {
                cart.write(addr | 0x55, 0x03);
            }
            // Dirty the persist domain: battery RAM, RTC time + accumulators.
            if !cart.ram_data.is_empty() {
                cart.ram_data[0] = 0xA5;
            }
            cart.mbc2_ram[3] = 0x0F;
            cart.rtc.seconds = 12;
            cart.rtc.minutes = 34;
            cart.rtc.hours = 5;
            cart.rtc.days_low = 0x67;
            cart.rtc.days_high = 0x01;
            cart.rtc_cycle_accum = 999;
            if !cart.huc3_rtc.mem.is_empty() {
                cart.huc3_rtc.mem[0x10] = 0xA;
            }
            cart.huc3_rtc.accum = 777;

            cart.reset();

            let mut fresh = Cartridge::from_bytes(&rom).unwrap();
            fresh.ram_data = cart.ram_data.clone();
            fresh.mbc2_ram = cart.mbc2_ram.clone();
            fresh.rtc.seconds = cart.rtc.seconds;
            fresh.rtc.minutes = cart.rtc.minutes;
            fresh.rtc.hours = cart.rtc.hours;
            fresh.rtc.days_low = cart.rtc.days_low;
            fresh.rtc.days_high = cart.rtc.days_high;
            fresh.rtc_cycle_accum = cart.rtc_cycle_accum;
            fresh.huc3_rtc.mem = cart.huc3_rtc.mem.clone();
            fresh.huc3_rtc.accum = cart.huc3_rtc.accum;
            assert_eq!(
                bincode::serialize(&cart).unwrap(),
                bincode::serialize(&fresh).unwrap(),
                "cartridge type {ct:#04x}: reset != power-on + persist domain"
            );
        }
    }

    #[test]
    fn attach_rom_expands_trimmed_mbc1m() {
        let rom = make_trimmed_mbc1m();
        let mut cart = Cartridge::from_bytes(&rom).unwrap();
        let expanded = cart.detach_rom();
        // Re-attaching the ORIGINAL file bytes (savestate reload path) must
        // produce the same physical image as the constructor did.
        cart.attach_rom(rom);
        assert_eq!(&cart.rom_data[..], &expanded[..]);
    }

    #[test]
    fn trimmed_mbc1m_predicate_rejects_normal_shapes() {
        let rom = make_trimmed_mbc1m();

        // Proper 1MB image: nothing to reconstruct (existing detection path).
        let mut full = rom.clone();
        full.resize(0x100000, 0xFF);
        assert!(Cartridge::reconstruct_trimmed_mbc1m(&full).is_none());

        // Non-MBC1 type byte.
        let mut t = rom.clone();
        t[CARTRIDGE_TYPE_OFFSET] = 0x13;
        assert!(Cartridge::reconstruct_trimmed_mbc1m(&t).is_none());

        // Header ROM-size byte other than 64 banks.
        let mut t = rom.clone();
        t[ROM_SIZE_OFFSET] = 0x04;
        assert!(Cartridge::reconstruct_trimmed_mbc1m(&t).is_none());

        // Uniform filler logo must not self-match.
        let mut t = rom.clone();
        for base in [0usize, 0x4000, 0x44000] {
            t[base + 0x104..base + 0x134].copy_from_slice(&[0u8; 48]);
        }
        assert!(Cartridge::reconstruct_trimmed_mbc1m(&t).is_none());

        // Corrupting game 2's header checksum leaves a >256KB segment: bail.
        let mut t = rom.clone();
        t[0x44000 + 0x14D] ^= 0xFF;
        assert!(Cartridge::reconstruct_trimmed_mbc1m(&t).is_none());

        // Single-header short-of-header MBC1 dump: stays plain MBC1.
        let mut single = make_sized_rom(0x01, 0x05, 33 * 0x4000);
        single[0x104..0x134].copy_from_slice(&LICENSED_LOGO);
        let sum = single[0x134..0x14D]
            .iter()
            .fold(0u8, |a, &b| a.wrapping_sub(b).wrapping_sub(1));
        single[0x14D] = sum;
        assert!(Cartridge::reconstruct_trimmed_mbc1m(&single).is_none());
        let cart = Cartridge::from_bytes(&single).unwrap();
        assert!(!cart.mbc1_multicart);
    }

    fn save_test_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rustyboi-{tag}-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// MBC2's built-in RAM is physically 512 x 4 bits, so the upper nibble has
    /// no storage cell. Every load path must mask it off, or `save_ram()`
    /// exports and the streamed sidecar carry bits the silicon cannot hold —
    /// and all three entry points must agree byte-for-byte.
    #[test]
    fn mbc2_save_loads_mask_to_four_bits_on_every_path() {
        let image: Vec<u8> = (0..MBC2_RAM_SIZE).map(|i| (i as u8) | 0xF0).collect();

        // Bytes entry point.
        let mut via_bytes = Cartridge::from_bytes(&make_rom(MBC2_BATTERY, 0x00)).unwrap();
        via_bytes.load_sram_bytes(&image).unwrap();
        assert!(
            via_bytes.save_ram().iter().all(|&b| b & 0xF0 == 0),
            "load_sram_bytes left MBC2 upper nibbles set"
        );

        // Path entry point.
        let dir = save_test_dir("mbc2-mask");
        let sav = dir.join("game.sav");
        fs::write(&sav, &image).unwrap();
        let mut via_path = Cartridge::from_bytes(&make_rom(MBC2_BATTERY, 0x00)).unwrap();
        via_path.attach_save_file(&sav).unwrap();
        assert!(
            via_path.save_ram().iter().all(|&b| b & 0xF0 == 0),
            "attach_save_file left MBC2 upper nibbles set"
        );

        // Import entry point (File -> Import Battery Save).
        let mut via_import = Cartridge::from_bytes(&make_rom(MBC2_BATTERY, 0x00)).unwrap();
        assert_eq!(via_import.import_save_ram(&image).unwrap(), MBC2_RAM_SIZE);
        assert!(
            via_import.save_ram().iter().all(|&b| b & 0xF0 == 0),
            "import_save_ram left MBC2 upper nibbles set"
        );

        assert_eq!(via_bytes.save_ram(), via_path.save_ram());
        assert_eq!(via_bytes.save_ram(), via_import.save_ram());
        // The masking is emulation-invisible (the read path re-masks the
        // undriven upper lines), but the exported image is what the user's
        // .sav ends up holding. RAMG must be open for the array to answer.
        via_path.write(0x0000, 0x0A);
        assert_eq!(via_path.read(0xA001), 0xF0 | (image[1] & 0x0F));

        fs::remove_dir_all(&dir).ok();
    }

    /// An oversized save loads its RAM-sized prefix rather than being silently
    /// discarded — including on MBC2, which used to skip the file entirely
    /// while still opening it for streaming writes.
    #[test]
    fn oversized_save_loads_prefix_instead_of_being_skipped() {
        let dir = save_test_dir("oversize");

        // MBC2: 512-byte array, hand it 512 + a trailing footer.
        let mut image: Vec<u8> = vec![0x03; MBC2_RAM_SIZE];
        image.extend_from_slice(&[0xAB; 64]);
        let sav = dir.join("mbc2.sav");
        fs::write(&sav, &image).unwrap();
        let mut cart = Cartridge::from_bytes(&make_rom(MBC2_BATTERY, 0x00)).unwrap();
        cart.attach_save_file(&sav).unwrap();
        assert!(
            cart.save_ram().iter().all(|&b| b == 0x03),
            "oversized MBC2 save was skipped instead of prefix-loaded"
        );

        // Non-MBC2 keeps its long-standing prefix behavior (the de-facto
        // RTC-footer .sav format depends on it).
        let mut image: Vec<u8> = vec![0x5A; 0x2000];
        image.extend_from_slice(&[0xCD; 48]);
        let sav = dir.join("mbc3.sav");
        fs::write(&sav, &image).unwrap();
        let mut cart = Cartridge::from_bytes(&make_rom(MBC3_RAM_BATTERY, 0x02)).unwrap();
        cart.attach_save_file(&sav).unwrap();
        assert!(cart.save_ram()[..0x2000].iter().all(|&b| b == 0x5A));

        fs::remove_dir_all(&dir).ok();
    }

    /// The other end of the same one-policy load: a short save file fills its
    /// prefix and leaves the rest of the array at its power-on fill rather than
    /// being rejected or zero-extending the cart's RAM. Both the path and the
    /// bytes entry point must agree, and MBC2 must still mask what it did take.
    #[test]
    fn undersized_save_loads_prefix_and_leaves_the_tail() {
        let dir = save_test_dir("undersize");

        // Non-MBC2: 8KB array, hand it 256 bytes.
        let sav = dir.join("mbc3.sav");
        fs::write(&sav, vec![0x5A; 0x100]).unwrap();
        let mut via_path = Cartridge::from_bytes(&make_rom(MBC3_RAM_BATTERY, 0x02)).unwrap();
        via_path.attach_save_file(&sav).unwrap();
        assert_eq!(via_path.save_ram().len(), 0x2000, "RAM was resized to the file");
        assert!(via_path.save_ram()[..0x100].iter().all(|&b| b == 0x5A));
        assert!(via_path.save_ram()[0x100..].iter().all(|&b| b == 0xFF));

        let mut via_bytes = Cartridge::from_bytes(&make_rom(MBC3_RAM_BATTERY, 0x02)).unwrap();
        assert_eq!(via_bytes.load_sram_bytes(&vec![0x5A; 0x100]).unwrap(), 0x100);
        assert_eq!(via_bytes.save_ram(), via_path.save_ram());

        // MBC2: 512x4 array, hand it 64 bytes with the (unstorable) upper
        // nibbles set. The prefix is masked; the tail keeps the power-on fill.
        let image: Vec<u8> = vec![0xA7; 64];
        let sav = dir.join("mbc2.sav");
        fs::write(&sav, &image).unwrap();
        let mut cart = Cartridge::from_bytes(&make_rom(MBC2_BATTERY, 0x00)).unwrap();
        cart.attach_save_file(&sav).unwrap();
        assert_eq!(cart.save_ram().len(), MBC2_RAM_SIZE);
        assert!(cart.save_ram()[..64].iter().all(|&b| b == 0x07));
        assert!(cart.save_ram()[64..].iter().all(|&b| b == 0xFF));

        let mut via_bytes = Cartridge::from_bytes(&make_rom(MBC2_BATTERY, 0x00)).unwrap();
        assert_eq!(via_bytes.load_sram_bytes(&image).unwrap(), 64);
        assert_eq!(via_bytes.save_ram(), cart.save_ram());
        assert_eq!(via_bytes.import_save_ram(&image).unwrap(), 64);
        assert_eq!(via_bytes.save_ram(), cart.save_ram());

        fs::remove_dir_all(&dir).ok();
    }

    /// Build an in-memory zip from (name, bytes) members.
    fn make_zip(members: &[(&str, Vec<u8>)]) -> Vec<u8> {
        use zip::write::SimpleFileOptions;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default();
        for (name, bytes) in members {
            w.start_file(*name, opts).unwrap();
            std::io::Write::write_all(&mut w, bytes).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    /// The extractor prefers a Game Boy extension over a larger sibling, and
    /// falls back to the largest member when no member carries one.
    #[test]
    fn zip_extraction_prefers_rom_extension_then_largest() {
        let rom = make_rom(MBC1, 0x00);
        let zipped = make_zip(&[
            ("readme.txt", vec![0xAA; rom.len() * 2]),
            ("game.gb", rom.clone()),
        ]);
        assert_eq!(Cartridge::extract_rom_from_zip_bytes(&zipped).unwrap(), rom);

        // No ROM extension anywhere: the largest non-directory member wins.
        let big = make_rom(MBC1, 0x00);
        let zipped = make_zip(&[("small.bin", vec![0x11; 16]), ("big.bin", big.clone())]);
        assert_eq!(Cartridge::extract_rom_from_zip_bytes(&zipped).unwrap(), big);

        // Nothing usable at all.
        assert!(Cartridge::extract_rom_from_zip_bytes(&make_zip(&[])).is_err());
    }

    /// `load` (path entry point) and `extract_rom_bytes` (bytes entry point)
    /// must agree, since both now route through one extractor.
    #[test]
    fn zip_path_and_bytes_entry_points_agree() {
        let rom = make_rom(MBC1, 0x00);
        let zipped = make_zip(&[("decoy.txt", vec![0xAA; 4]), ("game.gb", rom.clone())]);

        let dir = std::env::temp_dir().join(format!(
            "rustyboi-zip-test-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("game.zip");
        fs::write(&zip_path, &zipped).unwrap();

        let from_path = Cartridge::load(zip_path.to_str().unwrap()).unwrap();
        assert_eq!(Cartridge::extract_rom_bytes(&zipped).unwrap(), rom);
        assert_eq!(from_path.rom_data[..rom.len()], rom[..]);

        fs::remove_dir_all(&dir).ok();
    }
}
