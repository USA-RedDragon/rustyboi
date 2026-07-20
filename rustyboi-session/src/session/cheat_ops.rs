//! Cheat codes and the two runtime-fetched databases (libretro cheat DB and
//! the No-Intro game-name index).

use super::{log_no_intro_attribution, Session};
use crate::cheats::{Cheat, CheatError};

impl Session {
    /// Add a Game Genie / GameShark code. Game Genie codes patch the ROM
    /// immediately; GameShark codes take effect on the next frame.
    pub fn add_cheat(&mut self, code: &str) -> Result<Cheat, CheatError> {
        let cheat = self.cheats.add(code)?;
        if matches!(cheat, Cheat::GameGenie { .. }) {
            self.cheats.apply_rom_patches(&mut self.gb);
        }
        Ok(cheat)
    }

    /// Remove a cheat by its raw code string. Game Genie removal takes effect on
    /// the next ROM (re)load (an applied ROM patch cannot be reverted in place).
    pub(crate) fn remove_cheat(&mut self, code: &str) -> bool {
        self.cheats.remove(code)
    }

    /// Remove all cheats (e.g. libretro's `retro_cheat_reset`). Like
    /// [`remove_cheat`](Self::remove_cheat), already-applied Game Genie ROM
    /// patches are only undone on the next ROM (re)load.
    pub fn clear_cheats(&mut self) {
        self.cheats.clear();
    }

    /// The active cheat codes.
    pub fn cheats(&self) -> impl Iterator<Item = &str> {
        self.cheats.codes()
    }

    /// The raw bytes of the currently-loaded ROM (unpatched), or `None` if no
    /// ROM has been loaded from bytes. Used to identify the game for the cheat-DB
    /// fetch.
    pub(crate) fn original_rom_bytes(&self) -> Option<&[u8]> {
        self.original_rom.as_deref()
    }

    /// The candidate libretro-cheat-DB URLs for the loaded game, or `None` if no
    /// ROM is loaded or the ROM isn't in the No-Intro index. The frontend fetches
    /// these (in order) and feeds the body to [`finish_fetched_cheats`].
    pub(crate) fn cheat_fetch_urls(&self) -> Option<Vec<String>> {
        let rom = self.original_rom.as_deref()?;
        let name = crate::no_intro::identify(rom)?;
        Some(crate::cheat_db::candidate_urls(&name, crate::cheat_db::is_cgb(rom)))
    }

    /// The two libretro No-Intro DAT URLs to download for offline game
    /// identification. Static (ROM-independent): the frontend fetches these once
    /// (caching the bodies) and feeds them to [`finish_no_intro_dats`]. The
    /// session performs no HTTP itself. The downloaded data is CC-BY-SA-4.0
    /// libretro-database material — not embedded in any binary — so callers log
    /// the attribution at download time.
    pub fn no_intro_fetch_urls(&self) -> Vec<String> {
        log_no_intro_attribution();
        crate::no_intro::dat_urls()
    }

    /// Feed downloaded No-Intro DAT bodies into the runtime identification index
    /// (merging with any already loaded), then re-resolve the current ROM's
    /// display name now that identification may succeed. Bodies may be supplied
    /// incrementally (one per DAT) or together.
    pub fn finish_no_intro_dats(&mut self, bodies: &[String]) {
        crate::no_intro::load_dats(bodies);
        if let Some(rom) = self.original_rom.as_deref() {
            self.game_name = crate::no_intro::resolve_game_name(rom);
        }
    }

    /// Parse a downloaded libretro `.cht` body into the pending fetched-cheat
    /// list (replacing any previous fetch). Returns the number of cheats parsed.
    /// The frontend then shows them for the user to pick; selected codes are
    /// added through the normal [`add_cheat`](Self::add_cheat) path.
    pub fn finish_fetched_cheats(&mut self, body: &str) -> usize {
        self.fetched_cheats = crate::cheat_db::parse_cht(body);
        self.fetched_cheats.len()
    }

    /// The cheats fetched from the libretro DB awaiting the user's selection.
    pub fn fetched_cheats(&self) -> &[crate::cheat_db::FetchedCheat] {
        &self.fetched_cheats
    }

    /// Discard the pending fetched-cheat list (user closed the picker).
    pub(crate) fn clear_fetched_cheats(&mut self) {
        self.fetched_cheats.clear();
    }
}
