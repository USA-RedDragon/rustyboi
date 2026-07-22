//! Game Boy Camera: the M64282FP sensor + MAC-GBD image pipeline, the capture
//! busy-window timing, and the CAM register file.

use super::*;
use super::mapper::{Banking, Geom};
use serde::{Deserialize, Serialize};

impl Cartridge {
    /// True for POCKET CAMERA carts (MAC-GBD + M64282FP sensor). Frontends
    /// use this to know when `set_camera_image` is meaningful; the bus uses
    /// it to gate the capture-countdown tick.
    pub fn has_camera(&self) -> bool {
        matches!(self.get_cartridge_type(), CartridgeType::PocketCamera)
    }
    /// Feed the live sensor image: 128x112 8-bit grayscale, row-major
    /// (`pixels[y * 128 + x]`), 0 = black. This is the frontend integration
    /// point for a webcam/still source; without it captures use a built-in
    /// deterministic test pattern. No-op outside camera carts' effect (the
    /// buffer is simply never consumed).
    pub fn set_camera_image(&mut self, pixels: &[u8; CAM_W * CAM_H]) {
        if self.cam_image.len() != CAM_W * CAM_H {
            self.cam_image = vec![0; CAM_W * CAM_H];
        }
        self.cam_image.copy_from_slice(pixels);
    }
    /// Built-in deterministic sensor input: a diagonal luminance gradient
    /// with a dark disc, a bright disc and a mid-gray border frame, spanning
    /// the full 0-255 range so all four GB shades appear after dithering.
    pub(super) fn cam_builtin_pattern() -> Vec<u8> {
        let mut img = vec![0u8; CAM_W * CAM_H];
        for y in 0..CAM_H {
            for x in 0..CAM_W {
                let mut v = ((x * 255) / (CAM_W - 1) + (y * 255) / (CAM_H - 1)) / 2;
                // Dark disc, upper-left quadrant.
                let (dx, dy) = (x as i32 - 40, y as i32 - 40);
                if dx * dx + dy * dy < 24 * 24 {
                    v = 24;
                }
                // Bright disc, lower-right quadrant.
                let (dx, dy) = (x as i32 - 92, y as i32 - 76);
                if dx * dx + dy * dy < 20 * 20 {
                    v = 232;
                }
                // Mid-gray frame border.
                if !(4..CAM_W - 4).contains(&x) || !(4..CAM_H - 4).contains(&y) {
                    v = 128;
                }
                img[y * CAM_W + x] = v as u8;
            }
        }
        img
    }
    /// Write to the CAM register file (index = addr & 0x7F).
    pub(super) fn cam_reg_write(&mut self, idx: u16, value: u8) {
        let mut start = false;
        if let Mapper::Camera(m) = &mut self.mapper {
            if idx == 0 {
                // Only the low 3 bits are wired.
                m.state.regs[0] = value & 0x07;
                if value & 0x01 != 0 {
                    if !m.state.running {
                        if m.state.clocks_left > 0 {
                            // Restart after a mid-capture stop: "it will continue
                            // the previous capture process with the old capture
                            // parameters, even if the registers are changed in
                            // between" -- cam_pending was already processed with
                            // the trigger-time parameters.
                            m.state.running = true;
                        } else {
                            start = true;
                        }
                    }
                } else if m.state.running {
                    // Stop the capture; RAM is readable again. The countdown
                    // freezes so a later '1' write resumes it.
                    m.state.running = false;
                }
            } else if (idx as usize) < CAM_REG_COUNT {
                m.state.regs[idx as usize] = value;
            }
            // A036-A07F: unmapped, writes ignored.
        }
        if start {
            self.cam_start_capture();
        }
    }
    /// Start a capture: compute the busy window and process the sensor
    /// image. The result is committed to RAM when the countdown expires (the
    /// real controller streams pixels into RAM during the sensor read period
    /// at the END of the window; committing at expiry keeps the previous
    /// image visible if the capture is stopped early, as documented).
    pub(super) fn cam_start_capture(&mut self) {
        let (n_bit, exposure) = match &self.mapper {
            Mapper::Camera(m) => {
                (m.state.regs[1] & 0x80 != 0, ((m.state.regs[2] as u64) << 8) | m.state.regs[3] as u64)
            }
            _ => return,
        };
        // Pan Docs: M-cycles(1MiHz) = 32446 + (N ? 0 : 512) + 16 * exposure.
        // Stored in master-clock T-cycles (x4); cam_tick halves the window
        // in CGB double-speed mode where PHI runs twice as fast.
        let clocks_left = 4 * (32446 + if n_bit { 0 } else { 512 } + 16 * exposure);
        let pending = self.cam_process_image();
        if let Mapper::Camera(m) = &mut self.mapper {
            m.state.clocks_left = clocks_left;
            m.state.running = true;
            m.state.pending = pending;
        }
    }
    /// Advance the capture countdown by `phi_quarters` PHI/4 units (master
    /// dots at single speed; the caller doubles the span in CGB double-speed
    /// mode, where the PHI cartridge clock runs at 2.097152 MHz). No-op
    /// unless a capture is actively running.
    pub(crate) fn cam_tick(&mut self, phi_quarters: u64) {
        // Advance the countdown against the live camera state, returning the
        // finished tile block (if the capture just expired) so the RAM/save
        // commit below borrows `ram_data`/`save_file` without the mapper held.
        let pending = {
            let Mapper::Camera(m) = &mut self.mapper else {
                return;
            };
            if !m.state.running || phi_quarters == 0 {
                return;
            }
            if m.state.clocks_left > phi_quarters {
                m.state.clocks_left -= phi_quarters;
                return;
            }
            // Capture finished: the controller has streamed the processed tile
            // data into RAM bank 0 at $0100 and the busy flag clears.
            m.state.clocks_left = 0;
            m.state.running = false;
            std::mem::take(&mut m.state.pending)
        };
        if self.ram_data.len() >= CAM_RAM_IMAGE_OFFSET + CAM_TILE_BYTES
            && pending.len() == CAM_TILE_BYTES
        {
            self.ram_data[CAM_RAM_IMAGE_OFFSET..CAM_RAM_IMAGE_OFFSET + CAM_TILE_BYTES]
                .copy_from_slice(&pending);
            // Stream the block to the battery .sav (single bulk write, not
            // 3584 per-byte writes).
            if let Some(file) = &mut self.save_file {
                let _ = file
                    .seek(SeekFrom::Start(CAM_RAM_IMAGE_OFFSET as u64))
                    .and_then(|_| file.write_all(&pending))
                    .and_then(|_| file.flush());
            }
        }
    }
    /// The M64282FP sensor + MAC-GBD controller pipeline, following the
    /// image-processing model documented in Pan Docs "Game Boy Camera" as its
    /// "Sample code for emulators", in exact-integer form: exposure
    /// scaling, optional inversion, the documented 3x3 edge kernels / 1-D
    /// filtering selected by N/VH/E3 and the A000 P/M bits, then the 4x4x3
    /// dither/contrast matrix, packed as GB 2bpp tiles (16x14 tiles, the
    /// layout the ROM expects at RAM bank 0 offset $0100).
    pub(super) fn cam_process_image(&self) -> Vec<u8> {
        let regs = match &self.mapper {
            Mapper::Camera(m) => &m.state.regs,
            _ => return Vec::new(),
        };
        // --- Sensor input: 128x120 window (112 visible + 4 padding rows
        // top/bottom standing in for the sensor's discarded edge rows).
        let builtin;
        let input: &[u8] = if self.cam_image.len() == CAM_W * CAM_H {
            &self.cam_image
        } else {
            builtin = Self::cam_builtin_pattern();
            &builtin
        };
        let src_row = |k: usize| {
            let y = (k as i32 - (CAM_SENSOR_EXTRA_LINES / 2) as i32)
                .clamp(0, CAM_H as i32 - 1) as usize;
            &input[y * CAM_W..(y + 1) * CAM_W]
        };

        // --- Configuration (registers latched at trigger time).
        // A000 bits 1-2 select the 1-D filter P/M sets (doc v1.1.1 §3.1.3).
        let (p_bits, m_bits) = match (regs[0] >> 1) & 3 {
            0 => (0x00u32, 0x01u32),
            1 => (0x01, 0x00),
            _ => (0x01, 0x02),
        };
        let n_bit = (regs[1] >> 7) as u32;
        let vh_bits = ((regs[1] >> 5) & 3) as u32;
        let exposure = ((regs[2] as i32) << 8) | regs[3] as i32;
        let e3_bit = (regs[4] >> 7) as u32;
        let i_bit = regs[4] & 0x08 != 0;
        // Edge enhancement ratio in quarters: 0.50,0.75,1.00,1.25,2,3,4,5.
        let alpha4 = [2i32, 3, 4, 5, 8, 12, 16, 20][((regs[4] >> 4) & 7) as usize];
        // alpha-scaled add in the documented sample's exact float->int form:
        // trunc(px + diff*alpha) == trunc((4*px + diff*alpha4) / 4).
        let edge = |px: i32, diff: i32| (px * 4 + diff * alpha4) / 4;

        // --- Analog stage: exposure scaling + level squash (the documented
        // sample's approximation of the sensor's gain/level control against
        // the ROM's ~$80-centered dither thresholds), optional inversion,
        // then signed representation for the edge kernels. Column-major
        // (x * CAM_SENSOR_H + y), matching the documented buffer layout.
        let h = CAM_SENSOR_H;
        let w = CAM_W;
        let at = |i: usize, j: usize| i * h + j;
        let mut buf = vec![0i32; w * h];
        for i in 0..w {
            for j in 0..h {
                let mut v = src_row(j)[i] as i32;
                v = v * exposure / 0x0300;
                v = 128 + (v - 128) / 8;
                v = v.clamp(0, 255);
                if i_bit {
                    v = 255 - v;
                }
                buf[at(i, j)] = v - 128;
            }
        }

        // 1-D filtering: vout = P/M-selected sum of the pixel and its south
        // neighbor (the sensor streams line pairs through the 1-D kernel).
        let one_d = |src: &[i32], dst: &mut [i32]| {
            for i in 0..w {
                for j in 0..h {
                    let px = src[at(i, j)];
                    let ms = src[at(i, (j + 1).min(h - 1))];
                    let mut value = 0;
                    if p_bits & 1 != 0 {
                        value += px;
                    }
                    if p_bits & 2 != 0 {
                        value += ms;
                    }
                    if m_bits & 1 != 0 {
                        value -= px;
                    }
                    if m_bits & 2 != 0 {
                        value -= ms;
                    }
                    dst[at(i, j)] = value.clamp(-128, 127);
                }
            }
        };

        let filtering_mode = (n_bit << 3) | (vh_bits << 1) | e3_bit;
        match filtering_mode {
            0x0 => {
                // Positive/negative image: plain 1-D filtering.
                let src = buf.clone();
                one_d(&src, &mut buf);
            }
            0x2 => {
                // Horizontal enhancement (P + {2P-(MW+ME)}*alpha), then 1-D.
                let mut temp = vec![0i32; w * h];
                for i in 0..w {
                    for j in 0..h {
                        let mw = buf[at(i.saturating_sub(1), j)];
                        let me = buf[at((i + 1).min(w - 1), j)];
                        let px = buf[at(i, j)];
                        temp[at(i, j)] = edge(px, 2 * px - mw - me).clamp(0, 255);
                    }
                }
                one_d(&temp, &mut buf);
            }
            0xE => {
                // 2D enhancement (P + {4P-(MN+MS+ME+MW)}*alpha). This is the
                // mode the GB Camera ROM shoots with (A001 = $E0|gain).
                let mut temp = vec![0i32; w * h];
                for i in 0..w {
                    for j in 0..h {
                        let ms = buf[at(i, (j + 1).min(h - 1))];
                        let mn = buf[at(i, j.saturating_sub(1))];
                        let mw = buf[at(i.saturating_sub(1), j)];
                        let me = buf[at((i + 1).min(w - 1), j)];
                        let px = buf[at(i, j)];
                        temp[at(i, j)] = edge(px, 4 * px - mw - me - mn - ms).clamp(-128, 127);
                    }
                }
                buf = temp;
            }
            0x1 => {
                // AntonioND: real cartridges output a constant color in this
                // configuration (likely a sensor bug); model as flat 0.
                buf.fill(0);
            }
            0x3 => {
                // Horizontal extraction ({2P-(MW+ME)}*alpha), then 1-D
                // (doc v1.1.1 Table 1; unused by the GB Camera ROM).
                let mut temp = vec![0i32; w * h];
                for i in 0..w {
                    for j in 0..h {
                        let mw = buf[at(i.saturating_sub(1), j)];
                        let me = buf[at((i + 1).min(w - 1), j)];
                        let px = buf[at(i, j)];
                        temp[at(i, j)] = edge(0, 2 * px - mw - me).clamp(0, 255);
                    }
                }
                one_d(&temp, &mut buf);
            }
            0xC | 0xD => {
                // Vertical enhancement / extraction (Table 1, no 1-D).
                let mut temp = vec![0i32; w * h];
                for i in 0..w {
                    for j in 0..h {
                        let ms = buf[at(i, (j + 1).min(h - 1))];
                        let mn = buf[at(i, j.saturating_sub(1))];
                        let px = buf[at(i, j)];
                        let base = if filtering_mode == 0xC { px } else { 0 };
                        temp[at(i, j)] = edge(base, 2 * px - mn - ms).clamp(-128, 127);
                    }
                }
                buf = temp;
            }
            0xF => {
                // 2D extraction ({4P-(MN+MS+ME+MW)}*alpha, Table 1).
                let mut temp = vec![0i32; w * h];
                for i in 0..w {
                    for j in 0..h {
                        let ms = buf[at(i, (j + 1).min(h - 1))];
                        let mn = buf[at(i, j.saturating_sub(1))];
                        let mw = buf[at(i.saturating_sub(1), j)];
                        let me = buf[at((i + 1).min(w - 1), j)];
                        let px = buf[at(i, j)];
                        temp[at(i, j)] = edge(0, 4 * px - mw - me - mn - ms).clamp(-128, 127);
                    }
                }
                buf = temp;
            }
            _ => {
                // Undefined combination: no filtering.
            }
        }

        // --- Controller stage: back to unsigned, 4x4x3 threshold matrix
        // (contrast + dithering), then GB 2bpp tile packing.
        let mut tiles = vec![0u8; CAM_TILE_BYTES];
        for j in 0..CAM_H {
            for i in 0..CAM_W {
                let value = (buf[at(i, j + CAM_SENSOR_EXTRA_LINES / 2)] + 128).clamp(0, 255);
                let base = 6 + ((j & 3) * 4 + (i & 3)) * 3;
                // sensor < DxyL -> black; < DxyM -> dark gray; < DxyH ->
                // light gray; else white (shades as 2bpp color numbers).
                let color: u8 = if value < regs[base] as i32 {
                    3
                } else if value < regs[base + 1] as i32 {
                    2
                } else if value < regs[base + 2] as i32 {
                    1
                } else {
                    0
                };
                // 16 tiles per row, 16 bytes per tile, MSB = leftmost pixel.
                let tile_base = ((j >> 3) * 16 + (i >> 3)) * 16 + (j & 7) * 2;
                let bit = 7 - (i & 7);
                tiles[tile_base] |= (color & 1) << bit;
                tiles[tile_base + 1] |= ((color >> 1) & 1) << bit;
            }
        }
        tiles
    }
}

