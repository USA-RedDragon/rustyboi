//! On-disk cache for the libretro No-Intro DAT files (game-name index).
//!
//! The No-Intro DATs are CC-BY-SA-4.0 libretro-database material and are never
//! embedded in the binary; the session asks each frontend to download them at
//! runtime (see [`Session::no_intro_fetch_urls`]). To avoid re-downloading on
//! every launch, the desktop caches each fetched DAT here (under the platform
//! data dir) and loads from the cache first, fetching only what's missing.
//!
//! [`Session::no_intro_fetch_urls`]: rustyboi_session::session::Session::no_intro_fetch_urls

use std::path::{Path, PathBuf};

/// The directory the cached DAT bodies live under (`<data-dir>/no_intro`).
fn dir(base: &Path) -> PathBuf {
    base.join("no_intro")
}

/// The cache filename for a DAT URL: its last path segment (kept URL-encoded, e.g.
/// `Nintendo%20-%20Game%20Boy.dat`). Deterministic from the URL, so a later
/// startup finds the same file the previous fetch wrote.
fn cache_filename(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

/// Persist a downloaded DAT `body` for `url`. Best-effort: a write failure just
/// means we re-download next launch.
pub fn store(base: &Path, url: &str, body: &str) {
    let d = dir(base);
    if std::fs::create_dir_all(&d).is_ok() {
        let _ = std::fs::write(d.join(cache_filename(url)), body);
    }
}

/// Split `urls` into `(cached bodies, urls still needing a download)`. A URL whose
/// cache file exists and reads back is served from disk; the rest are returned for
/// the caller to fetch (and later [`store`]).
pub fn split_cached(base: &Path, urls: &[String]) -> (Vec<String>, Vec<String>) {
    let d = dir(base);
    let mut cached = Vec::new();
    let mut missing = Vec::new();
    for url in urls {
        match std::fs::read_to_string(d.join(cache_filename(url))) {
            Ok(body) if !body.is_empty() => cached.push(body),
            _ => missing.push(url.clone()),
        }
    }
    (cached, missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_by_url_segment() {
        let base =
            std::env::temp_dir().join(format!("rustyboi_nointro_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let url = "https://example/metadat/no-intro/Nintendo%20-%20Game%20Boy.dat";
        let other = "https://example/metadat/no-intro/Nintendo%20-%20Game%20Boy%20Color.dat";

        // Nothing cached yet: both are missing.
        let (cached, missing) = split_cached(&base, &[url.to_string(), other.to_string()]);
        assert!(cached.is_empty());
        assert_eq!(missing.len(), 2);

        store(&base, url, "BODY-A");
        let (cached, missing) = split_cached(&base, &[url.to_string(), other.to_string()]);
        assert_eq!(cached, vec!["BODY-A".to_string()]);
        assert_eq!(missing, vec![other.to_string()]);

        let _ = std::fs::remove_dir_all(&base);
    }
}
