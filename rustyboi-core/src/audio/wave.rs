use serde::{Deserialize, Serialize};
use crate::audio::{NR30, NR31, NR32, NR33, NR34, WAV_START, WAV_END};
use crate::audio::length::COUNTER_DISABLED;
use crate::memory::Addressable;

fn to_period(nr3: u8, nr4: u8) -> u32 {
    0x800 - (((nr4 as u32) << 8 & 0x700) | nr3 as u32)
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Wave {
    nr30: u8, // DAC enable
    nr31: u8, // Sound length
    nr32: u8, // Output level
    nr33: u8, // Period low
    nr34: u8, // Period high and control

    wave_ram: [u8; 16],

    // Length counter (cc-driven absolute expiry).
    enabled: bool,
    length_counter: u16,
    #[serde(default = "crate::audio::length::counter_disabled")]
    len_counter: u32,
    #[serde(default)]
    len_cc: u32,

    // Free-running 2 MHz cycle counter, pushed by the controller. Channel 3's
    // wave fetch is modelled cc-based.
    #[serde(default)]
    cc: u32,

    // Wave fetch timing: `wave_counter` is the cc of the next pending sample
    // fetch, `last_read_time` the cc of the most recent one, and `wave_pos` the
    // current nibble position (0..31 over the 16 wave-RAM bytes).
    #[serde(default = "crate::audio::length::counter_disabled")]
    wave_counter: u32,
    #[serde(default)]
    last_read_time: u32,
    #[serde(default)]
    wave_pos: u8, // 0..31 (16 bytes = 32 nibbles)
    #[serde(default)]
    sample_buf: u8,

    // Channel master enable: DAC on and channel triggered (drives the wave
    // fetch / read gate).
    #[serde(default)]
    master: bool,
    #[serde(default)]
    dac_enabled: bool,

    cgb: bool,
    // AGB ch3 wave-RAM behavior: while playing,
    // wave-RAM reads return 0xFF and writes are dropped unconditionally, and
    // the setNr0 sample-buffer restore is skipped.
    #[serde(default)]
    agb: bool,
    // CGB-B-or-earlier APU revision gate (see `len_nr4_change`).
    #[serde(default)]
    cgb_le_b: bool,
    // CPU-CGB-A/B only (NOT CGB-0): the wave channel swallows the FIRST
    // parity-armed value-irrelevant length-glitch write after APU power-on;
    // subsequent glitch writes clock (SameSuite channel_3_extra_length_-
    // clocking-cgbB: "On CPU CGB B, CH3 requires TWO writes to disable the
    // channel when the length counter is 1", vs ONE on CGB-0).
    #[serde(default)]
    cgb_b: bool,
    #[serde(default)]
    glitch_armed: bool,
}

impl Wave {
    pub(super) fn new() -> Self {
        Wave {
            nr30: 0,
            nr31: 0,
            nr32: 0,
            nr33: 0,
            nr34: 0,
            wave_ram: [0; 16],
            enabled: false,
            length_counter: 0,
            len_counter: COUNTER_DISABLED,
            len_cc: 0,
            cc: 0,
            wave_counter: COUNTER_DISABLED,
            last_read_time: 0,
            wave_pos: 0,
            sample_buf: 0,
            master: false,
            dac_enabled: false,
            cgb: false,
            agb: false,
            cgb_le_b: false,
            cgb_b: false,
            glitch_armed: false,
        }
    }

    pub(super) fn set_cc(&mut self, cc: u32) {
        self.cc = cc;
    }

    /// Shift the last-read and next-fetch cc anchors back by the cc delta
    /// caused by a DIV write.
    pub(super) fn reset_cc(&mut self, delta: u32) {
        self.last_read_time = self.last_read_time.wrapping_sub(delta);
        if self.wave_counter != COUNTER_DISABLED {
            self.wave_counter = self.wave_counter.wrapping_sub(delta);
        }
    }

    /// Master-clock epoch rebase: shift every absolute-cc anchor down by
    /// `delta`. `last_read_time` is shifted even though it is compared for
    /// equality only, to stay consistent with `wave_counter`.
    pub fn epoch_fold(&mut self, delta: u32) {
        self.cc = self.cc.wrapping_sub(delta);
        self.len_cc = self.len_cc.wrapping_sub(delta);
        self.last_read_time = self.last_read_time.wrapping_sub(delta);
        if self.wave_counter != COUNTER_DISABLED {
            self.wave_counter = self.wave_counter.wrapping_sub(delta);
        }
        if self.len_counter != COUNTER_DISABLED {
            self.len_counter = self.len_counter.wrapping_sub(delta);
        }
    }

    /// PSG reset: clears the sample buffer. Length counter / wave RAM are
    /// preserved.
    pub(super) fn psg_reset(&mut self) {
        self.sample_buf = 0;
        // CGB-B first-glitch-write swallow re-arms at APU power-on.
        self.glitch_armed = false;
    }

    /// The channel's NR34 register byte (the length unit's enable/trigger).
    fn nr4(&self) -> u8 {
        self.nr34
    }

    /// Length teardown for channel 3. Unlike channels 1/2/4 this also drops
    /// `master` and disarms the fetch counter, because CH3's DAC is NR30 bit 7
    /// rather than an NRx2 high nibble.
    fn len_disable(&mut self) {
        self.enabled = false;
        self.master = false;
        self.wave_counter = COUNTER_DISABLED;
    }

    /// CPU-CGB-A/B wave-only quirk: the value-irrelevant extra-clock leg
    /// (written bit 6 clear) swallows its FIRST parity-armed write after
    /// power-on; later glitch writes clock normally. CGB-0 clocks on every
    /// parity-armed write (SameSuite ch3 extra_length_clocking -cgb0 vs -cgbB
    /// tables). No other channel forks here.
    fn len_swallow(&mut self, new_nr4: u8, dec: u16) -> u16 {
        if self.cgb_b && new_nr4 & 0x40 == 0 && dec != 0 && !self.glitch_armed {
            self.glitch_armed = true;
            return 0;
        }
        dec
    }

    // The six shared length helpers (set_len_cc, len_expired,
    // set_length_counter, length_event, len_nr1_change, len_nr4_change); see
    // audio/length.rs.
    crate::audio::length::impl_length_unit!(
        mask: 0xFF,
        counter: u16,
        on_disable: len_disable,
        pre_dec: len_swallow,
    );

    fn period(&self) -> u32 {
        // The APU cycle counter advances at `>>(1+ds)` (half-rate at double
        // speed). The wave fetch period `0x800 - freq` is in those same units
        // regardless of speed, so no double-speed scaling.
        to_period(self.nr33, self.nr34)
    }

    /// Advance the wave channel's sample-position up to the current cc. The
    /// channel fetches one nibble-pair every `period` cc; `wave_counter` holds
    /// the cc of the next pending fetch.
    fn update_wave_counter(&mut self) {
        let cc = self.cc;
        if self.wave_counter == COUNTER_DISABLED || cc < self.wave_counter {
            return;
        }
        let period = self.period();
        // The pending fetch at `wave_counter`, plus every whole period elapsed
        // since, each step the 32-entry position once (32 nibble-pairs wrap).
        let elapsed = (cc - self.wave_counter) / period;
        self.wave_pos = ((self.wave_pos as u32 + elapsed + 1) & 31) as u8;
        // Re-anchor: the latest fetch sits `elapsed` periods past the pending
        // one, and the next is scheduled one period beyond that.
        self.last_read_time = self.wave_counter + elapsed * period;
        self.wave_counter = self.last_read_time + period;
        self.sample_buf = self.wave_ram[(self.wave_pos >> 1) as usize];
    }

    /// Seed the AGB flag before the first `step` so an early wave-RAM access
    /// (before the channel has ticked) already sees AGB semantics.
    pub(super) fn set_agb(&mut self, agb: bool) {
        self.agb = agb;
    }

    /// CGB-B-or-earlier APU revision gate.
    pub(super) fn set_cgb_le_b(&mut self, le_b: bool) {
        self.cgb_le_b = le_b;
    }

    /// CPU-CGB-A/B (Hardware::CGBB) wave first-glitch-write swallow.
    pub(super) fn set_cgb_b(&mut self, b: bool) {
        self.cgb_b = b;
    }

    pub(super) fn step(&mut self, cgb: bool, agb: bool) {
        self.cgb = cgb;
        self.agb = agb;
        if self.master {
            self.update_wave_counter();
        }
    }

    /// Advance the wave fetch counter to the current cc for the CPU read path.
    pub(super) fn sync_for_read(&mut self) {
        if self.master {
            self.update_wave_counter();
        }
    }

    fn get_output_level(&self) -> u8 {
        (self.nr32 >> 5) & 0x03
    }

    fn write_nrx4(&mut self, value: u8) {
        let trigger = (value >> 7) & 0x01 != 0;
        // `self.nr34` already carries bit 6 (stored as `value & !0x80`).
        let old_nr4 = self.nr34;

        self.len_nr4_change(old_nr4, value);
        self.nr34 = value & !0x80;

        if trigger {
            self.trigger();
        }
    }

    /// NR34 trigger (DAC-gated). Length reload is handled in `len_nr4_change`
    /// (folded into the length unit).
    fn trigger(&mut self) {
        self.enabled = true;

        if self.dac_enabled {
            // DMG wave-RAM corruption when triggering during an active fetch.
            if self.wave_counter == self.cc.wrapping_add(1) {
                self.sample_buf = self.wave_ram[0];
                if !self.cgb {
                    let pos = (self.wave_pos as usize).div_ceil(2) % 16;
                    if pos < 4 {
                        self.wave_ram[0] = self.wave_ram[pos];
                    } else {
                        let base = pos & !3;
                        let copy = [
                            self.wave_ram[base],
                            self.wave_ram[base + 1],
                            self.wave_ram[base + 2],
                            self.wave_ram[base + 3],
                        ];
                        self.wave_ram[0..4].copy_from_slice(&copy);
                    }
                }
            }
            self.master = true;
            self.wave_pos = 0;
            // The trigger schedules the first fetch one full period plus the
            // fixed 3-cc trigger latency out from the current cc, in APU cc
            // units with no double-speed term (the unified APU cc already
            // carries the speed via its `>>(1+ds)` rate).
            self.wave_counter = self.cc + self.period() + 3;
            self.last_read_time = self.wave_counter;
        } else {
            self.enabled = false;
            self.master = false;
            self.wave_counter = COUNTER_DISABLED;
        }
    }

    /// NR30 write: DAC enable/disable with the sample-buffer latch.
    fn write_nr0(&mut self, value: u8) {
        let new_nr0 = value & 0x80;
        self.nr30 = new_nr0;
        self.dac_enabled = new_nr0 != 0;
        if new_nr0 == 0 {
            // On DAC-disable while playing, AGB silicon skips the sample-buffer
            // restore (`!agb && master`).
            if !self.agb && self.master {
                if self.wave_counter == self.cc.wrapping_add(1) {
                    self.sample_buf = self.wave_ram[0];
                } else if !self.cgb && self.last_read_time == self.cc {
                    self.sample_buf = self.wave_ram[0xA];
                }
            }
            self.enabled = false;
            self.master = false;
            self.wave_counter = COUNTER_DISABLED;
        }
    }

    /// Whether the channel's DAC is powered. CH3 is the exception to the
    /// `NRx2 & $F8` rule: its DAC is bit 7 of NR30 (Pan Docs, DACs).
    pub(super) fn dac_on(&self) -> bool {
        self.dac_enabled
    }

    /// The channel's analog output, taken from the LATCHED `sample_buf` by way
    /// of [`Wave::pcm_nibble`] — not from a live wave-RAM read. Pan Docs:
    /// "CH3 does not emit samples directly, but stores every sample read into a
    /// buffer, and emits that continuously; (re)triggering the channel does not
    /// clear nor refresh this buffer, so the last sample ever read will be
    /// emitted again." The audible path and the CGB-observable PCM34 path are
    /// therefore the same latch, which is what hardware has.
    pub(super) fn get_output(&self) -> f32 {
        if !self.dac_on() {
            return 0.0;
        }
        crate::audio::analog::dac_analog(self.pcm_nibble())
    }

    pub(super) fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// CGB PCM34 low nibble for the wave channel: while the channel master is
    /// on, the selected nibble (`pos` even -> high nibble, odd -> low nibble) is
    /// right-shifted by the output-level attenuation, where the shift is
    /// `min((nr32>>5 & 3) - 1, 4)` so output level 0 mutes (shift past the data).
    ///
    /// The sample comes from the LATCHED `sample_buf` (not a live wave-RAM read):
    /// after a fresh trigger `wave_pos=0` and `sample_buf` still holds its old /
    /// power-on-zeroed value until the first fetch (`update_wave_counter`) at
    /// `wave_counter`, so the very first samples read 0.
    pub(super) fn pcm_nibble(&self) -> u8 {
        if !self.master {
            return 0;
        }
        let sample = if self.wave_pos.is_multiple_of(2) {
            (self.sample_buf >> 4) & 0x0F
        } else {
            self.sample_buf & 0x0F
        };
        let output_level = self.get_output_level();
        if output_level == 0 {
            0
        } else {
            (sample >> (output_level - 1)) & 0x0F
        }
    }

    /// Wave-RAM read, evaluated at the exact read cc.
    pub(super) fn read_wave_ram(&self, addr: u16) -> u8 {
        let mut index = (addr - WAV_START) as usize;
        if index >= 16 {
            return 0xFF;
        }
        if self.master {
            // Wave-RAM read while playing: AGB returns 0xFF unconditionally;
            // CGB allows only the just-accessed byte; DMG only when the read
            // coincides with the channel's own fetch cc.
            if self.agb || (!self.cgb && self.cc != self.last_read_time) {
                return 0xFF;
            }
            index = (self.wave_pos / 2) as usize;
        }
        self.wave_ram[index]
    }

    /// Wave-RAM write.
    pub(super) fn write_wave_ram(&mut self, addr: u16, value: u8) {
        let mut index = (addr - WAV_START) as usize;
        if index >= 16 {
            return;
        }
        if self.master {
            // Wave-RAM write while playing: AGB drops it unconditionally
            // (mirrors the read rule above).
            if self.agb || (!self.cgb && self.cc != self.last_read_time) {
                return;
            }
            index = (self.wave_pos / 2) as usize;
        }
        self.wave_ram[index] = value;
    }
}

impl Addressable for Wave {
    fn read(&self, addr: u16) -> u8 {
        match addr {
            NR30..=NR34 => match addr {
                NR30 => self.nr30 | 0x7F,
                NR31 => 0xFF,
                NR32 => self.nr32 | 0x9F,
                NR33 => 0xFF,
                NR34 => self.nr34 | 0xBF,
                _ => 0xFF,
            },
            WAV_START..=WAV_END => self.read_wave_ram(addr),
            _ => panic!("Invalid address for Wave: {:#X}", addr),
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            NR30..=NR34 => match addr {
                NR30 => self.write_nr0(value),
                NR31 => {
                    self.nr31 = value;
                    self.len_nr1_change(value);
                }
                NR32 => self.nr32 = value,
                NR33 => self.nr33 = value,
                NR34 => self.write_nrx4(value),
                _ => {}
            },
            WAV_START..=WAV_END => self.write_wave_ram(addr, value),
            _ => panic!("Invalid address for Wave: {:#X}", addr),
        }
    }
}
