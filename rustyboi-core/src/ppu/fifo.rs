use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct Fifo {
    // Use a fixed-size circular buffer to avoid allocations
    // PPU FIFO typically needs max 16 pixels (8 background + 8 sprite overlap)
    buffer: [u8; 32], // Power of 2 for efficient modulo
    head: usize,      // Next position to read from
    tail: usize,      // Next position to write to  
    size: usize,      // Current number of elements
}

impl Fifo {
    pub fn new() -> Self {
        Fifo {
            buffer: [0; 32],
            head: 0,
            tail: 0,
            size: 0,
        }
    }

    pub fn reset(&mut self) {
        self.head = 0;
        self.tail = 0;
        self.size = 0;
        // Don't need to clear buffer data - just reset pointers
    }

    pub fn push(&mut self, value: u8) {
        if self.size < self.buffer.len() {
            self.buffer[self.tail] = value;
            self.tail = (self.tail + 1) & (self.buffer.len() - 1); // Efficient modulo for power of 2
            self.size += 1;
        }
        // If full, we could either panic or overwrite - Game Boy hardware would likely drop
    }

    pub fn pop(&mut self) -> Result<u8, &'static str> {
        if self.size == 0 {
            return Err("FIFO is empty");
        }
        let value = self.buffer[self.head];
        self.head = (self.head + 1) & (self.buffer.len() - 1); // Efficient modulo for power of 2
        self.size -= 1;
        Ok(value)
    }

    pub fn size(&self) -> usize {
        self.size
    }
}
