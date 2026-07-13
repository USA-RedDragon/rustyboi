//! 4-Player Adapter (DMG-07) protocol.
//!
//! Grounded in Pan Docs "4-Player Adapter". The DMG-07 is a serial hub: it
//! supplies the clock for all transfers, so every connected Game Boy runs in
//! *external* clock mode (SC bit 0 = 0, bit 7 = 1 to arm). The adapter drives
//! two phases:
//!
//!   * **Ping phase** — the adapter broadcasts a 4-byte packet to each Game Boy,
//!     `[$FE, STAT1, STAT2, STAT3]`, where each STAT byte carries the connection
//!     bitmap in bits 4-7 (P1..P4) and that port's player ID (1-4) in bits 0-2.
//!     Each Game Boy replies (loaded into SB for the *next* transfer): `$88`
//!     (ACK1) to the header, `$88` (ACK2) to STAT1, RATE to STAT2, SIZE to
//!     STAT3. A port counts as connected once it ACKs the header with `$88`.
//!
//!   * **Transmission phase** — a Game Boy (normally P1) sends four `$AA` bytes
//!     aligned to the ping header; the adapter answers with four `$CC` bytes and
//!     then broadcasts data packets. A packet is `SIZE*4` bytes: each player
//!     submits SIZE bytes during the first SIZE transfers, and the adapter
//!     broadcasts every player's data from the *previous* packet in P1,P2,P3,P4
//!     order (zeros for absent players) — a one-packet delay. Sending `SIZE*4`
//!     consecutive `$FF` bytes restarts the ping phase.
//!
//! What is grounded here: the exact byte protocol (the unit tests reproduce Pan
//! Docs' own worked example sequences). What is *not* silicon-verified (Pan Docs
//! gives analog timings but no capture exists to grade against, and the adapter
//! has no public test ROM): the packet *cadence*. This model advances one
//! exchange whenever a connected Game Boy pulls a byte (deposit-on-arm, like the
//! link cable's external-clock side), so it delivers the correct byte *sequence*
//! rather than the ~17 ms silicon packet period. Games that gate purely on the
//! byte protocol work; any that also time the analog cadence are out of scope.

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

const PING_HEADER: u8 = 0xFE;
const ACK: u8 = 0x88;
const XMIT_ENTER_REQ: u8 = 0xAA; // Game Boy -> adapter: enter transmission
const XMIT_ENTER_IND: u8 = 0xCC; // adapter -> Game Boy: transmission indicator
const PING_RESTART: u8 = 0xFF; // both directions: restart ping

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Phase {
    #[default]
    Ping,
    /// Sending the four-byte `$CC` indicator that precedes data packets.
    EnterIndicator,
    Transmission,
    /// Sending the `SIZE*4`-byte `$FF` indicator that precedes the ping return.
    RestartIndicator,
}

/// Shared DMG-07 state. Up to four Game Boy ports attach to one hub; each port
/// pumps its own timeline and pulls bytes through [`Dmg07::exchange`].
#[derive(Default)]
pub struct Dmg07 {
    phase: Phase,
    /// bit4..bit7 set = player 1..4 present and ACKing (the STAT upper nibble).
    connected: u8,
    /// Latched from P1's replies; RATE is cosmetic here (timing only), SIZE sets
    /// the data-bytes-per-player (1..=4, clamped). `$00` keeps the old value.
    rate: u8,
    size: u8,
    /// Which physical ports have a Game Boy plugged in (independent of ACK).
    attached: [bool; 4],
    /// Per-port byte cursor within the current 4-byte (ping) or SIZE*4 (data)
    /// packet, and the run-length of consecutive AA / FF replies seen.
    pos: [usize; 4],
    aa_run: [u8; 4],
    ff_run: [u8; 4],
    /// Transmission double-buffer: `prev` is broadcast this packet, `cur`
    /// collects each port's submission for the next one. Up to 4 bytes/player.
    prev: [[u8; 4]; 4],
    cur: [[u8; 4]; 4],
    /// Indicator-byte cursor for the CC / FF transition packets.
    ind_pos: usize,
}

