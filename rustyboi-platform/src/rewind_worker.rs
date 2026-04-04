//! Off-thread rewind savestate serialization (native desktop only).
//!
//! The emulation thread must stay hitch-free. Rewind capture used to call
//! `GB::to_state_bytes` (a full `serde_json` serialize of VRAM×2 / WRAM×8 / OAM
//! / framebuffers / every peripheral) inline every `interval_frames` — a
//! periodic stall on the deterministic core loop.
//!
//! This worker moves that serialize off the emulation thread. The session's
//! offloaded-capture hook hands us a cheap `GB::clone` (a memcpy, no encode);
//! we serialize it on a dedicated background thread and hand the finished blob
//! back to be pushed into the session's rewind ring. The emulation thread pays
//! a clone, never a serialize.
//!
//! Backpressure: the job queue is bounded and drop-oldest. If the worker falls
//! behind (e.g. a fast-forward burst), the newest snapshot always wins and
//! stale pending clones are discarded — rewind history stays recent rather than
//! stalling the emulator. Finished blobs are self-describing (they carry their
//! own frame index) so out-of-order completion never corrupts restore.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;

use rustyboi_session::GB;

/// A capture job: serialize this cloned machine and tag the result with `frame`.
/// `GB` is `Send` (its audio sink is `Box<dyn AudioOutput + Send>`), so the
/// clone moves to the worker thread directly — no `unsafe`.
struct Job {
    frame: u64,
    gb: Box<GB>,
}

/// A completed serialization, ready to push into the rewind ring.
pub struct Finished {
    pub frame: u64,
    pub bytes: Vec<u8>,
}

/// Owns the background serializer thread and the two channels to it.
pub struct RewindWorker {
    /// Pending clones awaiting serialization. Drop-oldest is enforced by the
    /// worker, which coalesces to the newest queued job (see `serializer_loop`),
    /// so a slow serialize can never stall the emulation thread or replay a
    /// stale backlog. `Option` so `Drop` can close the channel (unblocking the
    /// worker's `recv`) before joining.
    tx: Option<Sender<Job>>,
    /// Finished blobs flowing back to the emulation thread.
    done_rx: Receiver<Finished>,
    handle: Option<JoinHandle<()>>,
}

impl RewindWorker {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        let (done_tx, done_rx) = mpsc::channel::<Finished>();

        let handle = std::thread::Builder::new()
            .name("rewind-serializer".to_string())
            .spawn(move || serializer_loop(rx, done_tx))
            .expect("spawn rewind serializer thread");

        RewindWorker { tx: Some(tx), done_rx, handle: Some(handle) }
    }

    /// Submit a cloned machine for serialization. Cheap on the emulation thread
    /// — it only moves the clone into the channel. If the worker is busy the
    /// clone queues and is coalesced away by a newer one (drop-oldest).
    pub fn submit(&mut self, frame: u64, gb: Box<GB>) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Job { frame, gb });
        }
    }

    /// Non-blocking drain of finished serializations. Returns each `(frame,
    /// bytes)` ready to push into the rewind ring.
    pub fn drain_finished(&mut self) -> Vec<Finished> {
        let mut out = Vec::new();
        loop {
            match self.done_rx.try_recv() {
                Ok(f) => out.push(f),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        out
    }
}

impl Drop for RewindWorker {
    fn drop(&mut self) {
        // Dropping the sender closes the channel; the worker loop exits when the
        // recv errors. Join so we don't leak the thread across ROM reloads.
        self.tx = None;
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Worker body: serialize clones as they arrive. Drop-oldest is realized here —
/// when several jobs are already waiting we coalesce to the newest so rewind
/// history tracks the present rather than replaying a stale backlog.
fn serializer_loop(rx: Receiver<Job>, done_tx: Sender<Finished>) {
    while let Ok(mut job) = rx.recv() {
        // Coalesce: if more jobs are already queued, skip straight to the newest
        // so we never fall behind live play. Older clones are simply dropped.
        while let Ok(newer) = rx.try_recv() {
            job = newer;
        }
        // The expensive part — off the emulation thread.
        if let Ok(bytes) = job.gb.to_state_bytes() {
            if done_tx.send(Finished { frame: job.frame, bytes }).is_err() {
                break; // main side gone
            }
        }
    }
}
