//! Attribute each byte of a graded cart-SRAM capture to the instruction that
//! wrote it.
//!
//! The `sram`-graded suites (`gbc_hw_tests` and friends) compare cart save RAM
//! byte-exact against a real-hardware `.sav`. When a cell disagrees, the runner
//! can say *which* cell (`RB_SRAM_VERBOSE=1`) but not *what produced it* — and
//! a probe table is meaningless until you know which IO register each column
//! samples. That gap once cost several agent-runs: a block of `0xE0` bytes was
//! analysed as "STAT mode bits" when it was really `IF` (FF0F) reading "no
//! interrupts pending".
//!
//! This module closes the loop entirely from the runner. The dump path already
//! drives `step_instruction` one instruction at a time, so no core callback is
//! needed: snapshot `save_ram()` around each step and any byte that changed was
//! written by the instruction that just retired. That gives offset -> (PC, cc,
//! value) directly.
//!
//! On top of that it resolves *provenance*: which IO register the stored byte
//! was read from. A shadow of the eight CPU register slots carries an "origin"
//! IO address, seeded by the three loads that can reach IO space (`ldh a,(n8)`,
//! `ld a,(c)`, `ld a,(nn)` with nn >= FF00) and propagated across `ld r,r'`.
//! Invalidation needs no opcode table at all: after every step, any register
//! whose value changed without an explaining opcode has its origin dropped.
//! The writing instruction is decoded to name its exact source register, so the
//! origin reported is the one that actually reached SRAM.
//!
//! Everything here is off unless `RB_SRAM_TRACE` is set, and the whole module
//! is reached through a single `Option<SramTrace>` that stays `None` otherwise.

use rustyboi_core_lib::gb::GB;
use std::collections::BTreeMap;
use std::path::Path;

/// Register slots in Game Boy opcode encoding order, so an opcode's 3-bit
/// register field indexes this array directly. Slot 6 is the `(HL)` escape and
/// is never a real register.
const REG_NAMES: [&str; 8] = ["B", "C", "D", "E", "H", "L", "(HL)", "A"];
const REG_A: usize = 7;
const REG_HL_ESCAPE: usize = 6;

/// Default cap on emitted `SRAM_WRITE` lines. The blame map is always complete;
/// only the chronological log is truncated.
const DEFAULT_LOG_LIMIT: usize = 4096;

/// One observed store into cart SRAM.
#[derive(Clone, Debug)]
pub(crate) struct SramWrite {
    pub offset: usize,
    pub value: u8,
    /// PC of the instruction that retired into this byte.
    pub pc: u16,
    /// Cycles elapsed in the case when the write was observed.
    pub cc: u64,
    /// Source register of the store, when the opcode named one.
    pub src_reg: Option<usize>,
    /// IO address the stored byte was last loaded from, when resolvable.
    pub origin: Option<u16>,
}

impl SramWrite {
    /// `FF0F(IF)` / `FF41(STAT)` / `-` — the field an agent actually greps for.
    fn origin_field(&self) -> String {
        match self.origin {
            Some(address) => match io_register_name(address) {
                Some(name) => format!("{:04X}({name})", address),
                None => format!("{:04X}", address),
            },
            None => "-".to_string(),
        }
    }

    fn src_field(&self) -> &'static str {
        match self.src_reg {
            Some(index) => REG_NAMES[index],
            None => "?",
        }
    }
}

/// Per-instruction SRAM write attribution for one case.
pub(crate) struct SramTrace {
    /// Shadow of SRAM as of the previous step; diffed to find writes.
    shadow: Vec<u8>,
    /// Last write seen for each SRAM offset (what grading blames).
    last_write: BTreeMap<usize, SramWrite>,
    /// Chronological log, capped at `log_limit`.
    log: Vec<SramWrite>,
    log_limit: usize,
    log_overflow: usize,
    /// IO address each register slot was last loaded from.
    reg_origin: [Option<u16>; 8],
    /// Register values as of the previous step, for no-opcode-table invalidation.
    reg_shadow: [u8; 8],
    /// PC of the instruction currently in flight (captured pre-step).
    pending_pc: u16,
    /// Origin assignment the in-flight instruction will make on retire.
    pending_origin: Option<(usize, u16)>,
    /// Origin propagation (`ld dst,src`) the in-flight instruction will make.
    pending_copy: Option<(usize, usize)>,
    /// Source register of the in-flight instruction, if it is a store.
    pending_store_src: Option<usize>,
    symbols: SymbolTable,
}