impl Dmg07 {
    /// Bytes per player in a data packet (SIZE, clamped to the documented 1..=4).
    fn size(&self) -> usize {
        (self.size as usize).clamp(1, 4)
    }

    /// Total bytes broadcast in one data packet (`SIZE*4`).
    fn packet_len(&self) -> usize {
        self.size() * 4
    }

    /// The STAT byte a given port sees: connection bitmap (bits 4-7) + this
    /// port's player ID 1-4 (bits 0-2).
    fn stat(&self, player: usize) -> u8 {
        self.connected | ((player as u8) + 1)
    }

    /// Advance the protocol for `player`'s transfer: the Game Boy shifted out
    /// `reply` (its SB) and the adapter returns the byte it shifts in. Ports
    /// pull independently; the packet content (STAT / broadcast data) is shared.
    pub fn exchange(&mut self, player: usize, reply: u8) -> u8 {
        // Track AA/FF runs from this port's replies for the phase transitions.
        self.aa_run[player] = if reply == XMIT_ENTER_REQ { self.aa_run[player] + 1 } else { 0 };
        self.ff_run[player] = if reply == PING_RESTART { self.ff_run[player] + 1 } else { 0 };

        match self.phase {
            Phase::Ping => self.exchange_ping(player, reply),
            Phase::EnterIndicator => self.exchange_indicator(player, XMIT_ENTER_IND, Phase::Transmission),
            Phase::Transmission => self.exchange_transmission(player, reply),
            Phase::RestartIndicator => self.exchange_indicator(player, PING_RESTART, Phase::Ping),
        }
    }

    fn exchange_ping(&mut self, player: usize, reply: u8) -> u8 {
        let pos = self.pos[player];
        // Consume this port's reply to the byte it was answering.
        match pos {
            0 => {
                // Reply to the previous packet's header. A `$88` ACK marks the
                // port connected; anything else drops it.
                if reply == ACK {
                    self.connected |= 0x10 << player;
                    self.attached[player] = true;
                } else {
                    self.connected &= !(0x10 << player);
                }
            }
            2 => {
                // Reply to STAT1 was ACK2; this position's reply is RATE.
                if player == 0 && reply != 0x00 {
                    self.rate = reply;
                }
            }
            3
                if player == 0 && reply != 0x00 => {
                    self.size = reply;
                }
            _ => {}
        }

        // Four `$AA` in a row (a full ping packet's worth, aligned at the
        // header) requests transmission entry.
        if self.aa_run[player] >= 4 {
            self.enter_indicator(Phase::EnterIndicator);
            return XMIT_ENTER_IND;
        }

        // The byte the adapter sends at this position.
        let out = match pos {
            0 => PING_HEADER,
            _ => self.stat(player),
        };
        self.advance_ping(player);
        out
    }

    fn advance_ping(&mut self, player: usize) {
        self.pos[player] = (self.pos[player] + 1) % 4;
    }

    /// Reset every port to the start of an indicator packet (CC or FF).
    fn enter_indicator(&mut self, phase: Phase) {
        self.phase = phase;
        self.ind_pos = 0;
        self.pos = [0; 4];
        self.aa_run = [0; 4];
        self.ff_run = [0; 4];
    }

    fn exchange_indicator(&mut self, player: usize, byte: u8, next: Phase) -> u8 {
        // The indicator packet is 4 bytes (CC) or SIZE*4 (FF). Use the P1 cursor
        // as the shared clock for the transition; other ports mirror it.
        let len = if byte == PING_RESTART { self.packet_len() } else { 4 };
        self.ind_pos += 1;
        if self.ind_pos >= len {
            // Transition complete on the next pull.
            if next == Phase::Transmission {
                self.begin_transmission();
            } else {
                self.begin_ping();
            }
        }
        let _ = player;
        byte
    }

    fn begin_transmission(&mut self) {
        self.phase = Phase::Transmission;
        self.pos = [0; 4];
        self.prev = [[0; 4]; 4];
        self.cur = [[0; 4]; 4];
    }

    fn begin_ping(&mut self) {
        self.phase = Phase::Ping;
        self.pos = [0; 4];
        self.aa_run = [0; 4];
        self.ff_run = [0; 4];
    }

