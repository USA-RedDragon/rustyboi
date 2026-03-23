//! Bounded rewind ring buffer.
//!
//! Every `interval_frames` frames the session captures a full savestate
//! (`GB::to_state_bytes`) and pushes it here. The buffer keeps at most `depth`
//! of them, dropping the oldest — so the memory bound is `depth * state_size`.
//! `rewind()` pops the most recent snapshot for the session to restore, so
//! repeated calls step further back in time.
//!
//! Pure data structure: it stores opaque savestate blobs and never touches the
//! emulator, filesystem, or a clock. The session owns capture cadence and the
//! restore.

use std::collections::VecDeque;

/// One captured point in time: the savestate bytes plus the frame index it was
/// taken at (metadata for UI / debugging, not required for restore).
#[derive(Clone, Debug)]
pub struct RewindSnapshot {
    pub frame: u64,
    pub state: Vec<u8>,
}

/// A fixed-capacity ring of savestates. `depth == 0` disables capture (a valid
/// "rewind off" state).
pub struct RewindBuffer {
    ring: VecDeque<RewindSnapshot>,
    depth: usize,
    interval: u32,
}

impl RewindBuffer {
    /// Create a buffer holding up to `depth` snapshots, capturing every
    /// `interval` frames. `interval` is clamped to ≥ 1.
    pub fn new(depth: usize, interval: u32) -> Self {
        RewindBuffer {
            ring: VecDeque::with_capacity(depth),
            depth,
            interval: interval.max(1),
        }
    }

    /// Should a snapshot be captured at this (0-based) frame index? True on
    /// multiples of the interval when the buffer is enabled.
    pub fn should_capture(&self, frame: u64) -> bool {
        self.depth > 0 && frame.is_multiple_of(self.interval as u64)
    }

    /// Push a snapshot, evicting the oldest if at capacity. No-op when the
    /// buffer is disabled (`depth == 0`).
    pub fn push(&mut self, frame: u64, state: Vec<u8>) {
        if self.depth == 0 {
            return;
        }
        if self.ring.len() == self.depth {
            self.ring.pop_front();
        }
        self.ring.push_back(RewindSnapshot { frame, state });
    }

    /// Pop the most recent snapshot (one step back), or `None` if empty.
    pub fn rewind(&mut self) -> Option<RewindSnapshot> {
        self.ring.pop_back()
    }

    /// Most recent snapshot without removing it.
    pub fn peek(&self) -> Option<&RewindSnapshot> {
        self.ring.back()
    }

    /// Number of retained snapshots.
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// Approximate memory footprint in bytes (sum of stored state blobs).
    pub fn memory_bytes(&self) -> usize {
        self.ring.iter().map(|s| s.state.len()).sum()
    }

    /// Drop all history (e.g. on reset or ROM change).
    pub fn clear(&mut self) {
        self.ring.clear();
    }

    /// Reconfigure depth/interval, trimming excess snapshots to the new bound.
    pub fn reconfigure(&mut self, depth: usize, interval: u32) {
        self.depth = depth;
        self.interval = interval.max(1);
        while self.ring.len() > self.depth {
            self.ring.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_evicts_oldest_at_capacity() {
        let mut buf = RewindBuffer::new(3, 1);
        for f in 0..5u64 {
            buf.push(f, vec![f as u8]);
        }
        assert_eq!(buf.len(), 3);
        // Oldest two (frames 0,1) evicted; newest is frame 4.
        assert_eq!(buf.peek().unwrap().frame, 4);
        assert_eq!(buf.rewind().unwrap().frame, 4);
        assert_eq!(buf.rewind().unwrap().frame, 3);
        assert_eq!(buf.rewind().unwrap().frame, 2);
        assert!(buf.rewind().is_none());
    }

    #[test]
    fn capture_cadence_follows_interval() {
        let buf = RewindBuffer::new(10, 4);
        assert!(buf.should_capture(0));
        assert!(!buf.should_capture(1));
        assert!(buf.should_capture(8));
    }

    #[test]
    fn depth_zero_disables() {
        let mut buf = RewindBuffer::new(0, 1);
        assert!(!buf.should_capture(0));
        buf.push(0, vec![1, 2, 3]);
        assert!(buf.is_empty());
    }

    #[test]
    fn reconfigure_trims() {
        let mut buf = RewindBuffer::new(5, 1);
        for f in 0..5 {
            buf.push(f, vec![0]);
        }
        buf.reconfigure(2, 2);
        assert_eq!(buf.len(), 2);
    }
}
