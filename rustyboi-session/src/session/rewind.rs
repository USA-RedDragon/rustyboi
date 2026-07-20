//! Rewind: the in-session ring buffer plus the offloaded-capture protocol
//! the native platforms' serializer worker drives.

use super::Session;
use rustyboi_core_lib::gb::GB;

impl Session {
    /// Step back to the most recent rewind snapshot, restoring the machine.
    /// Returns the frame index restored to, or `None` if history is empty.
    pub fn rewind(&mut self) -> Option<u64> {
        let snap = self.rewind.rewind()?;
        // Ring blobs are deflate-framed (see `rewind::compress_snapshot`);
        // unframed raw blobs pass through for hosts that push uncompressed.
        let state = crate::rewind::decompress_snapshot(&snap.state)?;
        if self.restore_state(&state).is_ok() {
            self.frame_count = snap.frame;
            Some(snap.frame)
        } else {
            None
        }
    }

    /// Retained rewind snapshots and their total byte footprint.
    pub fn rewind_stats(&self) -> (usize, usize) {
        (self.rewind.len(), self.rewind.memory_bytes())
    }

    /// Drop rewind history (e.g. on ROM change).
    pub(crate) fn clear_rewind(&mut self) {
        self.rewind.clear();
        self.pending_snapshot = None;
    }

    // --- offloaded rewind capture (native platform worker) ------------------
    //
    // These let a host run the expensive savestate serialization off the
    // emulation thread. The session stays thread-free: it only produces a cheap
    // `GB::clone` snapshot and accepts the finished blob back. The host owns the
    // worker thread and the snapshot->serialize->push handoff.

    /// Switch rewind capture into offloaded mode. When enabled, `run_frame`
    /// stops serializing snapshots inline; instead each due capture is a cheap
    /// `GB::clone` retrievable via [`Session::take_pending_snapshot`]. Disabling
    /// restores the self-contained inline serialize path.
    pub fn set_rewind_offloaded(&mut self, offloaded: bool) {
        self.rewind_offloaded = offloaded;
        if !offloaded {
            self.pending_snapshot = None;
        }
    }

    /// Take the cheap snapshot captured this frame (offloaded mode only), if a
    /// capture was due. The caller serializes the returned `GB` on a worker
    /// (via [`rustyboi_core_lib::gb::GB::to_state_bytes`]) and feeds the result
    /// back with [`Session::push_rewind_bytes`]. `None` when no capture was due
    /// or not in offloaded mode.
    pub fn take_pending_snapshot(&mut self) -> Option<(u64, Box<GB>)> {
        self.pending_snapshot.take()
    }

    /// Feed a serialized rewind blob (produced off-thread from a
    /// [`Session::take_pending_snapshot`] clone) into the rewind ring, applying
    /// the same drop-oldest policy as inline capture. Frames may arrive slightly
    /// out of order relative to live play, but each blob is self-describing
    /// (carries its own frame index) so restore is unaffected.
    ///
    /// Hosts should run the blob through [`crate::rewind::compress_snapshot`]
    /// on their worker (as the inline path does) so the ring stores the compact
    /// framed form; raw uncompressed blobs still restore correctly but forgo
    /// the memory saving.
    pub fn push_rewind_bytes(&mut self, frame: u64, bytes: Vec<u8>) {
        self.rewind.push(frame, bytes);
    }
}
