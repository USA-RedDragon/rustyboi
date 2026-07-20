//! CGB infrared port transport (RP register, $FF56).
//!
//! Grounded in Pan Docs "GBC Infrared Communication": the GBC has one IR port
//! with a separate emitter and receiver. RP bit 0 turns the emitter (LED) on;
//! RP bit 1 reads the receiver (1 = no signal, 0 = signal seen) but only while
//! the read-enable bits 6-7 are both set ($C0). One GBC's emitter illuminates
//! the *peer's* receiver — a device does not read its own LED.
//!
//! This models the digital emitter/receiver coupling that the two-player IR
//! protocols rely on (Pokémon G/S/C Mystery Gift, TCG "Card Pop", Pokémon
//! Pinball score exchange, Bomberman trades — all pure on/off pulse coupling).
//! Deliberately NOT modelled, because they have no digital specification and no
//! two-GBC protocol depends on them:
//!   * the analog signal "fade" (a sustained level reads as 1 again after
//!     ~3ms, a distance-dependent analog behaviour), and
//!   * the ambient/bright-light $00->$C0 disable/re-enable sensing quirk
//!     (used only by light-sensing accessories, never GBC<->GBC exchange).
//!
//! Like [`crate::serial::LinkCable`] the channel is a passive level exchange:
//! no clock and no threads, so determinism comes entirely from the harness
//! pump schedule. Clones and savestates sever the channel (a cloned instance
//! must not ghost-drive its twin), behaving like an unplugged port.

use std::sync::{Arc, Mutex};

/// Shared IR channel joining two GBC IR ports. Each side publishes its emitter
/// level; each side reads the *other* side's emitter as its received signal.
#[derive(Default)]
struct IrChannel {
    led: [bool; 2],
}

/// One end of a shared IR channel.
pub(crate) struct IrLink {
    channel: Arc<Mutex<IrChannel>>,
    side: usize,
}

impl IrLink {
    #[allow(dead_code)] // KEEP (owner decision 2026-07-20): implemented peripheral awaiting frontend
    // wiring, not rot. No in-tree caller, so `dead_code` fires; do not delete.
    /// Mint a channel and hand back its two ends. Attach each to a GBC via
    /// [`crate::gb::GB::attach_ir_peer`] (or [`crate::gb::GB::connect_ir`],
    /// which does both).
    pub fn pair() -> (IrLink, IrLink) {
        let channel = Arc::new(Mutex::new(IrChannel::default()));
        (
            IrLink { channel: channel.clone(), side: 0 },
            IrLink { channel, side: 1 },
        )
    }

    /// Publish this port's emitter level (RP bit 0).
    fn publish(&self, led_on: bool) {
        self.channel.lock().unwrap().led[self.side] = led_on;
    }

    /// True while the peer's emitter is lit (this port's received signal).
    fn peer_lit(&self) -> bool {
        self.channel.lock().unwrap().led[self.side ^ 1]
    }
}

impl Clone for IrLink {
    fn clone(&self) -> Self {
        // Sever: a cloned instance gets a fresh, partnerless channel.
        IrLink { channel: Arc::new(Mutex::new(IrChannel::default())), side: self.side }
    }
}

/// The IR partner plugged into the GBC IR port. Disconnected by default — a
/// lone GBC's receiver always reads "no light", exactly as with no transport.
#[derive(Clone, Default)]
pub(crate) enum IrDevice {
    #[default]
    Disconnected,
    #[allow(dead_code)] // KEEP (owner decision 2026-07-20): implemented peripheral awaiting frontend
    // wiring, not rot. No in-tree caller, so `dead_code` fires; do not delete.
    /// A second GBC instance sharing an [`IrLink`].
    Link(IrLink),
    #[allow(dead_code)] // KEEP (owner decision 2026-07-20): implemented peripheral awaiting frontend
    // wiring, not rot. No in-tree caller, so `dead_code` fires; do not delete.
    /// Diagnostic self-test: the port sees its OWN emitter (as though an IR
    /// mirror were held to it). Not how two GBCs communicate — tests only.
    Loopback,
}

impl IrDevice {
    /// Publish this port's emitter state (RP bit 0) to any connected partner.
    pub(crate) fn set_emitter(&self, led_on: bool) {
        if let IrDevice::Link(l) = self {
            l.publish(led_on);
        }
    }

    /// True if the port currently sees incoming IR light. `own_led` is this
    /// port's own emitter, consulted only by the loopback self-test.
    pub(crate) fn receiving(&self, own_led: bool) -> bool {
        match self {
            IrDevice::Disconnected => false,
            IrDevice::Link(l) => l.peer_lit(),
            IrDevice::Loopback => own_led,
        }
    }

    #[allow(dead_code)] // KEEP (owner decision 2026-07-20): implemented peripheral awaiting frontend
    // wiring, not rot. No in-tree caller, so `dead_code` fires; do not delete.
    pub(crate) fn is_connected(&self) -> bool {
        !matches!(self, IrDevice::Disconnected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_sees_the_other_emitter_not_its_own() {
        let (a, b) = IrLink::pair();
        let (da, db) = (IrDevice::Link(a), IrDevice::Link(b));
        // Both dark.
        assert!(!da.receiving(false) && !db.receiving(false));
        // A lights its LED: B sees it, A does not see its own.
        da.set_emitter(true);
        assert!(db.receiving(false), "B must see A's emitter");
        assert!(!da.receiving(true), "A must not see its own emitter");
        // A turns off, B lights up: now A sees B.
        da.set_emitter(false);
        db.set_emitter(true);
        assert!(da.receiving(false) && !db.receiving(true));
    }

    #[test]
    fn loopback_sees_own_emitter() {
        let d = IrDevice::Loopback;
        assert!(d.receiving(true));
        assert!(!d.receiving(false));
    }

    #[test]
    fn clone_severs_the_channel() {
        let (a, b) = IrLink::pair();
        let (da, db) = (IrDevice::Link(a), IrDevice::Link(b.clone()));
        da.set_emitter(true);
        // db was cloned from b, so it no longer shares a's channel.
        assert!(!db.receiving(false), "a cloned end must not ghost-drive");
    }
}
