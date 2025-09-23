pub struct Disassembler;

impl Disassembler {
    /// Disassemble a single instruction using a read function
    /// Returns (mnemonic, instruction_length)
    pub fn disassemble_with_reader<F>(addr: u16, mut read_fn: F) -> (String, u16)
    where
        F: FnMut(u16) -> u8,
    {
        let opcode = read_fn(addr);
        Self::disassemble_opcode(opcode, addr, |offset| read_fn(addr + offset))
    }
    
    /// Internal helper to disassemble an opcode using a read function
    fn disassemble_opcode<F>(opcode: u8, pc: u16, mut read_fn: F) -> (String, u16)
    where
        F: FnMut(u16) -> u8,
    {
        
        match opcode {
            0x00 => ("NOP".to_string(), 1),
            0x01 => {
                let low = read_fn(1);
                let high = read_fn(2);
                let imm = ((high as u16) << 8) | (low as u16);
                (format!("LD BC, ${:04X}", imm), 3)
            },
            0x02 => ("LD (BC), A".to_string(), 1),
            0x03 => ("INC BC".to_string(), 1),
            0x04 => ("INC B".to_string(), 1),
            0x05 => ("DEC B".to_string(), 1),
            0x06 => {
                let imm = read_fn(1);
                (format!("LD B, ${:02X}", imm), 2)
            },
            0x07 => ("RLCA".to_string(), 1),
            0x08 => {
                let low = read_fn(1);
                let high = read_fn(2);
                let imm = ((high as u16) << 8) | (low as u16);
                (format!("LD (${:04X}), SP", imm), 3)
            },
            0x09 => ("ADD HL, BC".to_string(), 1),
            0x0A => ("LD A, (BC)".to_string(), 1),
            0x0B => ("DEC BC".to_string(), 1),
            0x0C => ("INC C".to_string(), 1),
            0x0D => ("DEC C".to_string(), 1),
            0x0E => {
                let imm = read_fn(1);
                (format!("LD C, ${:02X}", imm), 2)
            },
            0x0F => ("RRCA".to_string(), 1),
            0x10 => ("STOP".to_string(), 1),
            0x11 => {
                let low = read_fn(1);
                let high = read_fn(2);
                let imm = ((high as u16) << 8) | (low as u16);
                (format!("LD DE, ${:04X}", imm), 3)
            },
            0x12 => ("LD (DE), A".to_string(), 1),
            0x13 => ("INC DE".to_string(), 1),
            0x14 => ("INC D".to_string(), 1),
            0x15 => ("DEC D".to_string(), 1),
            0x16 => {
                let imm = read_fn(1);
                (format!("LD D, ${:02X}", imm), 2)
            },
            0x17 => ("RLA".to_string(), 1),
            0x18 => {
                let offset = read_fn(1) as i8;
                let target = pc.wrapping_add(2).wrapping_add(offset as u16);
                (format!("JR ${:04X}", target), 2)
            },
            0x19 => ("ADD HL, DE".to_string(), 1),
            0x1A => ("LD A, (DE)".to_string(), 1),
            0x1B => ("DEC DE".to_string(), 1),
            0x1C => ("INC E".to_string(), 1),
            0x1D => ("DEC E".to_string(), 1),
            0x1E => {
                let imm = read_fn(1);
                (format!("LD E, ${:02X}", imm), 2)
            },
            0x1F => ("RRA".to_string(), 1),
            0x20 => {
                let offset = read_fn(1) as i8;
                let target = pc.wrapping_add(2).wrapping_add(offset as u16);
                (format!("JR NZ, ${:04X}", target), 2)
            },
            0x21 => {
                let low = read_fn(1);
                let high = read_fn(2);
                let imm = ((high as u16) << 8) | (low as u16);
                (format!("LD HL, ${:04X}", imm), 3)
            },
            0x22 => ("LD (HL+), A".to_string(), 1),
            0x23 => ("INC HL".to_string(), 1),
            0x24 => ("INC H".to_string(), 1),
            0x25 => ("DEC H".to_string(), 1),
            0x26 => {
                let imm = read_fn(1);
                (format!("LD H, ${:02X}", imm), 2)
            },
            0x27 => ("DAA".to_string(), 1),
            0x28 => {
                let offset = read_fn(1) as i8;
                let target = pc.wrapping_add(2).wrapping_add(offset as u16);
                (format!("JR Z, ${:04X}", target), 2)
            },
            0x29 => ("ADD HL, HL".to_string(), 1),
            0x2A => ("LD A, (HL+)".to_string(), 1),
            0x2B => ("DEC HL".to_string(), 1),
            0x2C => ("INC L".to_string(), 1),
            0x2D => ("DEC L".to_string(), 1),
            0x2E => {
                let imm = read_fn(1);
                (format!("LD L, ${:02X}", imm), 2)
            },
            0x2F => ("CPL".to_string(), 1),
            0x30 => {
                let offset = read_fn(1) as i8;
                let target = pc.wrapping_add(2).wrapping_add(offset as u16);
                (format!("JR NC, ${:04X}", target), 2)
            },
            0x31 => {
                let low = read_fn(1);
                let high = read_fn(2);
                let imm = ((high as u16) << 8) | (low as u16);
                (format!("LD SP, ${:04X}", imm), 3)
            },
            0x32 => ("LD (HL-), A".to_string(), 1),
            0x33 => ("INC SP".to_string(), 1),
            0x34 => ("INC (HL)".to_string(), 1),
            0x35 => ("DEC (HL)".to_string(), 1),
            0x36 => {
                let imm = read_fn(1);
                (format!("LD (HL), ${:02X}", imm), 2)
            },
            0x37 => ("SCF".to_string(), 1),
            0x38 => {
                let offset = read_fn(1) as i8;
                let target = pc.wrapping_add(2).wrapping_add(offset as u16);
                (format!("JR C, ${:04X}", target), 2)
            },
            0x39 => ("ADD HL, SP".to_string(), 1),
            0x3A => ("LD A, (HL-)".to_string(), 1),
            0x3B => ("DEC SP".to_string(), 1),
            0x3C => ("INC A".to_string(), 1),
            0x3D => ("DEC A".to_string(), 1),
            0x3E => {
                let imm = read_fn(1);
                (format!("LD A, ${:02X}", imm), 2)
            },
            0x3F => ("CCF".to_string(), 1),
            
            // LD r,r instructions (0x40-0x7F except 0x76)
            0x40 => ("LD B, B".to_string(), 1),
            0x41 => ("LD B, C".to_string(), 1),
            0x42 => ("LD B, D".to_string(), 1),
            0x43 => ("LD B, E".to_string(), 1),
            0x44 => ("LD B, H".to_string(), 1),
            0x45 => ("LD B, L".to_string(), 1),
            0x46 => ("LD B, (HL)".to_string(), 1),
            0x47 => ("LD B, A".to_string(), 1),
            0x48 => ("LD C, B".to_string(), 1),
            0x49 => ("LD C, C".to_string(), 1),
            0x4A => ("LD C, D".to_string(), 1),
            0x4B => ("LD C, E".to_string(), 1),
            0x4C => ("LD C, H".to_string(), 1),
            0x4D => ("LD C, L".to_string(), 1),
            0x4E => ("LD C, (HL)".to_string(), 1),
            0x4F => ("LD C, A".to_string(), 1),
            0x50 => ("LD D, B".to_string(), 1),
            0x51 => ("LD D, C".to_string(), 1),
            0x52 => ("LD D, D".to_string(), 1),
            0x53 => ("LD D, E".to_string(), 1),
            0x54 => ("LD D, H".to_string(), 1),
            0x55 => ("LD D, L".to_string(), 1),
            0x56 => ("LD D, (HL)".to_string(), 1),
            0x57 => ("LD D, A".to_string(), 1),
            0x58 => ("LD E, B".to_string(), 1),
            0x59 => ("LD E, C".to_string(), 1),
            0x5A => ("LD E, D".to_string(), 1),
            0x5B => ("LD E, E".to_string(), 1),
            0x5C => ("LD E, H".to_string(), 1),
            0x5D => ("LD E, L".to_string(), 1),
            0x5E => ("LD E, (HL)".to_string(), 1),
            0x5F => ("LD E, A".to_string(), 1),
            0x60 => ("LD H, B".to_string(), 1),
            0x61 => ("LD H, C".to_string(), 1),
            0x62 => ("LD H, D".to_string(), 1),
            0x63 => ("LD H, E".to_string(), 1),
            0x64 => ("LD H, H".to_string(), 1),
            0x65 => ("LD H, L".to_string(), 1),
            0x66 => ("LD H, (HL)".to_string(), 1),
            0x67 => ("LD H, A".to_string(), 1),
            0x68 => ("LD L, B".to_string(), 1),
            0x69 => ("LD L, C".to_string(), 1),
            0x6A => ("LD L, D".to_string(), 1),
            0x6B => ("LD L, E".to_string(), 1),
            0x6C => ("LD L, H".to_string(), 1),
            0x6D => ("LD L, L".to_string(), 1),
            0x6E => ("LD L, (HL)".to_string(), 1),
            0x6F => ("LD L, A".to_string(), 1),
            0x70 => ("LD (HL), B".to_string(), 1),
            0x71 => ("LD (HL), C".to_string(), 1),
            0x72 => ("LD (HL), D".to_string(), 1),
            0x73 => ("LD (HL), E".to_string(), 1),
            0x74 => ("LD (HL), H".to_string(), 1),
            0x75 => ("LD (HL), L".to_string(), 1),
            0x76 => ("HALT".to_string(), 1),
            0x77 => ("LD (HL), A".to_string(), 1),
            0x78 => ("LD A, B".to_string(), 1),
            0x79 => ("LD A, C".to_string(), 1),
            0x7A => ("LD A, D".to_string(), 1),
            0x7B => ("LD A, E".to_string(), 1),
            0x7C => ("LD A, H".to_string(), 1),
            0x7D => ("LD A, L".to_string(), 1),
            0x7E => ("LD A, (HL)".to_string(), 1),
            0x7F => ("LD A, A".to_string(), 1),
            
            // ALU operations
            0x80 => ("ADD A, B".to_string(), 1),
            0x81 => ("ADD A, C".to_string(), 1),
            0x82 => ("ADD A, D".to_string(), 1),
            0x83 => ("ADD A, E".to_string(), 1),
            0x84 => ("ADD A, H".to_string(), 1),
            0x85 => ("ADD A, L".to_string(), 1),
            0x86 => ("ADD A, (HL)".to_string(), 1),
            0x87 => ("ADD A, A".to_string(), 1),
            0x88 => ("ADC A, B".to_string(), 1),
            0x89 => ("ADC A, C".to_string(), 1),
            0x8A => ("ADC A, D".to_string(), 1),
            0x8B => ("ADC A, E".to_string(), 1),
            0x8C => ("ADC A, H".to_string(), 1),
            0x8D => ("ADC A, L".to_string(), 1),
            0x8E => ("ADC A, (HL)".to_string(), 1),
            0x8F => ("ADC A, A".to_string(), 1),
            0x90 => ("SUB B".to_string(), 1),
            0x91 => ("SUB C".to_string(), 1),
            0x92 => ("SUB D".to_string(), 1),
            0x93 => ("SUB E".to_string(), 1),
            0x94 => ("SUB H".to_string(), 1),
            0x95 => ("SUB L".to_string(), 1),
            0x96 => ("SUB (HL)".to_string(), 1),
            0x97 => ("SUB A".to_string(), 1),
            0x98 => ("SBC A, B".to_string(), 1),
            0x99 => ("SBC A, C".to_string(), 1),
            0x9A => ("SBC A, D".to_string(), 1),
            0x9B => ("SBC A, E".to_string(), 1),
            0x9C => ("SBC A, H".to_string(), 1),
            0x9D => ("SBC A, L".to_string(), 1),
            0x9E => ("SBC A, (HL)".to_string(), 1),
            0x9F => ("SBC A, A".to_string(), 1),
            0xA0 => ("AND B".to_string(), 1),
            0xA1 => ("AND C".to_string(), 1),
            0xA2 => ("AND D".to_string(), 1),
            0xA3 => ("AND E".to_string(), 1),
            0xA4 => ("AND H".to_string(), 1),
            0xA5 => ("AND L".to_string(), 1),
            0xA6 => ("AND (HL)".to_string(), 1),
            0xA7 => ("AND A".to_string(), 1),
            0xA8 => ("XOR B".to_string(), 1),
            0xA9 => ("XOR C".to_string(), 1),
            0xAA => ("XOR D".to_string(), 1),
            0xAB => ("XOR E".to_string(), 1),
            0xAC => ("XOR H".to_string(), 1),
            0xAD => ("XOR L".to_string(), 1),
            0xAE => ("XOR (HL)".to_string(), 1),
            0xAF => ("XOR A".to_string(), 1),
            0xB0 => ("OR B".to_string(), 1),
            0xB1 => ("OR C".to_string(), 1),
            0xB2 => ("OR D".to_string(), 1),
            0xB3 => ("OR E".to_string(), 1),
            0xB4 => ("OR H".to_string(), 1),
            0xB5 => ("OR L".to_string(), 1),
            0xB6 => ("OR (HL)".to_string(), 1),
            0xB7 => ("OR A".to_string(), 1),
            0xB8 => ("CP B".to_string(), 1),
            0xB9 => ("CP C".to_string(), 1),
            0xBA => ("CP D".to_string(), 1),
            0xBB => ("CP E".to_string(), 1),
            0xBC => ("CP H".to_string(), 1),
            0xBD => ("CP L".to_string(), 1),
            0xBE => ("CP (HL)".to_string(), 1),
            0xBF => ("CP A".to_string(), 1),
            
            // Conditional returns and jumps
            0xC0 => ("RET NZ".to_string(), 1),
            0xC1 => ("POP BC".to_string(), 1),
            0xC2 => {
                let low = read_fn(1);
                let high = read_fn(2);
                let imm = ((high as u16) << 8) | (low as u16);
                (format!("JP NZ, ${:04X}", imm), 3)
            },
            0xC3 => {
                let low = read_fn(1);
                let high = read_fn(2);
                let imm = ((high as u16) << 8) | (low as u16);
                (format!("JP ${:04X}", imm), 3)
            },
            0xC4 => {
                let low = read_fn(1);
                let high = read_fn(2);
                let imm = ((high as u16) << 8) | (low as u16);
                (format!("CALL NZ, ${:04X}", imm), 3)
            },
            0xC5 => ("PUSH BC".to_string(), 1),
            0xC6 => {
                let imm = read_fn(1);
                (format!("ADD A, ${:02X}", imm), 2)
            },
            0xC7 => ("RST 00H".to_string(), 1),
            0xC8 => ("RET Z".to_string(), 1),
            0xC9 => ("RET".to_string(), 1),
            0xCA => {
                let low = read_fn(1);
                let high = read_fn(2);
                let imm = ((high as u16) << 8) | (low as u16);
                (format!("JP Z, ${:04X}", imm), 3)
            },
            0xCB => {
                // CB-prefixed instructions
                let cb_opcode = read_fn(1);
                let mnemonic = Self::disassemble_cb_instruction(cb_opcode);
                (mnemonic, 2)
            },
            0xCC => {
                let low = read_fn(1);
                let high = read_fn(2);
                let imm = ((high as u16) << 8) | (low as u16);
                (format!("CALL Z, ${:04X}", imm), 3)
            },
            0xCD => {
                let low = read_fn(1);
                let high = read_fn(2);
                let imm = ((high as u16) << 8) | (low as u16);
                (format!("CALL ${:04X}", imm), 3)
            },
            0xCE => {
                let imm = read_fn(1);
                (format!("ADC A, ${:02X}", imm), 2)
            },
            0xCF => ("RST 08H".to_string(), 1),
            0xD0 => ("RET NC".to_string(), 1),
            0xD1 => ("POP DE".to_string(), 1),
            0xD2 => {
                let low = read_fn(1);
                let high = read_fn(2);
                let imm = ((high as u16) << 8) | (low as u16);
                (format!("JP NC, ${:04X}", imm), 3)
            },
            0xD3 => ("INVALID".to_string(), 1),
            0xD4 => {
                let low = read_fn(1);
                let high = read_fn(2);
                let imm = ((high as u16) << 8) | (low as u16);
                (format!("CALL NC, ${:04X}", imm), 3)
            },
            0xD5 => ("PUSH DE".to_string(), 1),
            0xD6 => {
                let imm = read_fn(1);
                (format!("SUB ${:02X}", imm), 2)
            },
            0xD7 => ("RST 10H".to_string(), 1),
            0xD8 => ("RET C".to_string(), 1),
            0xD9 => ("RETI".to_string(), 1),
            0xDA => {
                let low = read_fn(1);
                let high = read_fn(2);
                let imm = ((high as u16) << 8) | (low as u16);
                (format!("JP C, ${:04X}", imm), 3)
            },
            0xDB => ("INVALID".to_string(), 1),
            0xDC => {
                let low = read_fn(1);
                let high = read_fn(2);
                let imm = ((high as u16) << 8) | (low as u16);
                (format!("CALL C, ${:04X}", imm), 3)
            },
            0xDD => ("INVALID".to_string(), 1),
            0xDE => {
                let imm = read_fn(1);
                (format!("SBC A, ${:02X}", imm), 2)
            },
            0xDF => ("RST 18H".to_string(), 1),
            0xE0 => {
                let imm = read_fn(1);
                (format!("LDH (${:02X}), A", imm), 2)
            },
            0xE1 => ("POP HL".to_string(), 1),
            0xE2 => ("LD (C), A".to_string(), 1),
            0xE3 => ("INVALID".to_string(), 1),
            0xE4 => ("INVALID".to_string(), 1),
            0xE5 => ("PUSH HL".to_string(), 1),
            0xE6 => {
                let imm = read_fn(1);
                (format!("AND ${:02X}", imm), 2)
            },
            0xE7 => ("RST 20H".to_string(), 1),
            0xE8 => {
                let offset = read_fn(1) as i8;
                (format!("ADD SP, {:+}", offset), 2)
            },
            0xE9 => ("JP (HL)".to_string(), 1),
            0xEA => {
                let low = read_fn(1);
                let high = read_fn(2);
                let imm = ((high as u16) << 8) | (low as u16);
                (format!("LD (${:04X}), A", imm), 3)
            },
            0xEB => ("INVALID".to_string(), 1),
            0xEC => ("INVALID".to_string(), 1),
            0xED => ("INVALID".to_string(), 1),
            0xEE => {
                let imm = read_fn(1);
                (format!("XOR ${:02X}", imm), 2)
            },
            0xEF => ("RST 28H".to_string(), 1),
            0xF0 => {
                let imm = read_fn(1);
                (format!("LDH A, (${:02X})", imm), 2)
            },
            0xF1 => ("POP AF".to_string(), 1),
            0xF2 => ("LD A, (C)".to_string(), 1),
            0xF3 => ("DI".to_string(), 1),
            0xF4 => ("INVALID".to_string(), 1),
            0xF5 => ("PUSH AF".to_string(), 1),
            0xF6 => {
                let imm = read_fn(1);
                (format!("OR ${:02X}", imm), 2)
            },
            0xF7 => ("RST 30H".to_string(), 1),
            0xF8 => {
                let offset = read_fn(1) as i8;
                (format!("LD HL, SP{:+}", offset), 2)
            },
            0xF9 => ("LD SP, HL".to_string(), 1),
            0xFA => {
                let low = read_fn(1);
                let high = read_fn(2);
                let imm = ((high as u16) << 8) | (low as u16);
                (format!("LD A, (${:04X})", imm), 3)
            },
            0xFB => ("EI".to_string(), 1),
            0xFC => ("INVALID".to_string(), 1),
            0xFD => ("INVALID".to_string(), 1),
            0xFE => {
                let imm = read_fn(1);
                (format!("CP ${:02X}", imm), 2)
            },
            0xFF => ("RST 38H".to_string(), 1),
        }
    }
    
