use crate::cpu;
use crate::memory::Addressable;
use crate::memory::mmio;
use crate::printer;

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

pub const SB: u16 = 0xFF01;
pub const SC: u16 = 0xFF02;

/// How long an internal-clock (master) transfer holds for a peer that hasn't
/// armed before falling back to exchanging with the peer's live SB mirror (an
/// idle powered peer shifts out its SB; a severed cable defaults to 0xFF =
/// disconnected ones). Master-cc, so deterministic under any pump. ~4 DMG
/// frames: beyond any real re-arm latency, short enough that a peer that never
/// joins degrades to disconnected behavior instead of hanging the game.
/// Pan Docs notes a transfer with no clock never completes, so software must
/// time out — Serial Data Transfer:
/// https://gbdev.io/pandocs/Serial_Data_Transfer_(Link_Cable).html
pub(crate) const LINK_STALL_TIMEOUT_CC: u64 = 4 * 70224;

/// What the link-port device offers an internal-clock transfer at start.
pub(crate) enum LinkStart {
    /// No peer (or a non-blocking peer that always has a byte ready is
    /// covered by `Ready`): 0xFF shifts in at the hardware disconnected
    /// timing.
    Disconnected,
    /// The peer's shift-register byte, latched now.
    Ready(u8),
    /// A link peer is attached but its side isn't armed yet: hold the
    /// transfer (freeze the shift clock) until it posts or times out.
    AwaitPeer,
}

/// One side of a [`LinkCable`].
/// Bytes a master's completed window may leave for an external-clock slave
/// before the slave's idle poll consumes them. A slave polls every dot while
/// idle and completes within one instruction, so under any sane pump (the
/// frontend drives both instances at least per frame) at most one byte is ever
/// in flight; the small FIFO exists so an uneven pump that lets a fast master
/// complete several windows before the slave runs never silently drops a
/// completed byte + its IRQ (each queued byte is delivered as its own external
/// completion). If the queue ever fills (a master out-running a wholly-stalled
/// slave by 4+ bytes — beyond any real link timeline) the oldest byte is
/// dropped, matching a real shift register overwritten by unserviced clocks.
const DEPOSIT_FIFO: usize = 4;

#[derive(Clone, Copy)]
struct LinkSideState {
    /// Write-through mirror of that side's SB register — what its shift
    /// register would clock out right now. 0xFF when no GB drives the side
    /// (unplugged / severed savestate), preserving disconnected ones.
    live_sb: u8,
    /// That side's SC.7 (transfer requested/in progress).
    armed: bool,
    /// That side's SC.0 (internal clock) as of the arming write.
    armed_internal: bool,
    /// Completed exchange bytes awaiting this side's external-clock poll, in
    /// arrival order (the peer master's shifted-out bytes).
    deposits: [u8; DEPOSIT_FIFO],
    deposit_len: u8,
}

impl Default for LinkSideState {
    fn default() -> Self {
        LinkSideState {
            live_sb: 0xFF,
            armed: false,
            armed_internal: false,
            deposits: [0; DEPOSIT_FIFO],
            deposit_len: 0,
        }
    }
}

impl LinkSideState {
    fn push_deposit(&mut self, byte: u8) {
        if (self.deposit_len as usize) < DEPOSIT_FIFO {
            self.deposits[self.deposit_len as usize] = byte;
            self.deposit_len += 1;
        } else {
            // Full: drop the oldest (shift down), append — a real shift
            // register overwritten by clocks the CPU never serviced.
            self.deposits.copy_within(1.., 0);
            self.deposits[DEPOSIT_FIFO - 1] = byte;
        }
    }

    fn pop_deposit(&mut self) -> Option<u8> {
        if self.deposit_len == 0 {
            return None;
        }
        let byte = self.deposits[0];
        self.deposits.copy_within(1.., 0);
        self.deposit_len -= 1;
        Some(byte)
    }
}

/// Shared state of a link cable joining two GB instances. Each instance holds
/// a [`LinkPeer`] handle onto one side; the harness/frontend pumps both
/// instances (any interleave) and the cable carries armed/posted bytes between
/// their timelines. Purely a passive exchange buffer: no wall clock, no
/// threads required — determinism is inherited from the pump schedule.
#[derive(Default)]
pub(crate) struct LinkCable {
    sides: [LinkSideState; 2],
}

impl LinkCable {
    /// Create a cable and hand back its two ends. Attach each end to a GB via
    /// [`crate::gb::GB::attach_link_peer`] (or use
    /// [`crate::gb::GB::connect_link`] which does both).
    pub fn pair() -> (LinkPeer, LinkPeer) {
        let cable = Arc::new(Mutex::new(LinkCable::default()));
        (
            LinkPeer {
                cable: cable.clone(),
                side: 0,
            },
            LinkPeer { cable, side: 1 },
        )
    }
}

/// One end of a link cable, owned by a GB's serial unit as
/// `SerialDevice::Link`. Savestates and clones sever the cable (the handle is
/// not serializable and a cloned instance must not ghost-drive its twin's
/// cable): a severed end behaves like a cable with no partner plugged in.
#[derive(Serialize, Deserialize)]
pub(crate) struct LinkPeer {
    #[serde(skip)]
    cable: Arc<Mutex<LinkCable>>,
    side: u8,
}

impl Clone for LinkPeer {
    fn clone(&self) -> Self {
        LinkPeer {
            cable: Arc::default(),
            side: self.side,
        }
    }
}

impl LinkPeer {
    fn me(&self) -> usize {
        (self.side & 1) as usize
    }

    fn peer(&self) -> usize {
        (self.side & 1) as usize ^ 1
    }

    /// Sync our live-SB mirror (SB writes, completed-shift results).
    fn mirror_sb(&self, sb: u8) {
        self.cable.lock().unwrap().sides[self.me()].live_sb = sb;
    }

    /// Seed the live-SB mirror at attach time with the instance's current SB.
    pub(crate) fn seed_live_sb(&self, sb: u8) {
        self.mirror_sb(sb);
    }

