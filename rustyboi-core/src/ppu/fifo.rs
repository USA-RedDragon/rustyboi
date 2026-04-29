use serde::{Deserialize, Serialize};

// A single BG/window pixel captured at fetch time. The color index is the
// 2-bit pattern; `attrs` is the CGB tile-attribute byte sampled when the tile
// was fetched (palette number in bits 0-2, BG-to-OBJ priority in bit 7). DMG
// stores 0 for attrs. The actual palette RAM / BGP color is resolved live at
// shift-out time, as on hardware.
#[derive(Serialize, Deserialize, Clone, Copy, Default)]
pub struct BgPixel {
    pub color: u8,
    pub attrs: u8,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Fifo {
    size: usize,
    head: usize,
    data: Vec<BgPixel>,
}

impl Fifo {
    pub fn new() -> Self {
        Fifo {
            size: 0,
            head: 0,
            data: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        self.size = 0;
        self.head = 0;
        self.data.clear();
    }

    pub fn push(&mut self, value: BgPixel) {
        self.data.push(value);
        self.size += 1;
    }

    pub fn pop(&mut self) -> Result<BgPixel, &'static str> {
        if self.size == 0 {
            return Err("FIFO is empty");
        }
        let value = self.data[self.head];
        self.head += 1;
        self.size -= 1;
        if self.head >= self.data.len() {
            self.data.clear();
            self.head = 0;
        }
        Ok(value)
    }

    pub fn size(&self) -> usize {
        self.size
    }

    // Overwrite the `n` oldest (front) entries in place, leaving the rest of the
    // queue intact. Used by the M3Start fine-scroll path when a mid-discard SCX
    // write moves the first displayed tile to a different tile-map column: the
    // already-queued first tile is stale and must be replaced with the tile at
    // the break column without disturbing the later tiles (which keep their
    // live-SCX columns). `n` must not exceed the current size.
    // Overwrite the `n` newest (most recently pushed) entries in place.
    // Used by the sub-cc SCX column lever to re-key the just-fetched tile to the
    // NEW scx column when a mid-mode-3 write's apply cc precedes the tile's plot.
    pub fn overwrite_newest(&mut self, pixels: &[BgPixel]) {
        let n = pixels.len();
        if n == 0 || n > self.size {
            return;
        }
        let start = self.data.len() - n;
        for (i, p) in pixels.iter().enumerate() {
            self.data[start + i] = *p;
        }
    }

    // Overwrite `pixels.len()` entries starting `offset` slots ahead of the
    // FIFO front (the next pixel to be popped). `offset == 0` targets the front.
    // Used by the DS two-tile straddle rekey to place each rewritten straddle
    // tile at its exact display column regardless of FIFO depth / sprite shift
    // (which makes the "newest N" target ambiguous). Entries beyond the current
    // queue are silently skipped.
    pub fn overwrite_at(&mut self, offset: usize, pixels: &[BgPixel]) {
        for (i, p) in pixels.iter().enumerate() {
            if offset + i >= self.size {
                break;
            }
            let idx = self.head + offset + i;
            if idx < self.data.len() {
                self.data[idx] = *p;
            }
        }
    }

    pub fn overwrite_oldest(&mut self, pixels: &[BgPixel]) {
        for (i, p) in pixels.iter().enumerate() {
            if i >= self.size {
                break;
            }
            let idx = self.head + i;
            if idx < self.data.len() {
                self.data[idx] = *p;
            }
        }
    }
}