    /// Disassemble CB-prefixed instructions
    fn disassemble_cb_instruction(opcode: u8) -> String {
        match opcode {
            0x00 => "RLC B".to_string(),
            0x01 => "RLC C".to_string(),
            0x02 => "RLC D".to_string(),
            0x03 => "RLC E".to_string(),
            0x04 => "RLC H".to_string(),
            0x05 => "RLC L".to_string(),
            0x06 => "RLC (HL)".to_string(),
            0x07 => "RLC A".to_string(),
            0x08 => "RRC B".to_string(),
            0x09 => "RRC C".to_string(),
            0x0A => "RRC D".to_string(),
            0x0B => "RRC E".to_string(),
            0x0C => "RRC H".to_string(),
            0x0D => "RRC L".to_string(),
            0x0E => "RRC (HL)".to_string(),
            0x0F => "RRC A".to_string(),
            0x10 => "RL B".to_string(),
            0x11 => "RL C".to_string(),
            0x12 => "RL D".to_string(),
            0x13 => "RL E".to_string(),
            0x14 => "RL H".to_string(),
            0x15 => "RL L".to_string(),
            0x16 => "RL (HL)".to_string(),
            0x17 => "RL A".to_string(),
            0x18 => "RR B".to_string(),
            0x19 => "RR C".to_string(),
            0x1A => "RR D".to_string(),
            0x1B => "RR E".to_string(),
            0x1C => "RR H".to_string(),
            0x1D => "RR L".to_string(),
            0x1E => "RR (HL)".to_string(),
            0x1F => "RR A".to_string(),
            0x20 => "SLA B".to_string(),
            0x21 => "SLA C".to_string(),
            0x22 => "SLA D".to_string(),
            0x23 => "SLA E".to_string(),
            0x24 => "SLA H".to_string(),
            0x25 => "SLA L".to_string(),
            0x26 => "SLA (HL)".to_string(),
            0x27 => "SLA A".to_string(),
            0x28 => "SRA B".to_string(),
            0x29 => "SRA C".to_string(),
            0x2A => "SRA D".to_string(),
            0x2B => "SRA E".to_string(),
            0x2C => "SRA H".to_string(),
            0x2D => "SRA L".to_string(),
            0x2E => "SRA (HL)".to_string(),
            0x2F => "SRA A".to_string(),
            0x30 => "SWAP B".to_string(),
            0x31 => "SWAP C".to_string(),
            0x32 => "SWAP D".to_string(),
            0x33 => "SWAP E".to_string(),
            0x34 => "SWAP H".to_string(),
            0x35 => "SWAP L".to_string(),
            0x36 => "SWAP (HL)".to_string(),
            0x37 => "SWAP A".to_string(),
            0x38 => "SRL B".to_string(),
            0x39 => "SRL C".to_string(),
            0x3A => "SRL D".to_string(),
            0x3B => "SRL E".to_string(),
            0x3C => "SRL H".to_string(),
            0x3D => "SRL L".to_string(),
            0x3E => "SRL (HL)".to_string(),
            0x3F => "SRL A".to_string(),
            
            // BIT instructions (0x40-0x7F)
            0x40..=0x47 => format!("BIT 0, {}", Self::get_register_name(opcode & 0x07)),
            0x48..=0x4F => format!("BIT 1, {}", Self::get_register_name(opcode & 0x07)),
            0x50..=0x57 => format!("BIT 2, {}", Self::get_register_name(opcode & 0x07)),
            0x58..=0x5F => format!("BIT 3, {}", Self::get_register_name(opcode & 0x07)),
            0x60..=0x67 => format!("BIT 4, {}", Self::get_register_name(opcode & 0x07)),
            0x68..=0x6F => format!("BIT 5, {}", Self::get_register_name(opcode & 0x07)),
            0x70..=0x77 => format!("BIT 6, {}", Self::get_register_name(opcode & 0x07)),
            0x78..=0x7F => format!("BIT 7, {}", Self::get_register_name(opcode & 0x07)),
            
            // RES instructions (0x80-0xBF)
            0x80..=0x87 => format!("RES 0, {}", Self::get_register_name(opcode & 0x07)),
            0x88..=0x8F => format!("RES 1, {}", Self::get_register_name(opcode & 0x07)),
            0x90..=0x97 => format!("RES 2, {}", Self::get_register_name(opcode & 0x07)),
            0x98..=0x9F => format!("RES 3, {}", Self::get_register_name(opcode & 0x07)),
            0xA0..=0xA7 => format!("RES 4, {}", Self::get_register_name(opcode & 0x07)),
            0xA8..=0xAF => format!("RES 5, {}", Self::get_register_name(opcode & 0x07)),
            0xB0..=0xB7 => format!("RES 6, {}", Self::get_register_name(opcode & 0x07)),
            0xB8..=0xBF => format!("RES 7, {}", Self::get_register_name(opcode & 0x07)),
            
            // SET instructions (0xC0-0xFF)
            0xC0..=0xC7 => format!("SET 0, {}", Self::get_register_name(opcode & 0x07)),
            0xC8..=0xCF => format!("SET 1, {}", Self::get_register_name(opcode & 0x07)),
            0xD0..=0xD7 => format!("SET 2, {}", Self::get_register_name(opcode & 0x07)),
            0xD8..=0xDF => format!("SET 3, {}", Self::get_register_name(opcode & 0x07)),
            0xE0..=0xE7 => format!("SET 4, {}", Self::get_register_name(opcode & 0x07)),
            0xE8..=0xEF => format!("SET 5, {}", Self::get_register_name(opcode & 0x07)),
            0xF0..=0xF7 => format!("SET 6, {}", Self::get_register_name(opcode & 0x07)),
            0xF8..=0xFF => format!("SET 7, {}", Self::get_register_name(opcode & 0x07)),
        }
    }
    
    /// Helper function to get register name for CB instructions
    fn get_register_name(reg_index: u8) -> &'static str {
        match reg_index {
            0 => "B",
            1 => "C",
            2 => "D",
            3 => "E",
            4 => "H",
            5 => "L",
            6 => "(HL)",
            7 => "A",
            _ => "??",
        }
    }
}