    fn exchange_transmission(&mut self, player: usize, reply: u8) -> u8 {
        let size = self.size();
        let pos = self.pos[player];

        // Any port sending SIZE*4 consecutive `$FF` restarts the ping phase.
        if self.ff_run[player] as usize >= self.packet_len() {
            self.enter_indicator(Phase::RestartIndicator);
            return PING_RESTART;
        }

        // The first SIZE transfers of a packet collect this port's submission.
        if pos < size {
            self.cur[player][pos] = reply;
        }

        // Broadcast byte: the packet streams P1's SIZE bytes, then P2's, P3's,
        // P4's, all from the previous packet (zeros for absent players).
        let src_player = pos / size;
        let src_byte = pos % size;
        let out = self.prev[src_player][src_byte];

        self.pos[player] += 1;
        if self.pos[player] >= self.packet_len() {
            // Packet done for this port: promote its freshly-collected data.
            self.pos[player] = 0;
            self.prev[player] = self.cur[player];
            self.cur[player] = [0; 4];
        }
        out
    }
}

/// One Game Boy's port into a shared [`Dmg07`]. Held by that instance's serial
/// unit; clones/savestates sever it (a cloned instance must not drive the hub),
/// behaving like an unplugged adapter.
#[derive(Serialize, Deserialize)]
pub struct FourPlayerPort {
    // The live hub is a connection, not persistable state: savestates sever it
    // (default = a fresh, partnerless hub), exactly like the link cable.
    #[serde(skip)]
    hub: Arc<Mutex<Dmg07>>,
    player: usize,
    /// The Game Boy's last SB write — the reply it will shift out next.
    reply: u8,
}

impl FourPlayerPort {
    /// Mint a hub and hand back `n` ports (2-4). Attach each to a Game Boy via
    /// [`crate::gb::GB::attach_four_player_port`] (or
    /// [`crate::gb::GB::connect_four_player`], which does both).
    pub fn hub(n: usize) -> Vec<FourPlayerPort> {
        let n = n.clamp(2, 4);
        let hub = Arc::new(Mutex::new(Dmg07::default()));
        (0..n)
            .map(|player| FourPlayerPort { hub: hub.clone(), player, reply: 0xFF })
            .collect()
    }

    /// Track the Game Boy's SB write (its outgoing reply).
    pub fn mirror_sb(&mut self, sb: u8) {
        self.reply = sb;
    }

    /// The adapter clocks the port whenever it is armed for an external-clock
    /// transfer: run one exchange and hand back the adapter's byte.
    pub fn clock(&mut self) -> u8 {
        self.hub.lock().unwrap().exchange(self.player, self.reply)
    }

    pub fn player_id(&self) -> usize {
        self.player + 1
    }
}