    /// Observe our SC write: maintain the armed mirror, and for an
    /// internal-clock start latch the peer's byte (or report it absent).
    fn sc_write(&self, value: u8, sb: u8) -> LinkStart {
        let mut cable = self.cable.lock().unwrap();
        let me = &mut cable.sides[self.me()];
        me.live_sb = sb;
        me.armed = value & SC_TRANSFER_START != 0;
        me.armed_internal = value & SC_INTERNAL_CLOCK != 0;
        if value & (SC_TRANSFER_START | SC_INTERNAL_CLOCK)
            == SC_TRANSFER_START | SC_INTERNAL_CLOCK
        {
            let peer = &cable.sides[self.peer()];
            if peer.armed {
                LinkStart::Ready(peer.live_sb)
            } else {
                LinkStart::AwaitPeer
            }
        } else {
            LinkStart::Disconnected
        }
    }

    /// Stalled master poll: the peer's byte once it arms.
    fn poll_peer(&self) -> Option<u8> {
        let cable = self.cable.lock().unwrap();
        let peer = &cable.sides[self.peer()];
        peer.armed.then_some(peer.live_sb)
    }

    /// Stall-timeout fallback: the peer's live shift register, armed or not.
    fn peer_live_sb(&self) -> u8 {
        self.cable.lock().unwrap().sides[self.peer()].live_sb
    }

    /// Our internal-clock window completed: our shift register now holds the
    /// received byte (and our SC.7 cleared, so drop the armed mirror). The
    /// peer receives what we shifted out ONLY when it is a genuine
    /// external-clock slave waiting on our clock (armed && !internal): that
    /// is the sole consumer of the deposit slot. A peer running its own
    /// internal-clock window already latched our byte at its SC write (the
    /// both-internal conflict), and an idle/unarmed peer must not have its
    /// shift register clobbered by a byte it never solicited — so neither
    /// gets a deposit.
    fn complete_master(&self, tx: u8, rx: u8) {
        let mut cable = self.cable.lock().unwrap();
        {
            let me = &mut cable.sides[self.me()];
            me.live_sb = rx;
            me.armed = false;
        }
        let peer = &mut cable.sides[self.peer()];
        if peer.armed && !peer.armed_internal {
            peer.push_deposit(tx);
        }
    }

    /// External-clock side: take the oldest byte a peer's completed window
    /// left us (FIFO, so a fast master's bytes arrive in order).
    fn take_deposit(&self) -> Option<u8> {
        self.cable.lock().unwrap().sides[self.me()].pop_deposit()
    }

    /// External-clock completion applied on our side (SB replaced, SC.7
    /// cleared): drop the armed mirror.
    fn disarm(&self, sb: u8) {
        let mut cable = self.cable.lock().unwrap();
        let me = &mut cable.sides[self.me()];
        me.live_sb = sb;
        me.armed = false;
    }
}

/// A device plugged into the link port. The serial unit latches the device's
/// preloaded response byte at transfer start (the peer shift register's
/// contents) and hands the completed outgoing byte back at transfer end, so a
/// device's reply to byte N can only depend on bytes < N — exactly the
/// simultaneous-exchange constraint of the real bus. `Disconnected` keeps the
/// no-peer behavior (0xFF shifts in) byte-identical. `Link` joins a second GB
/// instance through a shared [`LinkCable`]: its side may additionally hold an
/// internal-clock transfer until the peer instance arms (the two timelines
/// are only loosely coupled), and completes external-clock transfers when the
/// peer's window deposits its byte.
#[derive(Serialize, Deserialize, Clone, Default)]
pub(crate) enum SerialDevice {
    #[default]
    Disconnected,
    Printer(printer::GbPrinter),
    Link(LinkPeer),
    /// 4-Player Adapter (DMG-07): a serial hub that clocks all transfers, so
    /// this Game Boy runs in external-clock mode and the adapter deposits bytes
    /// through the same external-clock path as a link peer.
    FourPlayer(crate::dmg07::FourPlayerPort),
    /// Mobile Adapter GB: an internal-clock serial slave (like the printer) that
    /// answers the libmobile packet protocol byte by byte.
    Mobile(crate::mobile::MobileAdapter),
}

impl SerialDevice {
    #[allow(dead_code)] // no in-tree caller; `pub` was masking dead_code. Unwired-peripheral and
    // unfinished-feature code lives here — check the feature roadmap before deleting.
    pub(crate) fn is_link(&self) -> bool {
        matches!(self, SerialDevice::Link(_))
    }

    /// True for devices that drive the clock externally and complete transfers
    /// via the idle deposit poll (a link peer or the DMG-07 adapter) rather than
    /// this Game Boy's own internal-clock window.
    pub(crate) fn drives_external_clock(&self) -> bool {
        matches!(self, SerialDevice::Link(_) | SerialDevice::FourPlayer(_))
    }

    /// Observe an SC write and answer what an internal-clock transfer start
    /// exchanges with. Also maintains a link peer's armed/live mirrors, so it
    /// must be called for every SC write.
    pub(crate) fn sc_write(&mut self, value: u8, sb: u8) -> LinkStart {
        match self {
            SerialDevice::Disconnected => LinkStart::Disconnected,
            SerialDevice::Printer(p) => LinkStart::Ready(p.preloaded_response()),
            SerialDevice::Link(l) => l.sc_write(value, sb),
            // The adapter is the clock master; driving an internal-clock
            // transfer against it yields garbage. External-clock arming
            // schedules no internal window, so this is only consumed on that
            // misuse.
            SerialDevice::FourPlayer(_) => LinkStart::Ready(0xFF),
            // The Mobile Adapter is an internal-clock slave: the Game Boy clocks
            // and receives the adapter's preloaded byte, like the printer.
            SerialDevice::Mobile(m) => LinkStart::Ready(m.preloaded_response()),
        }
    }

