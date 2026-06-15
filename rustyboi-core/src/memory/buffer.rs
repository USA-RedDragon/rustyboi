use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
pub struct Memory<const START: u16, const SIZE: usize> {
    #[serde(with = "serde_bytes")]
    data: [u8; SIZE],
}

impl<const START: u16, const SIZE: usize> Default for Memory<START, SIZE> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const START: u16, const SIZE: usize> Memory<START, SIZE> {
    pub fn new() -> Self {
        Memory {
            data: [0; SIZE],
        }
    }

    fn normalize_addr(addr: u16) -> u16 {
        addr - START
    }

    /// Raw view of the backing bytes (used to expose stable slices/pointers to
    /// a libretro frontend for memory maps and direct RAM access).
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    /// Mutable raw view of the backing bytes.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

pub trait Addressable {
    fn read(&self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, value: u8);
}

impl<const START: u16, const SIZE: usize> Addressable for Memory<START, SIZE> {
    fn read(&self, addr: u16) -> u8 {
        let offset = Self::normalize_addr(addr);
        self.data[offset as usize]
    }

    fn write(&mut self, addr: u16, value: u8) {
        let offset = Self::normalize_addr(addr);
        self.data[offset as usize] = value;
    }
}
