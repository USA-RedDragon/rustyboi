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

// Ring blobs are deflate-compressed: a raw bincode savestate is ~190 KB and the
// default ring holds 90 of them (~17 MB resident). The raw sections (VRAM/WRAM/
// cart RAM) compress well, so the ring shrinks several-fold with zero behavior
// change — capture cadence, depth, and restore fidelity are untouched, and
// savestate FILES keep their exact format (only the in-RAM ring is encoded).
//
// Framing: 4-byte magic + u32 LE raw length + payload. `RBRZ` = deflate,
// `RBRU` = stored raw (compression didn't pay). `decompress_snapshot` passes
// unframed blobs through unchanged so pre-existing raw blobs (and tests that
// push arbitrary bytes) keep working.

const MAGIC_DEFLATE: &[u8; 4] = b"RBRZ";
const MAGIC_STORED: &[u8; 4] = b"RBRU";
/// Fastest miniz level: the offloaded worker absorbs it off-thread, and the
/// inline (web) path pays ~1 ms per capture at most, once every few frames.
const DEFLATE_LEVEL: u8 = 1;

/// Frame + compress a raw savestate blob for the rewind ring. Falls back to a
/// stored (uncompressed) frame when deflate doesn't pay, so the worst case is
/// raw size + 8 bytes.
pub fn compress_snapshot(raw: Vec<u8>) -> Vec<u8> {
    let deflated = miniz_oxide::deflate::compress_to_vec(&raw, DEFLATE_LEVEL);
    let (magic, payload) = if deflated.len() < raw.len() {
        (MAGIC_DEFLATE, deflated.as_slice())
    } else {
        (MAGIC_STORED, raw.as_slice())
    };
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(magic);
    out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Decode a ring blob back to the raw savestate bytes. Unframed blobs are
/// returned as-is (legacy/raw input); a corrupt deflate stream yields `None`.
pub fn decompress_snapshot(blob: &[u8]) -> Option<Vec<u8>> {
    if blob.len() < 8 {
        return Some(blob.to_vec());
    }
    let (head, rest) = blob.split_at(8);
    let magic: &[u8; 4] = head[..4].try_into().unwrap();
    let raw_len = u32::from_le_bytes(head[4..8].try_into().unwrap()) as usize;
    match magic {
        m if m == MAGIC_DEFLATE => {
            miniz_oxide::inflate::decompress_to_vec_with_limit(rest, raw_len)
                .ok()
                .filter(|v| v.len() == raw_len)
        }
        m if m == MAGIC_STORED => (rest.len() == raw_len).then(|| rest.to_vec()),
        _ => Some(blob.to_vec()),
    }
}

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
    fn compress_roundtrip_compressible_and_incompressible() {
        // Compressible: repeated pattern (like zeroed cart RAM).
        let raw = vec![0u8; 64 * 1024];
        let blob = compress_snapshot(raw.clone());
        assert!(blob.len() < raw.len() / 4, "expected large ratio, got {}", blob.len());
        assert_eq!(decompress_snapshot(&blob).unwrap(), raw);

        // Incompressible: pseudo-random bytes take the stored path but still round-trip.
        let mut x = 0x9e3779b97f4a7c15u64;
        let noisy: Vec<u8> = (0..4096)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        let blob = compress_snapshot(noisy.clone());
        assert!(blob.len() <= noisy.len() + 8);
        assert_eq!(decompress_snapshot(&blob).unwrap(), noisy);
    }

    #[test]
    fn decompress_passes_unframed_blobs_through() {
        // Legacy/raw blobs (no magic) come back unchanged — including short ones.
        assert_eq!(decompress_snapshot(b"hello").unwrap(), b"hello");
        let raw = vec![7u8; 100];
        assert_eq!(decompress_snapshot(&raw).unwrap(), raw);
        // Corrupt deflate payload yields None, not garbage.
        let mut blob = compress_snapshot(vec![0u8; 1024]);
        let n = blob.len();
        blob[n - 1] ^= 0xFF;
        blob.truncate(n - 4);
        assert!(decompress_snapshot(&blob).is_none());
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
