//! Mobile Adapter GB (serial peripheral).
//!
//! The Mobile Adapter is not covered by Pan Docs; this is grounded in the
//! REONTeam **libmobile** reference implementation (github.com/REONTeam/
//! libmobile, `serial.c` / `commands.c`), the de-facto spec. The adapter is a
//! serial slave (the Game Boy drives the internal clock, exactly like the Game
//! Boy Printer): each transfer the Game Boy shifts a byte out and the adapter
//! shifts its response byte back.
//!
//! ## Packet framing (libmobile `serial.c`, 8-bit mode — exact port)
//!  1. magic `$99 $66`
//!  2. 4-byte header `[command, 0, length_hi (must be 0), length_lo = N]`
//!  3. `N` data bytes
//!  4. 16-bit big-endian checksum = sum of every header + data byte
//!  5. the adapter returns `device | $80`, then an acknowledge exchange
//!     (`command ^ $80` on success, or an error byte)
//!  6. after an idle `$4B`, the adapter clocks the *response* packet back in the
//!     same frame shape. Idle transfers return `$D2` (`MOBILE_SERIAL_IDLE_BYTE`).
//!
//! ## Scope (grounded vs backend)
//! Implemented deterministically: the full framing/checksum/ack transport, plus
//! the commands that let a game **detect and configure** the adapter — START
//! (the "NINTENDO" magic handshake that begins a session), END, REINIT, and the
//! EEPROM config read/write over a local 512-byte config (`MOBILE_CONFIG_SIZE`).
//! CHECK_STATUS reports "line disconnected".
//!
//! NOT implemented (needs a transport backend, and is not a deterministic
//! emulator feature): the actual telephone/PPP/TCP/UDP/DNS networking. libmobile
//! itself delegates all sockets to host callbacks; there is no offline spec for
//! *what a server would answer*. Those commands return a libmobile-shaped error
//! packet ("not connected") so a game fails them cleanly rather than hanging.

use serde::{Deserialize, Serialize};

const MAGIC0: u8 = 0x99;
const MAGIC1: u8 = 0x66;
const IDLE_BYTE: u8 = 0xD2;
const IDLE_CHECK_BYTE: u8 = 0x4B;
const CONFIG_SIZE: usize = 0x200;

// libmobile `enum mobile_command` (commands.h).
const CMD_NULL: u8 = 0x0F;
const CMD_START: u8 = 0x10;
const CMD_END: u8 = 0x11;
const CMD_REINIT: u8 = 0x16;
const CMD_CHECK_STATUS: u8 = 0x17;
const CMD_EEPROM_READ: u8 = 0x19;
const CMD_EEPROM_WRITE: u8 = 0x1A;
const CMD_ERROR: u8 = 0x6E;

// libmobile `enum mobile_serial_state` error bytes (serial.h).
const ERR_UNKNOWN_COMMAND: u8 = 0xF0;
const ERR_CHECKSUM: u8 = 0xF1;

// START magic strings (commands.c). GB uses "NINTENDO"; the 32-byte "EVERYONE
// HAPPY MOBILE CONNECTION" is the GBA-mode variant.
const NINTENDO: &[u8; 8] = b"NINTENDO";
const HAPPY: &[u8] = b"EVERYONE HAPPY MOBILE CONNECTION";

/// Blue adapter (`MOBILE_ADAPTER_BLUE = 8`); the device ID byte is `device|$80`.
const DEVICE_BLUE: u8 = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
enum State {
    #[default]
    Waiting,
    Header,
    Data,
    Checksum,
    Acknowledge,
    IdleCheck,
    ResponseStart,
    ResponseHeader,
    ResponseData,
    ResponseChecksum,
    ResponseAcknowledge,
}

/// A Mobile Adapter GB plugged into the link port. See the module docs.
#[derive(Clone, Serialize, Deserialize)]
pub struct MobileAdapter {
    state: State,
    /// Received-packet header `[command, 0, len_hi, len_lo]`; reused to hold the
    /// response header while clocking the reply out.
    header: [u8; 4],
    /// Data buffer: the received command's data, then overwritten with the
    /// response data. Sized to the largest packet the framing allows.
    #[serde(with = "serde_bytes")]
    buffer: Vec<u8>,
    footer: [u8; 2],
    cursor: usize,
    data_size: usize,
    checksum: u16,
    error: u8,
    /// The byte to shift back on the next transfer (loaded a transfer ahead, as
    /// the adapter's shift register is on real hardware — see the printer).
    out: u8,
    session_started: bool,
    device: u8,
    #[serde(with = "serde_bytes")]
    config: Vec<u8>,
}