impl Clone for FourPlayerPort {
    fn clone(&self) -> Self {
        // Sever: a cloned port gets its own partnerless hub.
        FourPlayerPort {
            hub: Arc::new(Mutex::new(Dmg07::default())),
            player: self.player,
            reply: self.reply,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Drive a single port's exchange, feeding replies and collecting the
    // adapter's returned bytes. `replies[i]` is answered to the byte the adapter
    // returned at step i-1 (the real "reply loaded into SB for the next
    // transfer" rule); reply[0] answers the very first pull.
    fn run(d: &mut Dmg07, player: usize, replies: &[u8]) -> Vec<u8> {
        replies.iter().map(|&r| d.exchange(player, r)).collect()
    }

    #[test]
    fn ping_packet_matches_pandocs_example() {
        // Pan Docs: a lone Player 1 sees `FE 01 01 01` once it has connected
        // (`FE 11 11 11`), with replies 88/88/RATE/SIZE.
        let mut d = Dmg07::default();
        // First packet: P1 has not ACKed yet, so connection nibble is 0 and the
        // header reply this round establishes the connection for next round.
        let out = run(&mut d, 0, &[ACK, ACK, 0x10, 0x01]);
        assert_eq!(out[0], PING_HEADER, "byte 0 is the ping header");
        // ID is 1 for player 0; connection bit for P1 set after the header ACK.
        assert_eq!(out[1] & 0x07, 0x01, "player ID 1 in STAT low bits");
        assert_eq!(out[1] & 0x10, 0x10, "P1 connected bit set after ACK");
        // RATE/SIZE latched from P1's replies.
        assert_eq!(d.rate, 0x10);
        assert_eq!(d.size, 0x01);
        // Next packet header still FE; STAT now `$11` (P1 connected, ID 1).
        let out2 = run(&mut d, 0, &[ACK, ACK, 0x10, 0x01]);
        assert_eq!(out2[0], PING_HEADER);
        assert_eq!(out2[1], 0x11, "STAT = P1 connected | ID 1");
    }

    #[test]
    fn four_aa_enters_transmission_then_cc_indicator() {
        let mut d = Dmg07 {
            size: 1,
            ..Default::default()
        };
        // Connect P1 first.
        run(&mut d, 0, &[ACK, ACK, 0x10, 0x01]);
        // Send AA aligned to the header: four AA replies request entry.
        let mut got = Vec::new();
        for _ in 0..4 {
            got.push(d.exchange(0, XMIT_ENTER_REQ));
        }
        // Once four AA are seen the adapter answers with CC indicators.
        assert_eq!(*got.last().unwrap(), XMIT_ENTER_IND, "adapter sends CC");
        // Drain the 4-byte CC indicator packet; then data packets begin.
        for _ in 0..4 {
            assert_eq!(d.exchange(0, 0x00), XMIT_ENTER_IND, "CC indicator byte");
        }
        assert_eq!(d.phase, Phase::Transmission);
    }

    #[test]
    fn transmission_broadcasts_previous_packet_with_one_packet_delay() {
        // SIZE = 2 (packet = 8 bytes), only P1 present. P1 submits two bytes per
        // packet in the first two transfers; the adapter echoes the PREVIOUS
        // packet's data (zeros on the first packet), P1..P4 order.
        let mut d = Dmg07 {
            phase: Phase::Transmission,
            size: 2,
            ..Default::default()
        };
        d.attached[0] = true;
        d.connected = 0x10;

        // Packet 1: P1 submits [0x12, 0x34] in the first two transfers. Broadcast
        // is all zeros (no previous packet yet).
        let p1: Vec<u8> = [0x12, 0x34, 0, 0, 0, 0, 0, 0]
            .iter()
            .map(|&r| d.exchange(0, r))
            .collect();
        assert_eq!(p1, vec![0, 0, 0, 0, 0, 0, 0, 0], "first packet broadcasts zeros");

        // Packet 2: the adapter now broadcasts P1.1 = [0x12, 0x34] in the P1
        // slot (bytes 1-2), zeros for absent P2/P3/P4.
        let p2: Vec<u8> = [0x56, 0x78, 0, 0, 0, 0, 0, 0]
            .iter()
            .map(|&r| d.exchange(0, r))
            .collect();
        assert_eq!(p2[0], 0x12, "P1 byte 1 from previous packet");
        assert_eq!(p2[1], 0x34, "P1 byte 2 from previous packet");
        assert_eq!(&p2[2..], &[0, 0, 0, 0, 0, 0], "absent players broadcast zeros");
    }

    #[test]
    fn ff_run_restarts_ping_phase() {
        let mut d = Dmg07 {
            phase: Phase::Transmission,
            size: 1, // packet_len = 4, so 4 FF restart
            ..Default::default()
        };
        d.attached[0] = true;
        d.connected = 0x10;
        let mut out = 0;
        for _ in 0..4 {
            out = d.exchange(0, PING_RESTART);
        }
        assert_eq!(out, PING_RESTART, "adapter sends FF restart indicator");
        // Drain the FF indicator packet (4 bytes) -> back to ping.
        for _ in 0..4 {
            d.exchange(0, 0x00);
        }
        assert_eq!(d.phase, Phase::Ping);
    }

    #[test]
    fn clone_severs_the_hub() {
        let ports = FourPlayerPort::hub(2);
        let mut a = ports.into_iter().next().unwrap();
        let mut b = a.clone();
        a.mirror_sb(ACK);
        b.mirror_sb(ACK);
        // Independent hubs: driving one does not advance the other's ping cursor.
        a.clock();
        assert_eq!(b.hub.lock().unwrap().pos[0], 0, "clone must not share the hub");
    }
}