impl SramTrace {
    /// Build a trace for this case if `RB_SRAM_TRACE` selects it.
    ///
    /// `RB_SRAM_TRACE=1` (or `all`) traces every SRAM-graded case; any other
    /// value is a substring matched against the ROM path, which is what you
    /// want when picking one row out of 342. `RB_SRAM_TRACE_LIMIT` overrides
    /// the chronological log cap.
    pub(crate) fn maybe_new(gb: &GB, rom_path: &Path) -> Option<Self> {
        let selector = std::env::var("RB_SRAM_TRACE").ok()?;
        let selector = selector.trim();
        if selector.is_empty() {
            return None;
        }
        let selects_all = selector == "1" || selector.eq_ignore_ascii_case("all");
        if !selects_all && !rom_path.to_string_lossy().contains(selector) {
            return None;
        }

        let shadow = gb.cartridge()?.save_ram().to_vec();
        if shadow.is_empty() {
            return None;
        }

        let log_limit = std::env::var("RB_SRAM_TRACE_LIMIT")
            .ok()
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .unwrap_or(DEFAULT_LOG_LIMIT);

        Some(Self {
            shadow,
            last_write: BTreeMap::new(),
            log: Vec::new(),
            log_limit,
            log_overflow: 0,
            reg_origin: [None; 8],
            reg_shadow: read_registers(gb),
            pending_pc: gb.get_cpu_registers().pc,
            pending_origin: None,
            pending_copy: None,
            pending_store_src: None,
            symbols: SymbolTable::for_rom(rom_path),
        })
    }

    /// Decode the instruction about to execute: remember its PC, whether it
    /// seeds or propagates a register origin, and whether it is a store (and
    /// from which register).
    pub(crate) fn before_step(&mut self, gb: &GB) {
        let registers = gb.get_cpu_registers();
        let pc = registers.pc;
        self.pending_pc = pc;
        self.pending_origin = None;
        self.pending_copy = None;
        self.pending_store_src = None;

        let opcode = gb.read_memory(pc);
        match opcode {
            // ldh a,(n8) — the canonical IO probe read.
            0xF0 => {
                let n = gb.read_memory(pc.wrapping_add(1));
                self.pending_origin = Some((REG_A, 0xFF00 | u16::from(n)));
            }
            // ld a,(c)
            0xF2 => {
                self.pending_origin = Some((REG_A, 0xFF00 | u16::from(registers.c)));
            }
            // ld a,(nn) — only an IO read when nn lands in FF00-FFFF.
            0xFA => {
                let low = gb.read_memory(pc.wrapping_add(1));
                let high = gb.read_memory(pc.wrapping_add(2));
                let address = u16::from_le_bytes([low, high]);
                if address >= 0xFF00 {
                    self.pending_origin = Some((REG_A, address));
                }
            }
            // ld (bc),a / ld (de),a / ld (hl+),a / ld (hl-),a / ld (nn),a
            0x02 | 0x12 | 0x22 | 0x32 | 0xEA => {
                self.pending_store_src = Some(REG_A);
            }
            // ld (hl),r — 0x76 in this range is halt, not a store.
            0x70..=0x77 if opcode != 0x76 => {
                self.pending_store_src = Some(usize::from(opcode & 0x07));
            }
            // ld r,r' — propagates origin. 0x76 is halt; slot 6 is the (HL) form.
            0x40..=0x7F if opcode != 0x76 => {
                let dst = usize::from((opcode >> 3) & 0x07);
                let src = usize::from(opcode & 0x07);
                if dst != REG_HL_ESCAPE && src != REG_HL_ESCAPE {
                    self.pending_copy = Some((dst, src));
                }
            }
            _ => {}
        }
    }

