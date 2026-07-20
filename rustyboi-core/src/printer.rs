//! Game Boy Printer (MGB-001) — a serial-link peripheral.
//!
//! Protocol (Pan Docs "Gameboy Printer" + gbdev wiki, validated byte-for-byte
//! against Raphael-Boichot's GameboyPrinterSniffer real-hardware captures of
//! Zelda DX and Pokémon Crystal print sessions):
//!
//!   GB -> printer packet:  88 33 | cmd | compression | len_lo len_hi |
//!                          data[len] | cksum_lo cksum_hi | 00 | 00
//!   printer -> GB:         00 for every byte until the two trailing slots,
//!                          which carry 0x81 ("alive") and the status byte.
//!
//! The checksum is the 16-bit sum of every byte from `cmd` through the end of
//! `data` (magic and checksum excluded). Commands: 0x01 INIT, 0x02 PRINT,
//! 0x04 DATA (<= 0x280 bytes of 2bpp tile graphics per packet), 0x08 BREAK,
//! 0x0F STATUS/NUL.
//!
//! Status byte: bit7 low battery, bit6 other error, bit5 paper jam, bit4
//! packet error, bit3 unprocessed data present, bit2 image data full /
//! processed data present, bit1 printing, bit0 checksum error.
//!
//! Status lifecycle, matched to the real captures: a packet's own response
//! reflects the state BEFORE its command lands (DATA's bit3 and PRINT's busy
//! only become visible from the following packet — the firmware applies
//! effects asynchronously after answering), while INIT/BREAK clear and a bad
//! checksum reports synchronously in the offending packet. After PRINT the
//! polled status reads 0x06 (printing | data present) for the duration of the
//! motor pass, then 0x04 until INIT. The real firmware additionally migrates
//! bit3 -> bit2 while idle between packets ("unprocessed" data gets converted
//! in the background); no game observes the distinction (both bits are outside
//! every error mask), so like other emulators we keep bit3 until PRINT.

use serde::{Deserialize, Serialize};

/// Print head width in pixels; every band is 20 tiles wide.
pub(crate) const PRINT_WIDTH: usize = 160;
/// Stripe RAM: 0x1680 bytes = 9 bands of 0x280 = one full 160x144 screen.
const BUFFER_CAPACITY: usize = 0x1680;
/// Inter-byte packet timeout. Pan Docs: the printer abandons packet reception
/// after ~100 ms of silence. Measured on the 4.194304 MHz master clock so it
/// is deterministic (no wall clock).
const PACKET_TIMEOUT_CC: u64 = 419_430;
/// Motor busy time charged per printed pixel row plus a fixed spin-up. Real
/// hardware takes ~1 s per band; games poll STATUS until the busy bit drops,
/// so any duration works — this keeps "PRINTING" on screen for a believable
/// beat (~0.5 s emulated for a full 144-row sheet) without stalling play.
const PRINT_BASE_CC: u64 = 1 << 20;
const PRINT_PER_ROW_CC: u64 = 8_192;

const STATUS_CHECKSUM_ERROR: u8 = 1 << 0;
const STATUS_PRINTING: u8 = 1 << 1;
const STATUS_DATA_FULL: u8 = 1 << 2;
const STATUS_UNPROCESSED: u8 = 1 << 3;
const STATUS_PACKET_ERROR: u8 = 1 << 4;

const CMD_INIT: u8 = 0x01;
const CMD_PRINT: u8 = 0x02;
const CMD_DATA: u8 = 0x04;
const CMD_BREAK: u8 = 0x08;
const CMD_STATUS: u8 = 0x0F;

/// One completed print job: palette/exposure already applied, one shade
/// (0 = blank paper .. 3 = full ink) per pixel, `PRINT_WIDTH` pixels per row.
#[derive(Serialize, Deserialize, Clone)]
pub struct PrintSheet {
    pub width: u32,
    pub height: u32,
    pub shades: Vec<u8>,
    /// Raw PRINT parameters, for consumers that want them.
    pub sheets: u8,
    pub margins: u8,
    pub palette: u8,
    pub exposure: u8,
}

