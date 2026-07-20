//! Game Boy Printer output: grouping freshly-drained strips into finished
//! photos and stitching each photo's strips into one tall sheet.

use super::Session;
use rustyboi_core_lib::printer::PrintSheet;

impl Session {
    /// Drain any Game Boy Printer photos finished since the last call, each as
    /// one tall [`PrintSheet`] with its strips stitched vertically (one long
    /// sheet of paper). A photo is emitted when a strip carries a paper-feed
    /// margin: a non-zero *before*-feed on a fresh strip closes the previous
    /// photo, and a non-zero *after*-feed ends the current one. Strips with no
    /// feed keep accumulating (a multi-band print is not ejected mid-image).
    ///
    /// Replaces draining `GB::take_printer_sheets` directly, so every frontend
    /// (which polls this each frame) saves one image per photo instead of one
    /// file/download per band.
    pub fn take_prints(&mut self) -> Vec<PrintSheet> {
        let strips = self.gb.take_printer_sheets();
        self.accumulate_prints(strips)
    }

    /// Group freshly-drained printer strips into finished photos by the
    /// paper-feed margins (see [`take_prints`](Self::take_prints)). Split out for
    /// unit testing without driving a print ROM.
    pub(super) fn accumulate_prints(&mut self, strips: Vec<PrintSheet>) -> Vec<PrintSheet> {
        let scale = self.config.printer_scale;
        let mut finished = Vec::new();
        let mut eject = |strips: Vec<PrintSheet>| {
            finished.push(scale_sheet(stitch_prints(strips), scale));
        };
        for strip in strips {
            // margins byte: high nibble = feed before, low nibble = feed after.
            let feed_before = strip.margins >> 4 != 0;
            let feed_after = strip.margins & 0x0F != 0;
            if feed_before && !self.printer_strips.is_empty() {
                eject(std::mem::take(&mut self.printer_strips));
            }
            self.printer_strips.push(strip);
            if feed_after {
                eject(std::mem::take(&mut self.printer_strips));
            }
        }
        finished
    }
}

/// Stitch a photo's strips (all `PRINT_WIDTH` wide) into one tall sheet by
/// vertically concatenating their shade rows. Metadata comes from the last strip
/// (the one that carried the ejecting feed). `strips` is always non-empty.
fn stitch_prints(strips: Vec<PrintSheet>) -> PrintSheet {
    if strips.len() == 1 {
        return strips.into_iter().next().unwrap();
    }
    let width = strips[0].width;
    let height: u32 = strips.iter().map(|s| s.height).sum();
    let mut shades = Vec::with_capacity(strips.iter().map(|s| s.shades.len()).sum());
    for s in &strips {
        shades.extend_from_slice(&s.shades);
    }
    let last = strips.last().unwrap();
    PrintSheet {
        width,
        height,
        shades,
        sheets: last.sheets,
        margins: last.margins,
        palette: last.palette,
        exposure: last.exposure,
    }
}

/// Nearest-neighbour integer upscale of a print sheet by `scale` (≥1). The
/// native printer image is 160px wide — tiny on a modern screen — so frontends
/// save/download this enlarged copy. `scale == 1` returns the sheet unchanged.
fn scale_sheet(sheet: PrintSheet, scale: u8) -> PrintSheet {
    let scale = scale.max(1) as u32;
    if scale == 1 {
        return sheet;
    }
    let (w, h) = (sheet.width, sheet.height);
    let (nw, nh) = (w * scale, h * scale);
    let mut shades = Vec::with_capacity((nw * nh) as usize);
    for ny in 0..nh {
        let row_start = (ny / scale * w) as usize;
        let row = &sheet.shades[row_start..row_start + w as usize];
        for nx in 0..nw {
            shades.push(row[(nx / scale) as usize]);
        }
    }
    PrintSheet {
        width: nw,
        height: nh,
        shades,
        sheets: sheet.sheets,
        margins: sheet.margins,
        palette: sheet.palette,
        exposure: sheet.exposure,
    }
}