// --- board struct + banking ---------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Camera {
    pub ram_enabled: bool,
    pub state: CameraState,
}

impl Banking for Camera {
    fn rom_bankn(&self, g: Geom) -> usize {
        (self.state.rom_bank as usize) % g.rom_banks
    }
    fn rom_bank0(&self, _g: Geom) -> usize {
        0
    }
    fn ram_bank(&self, g: Geom) -> usize {
        (self.state.ram_bank as usize) % g.ram_banks.max(1)
    }
}


// --- state ---------------------------------------------------------------

// POCKET CAMERA geometry/constants (Pan Docs "Game Boy Camera" /
// AntonioND gbcam-rev-engineer doc v1.1.1).
// The CAM register file: A000 trigger/status, A001-A005 M64282FP sensor
// parameters, A006-A035 the 4x4x3 dither/contrast matrix. 54 bytes total,
// mirrored every $80 across A000-BFFF while selected.
pub(super) const CAM_REG_COUNT: usize = 0x36;

// Visible capture output: 128x112 pixels, 2bpp GB tiles (16x14 tiles x 16
// bytes) written by the controller to RAM bank 0 at offset $0100.
pub(super) const CAM_W: usize = 128;

pub(super) const CAM_H: usize = 112;

pub(super) const CAM_TILE_BYTES: usize = (CAM_W / 8) * (CAM_H / 8) * 16;
pub(super) const CAM_RAM_IMAGE_OFFSET: usize = 0x0100;