impl PrintSheet {
    /// Encode as an 8-bit grayscale PNG (blank paper = 0xFF).
    pub fn to_png(&self) -> Vec<u8> {
        let gray: Vec<u8> = self.shades.iter().map(|&s| 0xFF - (s & 3) * 0x55).collect();
        encode_gray8_png(self.width, self.height, &gray)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
enum PacketState {
    #[default]
    Magic0,
    Magic1,
    Command,
    Compression,
    LenLo,
    LenHi,
    Data,
    ChecksumLo,
    ChecksumHi,
    Alive,
    Status,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub(crate) struct GbPrinter {
    state: PacketState,
    command: u8,
    compression: bool,
    data_len: u16,
    payload: Vec<u8>,
    checksum_calc: u16,
    checksum_recv: u16,
    /// True when the in-flight packet's checksum failed (packet is rejected).
    packet_bad: bool,
    /// The byte preloaded into the printer's output shift register: what the
    /// GB receives during the NEXT byte exchange.
    next_response: u8,
    status: u8,
    /// Accumulated 2bpp tile data (already RLE-decoded) since INIT/last print.
    buffer: Vec<u8>,
    /// Master-cc deadline for the print motor; 0 = idle.
    busy_until_cc: u64,
    /// Master cc of the previous received byte, for the packet timeout.
    last_byte_cc: u64,
    /// Completed prints awaiting collection by the frontend.
    completed: Vec<PrintSheet>,
}

impl GbPrinter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The byte the printer would shift out during the next exchange. Latched
    /// by the serial unit at transfer start (the real shift register's
    /// contents), so mid-transfer SB reads reconstruct the correct bits.
    pub(crate) fn preloaded_response(&self) -> u8 {
        self.next_response
    }

    /// Completed byte exchange: the GB shifted `tx` out (and already received
    /// the previously preloaded response). `cc` is the master clock at
    /// completion; used only for deterministic timeouts/busy modeling.
    pub(crate) fn receive_byte(&mut self, tx: u8, cc: u64) {
        if self.busy_until_cc != 0 && cc >= self.busy_until_cc {
            self.busy_until_cc = 0;
            self.status &= !STATUS_PRINTING;
        }
        if self.state != PacketState::Magic0
            && cc.wrapping_sub(self.last_byte_cc) > PACKET_TIMEOUT_CC
        {
            self.state = PacketState::Magic0;
            self.next_response = 0x00;
        }
        self.last_byte_cc = cc;

        self.state = match self.state {
            PacketState::Magic0 => {
                self.next_response = 0x00;
                if tx == 0x88 { PacketState::Magic1 } else { PacketState::Magic0 }
            }
            PacketState::Magic1 => {
                if tx == 0x33 {
                    // New packet: per-packet error bits report on the packet
                    // that caused them and clear on the next one.
                    self.status &= !(STATUS_CHECKSUM_ERROR | STATUS_PACKET_ERROR);
                    self.checksum_calc = 0;
                    self.packet_bad = false;
                    PacketState::Command
                } else if tx == 0x88 {
                    PacketState::Magic1
                } else {
                    PacketState::Magic0
                }
            }
            PacketState::Command => {
                self.command = tx;
                self.checksum_calc = self.checksum_calc.wrapping_add(tx as u16);
                PacketState::Compression
            }
            PacketState::Compression => {
                self.compression = tx & 1 != 0;
                self.checksum_calc = self.checksum_calc.wrapping_add(tx as u16);
                PacketState::LenLo
            }
            PacketState::LenLo => {
                self.data_len = tx as u16;
                self.checksum_calc = self.checksum_calc.wrapping_add(tx as u16);
                PacketState::LenHi
            }
            PacketState::LenHi => {
                self.data_len |= (tx as u16) << 8;
                self.checksum_calc = self.checksum_calc.wrapping_add(tx as u16);
                self.payload.clear();
                if self.data_len == 0 { PacketState::ChecksumLo } else { PacketState::Data }
            }
            PacketState::Data => {
                self.payload.push(tx);
                self.checksum_calc = self.checksum_calc.wrapping_add(tx as u16);
                if self.payload.len() as u16 == self.data_len {
                    PacketState::ChecksumLo
                } else {
                    PacketState::Data
                }
            }
            PacketState::ChecksumLo => {
                self.checksum_recv = tx as u16;
                PacketState::ChecksumHi
            }
            PacketState::ChecksumHi => {
                self.checksum_recv |= (tx as u16) << 8;
                self.packet_bad = self.checksum_recv != self.checksum_calc;
                if self.packet_bad {
                    self.status |= STATUS_CHECKSUM_ERROR;
                } else {
                    match self.command {
                        // INIT and BREAK land synchronously: their own status
                        // response already reads cleared (real captures).
                        CMD_INIT => {
                            self.buffer.clear();
                            self.status = 0;
                            self.busy_until_cc = 0;
                        }
                        CMD_BREAK => {
                            self.buffer.clear();
                            self.status = 0;
                            self.busy_until_cc = 0;
                        }
                        CMD_PRINT | CMD_DATA | CMD_STATUS => {}
                        _ => self.status |= STATUS_PACKET_ERROR,
                    }
                }
                self.next_response = 0x81;
                PacketState::Alive
            }
            PacketState::Alive => {
                self.next_response = self.status;
                PacketState::Status
            }
            PacketState::Status => {
                self.next_response = 0x00;
                // DATA/PRINT effects land after the response is out, so the
                // packet's own status reflected the pre-application state and
                // the next packet sees the result (matches the captures:
                // PRINT answers with the pre-print status, the first poll
                // after it reads 0x06).
                if !self.packet_bad {
                    match self.command {
                        CMD_DATA => self.apply_data(),
                        CMD_PRINT => self.apply_print(cc),
                        _ => {}
                    }
                }
                PacketState::Magic0
            }
        };
    }

    fn apply_data(&mut self) {
        if self.data_len == 0 {
            // Empty DATA marks end-of-transfer before PRINT; no buffer change.
            return;
        }
        let decoded: Vec<u8> = if self.compression {
            decompress_rle(&self.payload)
        } else {
            std::mem::take(&mut self.payload)
        };
        let room = BUFFER_CAPACITY - self.buffer.len();
        if decoded.len() > room {
            self.buffer.extend_from_slice(&decoded[..room]);
            self.status |= STATUS_DATA_FULL;
        } else {
            self.buffer.extend_from_slice(&decoded);
        }
        if !self.buffer.is_empty() {
            self.status |= STATUS_UNPROCESSED;
        }
    }

    fn apply_print(&mut self, cc: u64) {
        let p = &self.payload;
        let (sheets, margins, palette, exposure) = (
            p.first().copied().unwrap_or(1),
            p.get(1).copied().unwrap_or(0),
            p.get(2).copied().unwrap_or(0xE4),
            p.get(3).copied().unwrap_or(0x40),
        );
        let height = (self.buffer.len() / 16 / 20) * 8;
        if sheets > 0 && height > 0 {
            let mut shades = Vec::with_capacity(PRINT_WIDTH * height);
            for y in 0..height {
                for x in 0..PRINT_WIDTH {
                    let tile = (y / 8) * 20 + x / 8;
                    let base = tile * 16 + (y % 8) * 2;
                    let bit = 7 - (x % 8);
                    let lo = (self.buffer[base] >> bit) & 1;
                    let hi = (self.buffer[base + 1] >> bit) & 1;
                    let color = (hi << 1) | lo;
                    shades.push((palette >> (color * 2)) & 3);
                }
            }
            self.completed.push(PrintSheet {
                width: PRINT_WIDTH as u32,
                height: height as u32,
                shades,
                sheets,
                margins,
                palette,
                exposure,
            });
        }
        // The data is consumed into the mechanism: "unprocessed" drops,
        // "image data present" holds until INIT, and the motor runs. sheets=0
        // is a bare paper feed — same motor cycle, nothing captured.
        self.buffer.clear();
        self.status = (self.status & !STATUS_UNPROCESSED) | STATUS_PRINTING | STATUS_DATA_FULL;
        self.busy_until_cc = cc + PRINT_BASE_CC + PRINT_PER_ROW_CC * height.max(16) as u64;
    }

    /// Drain completed prints (oldest first).
    pub(crate) fn take_completed(&mut self) -> Vec<PrintSheet> {
        std::mem::take(&mut self.completed)
    }

}

/// Pan Docs printer RLE: control byte high-bit set = repeat the following
/// byte (n & 0x7F) + 2 times, else copy the next n + 1 bytes literally.
fn decompress_rle(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 2);
    let mut i = 0;
    while i < data.len() {
        let control = data[i];
        i += 1;
        if control & 0x80 != 0 {
            let Some(&value) = data.get(i) else { break };
            i += 1;
            out.extend(std::iter::repeat_n(value, (control & 0x7F) as usize + 2));
        } else {
            let n = (control as usize + 1).min(data.len() - i);
            out.extend_from_slice(&data[i..i + n]);
            i += n;
        }
    }
    out
}

/// Minimal deterministic PNG encoder: 8-bit grayscale, filter 0, zlib stream
/// with stored (uncompressed) deflate blocks. No external deps, wasm-safe.
pub(crate) fn encode_gray8_png(width: u32, height: u32, gray: &[u8]) -> Vec<u8> {
    assert_eq!(gray.len(), width as usize * height as usize);
    let mut png = Vec::with_capacity(gray.len() + gray.len() / 32 + 128);
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]); // 8-bit grayscale
    write_chunk(&mut png, b"IHDR", &ihdr);

    // Raw scanlines: filter byte 0 + row.
    let mut raw = Vec::with_capacity((width as usize + 1) * height as usize);
    for row in gray.chunks(width as usize) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut idat = vec![0x78, 0x01]; // zlib: deflate, 32K window, no dict
    for (i, block) in raw.chunks(0xFFFF).enumerate() {
        let last = (i + 1) * 0xFFFF >= raw.len();
        idat.push(last as u8);
        idat.extend_from_slice(&(block.len() as u16).to_le_bytes());
        idat.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        idat.extend_from_slice(block);
    }
    idat.extend_from_slice(&adler32(&raw).to_be_bytes());
    write_chunk(&mut png, b"IDAT", &idat);
    write_chunk(&mut png, b"IEND", &[]);
    png
}

