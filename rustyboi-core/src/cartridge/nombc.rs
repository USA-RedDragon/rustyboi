//! NOMBC board: register state + address->bank math.

use super::mapper::{Banking, Geom};
use serde::{Deserialize, Serialize};

// --- board struct + banking ---------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct NoMbc {
    pub battery: bool,
}

impl Banking for NoMbc {
    fn rom_bankn(&self, _g: Geom) -> usize {
        1 // bankless cart always maps bank 1 to the upper area
    }
    fn rom_bank0(&self, _g: Geom) -> usize {
        0
    }
    fn ram_bank(&self, _g: Geom) -> usize {
        0
    }
}

