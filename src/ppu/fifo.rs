use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct FIFO {
    size: usize,
    data: Vec<u8>,
}

impl FIFO {
    pub fn new() -> Self {
        let mut fifo = FIFO {
            size: 0,
            data: Vec::new(),
        };
        fifo.reset();
        fifo
    }

    pub fn reset(&mut self) {
        self.size = 0;
        self.data = Vec::new();
    }

    pub fn push(&mut self, value: u8) {
        if self.size < self.data.len() {
            self.data[self.size] = value;
        } else {
            self.data.push(value);
        }
        self.size += 1;
    }

    pub fn pop(&mut self) -> Result<u8, &'static str> {
        if self.size == 0 {
            return Err("FIFO is empty");
        }
        let value = self.data.remove(0);
        self.size -= 1;
        Ok(value)
    }

    pub fn size(&self) -> usize {
        self.size
    }
}
