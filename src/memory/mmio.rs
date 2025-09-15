use crate::memory;

const CARTRIDGE_START: u16 = 0x0000;
const CARTRIDGE_SIZE: usize = 16384; // 16KB
const CARTRIDGE_END: u16 = CARTRIDGE_START + CARTRIDGE_SIZE as u16 - 1;
const CARTRIDGE_BANK_START: u16 = 0x4000;
const CARTRIDGE_BANK_SIZE: usize = 16384; // 16KB
const CARTRIDGE_BANK_END: u16 = CARTRIDGE_BANK_START + CARTRIDGE_BANK_SIZE as u16 - 1;
const VRAM_START: u16 = 0x8000;
const VRAM_SIZE: usize = 8192; // 8KB
const VRAM_END: u16 = VRAM_START + VRAM_SIZE as u16 - 1;
const RAM_START: u16 = 0xA000;
const RAM_SIZE: usize = 8192; // 8KB
const RAM_END: u16 = RAM_START + RAM_SIZE as u16 - 1;
const WRAM_START: u16 = 0xC000;
const WRAM_SIZE: usize = 4096; // 4KB
const WRAM_END: u16 = WRAM_START + WRAM_SIZE as u16 - 1;
const WRAM_BANK_START: u16 = 0xD000;
const WRAM_BANK_SIZE: usize = 4096; // 4KB
const WRAM_BANK_END: u16 = WRAM_BANK_START + WRAM_BANK_SIZE as u16 - 1;
const ECHO_RAM_START: u16 = 0xE000;
const ECHO_RAM_SIZE: usize = 7680; // 7.5KB
const ECHO_RAM_END: u16 = ECHO_RAM_START + ECHO_RAM_SIZE as u16 - 1;
const ECHO_RAM_MIRROR_END: u16 = 0xDDFF; // Echo RAM mirrors WRAM and most of WRAM_BANK
const OAM_START: u16 = 0xFE00;
const OAM_SIZE: usize = 160; // 160 bytes
const OAM_END: u16 = OAM_START + OAM_SIZE as u16 - 1;
const UNUSED_START: u16 = 0xFEA0;
const UNUSED_SIZE: usize = 96; // 96 bytes
const UNUSED_END: u16 = UNUSED_START + UNUSED_SIZE as u16 - 1;
const IO_REGISTERS_START: u16 = 0xFF00;
const IO_REGISTERS_SIZE: usize = 128; // 128 bytes
const IO_REGISTERS_END: u16 = IO_REGISTERS_START + IO_REGISTERS_SIZE as u16 - 1;
const HRAM_START: u16 = 0xFF80;
const HRAM_SIZE: usize = 127; // 127 bytes
const HRAM_END: u16 = HRAM_START + HRAM_SIZE as u16 - 1;
const IE_REGISTER: u16 = 0xFFFF; // Interrupt Enable Register

pub struct MMIO {
    cartridge: memory::Memory<CARTRIDGE_START, CARTRIDGE_SIZE>,
    cartridge_bank: memory::Memory<CARTRIDGE_BANK_START, CARTRIDGE_BANK_SIZE>,
    vram: memory::Memory<VRAM_START, VRAM_SIZE>,
    ram: memory::Memory<RAM_START, RAM_SIZE>,
    wram: memory::Memory<WRAM_START, WRAM_SIZE>,
    wram_bank: memory::Memory<WRAM_BANK_START, WRAM_BANK_SIZE>,
    oam: memory::Memory<OAM_START, OAM_SIZE>,
    io_registers: memory::Memory<IO_REGISTERS_START, IO_REGISTERS_SIZE>,
    hram: memory::Memory<HRAM_START, HRAM_SIZE>,
    ie_register: u8,
}

impl MMIO {
    pub fn new() -> Self {
        MMIO {
            cartridge: memory::Memory::new(),
            cartridge_bank: memory::Memory::new(),
            vram: memory::Memory::new(),
            ram: memory::Memory::new(),
            wram: memory::Memory::new(),
            wram_bank: memory::Memory::new(),
            oam: memory::Memory::new(),
            io_registers: memory::Memory::new(),
            hram: memory::Memory::new(),
            ie_register: 0,
        }
    }
}

impl memory::Addressable for MMIO {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            CARTRIDGE_START..=CARTRIDGE_END => self.cartridge.read(addr),
            CARTRIDGE_BANK_START..=CARTRIDGE_BANK_END => self.cartridge_bank.read(addr),
            VRAM_START..=VRAM_END => self.vram.read(addr),
            RAM_START..=RAM_END => self.ram.read(addr),
            WRAM_START..=WRAM_END => self.wram.read(addr),
            WRAM_BANK_START..=WRAM_BANK_END => self.wram_bank.read(addr),
            ECHO_RAM_START..=ECHO_RAM_END => {
                let addr = addr - 0x2000;
                match addr {
                    0..WRAM_START => panic!("This is literally never possible"),
                    WRAM_START..=WRAM_END => self.wram.read(addr),
                    WRAM_BANK_START..=ECHO_RAM_MIRROR_END => self.wram_bank.read(addr),
                    0xDE00..=0xFFFF => panic!("This is literally never possible"),
                }
            },
            OAM_START..=OAM_END => self.oam.read(addr),
            UNUSED_START..=UNUSED_END => 0xFF, // Unused memory returns
            IO_REGISTERS_START..=IO_REGISTERS_END => self.io_registers.read(addr),
            HRAM_START..=HRAM_END => self.hram.read(addr),
            IE_REGISTER => self.ie_register,
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            CARTRIDGE_START..=CARTRIDGE_END => self.cartridge.write(addr, value),
            CARTRIDGE_BANK_START..=CARTRIDGE_BANK_END => self.cartridge_bank.write(addr, value),
            VRAM_START..=VRAM_END => self.vram.write(addr, value),
            RAM_START..=RAM_END => self.ram.write(addr, value),
            WRAM_START..=WRAM_END => self.wram.write(addr, value),
            WRAM_BANK_START..=WRAM_BANK_END => self.wram_bank.write(addr, value),
            ECHO_RAM_START..=ECHO_RAM_END => {
                let addr = addr - 0x2000;
                match addr {
                    0..WRAM_START => panic!("This is literally never possible"),
                    WRAM_START..=WRAM_END => self.wram.write(addr, value),
                    WRAM_BANK_START..=ECHO_RAM_MIRROR_END => self.wram_bank.write(addr, value),
                    0xDE00..=0xFFFF => panic!("This is literally never possible"),
                }
            },
            OAM_START..=OAM_END => self.oam.write(addr, value),
            UNUSED_START..=UNUSED_END => (), // Writes to unused memory are ignored
            IO_REGISTERS_START..=IO_REGISTERS_END => self.io_registers.write(addr, value),
            HRAM_START..=HRAM_END => self.hram.write(addr, value),
            IE_REGISTER => self.ie_register = value,
        }
    }
}