fn write_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let mut crc = 0xFFFF_FFFFu32;
    for &b in kind.iter().chain(data) {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    png.extend_from_slice(&(!crc).to_be_bytes());
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a full packet through the printer byte-by-byte the way the serial
    /// unit does, returning the (alive, status) response pair the GB reads.
    /// `cc` advances ~1 byte-time (4096 cc at 8 KHz) per exchange.
    fn send_packet(
        printer: &mut GbPrinter,
        cc: &mut u64,
        cmd: u8,
        compression: u8,
        data: &[u8],
    ) -> (u8, u8) {
        let mut bytes = vec![0x88, 0x33, cmd, compression];
        bytes.extend_from_slice(&(data.len() as u16).to_le_bytes());
        bytes.extend_from_slice(data);
        let cksum: u16 = bytes[2..]
            .iter()
            .fold(0u16, |acc, &b| acc.wrapping_add(b as u16));
        bytes.extend_from_slice(&cksum.to_le_bytes());
        bytes.extend_from_slice(&[0x00, 0x00]); // alive + status slots
        let mut responses = Vec::new();
        for b in bytes {
            responses.push(printer.preloaded_response());
            printer.receive_byte(b, *cc);
            *cc += 4096;
        }
        let n = responses.len();
        (responses[n - 2], responses[n - 1])
    }

    fn band(fill: u8) -> Vec<u8> {
        vec![fill; 0x280]
    }

    /// The packet/status sequence of the real Zelda DX capture
    /// (GameboyPrinterSniffer RealCapture): INIT, INQY, 9 x DATA(0x280),
    /// empty DATA, PRINT(01 13 E4 80), then INQY polls until done. Response
    /// statuses must reproduce the capture exactly.
    #[test]
    fn zelda_dx_capture_sequence() {
        let mut p = GbPrinter::new();
        let mut cc = 0u64;
        assert_eq!(send_packet(&mut p, &mut cc, CMD_INIT, 0, &[]), (0x81, 0x00));
        assert_eq!(send_packet(&mut p, &mut cc, CMD_STATUS, 0, &[]), (0x81, 0x00));
        for i in 0..9 {
            let expect = if i == 0 { 0x00 } else { 0x08 };
            assert_eq!(
                send_packet(&mut p, &mut cc, CMD_DATA, 0, &band(i as u8)),
                (0x81, expect),
                "DATA band {i}"
            );
        }
        assert_eq!(send_packet(&mut p, &mut cc, CMD_DATA, 0, &[]), (0x81, 0x08));
        assert_eq!(
            send_packet(&mut p, &mut cc, CMD_PRINT, 0, &[0x01, 0x13, 0xE4, 0x80]),
            (0x81, 0x08),
            "PRINT answers with the pre-print status"
        );
        // Busy polls: 0x06 while the motor runs, 0x04 after.
        let (_, first_poll) = send_packet(&mut p, &mut cc, CMD_STATUS, 0, &[]);
        assert_eq!(first_poll, 0x06);
        let mut saw_done = false;
        for _ in 0..600 {
            let (alive, status) = send_packet(&mut p, &mut cc, CMD_STATUS, 0, &[]);
            assert_eq!(alive, 0x81);
            if status == 0x04 {
                saw_done = true;
                break;
            }
            assert_eq!(status, 0x06);
        }
        assert!(saw_done, "busy bit never cleared");
        // One 160x144 sheet with palette E4 applied: band fill i => both
        // planes = fill, and the capture's 9 bands were 0x00..0x08 fills.
        let sheets = p.take_completed();
        assert_eq!(sheets.len(), 1);
        let s = &sheets[0];
        assert_eq!((s.width, s.height), (160, 144));
        assert_eq!(s.palette, 0xE4);
        assert_eq!(s.margins, 0x13);
        assert_eq!(s.exposure, 0x80);
        // Row 0 comes from band 0 (fill 0x00 -> color 0 -> shade 0), row 16
        // from band 1 (fill 0x01 -> pixel column 7 has color 3 -> shade 3).
        assert_eq!(s.shades[0], 0);
        assert_eq!(s.shades[16 * 160 + 7], 3);
        assert_eq!(s.shades[16 * 160 + 6], 0);
        // INIT clears the post-print 0x04.
        assert_eq!(send_packet(&mut p, &mut cc, CMD_INIT, 0, &[]), (0x81, 0x00));
        assert_eq!(send_packet(&mut p, &mut cc, CMD_STATUS, 0, &[]), (0x81, 0x00));
    }

    #[test]
    fn checksum_error_rejects_packet() {
        let mut p = GbPrinter::new();
        let mut cc = 0u64;
        // Hand-build a DATA packet with a corrupted checksum.
        let mut bytes = vec![0x88, 0x33, CMD_DATA, 0x00, 0x02, 0x00, 0xAA, 0xBB];
        bytes.extend_from_slice(&[0x12, 0x34]); // wrong checksum
        bytes.extend_from_slice(&[0x00, 0x00]);
        let mut responses = Vec::new();
        for b in bytes {
            responses.push(p.preloaded_response());
            p.receive_byte(b, cc);
            cc += 4096;
        }
        let n = responses.len();
        assert_eq!(responses[n - 2], 0x81);
        assert_eq!(responses[n - 1] & STATUS_CHECKSUM_ERROR, STATUS_CHECKSUM_ERROR);
        // Rejected: no data committed, and the next packet reports clean.
        assert_eq!(send_packet(&mut p, &mut cc, CMD_STATUS, 0, &[]), (0x81, 0x00));
    }

    #[test]
    fn rle_compression_roundtrip() {
        let mut p = GbPrinter::new();
        let mut cc = 0u64;
        // 0x280 bytes of 0xFF as RLE runs: 5 x (0x7F+2=129) + 1 x (dangling 5).
        let mut compressed = Vec::new();
        for _ in 0..4 {
            compressed.extend_from_slice(&[0xFF, 0xFF]); // repeat 0xFF x129
        }
        // 4*129 = 516; need 640 - 516 = 124 more => control 0x80 | 122 = repeat 124.
        compressed.extend_from_slice(&[0x80 | 122, 0xFF]);
        send_packet(&mut p, &mut cc, CMD_DATA, 1, &compressed);
        send_packet(&mut p, &mut cc, CMD_PRINT, 0, &[1, 0, 0xE4, 0x40]);
        let sheets = p.take_completed();
        assert_eq!(sheets.len(), 1);
        assert_eq!(sheets[0].height, 16);
        assert!(sheets[0].shades.iter().all(|&s| s == 3));
    }

    #[test]
    fn timeout_resets_packet_reception() {
        let mut p = GbPrinter::new();
        // Half a packet...
        for (i, b) in [0x88u8, 0x33, CMD_DATA, 0x00, 0x80, 0x02].iter().enumerate() {
            p.receive_byte(*b, i as u64 * 4096);
        }
        // ...then silence past the timeout; a fresh INIT must parse cleanly.
        let mut cc = 10_000_000u64;
        assert_eq!(send_packet(&mut p, &mut cc, CMD_INIT, 0, &[]), (0x81, 0x00));
    }

    #[test]
    fn png_encoder_structure() {
        let png = encode_gray8_png(4, 2, &[0, 85, 170, 255, 255, 170, 85, 0]);
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 4);
        assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 2);
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
        // zlib stored block: raw stream = 2 rows x (1 filter + 4 px) = 10 bytes.
        let idat = &png[41..]; // 8 sig + 25 IHDR chunk + 8 IDAT len/type
        assert_eq!(&png[37..41], b"IDAT");
        assert_eq!(idat[0], 0x78);
        assert_eq!(idat[2], 0x01); // final stored block
        assert_eq!(u16::from_le_bytes([idat[3], idat[4]]), 10);
    }
}
