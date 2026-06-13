use serde::{Deserialize, Serialize};

// A single BG/window pixel captured at fetch time. The color index is the
// 2-bit pattern; `attrs` is the CGB tile-attribute byte sampled when the tile
// was fetched (palette number in bits 0-2, BG-to-OBJ priority in bit 7). DMG
// stores 0 for attrs. The actual palette RAM / BGP color is resolved live at
// shift-out time, per Gambatte.
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
}