    /// Diff SRAM against the shadow to find what the retired instruction wrote,
    /// then update register-origin tracking.
    pub(crate) fn after_step(&mut self, gb: &GB, cc: u64) {
        let Some(cartridge) = gb.cartridge() else {
            return;
        };
        let sram = cartridge.save_ram();

        // Fast path: one memcmp per instruction, and only when it trips do we
        // pay for locating the changed bytes.
        if sram.len() == self.shadow.len() && sram != self.shadow.as_slice() {
            let src_reg = self.pending_store_src;
            let origin = src_reg.and_then(|index| self.reg_origin[index]);
            for (offset, (&value, &previous)) in
                sram.iter().zip(self.shadow.iter()).enumerate()
            {
                if value == previous {
                    continue;
                }
                let write = SramWrite {
                    offset,
                    value,
                    pc: self.pending_pc,
                    cc,
                    src_reg,
                    origin,
                };
                if self.log.len() < self.log_limit {
                    self.log.push(write.clone());
                } else {
                    self.log_overflow += 1;
                }
                self.last_write.insert(offset, write);
            }
            self.shadow.copy_from_slice(sram);
        } else if sram.len() != self.shadow.len() {
            self.shadow = sram.to_vec();
        }

        // Origin bookkeeping. Any register whose value moved has an unexplained
        // new source, so drop its origin; the in-flight instruction's own
        // effects are then applied on top. This keeps the tracker honest
        // without enumerating every opcode that writes a register.
        let registers = read_registers(gb);
        for (index, (&value, &previous)) in
            registers.iter().zip(self.reg_shadow.iter()).enumerate()
        {
            if index != REG_HL_ESCAPE && value != previous {
                self.reg_origin[index] = None;
            }
        }
        if let Some((dst, src)) = self.pending_copy {
            self.reg_origin[dst] = self.reg_origin[src];
        }
        if let Some((index, address)) = self.pending_origin {
            self.reg_origin[index] = Some(address);
        }
        self.reg_shadow = registers;
    }

    /// The instruction that last CHANGED a graded offset.
    ///
    /// Not "last wrote": [`Self::after_step`] recovers writes by diffing SRAM
    /// against a shadow copy, so a store that writes a byte's existing value is
    /// invisible and leaves the older attribution standing. On a capture whose
    /// probe results collide with the ROM's own fill byte this reads as "the
    /// test loop never got here" when the loop did run and simply stored the
    /// fill value -- it has already produced one misdiagnosis on
    /// timers/tac_set_disabled, where three cells blamed on `memset` were really
    /// ordinary probe results that happened to equal the 0x00 fill. Corroborate
    /// a suspicious blame against the ROM's write order before believing it.
    pub(crate) fn blame(&self, offset: usize) -> Option<&SramWrite> {
        self.last_write.get(&offset)
    }

    fn symbol_field(&self, pc: u16) -> String {
        match self.symbols.resolve(pc) {
            Some(symbol) => format!(" sym={symbol}"),
            None => String::new(),
        }
    }

    /// One `SRAM_BLAME` line: offset -> the PC that last CHANGED it, joinable
    /// straight onto a `RB_SRAM_VERBOSE` offset. See [`Self::blame`] for why
    /// same-value stores do not appear here.
    pub(crate) fn blame_line(&self, offset: usize, want: u8, got: u8) -> String {
        match self.blame(offset) {
            Some(write) => format!(
                "SRAM_BLAME off={:#06X} want={want:#04X} got={got:#04X} pc={:#06X} cc={} src={} from={}{}",
                offset,
                write.pc,
                write.cc,
                write.src_field(),
                write.origin_field(),
                self.symbol_field(write.pc),
            ),
            None => format!(
                "SRAM_BLAME off={offset:#06X} want={want:#04X} got={got:#04X} pc=? (never changed during the run; a store of the byte's existing value leaves no record)"
            ),
        }
    }