impl Default for MobileAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl MobileAdapter {
    pub fn new() -> Self {
        MobileAdapter {
            state: State::Waiting,
            header: [0; 4],
            buffer: vec![0; 0x100],
            footer: [0; 2],
            cursor: 0,
            data_size: 0,
            checksum: 0,
            error: 0,
            out: IDLE_BYTE,
            session_started: false,
            device: DEVICE_BLUE,
            config: vec![0; CONFIG_SIZE],
        }
    }

    /// The byte the adapter shifts back on the current transfer (loaded from the
    /// previous transfer's processing). Mirrors the printer's `preloaded_response`.
    pub fn preloaded_response(&self) -> u8 {
        self.out
    }

    /// Feed the byte the Game Boy just shifted out; the adapter advances its
    /// state machine and preloads the response for the next transfer.
    pub fn receive_byte(&mut self, tx: u8) {
        self.out = self.transfer(tx);
    }

    /// One transfer of the libmobile 8-bit serial state machine: consume the
    /// received byte `c`, return the byte to shift back this transfer.
    fn transfer(&mut self, c: u8) -> u8 {
        match self.state {
            State::Waiting => {
                // Wait for the `$99 $66` packet-start magic.
                if c == MAGIC0 {
                    self.cursor = 1;
                } else if c == MAGIC1 && self.cursor == 1 {
                    self.data_size = 0;
                    self.checksum = 0;
                    self.error = 0;
                    self.cursor = 0;
                    self.state = State::Header;
                } else {
                    self.cursor = 0;
                }
                IDLE_BYTE
            }
            State::Header => {
                self.header[self.cursor] = c;
                self.cursor += 1;
                self.checksum = self.checksum.wrapping_add(c as u16);
                if self.cursor < 4 {
                    return IDLE_BYTE;
                }
                self.data_size = self.header[3] as usize;
                // Length is u16be but never exceeds 0xFF (header[2] must be 0).
                if self.header[2] != 0 {
                    self.cursor = 0;
                    self.state = State::Waiting;
                    return IDLE_BYTE;
                }
                // Before a session, ignore anything but START.
                if !self.session_started && self.header[0] != CMD_START {
                    self.cursor = 0;
                    self.state = State::Waiting;
                    return IDLE_BYTE;
                }
                if !command_exists(self.header[0]) {
                    self.error = ERR_UNKNOWN_COMMAND;
                }
                self.cursor = 0;
                self.state = if self.data_size > 0 { State::Data } else { State::Checksum };
                IDLE_BYTE
            }
            State::Data => {
                if self.cursor < self.buffer.len() {
                    self.buffer[self.cursor] = c;
                }
                self.cursor += 1;
                self.checksum = self.checksum.wrapping_add(c as u16);
                if self.cursor >= self.data_size {
                    self.cursor = 0;
                    self.state = State::Checksum;
                }
                IDLE_BYTE
            }
            State::Checksum => {
                self.footer[self.cursor] = c;
                self.cursor += 1;
                if self.cursor < 2 {
                    return IDLE_BYTE;
                }
                let received = ((self.footer[0] as u16) << 8) | self.footer[1] as u16;
                if self.checksum != received {
                    self.error = ERR_CHECKSUM;
                }
                self.cursor = 0;
                self.state = State::Acknowledge;
                // On the last checksum byte the adapter answers with its ID.
                self.device | 0x80
            }
            State::Acknowledge => {
                // The Game Boy sends its device ID; the adapter answers the
                // command acknowledgement (command ^ $80) or the error byte.
                self.cursor = 1;
                self.state = State::IdleCheck;
                if self.error != 0 {
                    self.error
                } else {
                    self.header[0] ^ 0x80
                }
            }
            State::IdleCheck => {
                // Skip one byte, then expect the `$4B` idle byte to proceed.
                if self.cursor > 0 {
                    self.cursor -= 1;
                    return IDLE_BYTE;
                }
                if self.header[0] == CMD_NULL || self.error != 0 {
                    self.state = State::Waiting;
                    if c == MAGIC0 {
                        self.cursor = 1;
                    }
                    return IDLE_BYTE;
                }
                if c != IDLE_CHECK_BYTE {
                    self.state = State::Waiting;
                    if c == MAGIC0 {
                        self.cursor = 1;
                    }
                    return IDLE_BYTE;
                }
                // Process the command and craft the response, then clock it out.
                self.process_command();
                self.cursor = 0;
                self.state = State::ResponseStart;
                IDLE_BYTE
            }
            State::ResponseStart => {
                if self.cursor == 0 {
                    self.cursor += 1;
                    MAGIC0
                } else {
                    self.data_size = self.header[3] as usize;
                    self.cursor = 0;
                    self.state = State::ResponseHeader;
                    MAGIC1
                }
            }
            State::ResponseHeader => {
                let b = self.header[self.cursor];
                self.cursor += 1;
                if self.cursor >= 4 {
                    self.cursor = 0;
                    self.state = if self.data_size > 0 {
                        State::ResponseData
                    } else {
                        State::ResponseChecksum
                    };
                }
                b
            }
            State::ResponseData => {
                let b = self.buffer[self.cursor.min(self.buffer.len() - 1)];
                self.cursor += 1;
                if self.cursor >= self.data_size {
                    self.cursor = 0;
                    self.state = State::ResponseChecksum;
                }
                b
            }
            State::ResponseChecksum => {
                let b = self.footer[self.cursor];
                self.cursor += 1;
                if self.cursor >= 2 {
                    self.cursor = 0;
                    self.state = State::ResponseAcknowledge;
                }
                b
            }
            State::ResponseAcknowledge => {
                match self.cursor {
                    0 => {
                        self.cursor += 1;
                        self.device | 0x80
                    }
                    1 => {
                        self.cursor += 1;
                        0
                    }
                    _ => {
                        self.cursor = 0;
                        self.state = State::Waiting;
                        IDLE_BYTE
                    }
                }
            }
        }
    }