    /// Deliver a completed byte exchange to the device. `tx` is the byte
    /// shifted out, `rx` the byte shifted in (== SB at completion), `cc` the
    /// master clock at completion (deterministic device timing; never
    /// wall-clock).
    pub(crate) fn receive_byte(&mut self, tx: u8, rx: u8, cc: u64) {
        match self {
            SerialDevice::Disconnected => {}
            SerialDevice::Printer(p) => p.receive_byte(tx, cc),
            SerialDevice::Link(l) => l.complete_master(tx, rx),
            // The adapter clocks externally; the Game Boy's reply is captured
            // via `link_mirror_sb` before each deposit, so nothing to do here.
            SerialDevice::FourPlayer(_) => {}
            // Feed the Game Boy's shifted-out byte into the packet FSM, which
            // preloads the response for the next transfer.
            SerialDevice::Mobile(m) => m.receive_byte(tx),
        }
    }

    /// Keep the peer's / adapter's view of our outgoing SB in sync.
    pub(crate) fn link_mirror_sb(&mut self, sb: u8) {
        match self {
            SerialDevice::Link(l) => l.mirror_sb(sb),
            SerialDevice::FourPlayer(p) => p.mirror_sb(sb),
            _ => {}
        }
    }

    /// Stalled internal-clock transfer: the peer's byte once its side arms.
    pub(crate) fn link_poll_peer(&mut self) -> Option<u8> {
        match self {
            SerialDevice::Link(l) => l.poll_peer(),
            _ => None,
        }
    }

    /// Stall-timeout fallback byte (peer's live shift register / 0xFF).
    pub(crate) fn link_peer_live_sb(&self) -> u8 {
        match self {
            SerialDevice::Link(l) => l.peer_live_sb(),
            _ => 0xFF,
        }
    }

    /// A byte the external clock source (link peer or DMG-07) has ready for our
    /// armed external-clock side. The adapter always has its next protocol byte
    /// ready, clocking one exchange per pull (deposit-on-arm cadence).
    pub(crate) fn link_take_deposit(&mut self) -> Option<u8> {
        match self {
            SerialDevice::Link(l) => l.take_deposit(),
            SerialDevice::FourPlayer(p) => Some(p.clock()),
            _ => None,
        }
    }

    /// External-clock completion applied: drop our armed mirror.
    pub(crate) fn link_disarm(&mut self, sb: u8) {
        if let SerialDevice::Link(l) = self {
            l.disarm(sb);
        }
    }
}

// SC (FF02) bits. Pan Docs: Serial Data Transfer —
// https://gbdev.io/pandocs/Serial_Data_Transfer_(Link_Cable).html
const SC_TRANSFER_START: u8 = 1 << 7;
const SC_FAST_CLOCK: u8 = 1 << 1; // CGB only
const SC_INTERNAL_CLOCK: u8 = 1 << 0;

#[derive(Serialize, Deserialize, Clone)]
pub struct Serial {
    sb: u8,
    sc: u8,
    // Absolute completion model: a transfer's interrupt fires at `complete_at`
    // (a master-cc value), one bit shifted out every `step_t` cc. Bits already
    // shifted are reconstructed from the remaining time so SB reads stay correct
    // mid-transfer.
    active: bool,
    complete_at: u64,
    step_t: u32,
    bits_shifted: u8,
    cgb: bool,
    // Link-peer exchange state. `rx_latch` is the peer's response byte,
    // latched at transfer start from the device's preloaded shift register
    // (0xFF when disconnected, preserving ones-shift-in). `tx_acc` collects
    // the bits actually shifted out (SB's MSB at each shift edge), delivered
    // to the device at completion.
    #[serde(default = "default_rx_latch")]
    rx_latch: u8,
    #[serde(default)]
    tx_acc: u8,
    // Internal-clock transfer holding for a link peer that hasn't armed yet:
    // the shift clock is frozen (no bits move, SB unchanged — hardware-true
    // for "no exchange happened yet") until the peer posts or the stall times
    // out. Only ever set with a `SerialDevice::Link` attached, so every other
    // configuration keeps its exact timing.
    #[serde(default)]
    link_wait: bool,
    #[serde(default)]
    link_wait_since: u64,
}

fn default_rx_latch() -> u8 {
    0xFF
}

impl Default for Serial {
    fn default() -> Self {
        Self::new()
    }
}

impl Serial {
    pub fn new() -> Self {
        Serial {
            sb: 0,
            sc: 0,
            active: false,
            complete_at: 0,
            step_t: 0,
            bits_shifted: 0,
            cgb: false,
            rx_latch: 0xFF,
            tx_acc: 0,
            link_wait: false,
            link_wait_since: 0,
        }
    }

    pub(crate) fn set_cgb(&mut self, cgb: bool) {
        self.cgb = cgb;
    }

    pub fn is_cgb(&self) -> bool {
        self.cgb
    }

    /// True while a serial transfer is in flight (its `complete_at` event is
    /// pending). Blocks the idle bulk-skip so the bit-shift and completion IRQ
    /// land at the exact cc.
    pub(crate) fn is_active(&self) -> bool {
        self.active
    }

    /// Latch an SC (FF02) write and (re)schedule the transfer event.
    /// `link` is what the attached serial device offers an internal-clock
    /// start, latched here — at transfer start — the way the peer's shift
    /// register contents would be.
    pub(crate) fn schedule_sc(&mut self, value: u8, divider: u16, phase: u64, link: LinkStart) {
        self.sc = value;
        self.schedule(divider, phase, link);
    }

    fn internal_start(&self) -> bool {
        (self.sc & SC_TRANSFER_START) != 0 && (self.sc & SC_INTERNAL_CLOCK) != 0
    }

    /// The transfer completion event cc for a window whose clock starts at
    /// `phase`: snap `phase` down to the DIV-aligned grid
    /// (`cc - (cc - div_anchor) % align`, where `div_anchor` is the cc of the
    /// last DIV-write), then add the 8-bit transfer span.
    /// The serial clock is modelled as a tap off DIV, so the first edge aligns to
    /// the DIV phase. This sub-cycle DIV alignment is not documented in Pan Docs
    /// or GBCTR.
    /// NOTE: TCAGBD §5.1 states the serial clock is NOT DIV-derived ("Serial clock
    /// is not derived from this counter, it is not affected by DIV register"; §6
    /// adds the serial unit has "an internal timer that can't be reseted by any
    /// means"). Our DIV-coupling follows the later reverse-engineered model and
    /// is NOT supported by TCAGBD. Unresolved vs hardware.
    fn event_time(&self, divider: u16, phase: u64) -> (u32, u64) {
        let fast = self.cgb && (self.sc & SC_FAST_CLOCK) != 0;
        // DIV-align residue mask: `% 8` for the fast clock, `% 0x100` for slow.
        let (step, align_mask) = if fast { (16u32, 0x07u64) } else { (512u32, 0xFFu64) };
        (step, phase - (divider as u64 & align_mask) + (step as u64) * 8)
    }