    /// Chronological write log plus the offset->register structure map.
    pub(crate) fn report(&self, graded_len: usize) {
        for write in &self.log {
            eprintln!(
                "SRAM_WRITE off={:#06X} val={:#04X} pc={:#06X} cc={} src={} from={}{}",
                write.offset,
                write.value,
                write.pc,
                write.cc,
                write.src_field(),
                write.origin_field(),
                self.symbol_field(write.pc),
            );
        }
        if self.log_overflow > 0 {
            eprintln!(
                "SRAM_WRITE ... {} further writes suppressed (raise RB_SRAM_TRACE_LIMIT)",
                self.log_overflow
            );
        }
        for line in self.structure_map(graded_len) {
            eprintln!("{line}");
        }
    }

    /// Collapse the graded region into runs of consecutive offsets that share
    /// an IO origin. This is what independently recovers a capture's probe
    /// layout (e.g. "16 STAT, 32 IF, 16 STAT, 32 IF") without assuming
    /// anything about the byte values.
    pub(crate) fn structure_map(&self, graded_len: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let mut offset = 0usize;
        while offset < graded_len {
            let origin = self.blame(offset).and_then(|write| write.origin);
            let mut end = offset;
            while end + 1 < graded_len
                && self.blame(end + 1).and_then(|write| write.origin) == origin
            {
                end += 1;
            }
            let label = match origin {
                Some(address) => match io_register_name(address) {
                    Some(name) => format!("{:04X}({name})", address),
                    None => format!("{:04X}", address),
                },
                None => "unresolved".to_string(),
            };
            let pcs: Vec<u16> = (offset..=end)
                .filter_map(|index| self.blame(index).map(|write| write.pc))
                .collect();
            let pc_field = match (pcs.iter().min(), pcs.iter().max()) {
                (Some(low), Some(high)) if low == high => format!("pc={low:#06X}"),
                (Some(low), Some(high)) => format!("pc={low:#06X}..{high:#06X}"),
                _ => "pc=?".to_string(),
            };
            lines.push(format!(
                "SRAM_MAP {:#06X}..{:#06X} n={} from={} {}",
                offset,
                end,
                end - offset + 1,
                label,
                pc_field
            ));
            offset = end + 1;
        }
        lines
    }
}

fn read_registers(gb: &GB) -> [u8; 8] {
    let r = gb.get_cpu_registers();
    [r.b, r.c, r.d, r.e, r.h, r.l, 0, r.a]
}

/// RGBDS `.sym` symbols for a ROM, when one sits beside it.
///
/// These test ROMs are 32 KiB (banks 0 and 1 both permanently mapped), so the
/// bank a PC belongs to follows from the address and no MBC state is needed.
#[derive(Default)]
struct SymbolTable {
    /// (bank, address, name), sorted.
    entries: Vec<(u16, u16, String)>,
}

impl SymbolTable {
    fn for_rom(rom_path: &Path) -> Self {
        let sym_path = rom_path.with_extension("sym");
        let Ok(text) = std::fs::read_to_string(&sym_path) else {
            return Self::default();
        };
        Self::parse(&text)
    }

    fn parse(text: &str) -> Self {
        let mut entries = Vec::new();
        for line in text.lines() {
            let line = line.split(';').next().unwrap_or("").trim();
            // Section headers such as `[labels]` are not symbol lines.
            if line.is_empty() || line.starts_with('[') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let (Some(location), Some(name)) = (parts.next(), parts.next()) else {
                continue;
            };
            let Some((bank, address)) = location.split_once(':') else {
                continue;
            };
            let (Ok(bank), Ok(address)) = (
                u16::from_str_radix(bank, 16),
                u16::from_str_radix(address, 16),
            ) else {
                continue;
            };
            entries.push((bank, address, name.to_string()));
        }
        entries.sort();
        Self { entries }
    }