    /// Turn the received command (in `header[0]` / `buffer[..data_size]`) into a
    /// response: set `header`/`buffer`/`data_size` to the reply and compute the
    /// response checksum into `footer`. Grounded in libmobile `commands.c`.
    fn process_command(&mut self) {
        let command = self.header[0];
        let data = self.buffer[..self.data_size].to_vec();

        let (resp_cmd, resp_data): (u8, Vec<u8>) = match command {
            CMD_START => {
                // START handshake: the data must be the "NINTENDO" magic (or the
                // 32-byte GBA variant). A match begins the session; the response
                // echoes the command + data.
                if self.session_started {
                    self.error_response(command, 1)
                } else if data == NINTENDO || data.as_slice() == HAPPY {
                    self.session_started = true;
                    (CMD_START, data)
                } else {
                    self.error_response(command, 2)
                }
            }
            CMD_END => {
                self.session_started = false;
                (CMD_END, Vec::new())
            }
            CMD_REINIT => (CMD_REINIT, Vec::new()),
            CMD_CHECK_STATUS => {
                // libmobile check_status: data[0] = connection state; 0 =
                // telephone line disconnected (our permanent offline state).
                (CMD_CHECK_STATUS, vec![0x00])
            }
            CMD_EEPROM_READ => {
                // data[0] = offset, data[1] = size; reply preserves the offset
                // byte then the requested config bytes.
                if data.len() < 2 {
                    self.error_response(command, 2)
                } else {
                    let offset = data[0] as usize;
                    let size = data[1] as usize;
                    if size > 0x80 || offset + size > CONFIG_SIZE {
                        self.error_response(command, 2)
                    } else {
                        let mut out = Vec::with_capacity(size + 1);
                        out.push(data[0]);
                        out.extend_from_slice(&self.config[offset..offset + size]);
                        (CMD_EEPROM_READ, out)
                    }
                }
            }
            CMD_EEPROM_WRITE => {
                // data[0] = offset, data[1..] = bytes to write.
                if data.is_empty() {
                    self.error_response(command, 2)
                } else {
                    let offset = data[0] as usize;
                    let size = data.len() - 1;
                    if size > 0x80 || offset + size > CONFIG_SIZE {
                        self.error_response(command, 2)
                    } else {
                        self.config[offset..offset + size].copy_from_slice(&data[1..]);
                        // libmobile replies with the written offset byte.
                        (CMD_EEPROM_WRITE, vec![data[0]])
                    }
                }
            }
            // Telephone / PPP / TCP / UDP / DNS and anything else: no offline
            // spec + no transport backend, so answer a "not connected"-style
            // error the game can handle (error code 1 = still connected/failed).
            _ => self.error_response(command, 1),
        };

        self.header[0] = resp_cmd;
        self.header[1] = 0;
        self.header[2] = 0;
        self.header[3] = resp_data.len() as u8;
        self.data_size = resp_data.len();
        for (i, &b) in resp_data.iter().enumerate() {
            if i < self.buffer.len() {
                self.buffer[i] = b;
            }
        }
        // Response checksum = sum of the response header + data bytes.
        let mut sum = 0u16;
        for &b in &self.header {
            sum = sum.wrapping_add(b as u16);
        }
        for &b in &resp_data {
            sum = sum.wrapping_add(b as u16);
        }
        self.footer = [(sum >> 8) as u8, sum as u8];
    }

    /// libmobile `error_packet`: command $6E, data `[original_command, code]`.
    fn error_response(&self, command: u8, code: u8) -> (u8, Vec<u8>) {
        let _ = self;
        (CMD_ERROR, vec![command, code])
    }