    /// Schedule (or cancel) the transfer event. `divider` is the timer's
    /// internal counter and `phase` the master cc at the SC write's resolution
    /// cc. The completion cc is DIV-aligned then advanced by the 8-bit span:
    /// `event_cc = cc - (cc - div_anchor) % align + step * 8`.
    fn schedule(&mut self, divider: u16, phase: u64, link: LinkStart) {
        if !self.internal_start() {
            self.active = false;
            self.link_wait = false;
            return;
        }
        // The SC write resolves at the exact write cc, so `event_time` gives the
        // completion cc directly with no phase offset to fold in.
        let (step, complete_at) = self.event_time(divider, phase);
        self.step_t = step;
        self.bits_shifted = 0;
        self.tx_acc = 0;
        self.complete_at = complete_at;
        match link {
            LinkStart::Disconnected => {
                self.rx_latch = 0xFF;
                self.link_wait = false;
            }
            LinkStart::Ready(b) => {
                self.rx_latch = b;
                self.link_wait = false;
            }
            LinkStart::AwaitPeer => {
                // Hold for the peer instance: the shift clock stays frozen
                // (`step` is a no-op) until `resume_link` re-anchors the
                // window at the cc the peer's byte became available.
                self.rx_latch = 0xFF;
                self.link_wait = true;
                self.link_wait_since = phase;
            }
        }
        self.active = true;
    }

    /// True while an internal-clock transfer is holding for the link peer.
    pub(crate) fn link_waiting(&self) -> bool {
        self.link_wait
    }

    /// Master cc at which the current link hold began.
    pub(crate) fn link_wait_since(&self) -> u64 {
        self.link_wait_since
    }

    /// Release a held internal-clock transfer: the peer's byte `rx` became
    /// available at master cc `phase`, so the 8-bit window's clock starts
    /// there (snapped to the DIV grid exactly like a fresh SC start).
    pub(crate) fn resume_link(&mut self, rx: u8, divider: u16, phase: u64) {
        debug_assert!(self.active && self.link_wait);
        self.rx_latch = rx;
        self.link_wait = false;
        let (step, complete_at) = self.event_time(divider, phase);
        self.step_t = step;
        self.complete_at = complete_at;
    }

    /// Raw SC latch (no unused-bit fill), for link-port state decisions.
    pub(crate) fn sc_raw(&self) -> u8 {
        self.sc
    }

    #[allow(dead_code)] // no in-tree caller; `pub` was masking dead_code. Unwired-peripheral and
    // unfinished-feature code lives here — check the feature roadmap before deleting.
    /// Debug/test: the pending transfer's completion event cc (None when idle
    /// or while holding for the link peer).
    pub(crate) fn transfer_complete_at(&self) -> Option<u64> {
        (self.active && !self.link_wait).then_some(self.complete_at)
    }

    /// An external-clock exchange completed by the link peer's window: the
    /// peer's byte replaces SB wholesale (all 8 external clock edges arrive
    /// "at once" from this instance's perspective) and the transfer-start
    /// flag clears. The caller raises the serial IRQ.
    pub(crate) fn complete_external(&mut self, byte: u8) {
        self.sb = byte;
        self.sc &= !SC_TRANSFER_START;
    }

    /// Re-align the pending transfer event to a DIV reset:
    /// `n = t + (cc - t) % align - 2 * ((cc - t) & half); event_cc = max(cc,n)`.
    /// A DIV write resets the internal divider that gates the serial shift clock,
    /// so the next (and only the next) shift edge snaps to the new divider phase.
    /// Only perturbs an in-flight, future-completing transfer. This DIV/serial
    /// coupling is not documented in Pan Docs or GBCTR.
    /// NOTE: TCAGBD §5.1 DIRECTLY CONTRADICTS this — "Serial clock is not derived
    /// from this counter, it is not affected by DIV register" (and §6: the serial
    /// internal timer "can't be reseted by any means"). Our DIV-write realignment
    /// follows the later reverse-engineered model, NOT TCAGBD. Unresolved vs hardware.
    pub(crate) fn realign_to_div(&mut self, phase: u64) {
        // A link-held transfer has no running clock to realign; its window is
        // re-anchored (DIV-snapped) wholesale when the peer's byte arrives.
        if !self.active || self.link_wait || self.complete_at <= phase {
            return;
        }
        let fast = self.cgb && (self.sc & SC_FAST_CLOCK) != 0;
        let (align, half) = if fast { (8u64, 4u64) } else { (0x100u64, 0x80u64) };
        // `complete_at` already holds the raw event time, so it is the `t` the
        // residue math operates on directly.
        let t = self.complete_at;
        let delta = phase.wrapping_sub(t); // (cc - t), wraps since t > cc
        let n = t
            .wrapping_add(delta % align)
            .wrapping_sub(2 * (delta & half));
        self.complete_at = n.max(phase);
    }

