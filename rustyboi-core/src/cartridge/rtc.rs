//! Real-time clock subsystem: MBC3/HuC-3 RTC register I/O, the cycle-derived
//! tick + wall-clock catch-up, and .rtc/.sav-footer persistence.

use super::*;
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

impl Cartridge {
    pub(super) const MBC3_RTC_BLOB_LEN: usize = 48;
    pub(super) const MBC3_RTC_BLOB_LEN_LEGACY: usize = 44;
    pub(super) const HUC3_RTC_BLOB_LEN: usize = 136;

    /// Read from MBC3 RTC registers
    pub(super) fn read_rtc_register(&self, sel: u8) -> u8 {
        // Reads always return the CPU-visible (latched) shadow register. On real
        // MBC3 the internal free-running counters (`rtc_seconds`..) are never read
        // directly — a latch (any write to 0x6000-0x7FFF) copies them into these
        // shadow registers, and software reads the shadows. Register writes go to
        // the internal counters only (see `write_rtc_register`), so a freshly
        // written value is not visible until the next latch.
        match sel {
            0x08 => self.rtc_latched.seconds,
            0x09 => self.rtc_latched.minutes,
            0x0A => self.rtc_latched.hours,
            0x0B => self.rtc_latched.days_low,
            0x0C => self.rtc_latched.days_high,
            _ => 0xFF,
        }
    }
    /// Write to MBC3 RTC registers. A write updates the INTERNAL free-running
    /// counter (`rtc_*`, advanced by the cycle-derived tick) only — it does NOT
    /// touch the CPU-visible latched shadow (`rtc_*_latched`, the read path).
    /// The written value only becomes visible on the next latch, exactly as on
    /// real MBC3 hardware (the write updates the internal counter, not the latch).
    /// Register widths are the documented MBC3 masks (seconds/minutes 6-bit,
    /// hours 5-bit, days_high = day bit 8 + HALT + carry).
    pub(super) fn write_rtc_register(&mut self, sel: u8, value: u8) {
        match sel {
            0x08 => {
                self.rtc.seconds = value & 0x3F;
                // Writing seconds resets the internal sub-second divider, so the
                // next tick is a full second away.
                self.rtc_cycle_accum = 0;
            }
            0x09 => self.rtc.minutes = value & 0x3F,
            0x0A => self.rtc.hours = value & 0x1F,
            0x0B => self.rtc.days_low = value,
            0x0C => self.rtc.days_high = value & 0xC1,
            _ => {}
        }
        // Persist software clock-sets / HALT toggles immediately.
        self.flush_rtc_file();
    }
    /// Copy the live internal RTC counters into the CPU-visible latch registers.
    /// On real MBC3 this happens on ANY write to the 0x6000-0x7FFF region (no
    /// 0x00->0x01 edge is required, the latch fires on any such write). The
    /// read path returns these shadows,
    /// so software must latch to observe the advancing clock.
    pub(super) fn latch_rtc(&mut self) {
        self.rtc_latched.seconds = self.rtc.seconds;
        self.rtc_latched.minutes = self.rtc.minutes;
        self.rtc_latched.hours = self.rtc.hours;
        self.rtc_latched.days_low = self.rtc.days_low;
        self.rtc_latched.days_high = self.rtc.days_high;
        // Keep the persisted latched shadows fresh: other tools reconstruct the
        // clock from the blob's LATCHED fields + timestamp, so they matter
        // for cross-tool reads. No-op without a sidecar.
        self.flush_rtc_file();
    }
    /// Advance the cycle-derived RTC by `cycles` master (dot) clock T-cycles.
    /// Driven from the bus tick loop (`master_cc` advances at 4.194304 MHz
    /// regardless of CPU speed), so the clock is fully deterministic. No-op
    /// unless this cart actually has an RTC (MBC3 timer or HuC-3). For MBC3
    /// the HALT bit (bit 6 of days_high) freezes advancement but the
    /// sub-second accumulator keeps running so the halt/resume boundary lands
    /// on an exact second, matching hardware.
    pub(crate) fn rtc_tick(&mut self, cycles: u64, kind: RtcTickKind) {
        if cycles == 0 {
            return;
        }
        match kind {
            RtcTickKind::Mbc3 => {
                // HALT bit frozen: the crystal still oscillates but the counters
                // do not advance. Do not accumulate while halted so no seconds
                // are "banked".
                if self.rtc.days_high & 0x40 != 0 {
                    return;
                }
                self.rtc_cycle_accum = self.rtc_cycle_accum.wrapping_add(cycles);
                const CYCLES_PER_SECOND: u64 = 4_194_304;
                let mut advanced = false;
                while self.rtc_cycle_accum >= CYCLES_PER_SECOND {
                    self.rtc_cycle_accum -= CYCLES_PER_SECOND;
                    self.advance_rtc_second();
                    advanced = true;
                }
                if advanced {
                    // Stream the advanced clock to the `.rtc` sidecar (no-op
                    // without one, keeping the test path I/O- and
                    // wall-clock-free).
                    self.flush_rtc_file();
                }
            }
            RtcTickKind::HuC3 => {
                // The HuC-3 clock counts whole minutes: minute-of-day rolls at
                // 1440 into a 12-bit day counter (Pan Docs RTC location map).
                self.huc3_rtc.accum = self.huc3_rtc.accum.wrapping_add(cycles);
                const CYCLES_PER_MINUTE: u64 = 60 * 4_194_304;
                let mut advanced = false;
                while self.huc3_rtc.accum >= CYCLES_PER_MINUTE {
                    self.huc3_rtc.accum -= CYCLES_PER_MINUTE;
                    let (mut minutes, mut days) = self.huc3_clock();
                    minutes += 1;
                    if minutes >= 1440 {
                        minutes = 0;
                        days = (days + 1) & 0x0FFF;
                    }
                    self.huc3_set_clock(minutes, days);
                    advanced = true;
                }
                if advanced {
                    self.flush_rtc_file();
                }
            }
            RtcTickKind::None => {}
        }
    }
    /// Increment the live RTC by one second with the full MBC3 cascade:
    /// seconds 0->59, minutes 0->59, hours 0->23, then the 9-bit day counter
    /// (days_low + bit 0 of days_high). Overflow of the day counter sets the
    /// day-carry flag (bit 7 of days_high), which latches until software clears
    /// it. Mirrors real MBC3: the 6-bit seconds/minutes registers can hold
    /// out-of-range values written by software; on the natural tick the seconds
    /// counter counts 0..59 and wraps, and an out-of-range value simply keeps
    /// counting up (it does NOT force-normalise), so a value like 60 advances to
    /// 61.. up to 63 then wraps to 0 with a minute carry — the documented
    /// hardware quirk the RTC test ROMs check.
    pub(super) fn advance_rtc_second(&mut self) {
        // Seconds: 6-bit counter. 59 -> 0 carries to minutes; any other value
        // (including out-of-range 60-62) just increments, and 63 -> 0 without a
        // carry (the 6-bit register simply overflows) — matching hardware where
        // only the 59->0 transition produces the minute carry.
        let sec = self.rtc.seconds & 0x3F;
        if sec == 59 {
            self.rtc.seconds = 0;
        } else {
            self.rtc.seconds = (sec + 1) & 0x3F;
            return;
        }

        let min = self.rtc.minutes & 0x3F;
        if min == 59 {
            self.rtc.minutes = 0;
        } else {
            self.rtc.minutes = (min + 1) & 0x3F;
            return;
        }

        let hour = self.rtc.hours & 0x1F;
        if hour == 23 {
            self.rtc.hours = 0;
        } else {
            self.rtc.hours = (hour + 1) & 0x1F;
            return;
        }

        // Day counter: 9 bits = days_low (8) + bit 0 of days_high. On overflow
        // past 0x1FF the counter wraps to 0 and the carry flag (bit 7) latches.
        let day = (self.rtc.days_low as u16) | (((self.rtc.days_high & 0x01) as u16) << 8);
        let next = day + 1;
        self.rtc.days_low = (next & 0xFF) as u8;
        // Preserve HALT (bit 6) and the already-latched carry (bit 7); set bit 0
        // from the new day counter, and set carry on the 0x1FF -> 0x200 wrap.
        let mut high = self.rtc.days_high & 0xC0;
        if next & 0x100 != 0 {
            high |= 0x01;
        }
        if next > 0x1FF {
            self.rtc.days_low = 0;
            high &= !0x01;
            high |= 0x80; // day-carry latches until software clears it
        }
        self.rtc.days_high = high;
    }
    pub(super) fn mbc3_rtc_serialize(&self, unix_time: u64) -> [u8; Self::MBC3_RTC_BLOB_LEN] {
        let regs = [
            self.rtc.seconds,
            self.rtc.minutes,
            self.rtc.hours,
            self.rtc.days_low,
            self.rtc.days_high,
            self.rtc_latched.seconds,
            self.rtc_latched.minutes,
            self.rtc_latched.hours,
            self.rtc_latched.days_low,
            self.rtc_latched.days_high,
        ];
        let mut out = [0u8; Self::MBC3_RTC_BLOB_LEN];
        for (i, r) in regs.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&(*r as u32).to_le_bytes());
        }
        out[40..48].copy_from_slice(&unix_time.to_le_bytes());
        out
    }
    /// Restore the MBC3 RTC registers from a 44/48-byte blob; returns the
    /// stored save-time timestamp. Registers are masked to their physical
    /// widths (as in `write_rtc_register`); out-of-range 6-bit values a game
    /// wrote (e.g. seconds 60-63) survive the round trip.
    pub(super) fn mbc3_rtc_deserialize(&mut self, data: &[u8]) -> Option<u64> {
        if data.len() < Self::MBC3_RTC_BLOB_LEN_LEGACY {
            return None;
        }
        let reg = |i: usize| u32::from_le_bytes(data[i * 4..i * 4 + 4].try_into().unwrap()) as u8;
        self.rtc.seconds = reg(0) & 0x3F;
        self.rtc.minutes = reg(1) & 0x3F;
        self.rtc.hours = reg(2) & 0x1F;
        self.rtc.days_low = reg(3);
        self.rtc.days_high = reg(4) & 0xC1;
        self.rtc_latched.seconds = reg(5) & 0x3F;
        self.rtc_latched.minutes = reg(6) & 0x3F;
        self.rtc_latched.hours = reg(7) & 0x1F;
        self.rtc_latched.days_low = reg(8);
        self.rtc_latched.days_high = reg(9) & 0xC1;
        // The restored state begins a fresh second.
        self.rtc_cycle_accum = 0;
        Some(if data.len() >= Self::MBC3_RTC_BLOB_LEN {
            u64::from_le_bytes(data[40..48].try_into().unwrap())
        } else {
            u32::from_le_bytes(data[40..44].try_into().unwrap()) as u64
        })
    }
    pub(super) fn huc3_rtc_serialize(&self, unix_time: u64) -> [u8; Self::HUC3_RTC_BLOB_LEN] {
        let mut out = [0u8; Self::HUC3_RTC_BLOB_LEN];
        for (i, chunk) in self.huc3_rtc.mem.chunks(2).take(0x80).enumerate() {
            out[i] = (chunk[0] & 0x0F) | (chunk.get(1).copied().unwrap_or(0) << 4);
        }
        out[128..136].copy_from_slice(&unix_time.to_le_bytes());
        out
    }
    pub(super) fn huc3_rtc_deserialize(&mut self, data: &[u8]) -> Option<u64> {
        if data.len() < Self::HUC3_RTC_BLOB_LEN || self.huc3_rtc.mem.len() < 0x100 {
            return None;
        }
        for (i, &d) in data[..0x80].iter().enumerate() {
            self.huc3_rtc.mem[i * 2] = d & 0x0F;
            self.huc3_rtc.mem[i * 2 + 1] = d >> 4;
        }
        // The restored state begins a fresh minute.
        self.huc3_rtc.accum = 0;
        Some(u64::from_le_bytes(data[128..136].try_into().unwrap()))
    }
    /// Serialize the RTC state to its persistence blob (see the format notes
    /// above); None for carts without an RTC.
    pub(super) fn rtc_serialize(&self, unix_time: u64) -> Option<Vec<u8>> {
        match self.get_cartridge_type() {
            CartridgeType::MBC3 { timer: true, .. } => {
                Some(self.mbc3_rtc_serialize(unix_time).to_vec())
            }
            CartridgeType::HuC3 => Some(self.huc3_rtc_serialize(unix_time).to_vec()),
            _ => None,
        }
    }
    /// Restore the RTC state from a persistence blob; returns the stored
    /// save-time timestamp on success.
    pub(super) fn rtc_deserialize(&mut self, data: &[u8]) -> Option<u64> {
        match self.get_cartridge_type() {
            CartridgeType::MBC3 { timer: true, .. } => self.mbc3_rtc_deserialize(data),
            CartridgeType::HuC3 => self.huc3_rtc_deserialize(data),
            _ => None,
        }
    }
    /// Closed-form advance of one MBC3 cascade stage. `width` is the physical
    /// register modulus (64 for the 6-bit seconds/minutes, 32 for the 5-bit
    /// hours), `period` the natural roll-over (60/60/24). Returns the final
    /// value and the carries produced into the next stage. Out-of-range
    /// values (e.g. seconds 60-63) keep counting up and wrap to 0 at `width`
    /// WITHOUT a carry -- exactly the `advance_rtc_second` behaviour.
    pub(super) fn counter_advance(value: u8, width: u64, period: u64, n: u64) -> (u8, u64) {
        let v = value as u64;
        if v < period {
            (((v + n) % period) as u8, (v + n) / period)
        } else if n < width - v {
            ((v + n) as u8, 0)
        } else {
            let m = n - (width - v);
            ((m % period) as u8, m / period)
        }
    }
    /// Advance the live MBC3 RTC by `n` seconds in closed form; equivalent to
    /// `n` calls of `advance_rtc_second` (unit-tested) but O(1), so
    /// multi-year wall-clock catch-up is instant. Latched shadows are not
    /// touched (they only move on an explicit latch), matching the standard
    /// catch-up which advances only the live counters.
    pub(super) fn mbc3_rtc_advance_seconds(&mut self, n: u64) {
        if n == 0 {
            return;
        }
        let (s, carries) = Self::counter_advance(self.rtc.seconds & 0x3F, 64, 60, n);
        self.rtc.seconds = s;
        if carries == 0 {
            return;
        }
        let (m, carries) = Self::counter_advance(self.rtc.minutes & 0x3F, 64, 60, carries);
        self.rtc.minutes = m;
        if carries == 0 {
            return;
        }
        let (h, carries) = Self::counter_advance(self.rtc.hours & 0x1F, 32, 24, carries);
        self.rtc.hours = h;
        if carries == 0 {
            return;
        }
        let day = (self.rtc.days_low as u64) | (((self.rtc.days_high & 0x01) as u64) << 8);
        let total = day + carries;
        self.rtc.days_low = (total & 0xFF) as u8;
        let mut high = self.rtc.days_high & 0xC0;
        high |= ((total >> 8) & 0x01) as u8;
        if total > 0x1FF {
            high |= 0x80; // day-counter overflow latches until software clears it
        }
        self.rtc.days_high = high;
    }
    /// Advance the HuC-3 minute-of-day/day counters by `n` minutes in closed
    /// form; equivalent to `n` iterations of the per-minute tick.
    pub(super) fn huc3_rtc_advance_minutes(&mut self, mut n: u64) {
        if n == 0 || self.huc3_rtc.mem.len() < 0x16 {
            return;
        }
        let (mut minutes, mut days) = self.huc3_clock();
        // An out-of-range minute-of-day (>= 1440, only reachable via a raw
        // nibble write) normalises to 0 with a day carry on its first tick,
        // same as the incremental path.
        if minutes >= 1440 {
            minutes = 0;
            days = (days + 1) & 0x0FFF;
            n -= 1;
        }
        let total = minutes as u64 + n;
        let final_minutes = (total % 1440) as u16;
        let final_days = ((days as u64 + total / 1440) & 0x0FFF) as u16;
        self.huc3_set_clock(final_minutes, final_days);
    }
    /// Wall-clock catch-up applied when RTC state is restored from
    /// persistence: advance the clock by the real seconds elapsed since the
    /// state was saved (Pan Docs MBC3: the coin cell keeps the oscillator
    /// running while the console is off). MBC3 honours the HALT bit (a halted
    /// clock stays put across sessions); the HuC-3 clock has no halt. Never
    /// reached on the deterministic in-memory path (nothing is restored
    /// there).
    pub(super) fn rtc_catch_up(&mut self, elapsed_seconds: u64) {
        if elapsed_seconds == 0 {
            return;
        }
        match self.get_cartridge_type() {
            CartridgeType::MBC3 { timer: true, .. } => {
                if self.rtc.days_high & 0x40 != 0 {
                    return; // halted
                }
                self.mbc3_rtc_advance_seconds(elapsed_seconds);
            }
            CartridgeType::HuC3 => {
                self.huc3_rtc_advance_minutes(elapsed_seconds / 60);
                // Sub-minute remainder feeds the cycle accumulator so the
                // next in-session minute fires early by the carried amount.
                self.huc3_rtc.accum = self
                    .huc3_rtc
                    .accum
                    .saturating_add((elapsed_seconds % 60) * 4_194_304);
            }
            _ => {}
        }
    }
    /// Restore RTC state from a blob and apply wall-clock catch-up. A zero
    /// timestamp (writer had no wall clock, e.g. an older rustyboi
    /// RETRO_MEMORY_RTC dump) or one from the future (host clock skew)
    /// restores the registers without catch-up.
    pub(super) fn rtc_restore_with_catch_up(&mut self, data: &[u8]) -> bool {
        let Some(saved_at) = self.rtc_deserialize(data) else {
            return false;
        };
        let now = Self::unix_now();
        if saved_at != 0 && saved_at < now {
            self.rtc_catch_up(now - saved_at);
        }
        true
    }
    /// Current wall clock as UNIX seconds. Only ever called on persistence
    /// paths (sidecar attach/flush, libretro RTC memory), never on the
    /// deterministic cycle-derived path.
    pub(super) fn unix_now() -> u64 {
        // `std::time::SystemTime::now()` traps (`unreachable`) on
        // wasm32-unknown-unknown; `web-time` reads the browser clock there.
        #[cfg(target_arch = "wasm32")]
        use web_time::{SystemTime, UNIX_EPOCH};
        #[cfg(not(target_arch = "wasm32"))]
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
    /// `.rtc` sidecar path: the ROM path with its extension replaced (same
    /// derivation as the `.sav`, so the two land side by side).
    pub(super) fn get_rtc_file_path(&self) -> Option<String> {
        self.rom_path.as_ref().map(|path| {
            let mut rtc_path = path.clone();
            if let Some(dot_pos) = rtc_path.rfind('.') {
                rtc_path.truncate(dot_pos);
            }
            rtc_path.push_str(".rtc");
            rtc_path
        })
    }
    /// A de-facto RTC blob appended to the `.sav`, if the file is
    /// exactly RAM+blob sized. Read-only interop: the `.rtc` sidecar is
    /// canonical for us and the footer is never (re)written, but a save
    /// imported from other tools restores its clock on first load.
    pub(super) fn read_sav_rtc_footer(&self) -> Option<Vec<u8>> {
        let expected: &[usize] = match self.get_cartridge_type() {
            CartridgeType::MBC3 { timer: true, .. } => {
                &[Self::MBC3_RTC_BLOB_LEN, Self::MBC3_RTC_BLOB_LEN_LEGACY]
            }
            CartridgeType::HuC3 => &[Self::HUC3_RTC_BLOB_LEN],
            _ => return None,
        };
        let sav_path = self.get_save_file_path()?;
        let data = fs::read(Path::new(&sav_path)).ok()?;
        let footer_len = data.len().checked_sub(self.ram_data.len())?;
        if expected.contains(&footer_len) {
            Some(data[self.ram_data.len()..].to_vec())
        } else {
            None
        }
    }
    /// Attach the `.rtc` sidecar (disk-load path only): restore persisted RTC
    /// state with wall-clock catch-up and keep the file open for streaming
    /// rewrites as the clock advances. When no sidecar exists, fall back to a
    /// `.sav` RTC footer, then create the sidecar. No-op without an RTC, for
    /// host-managed carts, and for in-memory carts (no `rom_path`).
    pub(super) fn attach_rtc_sidecar(&mut self) -> Result<(), io::Error> {
        if !self.has_rtc() || self.host_managed_saves {
            return Ok(());
        }
        let Some(rtc_path) = self.get_rtc_file_path() else {
            return Ok(());
        };
        let rtc_path = Path::new(&rtc_path);
        if rtc_path.exists() {
            let data = fs::read(rtc_path)?;
            if self.rtc_restore_with_catch_up(&data) {
                println!("Loaded RTC file: {}", rtc_path.display());
            }
        } else if let Some(footer) = self.read_sav_rtc_footer()
            && self.rtc_restore_with_catch_up(&footer)
        {
            println!("Loaded RTC footer from existing save file");
        }
        self.rtc_file = Some(
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(rtc_path)?,
        );
        // Seed/refresh the sidecar so it is valid from the first second.
        self.flush_rtc_file();
        Ok(())
    }
    /// Rewrite the `.rtc` sidecar with the current state stamped with the
    /// current wall clock. No-op unless a sidecar is attached, so the
    /// deterministic test path performs no I/O and never reads the host
    /// clock. I/O errors are swallowed like the `.sav` streaming writes.
    pub(super) fn flush_rtc_file(&mut self) {
        if self.rtc_file.is_none() {
            return;
        }
        let Some(blob) = self.rtc_serialize(Self::unix_now()) else {
            return;
        };
        if let Some(file) = self.rtc_file.as_mut() {
            let _ = file.seek(SeekFrom::Start(0));
            let _ = file.write_all(&blob);
            let _ = file.flush();
        }
    }
    /// True if this cartridge has a real-time clock (MBC3 timer or HuC-3).
    /// Gates the bus-driven `rtc_tick` path.
    pub fn has_rtc(&self) -> bool {
        matches!(
            self.get_cartridge_type(),
            CartridgeType::MBC3 { timer: true, .. } | CartridgeType::HuC3
        )
    }
    /// Classify the per-dot RTC advance once, so the hot path can cache it.
    pub(crate) fn rtc_kind(&self) -> RtcTickKind {
        match self.get_cartridge_type() {
            CartridgeType::MBC3 { timer: true, .. } => RtcTickKind::Mbc3,
            CartridgeType::HuC3 => RtcTickKind::HuC3,
            _ => RtcTickKind::None,
        }
    }
    /// True if the cartridge needs the per-dot peripheral clock tick (an RTC
    /// crystal or the camera capture countdown).
    pub(crate) fn needs_clock_tick(&self) -> bool {
        self.has_rtc() || self.has_camera()
    }
    /// Mutable view of the RTC bytes for `RETRO_MEMORY_RTC`, in the exact
    /// `.rtc` persistence format (MBC3: the 48-byte block; HuC-3:
    /// the 136-byte block) stamped with the current wall clock, so the
    /// frontend's `.rtc` files are byte-compatible with the de-facto format. The buffer
    /// allocation stays stable across calls (the frontend caches the raw
    /// pointer). Empty for carts without an RTC.
    pub fn rtc_memory_mut(&mut self) -> &mut [u8] {
        self.rtc_memory_refresh();
        &mut self.rtc_memory
    }
    #[allow(dead_code)] // no in-tree caller; `pub` was masking dead_code. Unwired-peripheral and
    // unfinished-feature code lives here — check the feature roadmap before deleting.
    /// Read-only mirror of [`rtc_memory_mut`](Self::rtc_memory_mut): the
    /// serialized RTC region. Empty for carts without an RTC. Takes `&mut self`
    /// only because it must refresh the region from live state first (the
    /// pointer stays stable), but performs no external mutation.
    pub(crate) fn rtc_memory(&mut self) -> &[u8] {
        self.rtc_memory_refresh();
        &self.rtc_memory
    }
    /// The current RTC state serialized to the de-facto `.rtc` sidecar format
    /// (File → Export RTC). `None` for carts without an RTC.
    pub fn export_rtc(&self) -> Option<Vec<u8>> {
        if !self.has_rtc() {
            return None;
        }
        self.rtc_serialize(Self::unix_now())
    }
    /// Import a `.rtc` sidecar blob (File → Import RTC): restore the persisted
    /// clock with wall-clock catch-up, then flush the attached sidecar (desktop)
    /// so the import survives a reload. Errors on a blob that doesn't match this
    /// cart's RTC layout. No-op-error for non-RTC carts.
    pub fn import_rtc(&mut self, bytes: &[u8]) -> Result<(), String> {
        if !self.has_rtc() {
            return Err("cartridge has no real-time clock".into());
        }
        if !self.rtc_restore_with_catch_up(bytes) {
            return Err("RTC data does not match this cartridge".into());
        }
        self.flush_rtc_file();
        Ok(())
    }
    /// Re-sync the RETRO_MEMORY_RTC buffer from the live state (+ a fresh
    /// timestamp) and remember what we wrote, so an external write into the
    /// region by the frontend is detectable.
    pub(super) fn rtc_memory_refresh(&mut self) {
        let Some(blob) = self.rtc_serialize(Self::unix_now()) else {
            self.rtc_memory.clear();
            self.rtc_memory_synced.clear();
            return;
        };
        if self.rtc_memory.len() == blob.len() {
            self.rtc_memory.copy_from_slice(&blob); // in place: pointer stays valid
        } else {
            self.rtc_memory = blob.clone();
        }
        if self.rtc_memory_synced.len() == blob.len() {
            self.rtc_memory_synced.copy_from_slice(&blob);
        } else {
            self.rtc_memory_synced = blob;
        }
    }
    /// Once-per-frame RTC sync for the libretro frontend. RetroArch loads an
    /// existing `.rtc` file by memcpying it straight into the
    /// RETRO_MEMORY_RTC region after `retro_load_game` (there is no load
    /// callback), so: if the buffer no longer matches what we last synced,
    /// adopt the externally-written state with wall-clock catch-up; then
    /// refresh the buffer so frontend (auto)saves always read current state.
    /// No-op until the frontend has requested the region.
    pub fn rtc_memory_frame_sync(&mut self) {
        if self.rtc_memory.is_empty() || !self.has_rtc() {
            return;
        }
        if self.rtc_memory != self.rtc_memory_synced {
            let external = std::mem::take(&mut self.rtc_memory);
            self.rtc_restore_with_catch_up(&external);
            self.rtc_memory = external; // hand the allocation back (cached ptr)
        }
        self.rtc_memory_refresh();
    }
}

// --- state ---------------------------------------------------------------

/// One MBC3 RTC register bank: the live counters, and (as a second instance)
/// the CPU-visible shadows a $6000-$7FFF write latches them into.
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub(super) struct Mbc3Rtc {
    pub(super) seconds: u8,   // 0-59
    pub(super) minutes: u8,   // 0-59
    pub(super) hours: u8,     // 0-23
    pub(super) days_low: u8,  // Lower 8 bits of day counter
    pub(super) days_high: u8, // Upper 1 bit of day counter + halt flag + day carry
}

/// The HuC-3 RTC MCU's battery-fed state: its 256-nibble internal memory (one
/// nibble per byte) plus the sub-minute accumulator. The live clock is stored
/// in-place: nibbles 0x10-0x12 = minute-of-day counter (rolls at 1440),
/// 0x13-0x15 = 12-bit day counter, little-endian nibbles (Pan Docs "RTC
/// Location Map"). `mem` is empty for non-HuC3 carts.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct HuC3Rtc {
    pub(super) mem: Vec<u8>,
    /// Sub-minute cycle accumulator, master-clock derived like the MBC3 RTC.
    pub(super) accum: u64,
}