    /// Test/inspection: has a session been started (adapter detected + handshook)?
    pub fn session_started(&self) -> bool {
        self.session_started
    }
}

/// Commands libmobile recognises (`mobile_commands_exists`): $10-$1A, $1F,
/// $21-$26, $28, $3F. Unknown commands raise the error byte.
fn command_exists(command: u8) -> bool {
    matches!(command,
        0x10..=0x1A | 0x1F | 0x21..=0x26 | 0x28 | 0x3F)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a whole request packet through `transfer`, collecting the adapter's
    /// same-cycle return bytes. Frames the payload exactly like the Game Boy:
    /// $99 $66, header, data, big-endian checksum, then the device-ID and idle
    /// bytes that clock out the acknowledgement and start the response.
    fn send_packet(a: &mut MobileAdapter, command: u8, data: &[u8]) -> Vec<u8> {
        let mut sum = 0u16;
        let header = [command, 0, 0, data.len() as u8];
        let mut bytes = vec![MAGIC0, MAGIC1];
        for &b in &header {
            bytes.push(b);
            sum = sum.wrapping_add(b as u16);
        }
        for &b in data {
            bytes.push(b);
            sum = sum.wrapping_add(b as u16);
        }
        bytes.push((sum >> 8) as u8); // checksum hi
        bytes.push(sum as u8); // checksum lo
        bytes.push(DEVICE_BLUE | 0x80); // Game Boy device-id exchange
        bytes.push(0x00); // acknowledge skip byte
        bytes.push(IDLE_CHECK_BYTE); // idle -> begin response
        bytes.iter().map(|&b| a.transfer(b)).collect()
    }

    /// Clock `n` idle bytes and collect the response the adapter shifts back.
    fn read_response(a: &mut MobileAdapter, n: usize) -> Vec<u8> {
        (0..n).map(|_| a.transfer(0x00)).collect()
    }

    /// Parse a `$99 $66` response frame from a byte stream; returns
    /// (command, data).
    fn parse_response(stream: &[u8]) -> (u8, Vec<u8>) {
        let start = stream.windows(2).position(|w| w == [MAGIC0, MAGIC1]).expect("magic");
        let h = start + 2;
        let command = stream[h];
        let size = stream[h + 3] as usize;
        let data = stream[h + 4..h + 4 + size].to_vec();
        (command, data)
    }

    #[test]
    fn start_handshake_begins_a_session_and_echoes_nintendo() {
        let mut a = MobileAdapter::new();
        assert!(!a.session_started());
        let ret = send_packet(&mut a, CMD_START, NINTENDO);
        // The device ID ($88) is answered on the final checksum byte.
        assert!(ret.contains(&(DEVICE_BLUE | 0x80)), "adapter ID in ack, got {ret:02X?}");
        assert!(a.session_started(), "START magic must begin a session");

        let resp = read_response(&mut a, 20);
        let (cmd, data) = parse_response(&resp);
        assert_eq!(cmd, CMD_START, "response echoes the START command");
        assert_eq!(data, NINTENDO, "response echoes the NINTENDO magic");
    }

    #[test]
    fn bad_start_magic_is_rejected() {
        let mut a = MobileAdapter::new();
        send_packet(&mut a, CMD_START, b"BOGUSXYZ");
        assert!(!a.session_started(), "wrong magic must not start a session");
    }

    #[test]
    fn eeprom_write_then_read_round_trips_config() {
        let mut a = MobileAdapter::new();
        send_packet(&mut a, CMD_START, NINTENDO);
        read_response(&mut a, 20);

        // Write 4 bytes at offset 0x10.
        send_packet(&mut a, CMD_EEPROM_WRITE, &[0x10, 0xDE, 0xAD, 0xBE, 0xEF]);
        read_response(&mut a, 12);

        // Read them back.
        send_packet(&mut a, CMD_EEPROM_READ, &[0x10, 0x04]);
        let resp = read_response(&mut a, 16);
        let (cmd, data) = parse_response(&resp);
        assert_eq!(cmd, CMD_EEPROM_READ);
        assert_eq!(data, vec![0x10, 0xDE, 0xAD, 0xBE, 0xEF], "offset byte + config data");
    }

    #[test]
    fn network_command_returns_error_packet_offline() {
        let mut a = MobileAdapter::new();
        send_packet(&mut a, CMD_START, NINTENDO);
        read_response(&mut a, 20);

        // TCP_CONNECT ($23): no backend -> error packet [command, code].
        send_packet(&mut a, 0x23, &[0, 0, 0, 0, 0, 0]);
        let resp = read_response(&mut a, 12);
        let (cmd, data) = parse_response(&resp);
        assert_eq!(cmd, CMD_ERROR, "network command answers an error packet");
        assert_eq!(data[0], 0x23, "error names the offending command");
    }
}