    /// Advance bookkeeping at master cc `phase` (the timer's `abs_cc`, sampled
    /// within this dot's tick). Shifts SB as bits clock out and raises the serial
    /// IRQ exactly when `complete_at` is reached.
    pub fn step(&mut self, phase: u64, mmio: &mut mmio::Mmio) {
        if !self.active || self.link_wait {
            return;
        }
        // Number of bits whose clock edge has passed by `phase`.
        let target = if phase >= self.complete_at {
            8
        } else {
            let remaining_t = self.complete_at - phase;
            let remaining_bits = remaining_t.div_ceil(self.step_t as u64) as u8;
            8u8.saturating_sub(remaining_bits)
        };
        while self.bits_shifted < target {
            // Outgoing bit is SB's MSB at each shift edge; incoming bit is the
            // peer's latched response, MSB first (0xFF when disconnected -> ones
            // shifted in). Pan Docs: SB shift table + Disconnects —
            // https://gbdev.io/pandocs/Serial_Data_Transfer_(Link_Cable).html
            self.tx_acc = (self.tx_acc << 1) | (self.sb >> 7);
            let in_bit = (self.rx_latch >> (7 - self.bits_shifted)) & 1;
            self.sb = (self.sb << 1) | in_bit;
            self.bits_shifted += 1;
        }
        if phase >= self.complete_at {
            // Completion clears SC.7 and requests the serial interrupt. Pan Docs:
            // INT $58 — https://gbdev.io/pandocs/Interrupt_Sources.html
            self.active = false;
            self.sc &= !SC_TRANSFER_START;
            mmio.request_interrupt(cpu::registers::InterruptFlag::Serial);
            // The device sees the byte at the transfer's true completion cc
            // (not the possibly-later observation phase after a bulk skip).
            // `self.sb` is the received byte (rx) after all 8 shifts.
            mmio.serial_device_receive(self.tx_acc, self.sb, self.complete_at);
        }
    }
}

impl Addressable for Serial {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            SB => self.sb,
            SC => {
                let unused = if self.cgb { 0x7C } else { 0x7E };
                self.sc | unused
            }
            _ => panic!("Serial: Invalid read address {:04X}", addr),
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            SB => self.sb = value,
            SC => self.sc = value,
            _ => panic!("Serial: Invalid write address {:04X}", addr),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::cartridge::Cartridge;
    use crate::gb::{GB, Hardware};