// The sensor array is 128x123; the controller discards the corrupt top and
// bottom rows and uses the middle 112 of a 120-row window (Pan Docs
// "Game Boy Camera": the discarded extra sensor edge lines).
pub(super) const CAM_SENSOR_EXTRA_LINES: usize = 8;

pub(super) const CAM_SENSOR_H: usize = CAM_H + CAM_SENSOR_EXTRA_LINES;
fn serde_cam_regs() -> Vec<u8> {
    vec![0; CAM_REG_COUNT]
}

/// POCKET CAMERA (MAC-GBD + M64282FP) state.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct CameraState {
    /// 6-bit ROM bank register; bank 0 is selectable at 4000-7FFF (AntonioND:
    /// "This area may contain any ROM bank (0 included)"). Initial bank 1.
    pub(super) rom_bank: u8,
    /// 4-bit RAM bank register (banks 0-$0F of the 128KB RAM).
    pub(super) ram_bank: u8,
    /// Bit 4 of the 4000-5FFF write maps the CAM register file over A000-BFFF
    /// instead of RAM (the ROM uses bank $10).
    pub(super) regs_selected: bool,
    /// The 54-byte register file. Write-only except index 0 (trigger/status).
    pub(super) regs: Vec<u8>,
    /// Remaining master-clock T-cycles of the in-flight (or stopped) capture.
    pub(super) clocks_left: u64,
    /// Capture actively running (A000 bit 0 reads 1). Cleared by writing bit
    /// 0 = 0 mid-capture (stop) and when the countdown expires.
    pub(super) running: bool,
    /// Fully processed tile data of the in-flight capture, committed to RAM
    /// bank 0 at $0100 when the countdown expires (the real controller streams
    /// pixels into RAM during the sensor read period at the end of the window;
    /// until then RAM keeps the previous image).
    pub(super) pending: Vec<u8>,
}

impl Default for CameraState {
    fn default() -> Self {
        Self {
            rom_bank: 1,
            ram_bank: 0,
            regs_selected: false,
            regs: serde_cam_regs(),
            clocks_left: 0,
            running: false,
            pending: Vec::new(),
        }
    }
}