    /// Nearest symbol at or below `pc` in `pc`'s bank, rendered as `name` or
    /// `name+0xN`. Falls back to any bank when the derived bank has no match,
    /// so banked ROMs still get a usable (if approximate) name.
    fn resolve(&self, pc: u16) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let bank = if pc < 0x4000 { 0 } else { 1 };
        let best = self
            .entries
            .iter()
            .filter(|(entry_bank, address, _)| *entry_bank == bank && *address <= pc)
            .max_by_key(|(_, address, _)| *address)
            .or_else(|| {
                self.entries
                    .iter()
                    .filter(|(_, address, _)| *address <= pc)
                    .max_by_key(|(_, address, _)| *address)
            })?;
        let delta = pc - best.1;
        if delta == 0 {
            Some(best.2.clone())
        } else {
            Some(format!("{}+{:#X}", best.2, delta))
        }
    }
}

/// Pan Docs names for the IO registers, so `FF0F` reads as `IF` and the
/// "which register am I even looking at" question answers itself.
fn io_register_name(address: u16) -> Option<&'static str> {
    Some(match address {
        0xFF00 => "P1/JOYP",
        0xFF01 => "SB",
        0xFF02 => "SC",
        0xFF04 => "DIV",
        0xFF05 => "TIMA",
        0xFF06 => "TMA",
        0xFF07 => "TAC",
        0xFF0F => "IF",
        0xFF10..=0xFF14 => "NR1x",
        0xFF16..=0xFF19 => "NR2x",
        0xFF1A..=0xFF1E => "NR3x",
        0xFF20..=0xFF23 => "NR4x",
        0xFF24 => "NR50",
        0xFF25 => "NR51",
        0xFF26 => "NR52",
        0xFF30..=0xFF3F => "WAVE",
        0xFF40 => "LCDC",
        0xFF41 => "STAT",
        0xFF42 => "SCY",
        0xFF43 => "SCX",
        0xFF44 => "LY",
        0xFF45 => "LYC",
        0xFF46 => "DMA",
        0xFF47 => "BGP",
        0xFF48 => "OBP0",
        0xFF49 => "OBP1",
        0xFF4A => "WY",
        0xFF4B => "WX",
        0xFF4D => "KEY1",
        0xFF4F => "VBK",
        0xFF51 => "HDMA1",
        0xFF52 => "HDMA2",
        0xFF53 => "HDMA3",
        0xFF54 => "HDMA4",
        0xFF55 => "HDMA5",
        0xFF56 => "RP",
        0xFF68 => "BCPS",
        0xFF69 => "BCPD",
        0xFF6A => "OCPS",
        0xFF6B => "OCPD",
        0xFF6C => "OPRI",
        0xFF70 => "SVBK",
        0xFF76 => "PCM12",
        0xFF77 => "PCM34",
        0xFF80..=0xFFFE => "HRAM",
        0xFFFF => "IE",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_registers_that_caused_the_misdiagnosis() {
        assert_eq!(io_register_name(0xFF0F), Some("IF"));
        assert_eq!(io_register_name(0xFF41), Some("STAT"));
        assert_eq!(io_register_name(0xFF44), Some("LY"));
        assert_eq!(io_register_name(0xFFFF), Some("IE"));
        assert_eq!(io_register_name(0xFF03), None);
    }

    #[test]
    fn parses_rgbds_sym_files() {
        let table = SymbolTable::parse(
            "; generated by rgbds\n[labels]\n00:0150 Main\n00:0180 Probe\n01:4000 Banked\n",
        );
        assert_eq!(table.resolve(0x0150).as_deref(), Some("Main"));
        assert_eq!(table.resolve(0x0154).as_deref(), Some("Main+0x4"));
        assert_eq!(table.resolve(0x0180).as_deref(), Some("Probe"));
        assert_eq!(table.resolve(0x4002).as_deref(), Some("Banked+0x2"));
    }

    #[test]
    fn sym_lookup_is_bank_aware() {
        // Same address in two banks: a bank-0 PC must not resolve to the bank-1
        // symbol, and vice versa.
        let table = SymbolTable::parse("00:3000 InBank0\n01:7000 InBank1\n");
        assert_eq!(table.resolve(0x3004).as_deref(), Some("InBank0+0x4"));
        assert_eq!(table.resolve(0x7004).as_deref(), Some("InBank1+0x4"));
    }

    #[test]
    fn missing_sym_file_resolves_to_nothing() {
        let table = SymbolTable::default();
        assert_eq!(table.resolve(0x0150), None);
    }

    /// The structure map must recover contiguous probe runs from origins alone,
    /// never from the stored byte values — that is the whole point of the tool.
    #[test]
    fn structure_map_recovers_probe_runs() {
        let mut trace = SramTrace {
            shadow: vec![0; 96],
            last_write: BTreeMap::new(),
            log: Vec::new(),
            log_limit: DEFAULT_LOG_LIMIT,
            log_overflow: 0,
            reg_origin: [None; 8],
            reg_shadow: [0; 8],
            pending_pc: 0,
            pending_origin: None,
            pending_copy: None,
            pending_store_src: None,
            symbols: SymbolTable::default(),
        };
        // 16 STAT, then 32 IF — with deliberately identical stored values so a
        // value-based grouping could not produce this split.
        for offset in 0..48usize {
            let origin = if offset < 16 { 0xFF41 } else { 0xFF0F };
            trace.last_write.insert(
                offset,
                SramWrite {
                    offset,
                    value: 0xE0,
                    pc: 0x0200 + offset as u16,
                    cc: offset as u64,
                    src_reg: Some(REG_A),
                    origin: Some(origin),
                },
            );
        }
        let map = trace.structure_map(48);
        assert_eq!(map.len(), 2);
        assert!(map[0].contains("n=16"), "{}", map[0]);
        assert!(map[0].contains("FF41(STAT)"), "{}", map[0]);
        assert!(map[1].contains("n=32"), "{}", map[1]);
        assert!(map[1].contains("FF0F(IF)"), "{}", map[1]);
    }

    #[test]
    fn blame_line_joins_on_the_verbose_offset_format() {
        let mut trace = SramTrace {
            shadow: vec![0; 16],
            last_write: BTreeMap::new(),
            log: Vec::new(),
            log_limit: DEFAULT_LOG_LIMIT,
            log_overflow: 0,
            reg_origin: [None; 8],
            reg_shadow: [0; 8],
            pending_pc: 0,
            pending_origin: None,
            pending_copy: None,
            pending_store_src: None,
            symbols: SymbolTable::default(),
        };
        trace.last_write.insert(
            0x10,
            SramWrite {
                offset: 0x10,
                value: 0xE3,
                pc: 0x0293,
                cc: 42,
                src_reg: Some(REG_A),
                origin: Some(0xFF0F),
            },
        );
        let line = trace.blame_line(0x10, 0xE0, 0xE3);
        // RB_SRAM_VERBOSE prints offsets as `0x0010`; the blame line must use
        // the identical rendering so the two outputs join on a plain grep.
        assert!(line.contains("off=0x0010"), "{line}");
        assert!(line.contains("pc=0x0293"), "{line}");
        assert!(line.contains("from=FF0F(IF)"), "{line}");
        assert!(line.contains("src=A"), "{line}");

        // A byte with no recorded store: no pc to point at, and the reason is
        // ambiguous — the run may never have touched it, or may have stored the
        // value it already held. The line must say so rather than name a pc.
        let unwritten = trace.blame_line(0x02, 0x00, 0xFF);
        assert!(unwritten.contains("off=0x0002"), "{unwritten}");
        assert!(unwritten.contains("pc=?"), "{unwritten}");
        assert!(unwritten.contains("never changed during the run"), "{unwritten}");
    }
}