    /// Hand-assembled ROM: sends the 10 bytes at 0x0200 over the link port
    /// (SB write, SC=0x81, poll SC bit 7, read SB) and stores each response to
    /// 0xC000+, then spins. The table holds a printer INIT packet.
    fn link_probe_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]); // nop; jp 0150
        rom[0x150..0x16E].copy_from_slice(&[
            0x21, 0x00, 0xC0, // ld hl, C000
            0x11, 0x00, 0x02, // ld de, 0200
            0x06, 0x0A, // ld b, 10
            0x1A, // ld a, (de)
            0x13, // inc de
            0xE0, 0x01, // ldh (SB), a
            0x3E, 0x81, // ld a, 81
            0xE0, 0x02, // ldh (SC), a
            0xF0, 0x02, // ldh a, (SC)
            0xE6, 0x80, // and 80
            0x20, 0xFA, // jr nz, poll
            0xF0, 0x01, // ldh a, (SB)
            0x22, // ld (hl+), a
            0x05, // dec b
            0x20, 0xEC, // jr nz, next byte
            0x18, 0xFE, // jr $ (done)
        ]);
        // Printer INIT packet: 88 33 | cmd 01 | comp 00 | len 0000 | cksum 0100 | 00 00
        rom[0x200..0x20A].copy_from_slice(&[0x88, 0x33, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00]);
        rom
    }

    fn run_probe(attach_printer: bool) -> Vec<u8> {
        let mut gb = GB::new(Hardware::DMG);
        gb.insert(Cartridge::from_bytes(&link_probe_rom()).unwrap());
        gb.skip_bios();
        if attach_printer {
            gb.attach_printer();
        }
        // 10 bytes at 8 KHz ≈ 41k cc; a handful of frames is plenty.
        for _ in 0..20 {
            gb.run_until_frame(false);
        }
        (0..10).map(|i| gb.read_memory(0xC000 + i)).collect()
    }

    /// Disconnected cable: every transferred byte reads back 0xFF.
    #[test]
    fn disconnected_link_shifts_ones() {
        assert_eq!(run_probe(false), vec![0xFF; 10]);
    }

    /// With a printer attached the INIT packet gets 0x00 during the body and
    /// the 0x81 alive + 0x00 status pair in the trailing slots, through the
    /// real serial timing path (schedule/shift/IRQ), not a shortcut.
    #[test]
    fn printer_answers_init_packet() {
        let responses = run_probe(true);
        assert_eq!(responses[..8], [0x00; 8]);
        assert_eq!(responses[8], 0x81, "alive byte");
        assert_eq!(responses[9], 0x00, "status after INIT");
    }

    // ---- two-instance link cable ------------------------------------------

    /// Hand-assembled link ROM: sends the 8 bytes at 0x0200 over the link
    /// port with SC=`sc_val`, stores each response to 0xC000+, clears IF
    /// after every byte (so the pump can watch per-byte IF edges), then
    /// spins. The master variant (`delay=true`) burns ~250 cc between the SB
    /// load and the SC start so a link slave with the same loop shape always
    /// re-arms first — mirroring real link protocols, where the slave waits
    /// armed and the master paces the exchange.
    fn link_xfer_rom(sc_val: u8, payload: &[u8; 8], delay: bool) -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]); // nop; jp 0150
        let mut p = 0x150;
        let mut emit = |bytes: &[u8], p: &mut usize| {
            rom[*p..*p + bytes.len()].copy_from_slice(bytes);
            *p += bytes.len();
        };
        emit(&[0x21, 0x00, 0xC0], &mut p); // ld hl, C000
        emit(&[0x11, 0x00, 0x02], &mut p); // ld de, 0200
        emit(&[0x06, 0x08], &mut p); // ld b, 8
        let next = p;
        emit(&[0x1A], &mut p); // ld a, (de)
        emit(&[0x13], &mut p); // inc de
        emit(&[0xE0, 0x01], &mut p); // ldh (SB), a
        if delay {
            emit(&[0x0E, 0x10], &mut p); // ld c, 16
            emit(&[0x0D], &mut p); // dec c
            emit(&[0x20, 0xFD], &mut p); // jr nz, -3 (dec c)
        }
        emit(&[0x3E, sc_val], &mut p); // ld a, sc_val
        emit(&[0xE0, 0x02], &mut p); // ldh (SC), a
        emit(&[0xF0, 0x02], &mut p); // poll: ldh a, (SC)
        emit(&[0xE6, 0x80], &mut p); // and 80
        emit(&[0x20, 0xFA], &mut p); // jr nz, poll
        emit(&[0xF0, 0x01], &mut p); // ldh a, (SB)
        emit(&[0x22], &mut p); // ld (hl+), a
        emit(&[0xAF], &mut p); // xor a
        emit(&[0xE0, 0x0F], &mut p); // ldh (IF), a
        emit(&[0x05], &mut p); // dec b
        let disp = (next as i32 - (p as i32 + 2)) as i8 as u8;
        emit(&[0x20, disp], &mut p); // jr nz, next
        emit(&[0x18, 0xFE], &mut p); // jr $
        rom[0x200..0x208].copy_from_slice(payload);
        rom
    }

    /// A ROM that never touches the serial registers (idle link partner).
    fn spin_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
        rom[0x150..0x152].copy_from_slice(&[0x18, 0xFE]); // jr $
        rom
    }

    /// Slave variant that burns `iters` x ~28 cc before running the transfer
    /// loop, so the master's first internal-clock start finds nobody armed.
    fn delayed_slave_rom(payload: &[u8; 8], iters: u16) -> Vec<u8> {
        let mut rom = link_xfer_rom(0x80, payload, false);
        // Move the entry jump to a delay stub at 0x1A0 (past the main body,
        // clear of the 0x100-0x14F header): ld bc,N; dec bc; ld a,b; or c;
        // jr nz,-5; jp 0150.
        rom[0x102] = 0xA0; // jp 01A0
        rom[0x1A0..0x1A3].copy_from_slice(&[0x01, (iters & 0xFF) as u8, (iters >> 8) as u8]);
        rom[0x1A3..0x1A8].copy_from_slice(&[0x0B, 0x78, 0xB1, 0x20, 0xFB]);
        rom[0x1A8..0x1AB].copy_from_slice(&[0xC3, 0x50, 0x01]);
        rom
    }

    fn gb_with(rom: Vec<u8>, hardware: Hardware) -> GB {
        let mut gb = GB::new(hardware);
        gb.insert(Cartridge::from_bytes(&rom).unwrap());
        gb.skip_bios();
        gb
    }

    /// Per-instance observation trace: SC.7 rise/fall and IF.3 rise edges,
    /// sampled at instruction boundaries of that instance's own timeline.
    #[derive(Default)]
    struct LinkTrace {
        /// (observed cc, scheduled completion event) per transfer start.
        /// `None` completion = the transfer is holding for the link peer.
        sc_up: Vec<(u64, Option<u64>)>,
        /// Observed cc per transfer-start-flag clear (completion).
        sc_down: Vec<u64>,
        /// Observed cc per serial-IF 0->1 edge.
        if_up: Vec<u64>,
        prev_sc7: bool,
        prev_if3: bool,
    }

    impl LinkTrace {
        fn observe(&mut self, gb: &GB) {
            let cc = gb.master_cc();
            let sc7 = gb.read_memory(0xFF02) & 0x80 != 0;
            let if3 = gb.read_memory(0xFF0F) & 0x08 != 0;
            if sc7 && !self.prev_sc7 {
                self.sc_up.push((cc, gb.serial_transfer_complete_at()));
            }
            if !sc7 && self.prev_sc7 {
                self.sc_down.push(cc);
            }
            if if3 && !self.prev_if3 {
                self.if_up.push(cc);
            }
            self.prev_sc7 = sc7;
            self.prev_if3 = if3;
        }
    }

    fn wram(gb: &GB, n: usize) -> Vec<u8> {
        (0..n).map(|i| gb.read_memory(0xC000 + i as u16)).collect()
    }

    /// Pump two linked instances in cc-lockstep: always step the one whose
    /// clock has advanced less since connect, so the two timelines never
    /// diverge by more than one instruction — the pattern a frontend's
    /// shared frame loop produces. Deterministic (ties step A). `steps`
    /// counts total single-instance instructions.
    fn pump_lockstep(
        a: &mut GB,
        b: &mut GB,
        ta: &mut LinkTrace,
        tb: &mut LinkTrace,
        steps: usize,
    ) {
        let (a0, b0) = (a.master_cc(), b.master_cc());
        for _ in 0..steps {
            if a.master_cc().wrapping_sub(a0) <= b.master_cc().wrapping_sub(b0) {
                a.step_instruction(false);
                ta.observe(a);
            } else {
                b.step_instruction(false);
                tb.observe(b);
            }
        }
    }

    /// The headless two-instance proof: instance A (internal clock) sends
    /// 0x01..=0x08, instance B (external clock) sends 0xA0..=0xA7, pumped in
    /// instruction-alternating lockstep. Each side must receive the other's
    /// bytes in order with the serial IRQ raised per byte, the master's
    /// timing must be byte-identical to the reference disconnected schedule
    /// (a ready link peer never perturbs internal-clock timing),
    /// and the slave's completions must land within lockstep skew of the
    /// master's exact completion cc.
    #[test]
    fn link_two_instance_exchange_bytes_cc_and_irq() {
        let tx_a: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
        let tx_b: [u8; 8] = [0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7];

        // Reference: the identical master ROM against a disconnected cable. Its
        // schedule (start cc, completion event cc, completion/IF observation cc)
        // is the timing a ready link peer must not perturb.
        let mut d = gb_with(link_xfer_rom(0x81, &tx_a, true), Hardware::DMG);
        let mut td = LinkTrace::default();
        for _ in 0..40_000 {
            d.step_instruction(false);
            td.observe(&d);
        }
        assert_eq!(wram(&d, 8), vec![0xFF; 8], "disconnected shifts ones");
        assert_eq!(td.sc_down.len(), 8);

        // Linked pair, pumped in pure alternation from reset so the two
        // timelines stay within one instruction of each other. The slave
        // reaches its SC=0x80 arm (~80 cc) well before the master's delayed
        // SC=0x81 start (~340 cc), as in real link protocols where the slave
        // waits armed and the master paces the exchange.
        let mut a = gb_with(link_xfer_rom(0x81, &tx_a, true), Hardware::DMG);
        let mut b = gb_with(link_xfer_rom(0x80, &tx_b, false), Hardware::DMG);
        GB::connect_link(&mut a, &mut b);
        let mut ta = LinkTrace::default();
        let mut tb = LinkTrace::default();
        pump_lockstep(&mut a, &mut b, &mut ta, &mut tb, 80_000);

        // The exchanged bytes, in order, on both sides.
        assert_eq!(wram(&a, 8), tx_b.to_vec(), "master received slave bytes");
        assert_eq!(wram(&b, 8), tx_a.to_vec(), "slave received master bytes");

        // Master timing byte-identical to the disconnected reference: same
        // start ccs, same scheduled completion events (all Ready — never a
        // link hold), same completion/IF observation ccs.
        assert_eq!(ta.sc_up, td.sc_up, "master start cc + completion events");
        assert!(
            ta.sc_up.iter().all(|(_, c)| c.is_some()),
            "every master transfer latched the slave byte at start"
        );
        assert_eq!(ta.sc_down, td.sc_down, "master completion observation cc");
        assert_eq!(ta.if_up, td.if_up, "master serial-IF edge cc");
        assert_eq!(ta.if_up.len(), 8, "one serial IRQ per master byte");

        // Slave side: one completion + one serial IRQ per byte, each landing
        // within lockstep skew (± a few instructions) of the master window's
        // exact completion event, and never before the master's window can
        // have finished.
        assert_eq!(tb.sc_down.len(), 8, "slave completed 8 transfers");
        assert_eq!(tb.if_up.len(), 8, "one serial IRQ per slave byte");
        for k in 0..8 {
            let master_done = ta.sc_up[k].1.unwrap();
            let skew = tb.sc_down[k] as i64 - master_done as i64;
            assert!(
                (-64..=128).contains(&skew),
                "slave completion {k} at {} vs master event {master_done} (skew {skew})",
                tb.sc_down[k]
            );
            let irq_skew = tb.if_up[k] as i64 - master_done as i64;
            assert!(
                (-64..=128).contains(&irq_skew),
                "slave IF edge {k} skew {irq_skew}"
            );
        }
    }

    /// Master starts with no slave armed: the transfer holds (SC.7 stays
    /// set, no IRQ, no bits shift) until the slave posts, then completes one
    /// full window later with the slave's byte — never the 0xFF a
    /// disconnected cable would have produced at the nominal schedule.
    #[test]
    fn link_master_stalls_until_slave_arms() {
        let tx_a: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
        let tx_b: [u8; 8] = [0xB0, 0xB1, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7];
        let mut a = gb_with(link_xfer_rom(0x81, &tx_a, true), Hardware::DMG);
        // ~6000 * 28 cc ~ 168k cc of slave delay: far beyond the nominal
        // 4096 cc window, well short of the 280k stall timeout.
        let mut b = gb_with(delayed_slave_rom(&tx_b, 6000), Hardware::DMG);
        GB::connect_link(&mut a, &mut b);
        let mut ta = LinkTrace::default();
        let mut tb = LinkTrace::default();
        let mut held_checked = false;
        let (a0, b0) = (a.master_cc(), b.master_cc());
        for _ in 0..160_000 {
            if a.master_cc().wrapping_sub(a0) <= b.master_cc().wrapping_sub(b0) {
                a.step_instruction(false);
                ta.observe(&a);
            } else {
                b.step_instruction(false);
                tb.observe(&b);
            }
            // Deep inside the hold (well past the nominal window, slave not
            // armed yet): transfer still pending, clock frozen, no IRQ.
            if !held_checked
                && ta.sc_up.len() == 1
                && tb.sc_up.is_empty()
                && a.master_cc() > ta.sc_up[0].0 + 3 * 4096
            {
                assert_eq!(a.read_memory(0xFF02) & 0x80, 0x80, "SC.7 held");
                assert!(ta.if_up.is_empty(), "no IRQ while held");
                assert_eq!(ta.sc_up[0].1, None, "no completion event while held");
                assert_eq!(
                    a.serial_transfer_complete_at(),
                    None,
                    "still holding for the peer"
                );
                held_checked = true;
            }
        }
        assert!(held_checked, "the hold window was observed");
        assert_eq!(wram(&a, 8), tx_b.to_vec());
        assert_eq!(wram(&b, 8), tx_a.to_vec());
        // First master completion: one window after the slave armed (its own
        // timeline), not the master's original schedule and not the timeout.
        let slave_armed = tb.sc_up[0].0;
        let done = ta.sc_down[0];
        assert!(
            done > slave_armed && done < slave_armed + 4096 + 512 + 256,
            "resumed window anchored at the arm cc (armed {slave_armed}, done {done})"
        );
        assert!(
            done < ta.sc_up[0].0 + super::LINK_STALL_TIMEOUT_CC,
            "peer arm released the hold, not the timeout"
        );
    }

    /// Master against a connected-but-idle partner (its game never touches
    /// serial): the hold falls back after the stall timeout and exchanges
    /// with the peer's live shift register (0x00 post-boot), so the game
    /// sees a completed transfer instead of hanging forever.
    #[test]
    fn link_master_times_out_against_idle_peer() {
        let tx_a: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut a = gb_with(link_xfer_rom(0x81, &tx_a, true), Hardware::DMG);
        let mut b = gb_with(spin_rom(), Hardware::DMG);
        GB::connect_link(&mut a, &mut b);
        let mut ta = LinkTrace::default();
        let mut iters = 0u32;
        while ta.sc_down.is_empty() {
            a.step_instruction(false);
            ta.observe(&a);
            b.step_instruction(false);
            iters += 1;
            assert!(iters < 400_000, "master never completed");
        }
        assert_eq!(ta.sc_up[0].1, None, "held (peer never arms)");
        let held_for = ta.sc_down[0] - ta.sc_up[0].0;
        assert!(
            (super::LINK_STALL_TIMEOUT_CC..super::LINK_STALL_TIMEOUT_CC + 4096 + 512 + 256).contains(&held_for),
            "timeout release at ~{} cc (held {held_for})",
            super::LINK_STALL_TIMEOUT_CC
        );
        assert_eq!(ta.if_up.len(), 1, "transfer completes with an IRQ");
        for _ in 0..8 {
            a.step_instruction(false); // let the ROM store the received byte
        }
        assert_eq!(
            a.read_memory(0xC000),
            0x00,
            "idle powered peer shifts out its live SB (0x00 post-boot)"
        );
    }

    /// One cable end attached, the other never plugged into any instance
    /// (also the severed savestate/clone shape): stall timeout, then 0xFF —
    /// disconnected semantics.
    #[test]
    fn link_severed_end_reads_ones() {
        let tx_a: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut a = gb_with(link_xfer_rom(0x81, &tx_a, true), Hardware::DMG);
        let (pa, _pb) = super::LinkCable::pair();
        a.attach_link_peer(pa);
        let mut ta = LinkTrace::default();
        let mut iters = 0u32;
        while ta.sc_down.is_empty() {
            a.step_instruction(false);
            ta.observe(&a);
            iters += 1;
            assert!(iters < 400_000, "master never completed");
        }
        assert!(ta.sc_down[0] - ta.sc_up[0].0 >= super::LINK_STALL_TIMEOUT_CC);
        for _ in 0..8 {
            a.step_instruction(false); // let the ROM store the received byte
        }
        assert_eq!(a.read_memory(0xC000), 0xFF);
    }

    /// Clock conflict: both sides start internal-clock transfers. On
    /// hardware both drive the clock line and both shift registers exchange;
    /// here each side completes its own window against the other's live
    /// byte, and the pairing stays index-locked across all 8 bytes.
    #[test]
    fn link_both_internal_clock_conflict_exchanges() {
        let tx_a: [u8; 8] = [0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18];
        let tx_b: [u8; 8] = [0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98];
        let mut a = gb_with(link_xfer_rom(0x81, &tx_a, true), Hardware::DMG);
        let mut b = gb_with(link_xfer_rom(0x81, &tx_b, true), Hardware::DMG);
        GB::connect_link(&mut a, &mut b);
        let mut ta = LinkTrace::default();
        let mut tb = LinkTrace::default();
        pump_lockstep(&mut a, &mut b, &mut ta, &mut tb, 120_000);
        assert_eq!(wram(&a, 8), tx_b.to_vec());
        assert_eq!(wram(&b, 8), tx_a.to_vec());
        assert_eq!(ta.sc_down.len(), 8, "A completed all its own windows");
        assert_eq!(tb.sc_down.len(), 8, "B completed all its own windows");
        assert_eq!(ta.if_up.len(), 8);
        assert_eq!(tb.if_up.len(), 8);
    }

    /// CGB fast internal clock (SC.1): the master window is 8 x 16 cc and
    /// the DMG slave — clocked externally — follows at the master's rate.
    #[test]
    fn link_cgb_fast_clock_master_dmg_slave() {
        let tx_a: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
        let tx_b: [u8; 8] = [0xC0, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7];
        let mut a = gb_with(link_xfer_rom(0x83, &tx_a, true), Hardware::CGBE);
        let mut b = gb_with(link_xfer_rom(0x80, &tx_b, false), Hardware::DMG);
        GB::connect_link(&mut a, &mut b);
        let mut ta = LinkTrace::default();
        let mut tb = LinkTrace::default();
        pump_lockstep(&mut a, &mut b, &mut ta, &mut tb, 80_000);
        assert_eq!(wram(&a, 8), tx_b.to_vec());
        assert_eq!(wram(&b, 8), tx_a.to_vec());
        for (k, (start, done)) in ta.sc_up.iter().zip(&ta.sc_down).enumerate() {
            let window = done - start.0;
            assert!(
                window < 512,
                "fast-clock window {k} took {window} cc (slow would be ~4096)"
            );
            let event_span = start.1.unwrap() - start.0;
            assert!(
                event_span <= 8 * 16,
                "fast completion event {k} within 128 cc of start ({event_span})"
            );
        }
    }

    /// Uneven pump: a slave starved of steps while the master completes several
    /// windows must not lose any completed byte or its serial IRQ — each queued
    /// byte is delivered as its own external-clock completion when the slave next
    /// runs. Tested directly at the cable API for deterministic timing; a
    /// single-slot deposit would drop all but the last byte.
    #[test]
    fn link_deposit_fifo_preserves_order_and_count() {
        let (master, slave) = super::LinkCable::pair();
        // Slave arms for an external-clock transfer (SC=0x80): master deposits
        // are now legal into the slave's slot.
        slave.sc_write(0x80, 0x00);
        // Master runs three back-to-back internal windows before the slave
        // polls even once. Each complete_master(tx, rx) deposits tx.
        master.complete_master(0xA1, 0x00);
        master.complete_master(0xA2, 0x00);
        master.complete_master(0xA3, 0x00);
        // The slave drains in FIFO order — no byte lost, none reordered.
        assert_eq!(slave.take_deposit(), Some(0xA1));
        assert_eq!(slave.take_deposit(), Some(0xA2));
        assert_eq!(slave.take_deposit(), Some(0xA3));
        assert_eq!(slave.take_deposit(), None);

        // Overflow past DEPOSIT_FIFO drops the OLDEST (a real shift register
        // overwritten by clocks the CPU never serviced), keeping the newest.
        slave.sc_write(0x80, 0x00);
        for b in 0..(super::DEPOSIT_FIFO as u8 + 2) {
            master.complete_master(0x10 + b, 0x00);
        }
        let drained: Vec<u8> = std::iter::from_fn(|| slave.take_deposit()).collect();
        assert_eq!(drained.len(), super::DEPOSIT_FIFO, "queue capped at FIFO depth");
        assert_eq!(
            *drained.last().unwrap(),
            0x10 + super::DEPOSIT_FIFO as u8 + 1,
            "newest byte retained on overflow"
        );

        // A deposit is NOT taken while the slave is unarmed (poll-gating: an
        // idle instance's shift register is never clobbered by an unsolicited
        // byte). Here the slave never armed, so complete_master deposits
        // nothing at all.
        let (m2, s2) = super::LinkCable::pair();
        m2.complete_master(0x77, 0x00); // s2 unarmed
        assert_eq!(s2.take_deposit(), None, "no deposit to an unarmed slave");
    }
}
