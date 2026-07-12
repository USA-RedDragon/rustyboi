//! Library-wide regression + performance sweep.
//!
//! Runs every ROM in a game library under a deterministic input masher,
//! recording per-ROM performance, a cumulative every-frame framebuffer hash,
//! checkpoint hashes, and a poster screenshot (plus, when `ffmpeg` is present, a
//! whole-run video the gallery plays). Two sweeps compare into a regression
//! report: any hash difference is a behavior change (the library is ~1200
//! canaries wide), any fps drop beyond tolerance is a perf regression.
//!
//! MEDIA (privacy + hardware matrix): every emitted media file is keyed on the
//! ROM's `rom_sha`, never the (trademarked) file name — `screens/<sha>_<hw>.webp`
//! (one poster) and `videos/<sha>_<hw>.mp4`. Media is captured on a
//! SET of hardware models (default DMG,CGB,SGB,AGB via `--hardware`) so the
//! gallery can tab between them; a CGB-only game on DMG legitimately renders the
//! "cannot run on this system" panel — a real artifact, not an error. The media
//! fan-out is gallery-only: the manifest that `compare` gates on carries exactly
//! ONE canonical row per ROM (auto-detected hardware, no audio drain), produced
//! by a code path byte-identical to a media-free run, so extra hardware/audio
//! never perturb the regression hashes. When `ffmpeg` is on PATH and screenshots
//! are enabled, each (ROM,hardware) also streams every frame to an HEVC encoder
//! with the emulated APU audio muxed in as AAC (`--no-videos` to disable).
//!
//!   sweep run      --roms DIR... [--list FILE] --out DIR [--frames N]
//!                  [--warmup N] [--jobs N] [--timeout SECS] [--no-screens]
//!                  [--strip-names] [--only SUBSTR] [--shard K/N]
//!       Sweep the library into DIR/manifest.jsonl + DIR/screens/*.webp.
//!       No-Intro names are always on: each ROM is labeled by its canonical
//!       No-Intro title (matched via CRC32 of the extracted ROM against the
//!       public GB/GBC DATs, auto-fetched+cached; see nointro_map). Unmatched
//!       ROMs (unlicensed, hacks, bad dumps) keep their file stem. The manifest
//!       also stores each ROM's crc so `gallery` can (re)apply names later.
//!       --strip-names produces the committable baseline: omits ROM
//!       title/path/crc (rom_sha stays as the join identity) so no trademarked
//!       names are committed, and drops machine-specific fps/ns_per_frame.
//!       Screenshots are still written (used before serialization).
//!
//!   sweep compare  --base A.jsonl --cand B.jsonl [--min-ratio R]
//!                  [--ignore-perf] [--out report.md]
//!       Diff two manifests. Exit 1 on behavior mismatches, new errors,
//!       missing ROMs, or (unless --ignore-perf) median-normalized per-ROM
//!       fps ratios below R (default 0.90).
//!
//!       PERF CAVEAT: the parallel sweep measures each ROM once under N-way
//!       memory-bandwidth contention, so per-ROM fps is noisy (±20% seen) —
//!       fine for spotting gross regressions, NOT for fine per-ROM gating.
//!       Confirm any flagged perf regression with a clean single-threaded
//!       best-of-3 (`sweep run --list <one-rom> --jobs 1`) before believing
//!       it. (Measured example: eight ROMs the parallel sweep called 0.75x
//!       were all 1.33-1.39x FASTER clean — pure contention noise.) CI runs
//!       --ignore-perf for this reason; perf is a local, quiet-machine check.
//!
//!   sweep gallery  --manifest M.jsonl --screens DIR [--out gallery.html]
//!       HTML gallery: screenshot (linked from ./screens), No-Intro name,
//!       hardware, fps, boot status. Emit --out alongside the screens dir so
//!       the relative image links resolve. Names are (re)resolved here from
//!       each row's stored crc, so a manifest swept before the DATs were
//!       cached still gets them.
//!
//! Determinism: the core has no wall-clock/thread inputs (MBC3 RTC is
//! cycle-derived; sidecars are never read by `Cartridge::from_bytes`), and the
//! masher derives all input from the ROM's SHA-256 + frame index, so every
//! hash in the manifest is reproducible across machines and builds. Only the
//! fps/ns_per_frame fields vary run to run.

use rayon::prelude::*;
use rustyboi_core_lib::audio::AudioOutput;
use rustyboi_core_lib::cartridge::{Cartridge, CgbSupport, Destination};
use rustyboi_core_lib::checksum::crc32;
use rustyboi_core_lib::gb::{Hardware, GB};
use rustyboi_core_lib::movie::{frame_hash, frame_is_non_blank, sha256};
use rustyboi_core_lib::ppu;
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[path = "shared/imaging.rs"]
#[allow(dead_code)] // shared toolbox: this bin uses a subset (e.g. not frame_rgb)
mod imaging;
#[path = "shared/masher.rs"]
mod masher;
use masher::masher;
use imaging::{encode_rgb_webp, frame_rgb, html_escape};

#[derive(Serialize, Deserialize, Clone)]
struct Row {
    /// `<parent dir>/<file name>` — human-readable, but a trademarked game
    /// title. Omitted from committed/anonymized manifests (`run --strip-names`);
    /// `rom_sha` is the join identity so the baseline needs no titles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// CRC32 (hex) of the extracted ROM — the No-Intro key. Kept in working
    /// manifests so `gallery`/`compare` can resolve No-Intro names after the
    /// fact (no re-sweep). Stripped from the committed baseline (--strip-names).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    crc: Option<String>,
    /// First 8 bytes of the *embedded* ROM's SHA-256 (post-unzip payload, not the
    /// zip container — see `rom_sha`; 64-bit, collision-negligible across a
    /// library). THE identity used to match rows between manifests: a row present
    /// in one manifest but not the other means the library changed, not the
    /// emulator. Container-independent (a re-zip keeps it) and opaque, so safe to
    /// commit.
    rom_sha: String,
    hardware: String,
    frames: usize,
    /// FNV fold of every frame's hash over the whole run (frame 0..frames).
    hash_all: String,
    /// (frame, hash) pairs for localizing where two runs diverge.
    checkpoints: Vec<(usize, String)>,
    boot_ok: bool,
    changed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    fps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ns_per_frame: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    // Cartridge-header enrichment for the gallery cards. All optional and
    // skip-if-none so the committed baseline (`--strip-names`) stays byte-
    // identical and old manifests deserialize unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mapper: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rom_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ram_bytes: Option<usize>,
    /// Feature bitfield: 0 battery, 1 rtc, 2 rumble, 3 camera, 4 cgb-compat,
    /// 5 cgb-only, 6 sgb, 7 unlicensed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cart_flags: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    destination: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    licensee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    header_ok: Option<bool>,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(String::as_str);
    let rest = &args[args.len().min(2)..];
    let result = match sub {
        Some("run") => cmd_run(rest),
        Some("compare") => cmd_compare(rest),
        Some("gallery") => cmd_gallery(rest),
        _ => {
            eprintln!(
                "usage:\n  sweep run     --roms DIR... [--list FILE] --out DIR [--frames N] \
                 [--warmup N] [--jobs N] [--timeout SECS] [--no-screens] [--no-videos] \
                 [--no-bios-meta] [--bios-dir DIR] [--hardware dmg,cgb,sgb,agb] [--only SUBSTR] \
                 [--shard K/N]\n  \
                 sweep compare --base A.jsonl --cand B.jsonl [--min-ratio R] [--ignore-perf] \
                 [--out report.md]\n  \
                 sweep gallery --manifest M.jsonl --screens DIR [--videos DIR] [--out gallery.html]"
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(ok) => {
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

// ---------------------------------------------------------------------------
// deterministic input masher
// ---------------------------------------------------------------------------


// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

struct RunCfg {
    frames: usize,
    warmup: usize,
    timeout_secs: u64,
    screens_dir: Option<PathBuf>,
    /// Where per-(ROM,hardware) HEVC+AAC videos land. `Some` only when
    /// screenshots are on, `--no-videos` is absent, and `ffmpeg` is on PATH.
    videos_dir: Option<PathBuf>,
    /// Hardware models the MEDIA fan-out captures (posters + videos). The
    /// canonical manifest row is always the auto-detected model regardless of
    /// this set (see `canonical_hardware`).
    hardware: Vec<Hardware>,
    /// CRC32 -> No-Intro game name (auto-fetched DATs). Empty = file stems.
    names: std::collections::HashMap<u32, String>,
}

/// Short lowercase tag for a hardware model — the media-filename suffix and the
/// gallery tab key. Equal to the lowercased `Debug` name, so DMG->"dmg",
/// CGB->"cgb", SGB->"sgb", AGB->"agb", etc.
fn hw_tag(hw: Hardware) -> String {
    format!("{hw:?}").to_ascii_lowercase()
}

/// Parse a hardware tag (case-insensitive) into a `Hardware`. Accepts the short
/// names in the default matrix plus every variant by its lowercased name.
fn parse_hw(tag: &str) -> Option<Hardware> {
    Some(match tag.trim().to_ascii_lowercase().as_str() {
        "dmg" => Hardware::DMG,
        "dmg0" => Hardware::DMG0,
        "mgb" => Hardware::MGB,
        "sgb" => Hardware::SGB,
        "sgb2" => Hardware::SGB2,
        "cgb0" => Hardware::CGB0,
        "cgbb" => Hardware::CGBB,
        "cgb" => Hardware::CGB,
        "cgbe" => Hardware::CGBE,
        "agb" => Hardware::AGB,
        _ => return None,
    })
}

/// The single hardware model the canonical manifest row is emulated on — the
/// pre-media behavior: CGB for CGB-aware carts, DMG otherwise. Unchanged so the
/// regression baseline and `compare` semantics stay byte-identical.
fn canonical_hardware(cart: &Cartridge) -> Hardware {
    if cart.supports_cgb() {
        Hardware::CGB
    } else {
        Hardware::DMG
    }
}

/// One audio duration second at the DMG dot rate — sample count / this = seconds.
const AUDIO_RATE: u32 = 44100;
/// GB frame rate as an exact rational (master clock / dots-per-frame).
const FPS_NUM: u64 = 4_194_304;
const FPS_DEN: u64 = 70_224;

/// Cache dir for the auto-fetched No-Intro DATs. Override with RB_NOINTRO_DIR
/// (e.g. to point at DATs fetched offline via tools/fetch-nointro-dats.sh).
fn nointro_dir() -> PathBuf {
    if let Ok(d) = std::env::var("RB_NOINTRO_DIR") {
        return PathBuf::from(d);
    }
    if let Ok(x) = std::env::var("XDG_CACHE_HOME") {
        return PathBuf::from(x).join("rustyboi/nointro");
    }
    if let Ok(h) = std::env::var("HOME") {
        return PathBuf::from(h).join(".cache/rustyboi/nointro");
    }
    PathBuf::from("nointro-dats")
}

/// No-Intro naming, always on: ensure the public GB+GBC DATs are cached
/// (fetch once via `curl` from libretro-database, best-effort, 30s cap) and
/// return the crc32->name map. Offline / no curl / fetch failure => empty map
/// (rows keep file-stem names), noted once on stderr. The DATs are cached
/// outside the repo, never committed.
fn nointro_map() -> std::collections::HashMap<u32, String> {
    const URLS: [(&str, &str); 2] = [
        ("gb.dat", "https://raw.githubusercontent.com/libretro/libretro-database/master/metadat/no-intro/Nintendo%20-%20Game%20Boy.dat"),
        ("gbc.dat", "https://raw.githubusercontent.com/libretro/libretro-database/master/metadat/no-intro/Nintendo%20-%20Game%20Boy%20Color.dat"),
    ];
    let dir = nointro_dir();
    let _ = std::fs::create_dir_all(&dir);
    let mut map = std::collections::HashMap::new();
    let mut loaded = false;
    for (fname, url) in URLS {
        let path = dir.join(fname);
        if !path.exists() {
            let _ = std::process::Command::new("curl")
                .args(["-fsSL", "--max-time", "30", url, "-o"])
                .arg(&path)
                .status();
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            parse_dat(&text, &mut map);
            loaded = true;
        }
    }
    if !loaded {
        eprintln!("sweep: no No-Intro DATs available (offline?); labeling by file name");
    }
    map
}

/// The baseline join identity: first 8 bytes of the SHA-256 of the *embedded*
/// ROM (the payload `Cartridge::from_bytes` feeds the core), hex-encoded. Hashing
/// the extracted ROM rather than the container means a bare ROM and any (re)zip
/// of the same bytes share one key — recompressing the library never churns the
/// baseline. A corrupt/unreadable archive falls back to the container hash so
/// every row still gets a stable key.
fn rom_sha(bytes: &[u8]) -> String {
    let rom = Cartridge::extract_rom_bytes(bytes).unwrap_or_else(|_| bytes.to_vec());
    sha256(&rom)[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Parse a No-Intro DAT into the crc->name map. Handles both formats DATs ship
/// in: Logiqx XML (`<game name="Rampart (USA)"><rom crc="ABCD1234".../></game>`)
/// and ClrMamePro (`game ( name "Rampart (USA)" rom ( ... crc D5AEED2E ... ) )`,
/// which is what libretro-database serves). Both scans run — their syntaxes are
/// disjoint, so a file only matches one. Unknown formats contribute nothing
/// (rows fall back to file stems).
fn parse_dat(text: &str, map: &mut std::collections::HashMap<u32, String>) {
    // --- Logiqx XML: split on '<', match `game`/`rom` tags by attribute. ---
    let attr = |s: &str, key: &str| -> Option<String> {
        let pat = format!("{key}=\"");
        let i = s.find(&pat)? + pat.len();
        let j = s[i..].find('"')? + i;
        Some(s[i..j].to_string())
    };
    let mut cur: Option<String> = None;
    for tag in text.split('<') {
        if let Some(rest) = tag.strip_prefix("game ").or_else(|| tag.strip_prefix("machine ")) {
            cur = attr(rest, "name");
        } else if let Some(rest) = tag.strip_prefix("rom ")
            && let (Some(name), Some(crc)) = (&cur, attr(rest, "crc"))
            && let Ok(v) = u32::from_str_radix(crc.trim(), 16)
        {
            map.entry(v).or_insert_with(|| name.clone());
        }
    }

    // --- ClrMamePro: line-oriented. A standalone `name "..."` line is the game
    // name; a `rom ( ... crc <HEX> ... )` line carries the CRC. ---
    let mut cur: Option<String> = None;
    for line in text.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("name \"") {
            if let Some(end) = rest.find('"') {
                cur = Some(rest[..end].to_string());
            }
        } else if t.starts_with("rom ")
            && let Some(name) = &cur
            && let Some(after) = t.split(" crc ").nth(1)
            && let Some(tok) = after.split_whitespace().next()
            && let Ok(v) = u32::from_str_radix(tok, 16)
        {
            map.entry(v).or_insert_with(|| name.clone());
        }
    }
}

fn cmd_run(args: &[String]) -> Result<bool, String> {
    let out_dir = PathBuf::from(arg(args, "--out").ok_or("run: --out <dir> required")?);
    let frames: usize = parse_num(args, "--frames", 3000)?;
    let warmup: usize = parse_num(args, "--warmup", 600)?;
    let timeout_secs: u64 = parse_num(args, "--timeout", 300)? as u64;
    let jobs: usize = parse_num(args, "--jobs", 0)?;
    let no_screens = args.iter().any(|a| a == "--no-screens");
    let no_videos = args.iter().any(|a| a == "--no-videos");
    let no_bios_meta = args.iter().any(|a| a == "--no-bios-meta");
    let bios_dir = arg(args, "--bios-dir").map(PathBuf::from);
    let strip_names = args.iter().any(|a| a == "--strip-names");
    let only = arg(args, "--only");
    let shard = arg(args, "--shard");
    if warmup >= frames {
        return Err("run: --warmup must be < --frames".into());
    }

    // Media hardware matrix (posters + videos). Default DMG,CGB,SGB,AGB.
    let hardware: Vec<Hardware> = match arg(args, "--hardware") {
        Some(spec) => {
            let mut hw = Vec::new();
            for tag in spec.split(',').map(str::trim).filter(|t| !t.is_empty()) {
                let h = parse_hw(tag).ok_or_else(|| format!("run: unknown --hardware {tag:?}"))?;
                if !hw.contains(&h) {
                    hw.push(h);
                }
            }
            if hw.is_empty() {
                return Err("run: --hardware listed no models".into());
            }
            hw
        }
        None => vec![Hardware::DMG, Hardware::MGB, Hardware::CGB, Hardware::SGB, Hardware::AGB],
    };

    let names = nointro_map();

    let mut roms: Vec<(String, PathBuf)> = Vec::new();
    for dir in multi_arg(args, "--roms") {
        let root = PathBuf::from(&dir);
        let prefix = root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        collect_roms(&root, &prefix, &mut roms)?;
    }
    if let Some(list) = arg(args, "--list") {
        let text = std::fs::read_to_string(&list).map_err(|e| format!("read {list}: {e}"))?;
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let p = PathBuf::from(line);
            let key = rom_key(&p);
            roms.push((key, p));
        }
    }
    if let Some(pat) = &only {
        roms.retain(|(k, _)| k.contains(pat.as_str()));
    }
    roms.sort();
    roms.dedup_by(|a, b| a.0 == b.0);
    // Shard AFTER sort+dedup so every shard slices the identical ordered list
    // into a disjoint subset; the N partial manifests reconstruct the library.
    if let Some(spec) = &shard {
        apply_shard(&mut roms, spec)?;
    }
    if roms.is_empty() {
        return Err("run: no ROMs found".into());
    }

    std::fs::create_dir_all(&out_dir).map_err(|e| format!("mkdir {}: {e}", out_dir.display()))?;
    let screens_dir = if no_screens {
        None
    } else {
        let d = out_dir.join("screens");
        std::fs::create_dir_all(&d).map_err(|e| format!("mkdir {}: {e}", d.display()))?;
        Some(d)
    };
    // Videos ride along with screenshots, but only if ffmpeg is present and not
    // opted out. No ffmpeg -> one warning, PNG-only (the lazy-APU fast path is
    // kept: no audio is drained unless a video is actually being encoded).
    let videos_dir = if screens_dir.is_some() && !no_videos && ffmpeg_available() {
        let d = out_dir.join("videos");
        std::fs::create_dir_all(&d).map_err(|e| format!("mkdir {}: {e}", d.display()))?;
        Some(d)
    } else {
        if screens_dir.is_some() && !no_videos {
            eprintln!("sweep: ffmpeg not on PATH; emitting screenshots only, no videos");
        }
        None
    };
    let cfg = RunCfg { frames, warmup, timeout_secs, screens_dir, videos_dir, hardware, names };

    let pool = {
        let mut b = rayon::ThreadPoolBuilder::new();
        if jobs > 0 {
            b = b.num_threads(jobs);
        }
        b.build().map_err(|e| format!("thread pool: {e}"))?
    };

    let total = roms.len();
    let done = AtomicUsize::new(0);
    let started = Instant::now();
    let mut rows: Vec<Row> = pool.install(|| {
        roms.par_iter()
            .map(|(key, path)| {
                let row = run_one(key, path, &cfg);
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                let status = match (&row.error, row.fps) {
                    (Some(e), _) => format!("ERROR: {e}"),
                    (None, Some(fps)) => {
                        format!("{fps:7.1} fps  {}", if row.boot_ok { "ok" } else { "BLANK" })
                    }
                    _ => "done".into(),
                };
                eprintln!("[{n:4}/{total}] {key}  {status}");
                row
            })
            .collect()
    });
    // Sort by rom_sha so the committed baseline has a stable, name-free order.
    rows.sort_by(|a, b| a.rom_sha.cmp(&b.rom_sha));

    // BIOS meta capture (once per bootable hardware model): the boot animation
    // from power-on to cart handoff. Produces BOTH a regression row in the
    // manifest (a deterministic `hash_all` over every boot frame, so `compare`
    // flags boot-behavior drift) AND gallery media (poster when screens on;
    // H.264/AAC video when videos are on). INDEPENDENT of the game `--hardware`
    // matrix: every model that has a provisioned boot ROM is captured, so the
    // boot rows are identical regardless of media selection or CI trimming.
    // `--no-bios-meta` is the full opt-out; `--no-screens`/`--no-videos` drop the
    // media but NOT the hash row (the gate must be consistent either way).
    // capture_bios cleanly skips (logs once) any model whose boot ROM is absent.
    // Wholly separate from `emulate` — its own synthetic cart, ADDITIONAL rows
    // keyed `bios_<tag>` that never touch or reorder any game row's bytes.
    // SGB/SGB2 are intentionally absent: the Super Game Boy renders its boot
    // logo/animation on the SNES side, not the GB LCD (its GB-side boot ROM just
    // hands off in ~27 frames without a logo scroll), so a GB-framebuffer boot
    // capture for them is meaningless. They stay fully supported for GAMES.
    const BOOT_MODELS: [Hardware; 8] = [
        Hardware::DMG,
        Hardware::DMG0,
        Hardware::MGB,
        Hardware::CGB0,
        Hardware::CGBB,
        Hardware::CGB,
        Hardware::CGBE,
        Hardware::AGB,
    ];
    let mut bios_rows: Vec<Row> = Vec::new();
    if !no_bios_meta {
        for hw in BOOT_MODELS {
            let tag = hw_tag(hw);
            match capture_bios(cfg.screens_dir.as_deref(), cfg.videos_dir.as_deref(), hw, bios_dir.as_deref(), timeout_secs) {
                Ok(Some(b)) => {
                    let vdur = b.frames as f64 * FPS_DEN as f64 / FPS_NUM as f64;
                    match b.handoff_frame {
                        Some(fr) => eprintln!(
                            "sweep: bios [{tag}] handoff at frame {fr} ({} frames, {vdur:.2}s, audio_samples={}, video={}, hash_all={:016x})",
                            b.frames, b.audio_samples, b.video_written, b.hash_all
                        ),
                        None => eprintln!(
                            "sweep: bios [{tag}] NO handoff ({} frames capped, video={}, hash_all={:016x})",
                            b.frames, b.video_written, b.hash_all
                        ),
                    }
                    // Reserved identity `bios_<tag>` (starts with a non-hex `_`,
                    // and 8 chars — can't collide with a 16-hex-char rom_sha).
                    bios_rows.push(Row {
                        key: Some(format!("bios/{tag}")),
                        name: Some(format!("BIOS boot — {}", tag.to_ascii_uppercase())),
                        crc: None,
                        rom_sha: format!("bios_{tag}"),
                        hardware: format!("{hw:?}"),
                        frames: b.frames,
                        hash_all: format!("{:016x}", b.hash_all),
                        checkpoints: b
                            .checkpoints
                            .into_iter()
                            .map(|(f, h)| (f, format!("{h:016x}")))
                            .collect(),
                        boot_ok: true,
                        changed: true,
                        fps: None,
                        ns_per_frame: None,
                        error: None,
                        mapper: None,
                        rom_bytes: None,
                        ram_bytes: None,
                        cart_flags: None,
                        destination: None,
                        licensee: None,
                        header_ok: None,
                    });
                }
                Ok(None) => {} // no dump for this model; capture_bios logged it
                Err(e) => eprintln!("sweep: bios [{tag}] failed: {e}"),
            }
        }
    }
    // Append the BIOS regression rows and re-sort: game rows keep byte-identical
    // content (only additional rows joined in), and the manifest stays ordered by
    // rom_sha for a deterministic file.
    if !bios_rows.is_empty() {
        rows.extend(bios_rows);
        rows.sort_by(|a, b| a.rom_sha.cmp(&b.rom_sha));
    }

    // Remove the encode-temp subdir wholesale: any `.tmp`/`.pcm` left behind by
    // an interrupted/panicked capture goes with it, so `videos/` holds only
    // final `<sha>_<hw>.mp4` files.
    if let Some(vdir) = &cfg.videos_dir {
        let _ = std::fs::remove_dir_all(vdir.join(".sweep-tmp"));
    }

    let manifest = out_dir.join("manifest.jsonl");
    let mut f = std::fs::File::create(&manifest).map_err(|e| format!("create manifest: {e}"))?;
    for row in &rows {
        // Screenshots were already written above using the in-memory key;
        // --strip-names only affects what lands in the manifest. It also drops
        // fps/ns_per_frame: this flag produces the committable baseline, and
        // machine-specific timing there is pure noise (churns every regen,
        // means nothing across machines). Behavior fields (hash_all,
        // checkpoints, boot_ok, changed) are what a committed baseline gates.
        let line = if strip_names {
            let mut r = row.clone();
            r.key = None;
            r.name = None;
            r.crc = None;
            r.fps = None;
            r.ns_per_frame = None;
            r.mapper = None;
            r.rom_bytes = None;
            r.ram_bytes = None;
            r.cart_flags = None;
            r.destination = None;
            r.licensee = None;
            r.header_ok = None;
            serde_json::to_string(&r)
        } else {
            serde_json::to_string(row)
        }
        .map_err(|e| format!("serialize: {e}"))?;
        writeln!(f, "{line}").map_err(|e| format!("write manifest: {e}"))?;
    }

    let errors = rows.iter().filter(|r| r.error.is_some()).count();
    let blank = rows.iter().filter(|r| r.error.is_none() && !(r.boot_ok && r.changed)).count();
    let med = median(
        rows.iter().filter_map(|r| r.fps).collect::<Vec<_>>(),
    );
    println!(
        "sweep: {} ROMs in {:.0}s — {} ok, {blank} blank/static, {errors} errors; median {:.0} fps -> {}",
        rows.len(),
        started.elapsed().as_secs_f64(),
        rows.len() - errors - blank,
        med.unwrap_or(0.0),
        manifest.display()
    );
    Ok(true)
}

fn collect_roms(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read dir {}: {e}", dir.display()))?;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            let sub = format!("{prefix}/{}", entry.file_name().to_string_lossy());
            collect_roms(&path, &sub, out)?;
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| matches!(e.to_ascii_lowercase().as_str(), "gb" | "gbc" | "zip"))
            .unwrap_or(false)
        {
            let key = format!("{prefix}/{}", path.file_name().unwrap_or_default().to_string_lossy());
            out.push((key, path));
        }
    }
    Ok(())
}

fn rom_key(path: &Path) -> String {
    let parent = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!("{parent}/{}", path.file_name().unwrap_or_default().to_string_lossy())
}

fn run_one(key: &str, path: &Path, cfg: &RunCfg) -> Row {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut row = Row {
        key: Some(key.to_string()),
        name: Some(name),
        crc: None,
        rom_sha: String::new(),
        hardware: String::new(),
        frames: cfg.frames,
        hash_all: String::new(),
        checkpoints: Vec::new(),
        boot_ok: false,
        changed: false,
        fps: None,
        ns_per_frame: None,
        error: None,
        mapper: None,
        rom_bytes: None,
        ram_bytes: None,
        cart_flags: None,
        destination: None,
        licensee: None,
        header_ok: None,
    };
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            row.error = Some(format!("read: {e}"));
            return row;
        }
    };
    let sha = sha256(&bytes);
    // The masher seed stays keyed on the file bytes (unchanged run behavior).
    let seed = u64::from_le_bytes(sha[..8].try_into().unwrap());
    // rom_sha — the baseline join key — is keyed on the *embedded* ROM (the
    // post-unzip payload the core actually loads), not the zip container, so a
    // recompressed/re-zipped file with byte-identical ROM content keeps its
    // identity. Bare .gb/.gbc hash the same either way.
    row.rom_sha = rom_sha(&bytes);

    // CRC32 of the extracted ROM (not the zip): stored for after-the-fact
    // naming, and used now to set the No-Intro name when the DAT has it.
    if let Ok(rom) = Cartridge::extract_rom_bytes(&bytes) {
        let c = crc32(&rom);
        row.crc = Some(format!("{c:08x}"));
        if let Some(nointro) = cfg.names.get(&c) {
            row.name = Some(nointro.clone());
        }
    }

    // Cartridge-header enrichment for the gallery. Parsing the header a second
    // time here (before `emulate`) is cheap and is the only place the cart is
    // available; a ROM that fails to parse keeps `None`s and still gets a row.
    if let Ok(cart) = Cartridge::from_bytes(&bytes) {
        row.mapper = Some(cart.mapper_name().to_string());
        row.rom_bytes = Some(cart.rom_size_bytes());
        row.ram_bytes = Some(cart.ram_size_bytes());
        let mut f = 0u16;
        if cart.has_battery() { f |= 1 << 0; }
        if cart.has_rtc() { f |= 1 << 1; }
        if cart.has_rumble() { f |= 1 << 2; }
        if cart.has_camera() { f |= 1 << 3; }
        match cart.get_cgb_support() {
            CgbSupport::Compatible => f |= 1 << 4,
            CgbSupport::Only => f |= 1 << 5,
            CgbSupport::None => {}
        }
        if cart.supports_sgb() { f |= 1 << 6; }
        if cart.is_unlicensed() { f |= 1 << 7; }
        row.cart_flags = Some(f);
        row.destination = cart.destination().map(|d| match d {
            Destination::Japanese => "jp".to_string(),
            Destination::Overseas => "intl".to_string(),
        });
        row.licensee = cart.licensee().map(str::to_string);
        row.header_ok = Some(cart.header_checksum_valid());
    }

    // Wild-library ROMs are a fuzz surface; a panic in one must not kill the
    // sweep (or its rayon worker), so it demotes to an error row.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        emulate(&bytes, seed, cfg)
    }));
    match result {
        Ok(Ok(out)) => {
            let canon_tag = out.hardware.to_ascii_lowercase();
            row.hardware = out.hardware;
            row.hash_all = format!("{:016x}", out.hash_all);
            row.checkpoints = out
                .checkpoints
                .into_iter()
                .map(|(f, h)| (f, format!("{h:016x}")))
                .collect();
            row.boot_ok = out.boot_ok;
            row.changed = out.changed;
            row.fps = Some(out.fps);
            row.ns_per_frame = Some(out.ns_per_frame);
            // Canonical-hardware media, keyed on rom_sha (never the filename, so
            // the user's collection is never fingerprinted on disk or in HTML).
            if let Some(dir) = &cfg.screens_dir {
                write_poster_png(dir, &row.rom_sha, &canon_tag, out.final_rgb.as_ref());
            }
        }
        Ok(Err(e)) => row.error = Some(e),
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            row.error = Some(format!("panic: {msg}"));
        }
    }

    // MEDIA fan-out: capture each requested hardware model for the gallery
    // (posters + optional HEVC/AAC videos), keyed on rom_sha. Runs only for
    // non-errored ROMs with screenshots enabled; never touches the manifest
    // row above. A panic in one model demotes to a skip, not a lost sweep.
    if row.error.is_none()
        && let Some(dir) = &cfg.screens_dir
    {
        let canon = out_hardware_of(&row);
        // Fan the per-model media passes across the SAME rayon pool as the
        // ROM-level par_iter: work-stealing keeps total concurrency bounded by
        // --jobs (no extra ffmpeg), but a ROM's models run in parallel whenever
        // cores are free, cutting per-ROM latency ~Nx. Each pass writes its own
        // <sha>_<hw> files and never mutates the manifest row, so it's race-free.
        cfg.hardware.par_iter().for_each(|&hw| {
            let tag = hw_tag(hw);
            let is_canon = canon.as_deref() == Some(tag.as_str());
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                capture_media(&bytes, seed, cfg, hw, &row.rom_sha)
            }));
            match res {
                Ok(Ok(m)) => {
                    // The canonical model's poster was already written by the
                    // byte-identical manifest pass; don't overwrite it
                    // (audio-drained media passes could differ if the APU ever
                    // fed back into the PPU — keep the canary pristine).
                    if !is_canon {
                        write_poster_png(dir, &row.rom_sha, &tag, m.poster.as_ref());
                    } else if out_hash_of(&row) != Some(m.hash_all) {
                        // Divergence gate: draining audio changed the canonical
                        // frame hashes. The manifest is safe (produced without
                        // audio), but this must be surfaced, not hidden.
                        eprintln!(
                            "sweep: WARNING audio-drain hash divergence on {} [{tag}]: media {:016x} vs manifest {}",
                            row.rom_sha, m.hash_all, out_hash_of(&row).unwrap_or_default()
                        );
                    }
                    // SGB border still (display-only, SGB-only, additional to
                    // the 160x144 poster): a 256x224 bordered frame, written
                    // only when the game uploaded a border.
                    if let Some(rgb) = m.border.as_ref() {
                        write_border_png(dir, &row.rom_sha, rgb);
                    }
                    if std::env::var_os("RB_SWEEP_MEDIA_LOG").is_some() {
                        let vdur = m.frames as f64 * FPS_DEN as f64 / FPS_NUM as f64;
                        let adur = m.audio_samples as f64 / AUDIO_RATE as f64;
                        eprintln!(
                            "media {} [{tag}] frames={} audio_samples={} v={vdur:.2}s a={adur:.2}s drift={:.3}s video={}",
                            row.rom_sha, m.frames, m.audio_samples, adur - vdur, m.video_written
                        );
                    }
                }
                Ok(Err(e)) => eprintln!("sweep: media {} [{tag}] failed: {e}", row.rom_sha),
                Err(_) => eprintln!("sweep: media {} [{tag}] panicked; skipped", row.rom_sha),
            }
        });
    }
    row
}

/// The canonical model tag stored on the manifest row (lowercased), or None if
/// the run errored before emulation.
fn out_hardware_of(row: &Row) -> Option<String> {
    (!row.hardware.is_empty()).then(|| row.hardware.to_ascii_lowercase())
}

/// The canonical row's hash_all parsed back to u64, for the audio-drain
/// divergence gate.
fn out_hash_of(row: &Row) -> Option<u64> {
    u64::from_str_radix(&row.hash_all, 16).ok()
}

struct EmuOut {
    hardware: String,
    hash_all: u64,
    checkpoints: Vec<(usize, u64)>,
    boot_ok: bool,
    changed: bool,
    fps: f64,
    ns_per_frame: f64,
    final_rgb: Option<Vec<u8>>,
}

fn emulate(bytes: &[u8], seed: u64, cfg: &RunCfg) -> Result<EmuOut, String> {
    let cart = Cartridge::from_bytes(bytes).map_err(|e| format!("load: {e}"))?;
    let hardware = canonical_hardware(&cart);
    let mut gb = GB::new(hardware);
    gb.insert(cart);
    gb.skip_bios();

    // Checkpoints at warmup and every ~quarter of the measured window.
    let span = cfg.frames - cfg.warmup;
    let checkpoint_at: Vec<usize> = (0..=4)
        .map(|i| cfg.warmup + span * i / 4)
        .collect();

    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash_all: u64 = 0xcbf2_9ce4_8422_2325;
    let mut checkpoints = Vec::with_capacity(checkpoint_at.len());
    let mut first_hash = None;
    let mut changed = false;
    let mut boot_ok = false;
    let mut final_rgb = None;
    let mut timer = None;
    let started = Instant::now();

    for f in 0..cfg.frames {
        if f == cfg.warmup {
            timer = Some(Instant::now());
        }
        gb.set_input_state(masher(f, seed));
        let (frame, _bp) = gb.run_until_frame(false);
        let h = frame_hash(&gb, &frame);
        hash_all = (hash_all ^ h).wrapping_mul(FNV_PRIME);
        if checkpoint_at.contains(&(f + 1)) {
            checkpoints.push((f + 1, hash_all));
        }
        match first_hash {
            None => first_hash = Some(h),
            Some(fh) if fh != h => changed = true,
            _ => {}
        }
        // Classify + screenshot on the last non-blank frame of the final 120
        // (a run ending mid fade-to-black is not a boot failure).
        if f + 120 >= cfg.frames {
            if frame_is_non_blank(&gb, &frame) {
                boot_ok = true;
                if cfg.screens_dir.is_some() {
                    final_rgb = Some(frame_rgb(&frame));
                }
            } else if f + 1 == cfg.frames && final_rgb.is_none() && cfg.screens_dir.is_some() {
                final_rgb = Some(frame_rgb(&frame));
            }
        }
        if f % 256 == 0 && started.elapsed().as_secs() > cfg.timeout_secs {
            return Err(format!("timeout after {} frames", f + 1));
        }
    }

    let elapsed = timer.expect("warmup < frames").elapsed();
    let measured = span as f64;
    Ok(EmuOut {
        hardware: format!("{hardware:?}"),
        hash_all,
        checkpoints,
        boot_ok,
        changed,
        fps: measured / elapsed.as_secs_f64(),
        ns_per_frame: elapsed.as_nanos() as f64 / measured,
        final_rgb,
    })
}

// ---------------------------------------------------------------------------
// media capture (per hardware): rom_sha-keyed posters + HEVC/AAC videos
// ---------------------------------------------------------------------------

struct MediaOut {
    /// Final non-blank frame (poster); mirrors `emulate`'s selection so the
    /// canonical model's media and manifest agree.
    poster: Option<Vec<u8>>,
    /// FNV fold over the run — compared to the manifest row for the canonical
    /// model to prove audio drain never perturbs the frame hashes.
    hash_all: u64,
    /// SGB-only: the 256x224 RGB888 border frame (GB screen composited at
    /// (48,40)) captured at the end of the run, `Some` only when the game
    /// actually uploaded a border. Display-only; never touches the manifest.
    border: Option<Vec<u8>>,
    frames: usize,
    audio_samples: usize,
    video_written: bool,
}

/// APU sink: `run_until_frame(true)` pushes generated samples here; the capture
/// loop drains and streams them to a PCM sidecar each frame (never buffering
/// the whole run). Registered ONLY when a video is being encoded, so poster-only
/// and `--no-videos`/`--no-screens` passes keep the core's lazy-APU fast path.
#[derive(Clone, Default)]
struct SampleSink(Arc<Mutex<Vec<(f32, f32)>>>);
impl AudioOutput for SampleSink {
    fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
    fn add_samples(&mut self, samples: &[(f32, f32)]) {
        self.0.lock().unwrap().extend_from_slice(samples);
    }
}

fn f32_to_s16le(x: f32) -> [u8; 2] {
    ((x.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()
}

/// Is `ffmpeg` on PATH and runnable? Probed once at startup.
fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// An `ffmpeg` invocation at low CPU priority so a big media sweep stays
/// responsive — the encoders yield to interactive apps instead of pinning
/// cores. Unix runs it under `nice -n 19` (always present on Linux/macOS);
/// elsewhere plain ffmpeg.
fn ffmpeg_command() -> Command {
    #[cfg(unix)]
    {
        let mut c = Command::new("nice");
        c.args(["-n", "19", "ffmpeg"]);
        c
    }
    #[cfg(not(unix))]
    {
        Command::new("ffmpeg")
    }
}

/// H.264 encoder reading rawvideo rgb24 over stdin -> a temp mp4. H.264 plays
/// in every browser (HEVC did not). `-threads 1` caps x264 since the sweep
/// already parallelizes across ROMs; `-g 300` (long GOP) + `crf 28` exploit the
/// heavy temporal redundancy of tiny 160x144 clips. `veryfast`: at 160x144 the
/// slower presets are ~2x the CPU for NO size win (measured slightly LARGER on
/// gameplay), so veryfast is both faster and smaller across a big library.
fn spawn_encoder(out: &Path) -> std::io::Result<std::process::Child> {
    ffmpeg_command()
        .args([
            "-loglevel", "error",
            "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", "160x144",
            "-framerate", "4194304/70224", "-i", "-",
            "-c:v", "libx264", "-preset", "veryslow", "-crf", "25",
            "-g", "300", "-threads", "1",
            "-pix_fmt", "yuv420p", "-f", "mp4",
        ])
        .arg(out)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Second pass (`-c:v copy`, no re-encode): mux the H.264 video with the PCM
/// audio as AAC. Returns true only if the muxed file was produced.
/// No `-shortest`: the whole run's video is the point, so keep every frame even
/// when the audio track is marginally shorter (it just falls silent at the tail).
///
/// `-nostdin` + a null stdin are load-bearing: this mux reads only from files,
/// but ffmpeg otherwise puts the inherited controlling terminal into
/// non-blocking keyboard mode. Run many muxes at once (the per-ROM hardware
/// passes are parallel) and they race on that shared tty's termios state until
/// one is left doing a *blocking* read on it — hanging forever at 0% CPU and
/// wedging every sweep thread that then `wait()`s on it. Detaching fd 0 from the
/// tty makes that impossible.
fn mux_av(video: &Path, pcm: &Path, out: &Path) -> bool {
    let status = ffmpeg_command()
        .args(["-nostdin", "-loglevel", "error", "-i"])
        .arg(video)
        .args(["-f", "s16le", "-ar", "44100", "-ac", "2", "-i"])
        .arg(pcm)
        .args([
            "-c:v", "copy", "-c:a", "aac", "-b:a", "96k",
            "-movflags", "+faststart",
        ])
        .arg(out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    matches!(status, Ok(s) if s.success()) && out.exists()
}

/// Write the single poster WebP, keyed on `sha`_`tag` (never a filename). This is
/// the video `poster=` and the no-ffmpeg/old-dir `<img>` fallback. Errors are
/// logged, not fatal.
fn write_poster_png(dir: &Path, sha: &str, tag: &str, poster: Option<&Vec<u8>>) {
    if let Some(rgb) = poster {
        let file = dir.join(format!("{sha}_{tag}.webp"));
        if let Err(e) = std::fs::write(&file, encode_rgb_webp(160, 144, rgb)) {
            eprintln!("screenshot {}: {e}", file.display());
        }
    }
}

/// Write the SGB 256x224 border still, keyed `<sha>_sgb_border.webp`. Display-only
/// and SGB-only; the gallery composites the 160x144 video into its center window.
/// Errors are logged, not fatal.
fn write_border_png(dir: &Path, sha: &str, rgb: &[u8]) {
    let file = dir.join(format!("{sha}_sgb_border.webp"));
    let (w, h) = (ppu::SGB_FRAME_WIDTH as u32, ppu::SGB_FRAME_HEIGHT as u32);
    if let Err(e) = std::fs::write(&file, encode_rgb_webp(w, h, rgb)) {
        eprintln!("border {}: {e}", file.display());
    }
}

/// Emulate `hardware` purely to produce media: a poster, and — when
/// `cfg.videos_dir` is set — an HEVC+AAC mp4 of the whole run. This never
/// contributes to the manifest; it may drain audio (which the canonical
/// manifest pass never does), and its frame hash is returned only so the caller
/// can assert the drain didn't perturb rendering.
fn capture_media(
    bytes: &[u8],
    seed: u64,
    cfg: &RunCfg,
    hardware: Hardware,
    sha: &str,
) -> Result<MediaOut, String> {
    let cart = Cartridge::from_bytes(bytes).map_err(|e| format!("load: {e}"))?;
    let mut gb = GB::new(hardware);
    gb.insert(cart);
    gb.skip_bios();

    let tag = hw_tag(hardware);
    // Audio is drained only when a video is actually being encoded. Encode temps
    // live in a dedicated `videos/.sweep-tmp/` subdir (same filesystem as the
    // final mp4, so the mux-failure rename is cheap and never crosses devices);
    // the whole subdir is removed at the end of the run, so an
    // interrupted/panicked capture never leaks a `.tmp`/`.pcm` into the artifact.
    let (tmp_video, tmp_pcm) = match &cfg.videos_dir {
        Some(d) => {
            let tmp = d.join(".sweep-tmp");
            let _ = std::fs::create_dir_all(&tmp);
            (
                Some(tmp.join(format!("{sha}_{tag}.mp4"))),
                Some(tmp.join(format!("{sha}_{tag}.pcm"))),
            )
        }
        None => (None, None),
    };
    let mut encoder = None;
    let mut pcm = None;
    if let (Some(tv), Some(tp)) = (&tmp_video, &tmp_pcm) {
        match spawn_encoder(tv) {
            Ok(child) => {
                encoder = Some(child);
                pcm = std::fs::File::create(tp).ok().map(std::io::BufWriter::new);
            }
            Err(e) => eprintln!("sweep: ffmpeg spawn {sha} [{tag}]: {e}"),
        }
    }
    let collect_audio = encoder.is_some();
    let sink = SampleSink::default();
    if collect_audio {
        gb.enable_audio(Box::new(sink.clone())).map_err(|e| format!("audio: {e}"))?;
    }

    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash_all: u64 = 0xcbf2_9ce4_8422_2325;
    let mut poster: Option<Vec<u8>> = None;
    let mut audio_samples = 0usize;
    let started = Instant::now();

    for f in 0..cfg.frames {
        gb.set_input_state(masher(f, seed));
        let (frame, _bp) = gb.run_until_frame(collect_audio);

        let in_tail = f + 120 >= cfg.frames;
        let need_rgb = encoder.is_some() || in_tail;
        let rgb = need_rgb.then(|| frame_rgb(&frame));

        if let Some(child) = &mut encoder
            && let (Some(stdin), Some(rgb)) = (child.stdin.as_mut(), &rgb)
            && stdin.write_all(rgb).is_err()
        {
            // Encoder died (disk full, killed): stop streaming, keep posters.
            encoder = None;
        }
        if collect_audio {
            let mut buf = sink.0.lock().unwrap();
            if !buf.is_empty() {
                audio_samples += buf.len();
                if let Some(w) = &mut pcm {
                    for (l, r) in buf.iter() {
                        let _ = w.write_all(&f32_to_s16le(*l));
                        let _ = w.write_all(&f32_to_s16le(*r));
                    }
                }
                buf.clear();
            }
        }

        let h = frame_hash(&gb, &frame);
        hash_all = (hash_all ^ h).wrapping_mul(FNV_PRIME);
        // Poster = last non-blank frame of the tail; falls back to the very
        // last frame if the run ended blank (mirrors `emulate`'s selection).
        if in_tail && (frame_is_non_blank(&gb, &frame) || (f + 1 == cfg.frames && poster.is_none())) {
            poster = rgb;
        }
        if f % 256 == 0 && started.elapsed().as_secs() > cfg.timeout_secs {
            break; // slow ROM: emit what we captured rather than error the pass
        }
    }

    // SGB border still: after the run the border (uploaded once, static) is
    // established, so grab the full 256x224 composited frame. `None` when the
    // game never sent a border (or on non-SGB hardware) — the caller then writes
    // no border still and that card renders as a normal 160x144 card. This is a
    // pure non-consuming read: it never perturbs the 160x144 frame path.
    let border = (hardware == Hardware::SGB)
        .then(|| gb.sgb_composited_frame())
        .flatten()
        .map(|f| f.to_vec());

    // Flush the PCM sidecar before muxing.
    if let Some(w) = pcm {
        let _ = w.into_inner().map(|mut f| f.flush());
    }
    // Close the encoder's stdin (EOF) and wait for the mp4 to finalize.
    let mut video_written = false;
    if let Some(mut child) = encoder {
        drop(child.stdin.take());
        video_written = matches!(child.wait(), Ok(s) if s.success());
    }
    if video_written
        && let (Some(vdir), Some(tv), Some(tp)) = (&cfg.videos_dir, &tmp_video, &tmp_pcm)
    {
        let final_mp4 = vdir.join(format!("{sha}_{tag}.mp4"));
        // Mux audio in; on failure keep the video-only mp4 (still emitted).
        if !(audio_samples > 0 && mux_av(tv, tp, &final_mp4)) {
            let _ = std::fs::rename(tv, &final_mp4);
        }
    }
    // Clean up temps (rename above may already have consumed tmp_video).
    if let Some(tv) = &tmp_video {
        let _ = std::fs::remove_file(tv);
    }
    if let Some(tp) = &tmp_pcm {
        let _ = std::fs::remove_file(tp);
    }

    Ok(MediaOut {
        poster,
        hash_all,
        border,
        frames: cfg.frames,
        audio_samples,
        video_written,
    })
}

// ---------------------------------------------------------------------------
// BIOS meta capture (one boot animation per hardware model, gallery-only)
// ---------------------------------------------------------------------------

/// Safety cap on a boot animation's length (~10s at 59.7fps): if the boot ROM
/// never hands off (wrong/corrupt dump), stop rather than loop forever.
const MAX_BIOS_FRAMES: usize = 600;

/// Boot-ROM filename per hardware model. Mirrors the test-runner's
/// `runner::bios_filename`; that module lives in a binary-only crate this bin
/// can't import, so the provisioned dumps are re-listed here. Keep in sync.
fn bios_filename(hw: Hardware) -> Option<&'static str> {
    match hw {
        Hardware::DMG => Some("dmg_boot.bin"),
        Hardware::CGB => Some("cgb_boot.bin"),
        // AGB uses the GBA's CGB-compat boot ROM (SameBoy naming: agb_boot.bin).
        Hardware::AGB => Some("agb_boot.bin"),
        Hardware::SGB => Some("sgb_boot.bin"),
        Hardware::DMG0 => Some("dmg0_boot.bin"),
        Hardware::MGB => Some("mgb_boot.bin"),
        Hardware::SGB2 => Some("sgb2_boot.bin"),
        Hardware::CGB0 => Some("cgb0_boot.bin"),
        Hardware::CGBE => Some("cgbE_boot.bin"),
        // CGB-A/B CPU revision shares the standard CGB boot ROM (no distinct dump).
        Hardware::CGBB => Some("cgb_boot.bin"),
    }
}

/// Locate a boot ROM the same way the test-runner does: `--bios-dir` first (if
/// given), then `bios/` relative to CWD, then `../bios/` relative to the crate
/// manifest. First existing path wins; None => the dump isn't provisioned.
fn resolve_bios_path(file: &str, bios_dir: Option<&Path>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = bios_dir {
        candidates.push(dir.join(file));
    }
    candidates.push(PathBuf::from("bios").join(file));
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("bios").join(file),
    );
    candidates.into_iter().find(|p| p.exists())
}

/// The 48-byte Nintendo logo the boot ROM checks the cart header against,
/// sourced AT RUNTIME from a canonical boot ROM in the bios dir that carries it
/// (dmg_boot.bin @0xA8, else cgb_boot.bin @0x42) — never embedded in source.
/// The logo is identical across every Nintendo boot ROM, so this one copy is
/// correct for ALL models. Needed because dmg0 keeps its copy at a different
/// offset (0xCB) and SGB/SGB2 don't embed the logo at all (the SNES side
/// verifies), so sourcing from the boot-ROM-under-test gives garbage for those.
fn canonical_boot_logo(bios_dir: Option<&Path>) -> Option<[u8; 48]> {
    for (file, off) in [("dmg_boot.bin", 0xA8usize), ("cgb_boot.bin", 0x42)] {
        let Some(p) = resolve_bios_path(file, bios_dir) else { continue };
        let Ok(b) = std::fs::read(&p) else { continue };
        if let Some(slice) = b.get(off..off + 48) {
            let mut logo = [0u8; 48];
            logo.copy_from_slice(slice);
            return Some(logo);
        }
    }
    None
}

/// Build a 32KB ROM-only cartridge whose header passes the boot ROM's logo +
/// header-checksum gate, so the real boot animation runs to handoff instead of
/// hanging. `logo` is the runtime-sourced Nintendo logo ([`canonical_boot_logo`])
/// — no logo bytes are ever embedded in committed source.
fn build_boot_cart(logo: &[u8; 48]) -> Vec<u8> {
    let mut cart = vec![0u8; 0x8000];
    // Entry point: NOP; JP $0150, then a quiet self-loop so the cart "runs"
    // silently once the boot ROM hands control to it.
    cart[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
    cart[0x150..0x152].copy_from_slice(&[0x18, 0xFE]); // JR -2 (spin in place)
    // Nintendo logo, runtime-sourced from the boot ROM (never committed).
    cart[0x104..0x134].copy_from_slice(logo);
    // Title 0x134..0x143 left zero: deterministic (the CGB boot's DMG-compat
    // palette hash is stable, and unused here since the cart declares CGB).
    cart[0x143] = 0x80; // CGB-compatible: CGB/AGB boot in colour; DMG/SGB ignore it
    cart[0x147] = 0x00; // ROM only
    cart[0x148] = 0x00; // 32KB
    cart[0x149] = 0x00; // no cart RAM
    // Header checksum over 0x134..=0x14C; the boot ROM hangs on a mismatch.
    let sum = cart[0x134..0x14D].iter().fold(0u8, |a, &b| a.wrapping_sub(b).wrapping_sub(1));
    cart[0x14D] = sum;
    // Global checksum (0x14E/0x14F) is NOT checked by the boot ROM; left zero.
    cart
}

struct BiosOut {
    frames: usize,
    audio_samples: usize,
    /// The frame index at which the boot ROM unmapped itself (FF50 0x00->0xFF).
    /// None => the run hit MAX_BIOS_FRAMES without handing off (bad dump).
    handoff_frame: Option<usize>,
    video_written: bool,
    /// FNV fold of every boot frame's hash — the deterministic regression key
    /// that goes into the manifest so `compare` flags boot-behavior drift. Stable
    /// run-to-run for a fixed boot ROM + synthetic cart.
    hash_all: u64,
    /// (frame, hash_all-snapshot) pairs every 64 frames + the final frame, for
    /// localizing where two boot captures diverge.
    checkpoints: Vec<(usize, u64)>,
}

/// Capture the boot animation for one hardware model: run the real boot ROM from
/// power-on (PC=0, no `skip_bios`) against a synthetic valid-logo cart, folding
/// every frame into a deterministic `hash_all` (the manifest regression key) and
/// — when `videos_dir` is set (ffmpeg present, videos enabled) — streaming every
/// frame to the H.264 encoder with the APU boot "ding" muxed in as AAC, exactly
/// the game-clip pipeline. Stops the frame after the boot ROM hands off (FF50
/// flips to 0xFF). Emits `screens/bios_<tag>.webp` when `screens_dir` is set and
/// `videos/bios_<tag>.mp4` when video is on; the `hash_all` regression row is
/// returned regardless of media so the gate is consistent with or without media.
///
/// Returns Ok(None) (logged once) when the model has no provisioned boot ROM or
/// the file is absent — that's how a missing dump stays skipped until the user
/// supplies it. Entirely separate from `emulate`: it builds its own cart and
/// never perturbs any game row or hash.
fn capture_bios(
    screens_dir: Option<&Path>,
    videos_dir: Option<&Path>,
    hw: Hardware,
    bios_dir: Option<&Path>,
    timeout_secs: u64,
) -> Result<Option<BiosOut>, String> {
    let tag = hw_tag(hw);
    let Some(file) = bios_filename(hw) else {
        return Ok(None); // no distinct dump for this model (revision variants)
    };
    let Some(path) = resolve_bios_path(file, bios_dir) else {
        eprintln!("sweep: no boot ROM ({file}) for {tag}, skipping BIOS meta");
        return Ok(None);
    };
    let Some(logo) = canonical_boot_logo(bios_dir) else {
        eprintln!("sweep: no boot ROM carrying the logo (dmg_boot.bin/cgb_boot.bin) in bios dir; skipping BIOS meta for {tag}");
        return Ok(None);
    };
    let cart_bytes = build_boot_cart(&logo);
    let cart = Cartridge::from_bytes(&cart_bytes).map_err(|e| format!("synthetic cart: {e}"))?;

    let mut gb = GB::new(hw);
    gb.insert(cart);
    // Validates the dump's CRC for `hw` (Part 1 accepts DMG/CGB/SGB/AGB); a wrong
    // or mismatched file Errs here and this model is skipped, not the whole run.
    gb.load_bios(&path.to_string_lossy()).map_err(|e| format!("load_bios: {e}"))?;
    // NO skip_bios: power-on PC=0 runs the boot ROM itself.

    // Video encoder + PCM sidecar (only when videos are on). The hash is computed
    // regardless, so the regression row exists even without ffmpeg.
    let (tmp_video, tmp_pcm) = match videos_dir {
        Some(d) => {
            let tmp = d.join(".sweep-tmp");
            let _ = std::fs::create_dir_all(&tmp);
            (Some(tmp.join(format!("bios_{tag}.mp4"))), Some(tmp.join(format!("bios_{tag}.pcm"))))
        }
        None => (None, None),
    };
    let mut encoder = None;
    let mut pcm = None;
    if let (Some(tv), Some(tp)) = (&tmp_video, &tmp_pcm) {
        match spawn_encoder(tv) {
            Ok(child) => {
                encoder = Some(child);
                pcm = std::fs::File::create(tp).ok().map(std::io::BufWriter::new);
            }
            Err(e) => eprintln!("sweep: ffmpeg spawn bios [{tag}]: {e}"),
        }
    }
    let collect_audio = encoder.is_some();
    let sink = SampleSink::default();
    if collect_audio {
        gb.enable_audio(Box::new(sink.clone())).map_err(|e| format!("audio: {e}"))?;
    }

    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash_all: u64 = 0xcbf2_9ce4_8422_2325;
    let mut checkpoints: Vec<(usize, u64)> = Vec::new();
    let mut poster: Option<Vec<u8>> = None;
    let mut audio_samples = 0usize;
    let mut handoff_frame = None;
    let mut frames = 0usize;
    let started = Instant::now();

    for f in 0..MAX_BIOS_FRAMES {
        let (frame, _bp) = gb.run_until_frame(collect_audio);
        frames = f + 1;
        let rgb = frame_rgb(&frame);
        if let Some(child) = &mut encoder
            && let Some(stdin) = child.stdin.as_mut()
            && stdin.write_all(&rgb).is_err()
        {
            encoder = None; // encoder died: keep the poster/hash, stop streaming
        }
        if collect_audio {
            let mut buf = sink.0.lock().unwrap();
            if !buf.is_empty() {
                audio_samples += buf.len();
                if let Some(w) = &mut pcm {
                    for (l, r) in buf.iter() {
                        let _ = w.write_all(&f32_to_s16le(*l));
                        let _ = w.write_all(&f32_to_s16le(*r));
                    }
                }
                buf.clear();
            }
        }
        let h = frame_hash(&gb, &frame);
        hash_all = (hash_all ^ h).wrapping_mul(FNV_PRIME);
        poster = Some(rgb);
        // Handoff: the boot ROM has written FF50 (read flips 0x00 -> 0xFF). Record
        // this frame (already folded/streamed) and stop.
        if gb.read_memory(0xFF50) == 0xFF {
            handoff_frame = Some(f);
            checkpoints.push((f + 1, hash_all));
            break;
        }
        if (f + 1) % 64 == 0 {
            checkpoints.push((f + 1, hash_all));
        }
        if started.elapsed().as_secs() > timeout_secs {
            checkpoints.push((f + 1, hash_all));
            break;
        }
    }
    if handoff_frame.is_none() {
        eprintln!("sweep: bios [{tag}] hit {frames}-frame cap without FF50 handoff; emitting anyway");
    }

    if let Some(w) = pcm {
        let _ = w.into_inner().map(|mut f| f.flush());
    }
    let mut video_written = false;
    if let Some(mut child) = encoder {
        drop(child.stdin.take());
        video_written = matches!(child.wait(), Ok(s) if s.success());
    }
    if video_written
        && let (Some(vdir), Some(tv), Some(tp)) = (videos_dir, &tmp_video, &tmp_pcm)
    {
        let final_mp4 = vdir.join(format!("bios_{tag}.mp4"));
        if !(audio_samples > 0 && mux_av(tv, tp, &final_mp4)) {
            let _ = std::fs::rename(tv, &final_mp4);
        }
    }
    if let Some(tv) = &tmp_video {
        let _ = std::fs::remove_file(tv);
    }
    if let Some(tp) = &tmp_pcm {
        let _ = std::fs::remove_file(tp);
    }

    // Poster = the handoff frame (or the last captured frame if capped). Gated on
    // screens being enabled; the hash row below is emitted regardless.
    if let Some(sdir) = screens_dir {
        write_poster_png(sdir, "bios", &tag, poster.as_ref());
    }

    Ok(Some(BiosOut {
        frames,
        audio_samples,
        handoff_frame,
        video_written,
        hash_all,
        checkpoints,
    }))
}

// ---------------------------------------------------------------------------
// compare
// ---------------------------------------------------------------------------

fn cmd_compare(args: &[String]) -> Result<bool, String> {
    let base_path = arg(args, "--base").ok_or("compare: --base <manifest> required")?;
    let cand_path = arg(args, "--cand").ok_or("compare: --cand <manifest> required")?;
    let min_ratio: f64 = arg(args, "--min-ratio").map_or(Ok(0.90), |v| {
        v.parse().map_err(|_| format!("bad --min-ratio {v:?}"))
    })?;
    let ignore_perf = args.iter().any(|a| a == "--ignore-perf");
    let out = arg(args, "--out");

    let base = load_manifest(&base_path)?;
    let cand = load_manifest(&cand_path)?;
    // Join on rom_sha, not the (trademarked, possibly-absent) title. The
    // committed baseline carries no names; the candidate (a fresh sweep) does,
    // so reports prefer the candidate's name and fall back to `sha:<hash>`.
    let cand_map: std::collections::BTreeMap<&str, &Row> =
        cand.iter().map(|r| (r.rom_sha.as_str(), r)).collect();
    let base_shas: std::collections::BTreeSet<&str> =
        base.iter().map(|r| r.rom_sha.as_str()).collect();
    let label = |r: &Row| -> String {
        r.name.clone().unwrap_or_else(|| format!("sha:{}", r.rom_sha))
    };

    let mut missing = Vec::new(); // sha in base, not in cand
    let extra: Vec<String> = cand
        .iter()
        .filter(|r| !base_shas.contains(r.rom_sha.as_str()))
        .map(&label)
        .collect();
    let mut new_errors = Vec::new();
    let mut fixed_errors = 0usize;
    let mut behavior = Vec::new();
    let mut ratios: Vec<(f64, String)> = Vec::new();

    for b in &base {
        let Some(c) = cand_map.get(b.rom_sha.as_str()) else {
            missing.push(label(b));
            continue;
        };
        match (&b.error, &c.error) {
            (None, Some(e)) => {
                new_errors.push(format!("{}: {e}", label(c)));
                continue;
            }
            (Some(_), None) => fixed_errors += 1,
            (Some(_), Some(_)) => continue,
            (None, None) => {}
        }
        if b.hash_all != c.hash_all {
            let first_diff = b
                .checkpoints
                .iter()
                .zip(&c.checkpoints)
                .find(|(x, y)| x.1 != y.1)
                .map(|(x, _)| x.0);
            behavior.push(match first_diff {
                Some(fr) => format!("{}: diverges by frame {fr}", label(c)),
                None => format!("{}: diverges after last checkpoint", label(c)),
            });
        }
        if let (Some(bf), Some(cf)) = (b.fps, c.fps)
            && bf > 0.0
        {
            ratios.push((cf / bf, label(c)));
        }
    }

    // Machine/load differences shift ALL ratios together; normalizing by the
    // median isolates per-ROM regressions from global scale. The median itself
    // is reported so a global slowdown is still visible.
    let median_ratio = median(ratios.iter().map(|(r, _)| *r).collect());
    let mut slow: Vec<(f64, &str)> = Vec::new();
    if let Some(m) = median_ratio
        && m > 0.0
    {
        slow = ratios
            .iter()
            .filter(|(r, _)| r / m < min_ratio)
            .map(|(r, k)| (r / m, k.as_str()))
            .collect();
        slow.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    }

    let mut report = String::new();
    report.push_str(&format!(
        "# sweep compare\n\nbase: `{base_path}` ({} ROMs)\ncand: `{cand_path}` ({} ROMs)\n\n",
        base.len(),
        cand.len()
    ));
    report.push_str(&format!(
        "| check | result |\n|---|---|\n| behavior (hash) mismatches | {} |\n\
         | new errors | {} |\n| fixed errors | {} |\n| missing ROMs (in base, gone from cand) | {} |\n\
         | extra ROMs (new in cand) | {} |\n\
         | median fps ratio (cand/base) | {} |\n| per-ROM slow outliers (<{min_ratio:.2}× median-normalized) | {} |\n\n",
        behavior.len(),
        new_errors.len(),
        fixed_errors,
        missing.len(),
        extra.len(),
        median_ratio.map_or("n/a".into(), |m| format!("{m:.3}")),
        if ignore_perf { "ignored".to_string() } else { slow.len().to_string() },
    ));
    let mut section = |title: &str, items: &[String]| {
        if !items.is_empty() {
            report.push_str(&format!("## {title} ({})\n\n", items.len()));
            for i in items.iter().take(200) {
                report.push_str(&format!("- {i}\n"));
            }
            if items.len() > 200 {
                report.push_str(&format!("- … {} more\n", items.len() - 200));
            }
            report.push('\n');
        }
    };
    section("Behavior mismatches", &behavior);
    section("New errors", &new_errors);
    section("Missing ROMs", &missing);
    section("Extra ROMs", &extra);
    if !ignore_perf {
        section(
            "Slow outliers (median-normalized)",
            &slow
                .iter()
                .map(|(r, k)| format!("{k}: {r:.3}×"))
                .collect::<Vec<_>>(),
        );
    }

    print!("{report}");
    if let Some(out) = out {
        std::fs::write(&out, &report).map_err(|e| format!("write {out}: {e}"))?;
    }

    let perf_fail = !ignore_perf && !slow.is_empty();
    let ok = behavior.is_empty() && new_errors.is_empty() && missing.is_empty() && !perf_fail;
    println!("compare: {}", if ok { "OK" } else { "REGRESSIONS FOUND" });
    Ok(ok)
}

fn load_manifest(path: &str) -> Result<Vec<Row>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(|e| format!("{path}: {e}")))
        .collect()
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some(v[v.len() / 2])
}

// ---------------------------------------------------------------------------
// region classifier
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Region {
    Us,
    Japan,
    Europe,
    Global,
}

impl Region {
    /// Filter/data-attribute key. Kept in sync with the gallery chip order.
    const ALL: [Region; 4] = [Region::Us, Region::Japan, Region::Europe, Region::Global];
    fn key(self) -> &'static str {
        match self {
            Region::Us => "us",
            Region::Japan => "jp",
            Region::Europe => "eu",
            Region::Global => "global",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Region::Us => "US",
            Region::Japan => "Japan",
            Region::Europe => "Europe",
            Region::Global => "Global",
        }
    }
}

/// One lowercased No-Intro region token -> bucket. Region words only; language
/// codes (en, fr, ja, …) are deliberately absent so they never register.
fn region_token(tok: &str) -> Option<Region> {
    Some(match tok {
        "usa" | "u" => Region::Us,
        "japan" | "j" => Region::Japan,
        "europe" | "e" | "uk" | "united kingdom" | "great britain" | "france" | "germany"
        | "spain" | "italy" | "netherlands" | "sweden" | "norway" | "denmark" | "finland"
        | "portugal" | "greece" | "ireland" | "poland" | "russia" | "belgium" | "austria"
        | "switzerland" | "australia" | "new zealand" | "scandinavia" => Region::Europe,
        "world" | "w" | "taiwan" | "hong kong" | "china" | "korea" | "asia" | "brazil"
        | "canada" | "mexico" => Region::Global,
        _ => return None,
    })
}

/// GoodTools region combo like `(UE)` / `(JUE)`: 2-5 of the region letters.
fn is_goodtools_combo(tok: &str) -> bool {
    (2..=5).contains(&tok.len()) && tok.bytes().all(|b| b"UEJWFGSIABKCDN".contains(&b))
}

/// Bucket a No-Intro filename/title by its specific region paren token, exactly
/// like `tools/sort-by-region.py`: the region is a *specific* paren token, not
/// "the first paren" — `(Rev 1)`, `(SGB Enhanced)` and language lists like
/// `(En,Fr,De)` are skipped. A multi-region list or a no-token name -> Global.
fn region_of(name: &str) -> Region {
    let mut rest = name;
    while let Some(open) = rest.find('(') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(')') else { break };
        let tok = after[..close].trim();
        rest = &after[close + 1..];
        if tok.contains(',') {
            let regions: Vec<Region> = tok
                .split(',')
                .filter_map(|p| region_token(p.trim().to_ascii_lowercase().as_str()))
                .collect();
            if regions.len() >= 2 {
                return Region::Global; // multi-region list
            }
            if let [only] = regions[..] {
                return only;
            }
            // else a language list etc. — not a region; keep scanning.
        } else {
            if let Some(r) = region_token(tok.to_ascii_lowercase().as_str()) {
                return r;
            }
            if is_goodtools_combo(tok) {
                return Region::Global;
            }
        }
    }
    Region::Global
}

/// Human ROM/RAM size. GB sizes are powers of two so the shifts are exact; the
/// fractional arms only guard odd bank counts.
fn human_bytes(n: usize) -> String {
    const K: usize = 1 << 10;
    const M: usize = 1 << 20;
    match n {
        0 => "—".to_string(),
        n if n.is_multiple_of(M) => format!("{} MiB", n / M),
        n if n >= M => format!("{:.1} MiB", n as f64 / M as f64),
        n if n.is_multiple_of(K) => format!("{} KiB", n / K),
        n if n >= K => format!("{:.1} KiB", n as f64 / K as f64),
        n => format!("{n} B"),
    }
}

/// Feature-glyph chips decoded from a `Row.cart_flags` bitfield.
fn flag_glyphs(flags: u16) -> String {
    let mut g = String::new();
    if flags & (1 << 0) != 0 { g.push_str("<span class=\"g bat\">🔋 BAT</span>"); }
    if flags & (1 << 1) != 0 { g.push_str("<span class=\"g rtc\">⏱ RTC</span>"); }
    if flags & (1 << 2) != 0 { g.push_str("<span class=\"g rum\">〜 RMBL</span>"); }
    if flags & (1 << 3) != 0 { g.push_str("<span class=\"g cam\">📷 CAM</span>"); }
    if flags & (1 << 5) != 0 {
        g.push_str("<span class=\"g cgb\">CGB only</span>");
    } else if flags & (1 << 4) != 0 {
        g.push_str("<span class=\"g cgb\">CGB</span>");
    }
    if flags & (1 << 6) != 0 { g.push_str("<span class=\"g sgb\">SGB</span>"); }
    if flags & (1 << 7) != 0 { g.push_str("<span class=\"g unl\">UNL</span>"); }
    g
}

/// The always-visible enrichment strip (mapper · crc · sizes · glyphs · pub).
/// Empty when the row carries no captured header facts (old manifest) so cards
/// degrade gracefully.
fn card_detail(r: &Row) -> String {
    if r.mapper.is_none() && r.rom_bytes.is_none() && r.cart_flags.is_none() {
        return String::new();
    }
    let mut facts = String::new();
    if let Some(m) = &r.mapper {
        facts.push_str(&format!("<span class=\"fact mapper\">{}</span>", html_escape(m)));
    }
    if let Some(c) = &r.crc {
        facts.push_str(&format!(
            "<span class=\"fact\"><span class=\"k\">crc</span> {}</span>",
            html_escape(&c.to_uppercase())
        ));
    }
    if let Some(n) = r.rom_bytes {
        facts.push_str(&format!(
            "<span class=\"fact\"><span class=\"k\">rom</span> {}</span>",
            human_bytes(n)
        ));
    }
    if let Some(n) = r.ram_bytes.filter(|&n| n > 0) {
        facts.push_str(&format!(
            "<span class=\"fact\"><span class=\"k\">ram</span> {}</span>",
            human_bytes(n)
        ));
    }
    let glyphs = r.cart_flags.map(flag_glyphs).unwrap_or_default();
    let glyph_row = if glyphs.is_empty() {
        String::new()
    } else {
        format!("<div class=\"glyphs\">{glyphs}</div>")
    };
    let mut pubs: Vec<String> = Vec::new();
    if let Some(l) = &r.licensee {
        pubs.push(html_escape(l));
    }
    if let Some(d) = &r.destination {
        pubs.push(if d == "jp" { "Japan".into() } else { "Overseas".into() });
    }
    let pub_row = if pubs.is_empty() {
        String::new()
    } else {
        format!("<div class=\"pub\"><b>{}</b></div>", pubs.join(" · "))
    };
    format!("<div class=\"detail\"><div class=\"facts\">{facts}</div>{glyph_row}{pub_row}</div>")
}

// ---------------------------------------------------------------------------
// gallery
// ---------------------------------------------------------------------------

fn cmd_gallery(args: &[String]) -> Result<bool, String> {
    let manifest = arg(args, "--manifest").ok_or("gallery: --manifest <file> required")?;
    let screens = arg(args, "--screens").ok_or("gallery: --screens <dir> required")?;
    let screens_dir = PathBuf::from(&screens);
    // Videos default to the screens dir's sibling `videos/`.
    let videos_dir = arg(args, "--videos").map(PathBuf::from).unwrap_or_else(|| {
        screens_dir
            .parent()
            .map(|p| p.join("videos"))
            .unwrap_or_else(|| PathBuf::from("videos"))
    });
    let out = arg(args, "--out").unwrap_or_else(|| "gallery.html".into());

    let mut rows = load_manifest(&manifest)?;
    // Resolve/refresh No-Intro names from each row's stored crc, so a manifest
    // swept before the DATs were cached still gets proper titles here — no
    // re-sweep needed.
    let names = nointro_map();
    if !names.is_empty() {
        for r in &mut rows {
            if let Some(crc) = r.crc.as_ref().and_then(|c| u32::from_str_radix(c, 16).ok())
                && let Some(nointro) = names.get(&crc)
            {
                r.name = Some(nointro.clone());
            }
        }
    }
    // BIOS meta rows (rom_sha `bios_<tag>`) are regression rows, not gameplay —
    // exclude them from the header's gameplay count and the fps median.
    let is_bios = |r: &Row| r.rom_sha.starts_with("bios_");
    let ok = rows
        .iter()
        .filter(|r| !is_bios(r) && r.error.is_none() && r.boot_ok && r.changed)
        .count();
    let med = median(rows.iter().filter(|r| !is_bios(r)).filter_map(|r| r.fps).collect());

    // Media is probed off disk purely by rom_sha (never the filename): scan the
    // screens dir for `<sha>_<tag>.webp` posters (any legacy `_cpN` files skipped)
    // to learn which hardware tabs actually have content. This keeps the compared
    // manifest schema untouched — no per-hardware rows, no media sidecar.
    let mut poster_tags: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    let mut tabs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // SGB border stills (`<sha>_sgb_border.webp`, 256x224): not a hardware tab —
    // tracked separately so the SGB tab can frame the 160x144 video inside it.
    let mut border_shas: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Ok(rd) = std::fs::read_dir(&screens_dir) {
        for e in rd.flatten() {
            let fname = e.file_name().to_string_lossy().into_owned();
            let Some(stem) = fname.strip_suffix(".webp") else { continue };
            if stem.contains("_cp") || stem.len() < 18 {
                continue; // legacy checkpoint frame, or too short to be sha_tag
            }
            let (sha, rest) = stem.split_at(16);
            let Some(tag) = rest.strip_prefix('_') else { continue };
            if tag.is_empty() || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            if tag == "sgb_border" {
                border_shas.insert(sha.to_string());
                continue; // display detail of the sgb tab, not its own tab
            }
            poster_tags.entry(sha.to_string()).or_default().insert(tag.to_string());
            tabs.insert(tag.to_string());
        }
    }
    // Preferred tab order (default matrix first), then any others alphabetically.
    let order = ["dmg", "cgb", "sgb", "agb"];
    let mut tab_list: Vec<String> = order.iter().filter(|t| tabs.contains(**t)).map(|t| t.to_string()).collect();
    for t in &tabs {
        if !tab_list.contains(t) {
            tab_list.push(t.clone());
        }
    }

    let mut s = String::new();
    s.push_str("<!doctype html><html><head><meta charset=\"utf-8\">");
    s.push_str("<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">");
    s.push_str("<title>rustyboi library sweep</title><style>");
    s.push_str(include_str!("gallery.css"));
    s.push_str("</style></head><body>");
    s.push_str(&format!(
        "<h1>rustyboi library sweep &mdash; {ok}/{} in gameplay, median {:.0} fps</h1>",
        rows.iter().filter(|r| !is_bios(r)).count(),
        med.unwrap_or(0.0)
    ));

    if tab_list.is_empty() {
        // Old-format dir (sanitize-named PNGs) or no media: still valid HTML.
        s.push_str("<p class=\"empty\">No rom_sha-keyed media found in this directory. \
                    Re-run <code>sweep run</code> to regenerate media (filenames changed to \
                    rom_sha).</p></body></html>");
        std::fs::write(&out, &s).map_err(|e| format!("write {out}: {e}"))?;
        println!("gallery: 0 cards (no media) -> {out}");
        return Ok(true);
    }

    // Region per row (from the No-Intro name), computed once and reused by both
    // the dashboard and the per-tab grouping.
    let regions: Vec<Region> = rows
        .iter()
        .map(|r| r.name.as_deref().map(region_of).unwrap_or(Region::Global))
        .collect();

    // Whole-library dashboard: status split, region split, top mappers. Counted
    // over game rows that actually have media (a card somewhere).
    let (mut n_ok, mut n_blank, mut n_err) = (0usize, 0usize, 0usize);
    let mut reg_counts = [0usize; 4];
    let mut disp = 0usize;
    let mut mapper_counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for (idx, r) in rows.iter().enumerate() {
        if is_bios(r) || !poster_tags.contains_key(&r.rom_sha) {
            continue;
        }
        disp += 1;
        match (&r.error, r.boot_ok && r.changed) {
            (Some(_), _) => n_err += 1,
            (None, true) => n_ok += 1,
            (None, false) => n_blank += 1,
        }
        if let Some(i) = Region::ALL.iter().position(|x| *x == regions[idx]) {
            reg_counts[i] += 1;
        }
        if let Some(m) = &r.mapper {
            *mapper_counts.entry(m.as_str()).or_default() += 1;
        }
    }
    let mut top_mappers: Vec<(&str, usize)> = mapper_counts.into_iter().collect();
    top_mappers.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let mapper_summary = top_mappers
        .iter()
        .take(6)
        .map(|(m, c)| format!("{} <b>{c}</b>", html_escape(m)))
        .collect::<Vec<_>>()
        .join(" &middot; ");
    s.push_str(&format!(
        "<div class=\"dash\">\
         <span class=\"stat\"><b>{disp}</b> games</span>\
         <span class=\"stat\"><b class=\"ok\">{n_ok}</b> ok &middot; <b class=\"fail\">{n_blank}</b> blank &middot; <b class=\"err\">{n_err}</b> error</span>\
         <span class=\"stat\">US <b>{}</b> &middot; JP <b>{}</b> &middot; EU <b>{}</b> &middot; GL <b>{}</b></span>{}</div>",
        reg_counts[0], reg_counts[1], reg_counts[2], reg_counts[3],
        if mapper_summary.is_empty() {
            String::new()
        } else {
            format!("<span class=\"stat mp\">mappers: {mapper_summary}</span>")
        },
    ));

    // Sticky control bar: search / sort / toggles, then tabs, then region chips.
    s.push_str(
        "<div class=\"bar\"><div class=\"toolbar\">\
         <input type=\"search\" id=\"q\" placeholder=\"Search titles\u{2026}\" autocomplete=\"off\">\
         <select id=\"sort\">\
         <option value=\"name\">Name A\u{2013}Z</option>\
         <option value=\"fps\">FPS \u{2193}</option>\
         <option value=\"size\">Size \u{2193}</option>\
         <option value=\"status\">Status</option>\
         <option value=\"mapper\">Mapper</option>\
         </select>\
         <label><input type=\"checkbox\" id=\"failonly\"> failures only</label>\
         <label><input type=\"checkbox\" id=\"dense\"> dense</label>\
         <span class=\"count\" id=\"count\"></span></div><div class=\"tabs\">",
    );
    for (i, tag) in tab_list.iter().enumerate() {
        let count = rows
            .iter()
            .filter(|r| poster_tags.get(&r.rom_sha).is_some_and(|t| t.contains(tag)))
            .count();
        s.push_str(&format!(
            "<button class=\"tab{}\" data-tab=\"{tag}\">{} ({count})</button>",
            if i == 0 { " active" } else { "" },
            html_escape(&tag.to_ascii_uppercase()),
        ));
    }
    s.push_str("</div><div class=\"chips\"><button class=\"chip active\" data-region=\"all\">All</button>");
    for region in Region::ALL {
        s.push_str(&format!(
            "<button class=\"chip\" data-region=\"{}\">{}</button>",
            region.key(),
            region.label(),
        ));
    }
    s.push_str("</div></div>");

    // One panel per hardware tab; within it, cards grouped into foldable region
    // sections. The BIOS meta card sits in its own always-visible row.
    for (i, tag) in tab_list.iter().enumerate() {
        s.push_str(&format!(
            "<div class=\"tab-panel{}\" id=\"panel-{tag}\">",
            if i == 0 { " active" } else { "" }
        ));
        // Pinned BIOS meta card (boot animation, key `bios_<tag>`): a lazy hero
        // just like a game card, degrading to a still or absent (old dirs).
        let bios_poster = format!("./screens/bios_{tag}.webp");
        let bios_video_file = format!("bios_{tag}.mp4");
        let bios_has_video = videos_dir.join(&bios_video_file).exists();
        let bios_has_poster = screens_dir.join(format!("bios_{tag}.webp")).exists();
        if bios_has_video || bios_has_poster {
            let hero = if bios_has_video {
                format!(
                    "<video class=\"hero\" muted loop playsinline preload=\"none\" \
                     data-poster=\"{bios_poster}\" data-src=\"./videos/{}\"></video>",
                    html_escape(&bios_video_file),
                )
            } else {
                format!("<img class=\"hero\" loading=\"lazy\" src=\"{bios_poster}\" alt=\"\">")
            };
            s.push_str(&format!(
                "<div class=\"biosrow\"><div class=\"grid\"><div class=\"card meta-card\">{hero}\
                 <div class=\"name\"><span class=\"meta-badge\">\u{25c8} Boot ROM \u{2014} {}</span></div>\
                 <div class=\"meta\"><span>{} boot animation</span><span class=\"ok\">meta</span></div></div></div></div>",
                html_escape(&tag.to_ascii_uppercase()),
                html_escape(&tag.to_ascii_uppercase()),
            ));
        }
        for region in Region::ALL {
            let group: Vec<&Row> = rows
                .iter()
                .enumerate()
                .filter(|(idx, r)| {
                    regions[*idx] == region
                        && poster_tags.get(&r.rom_sha).is_some_and(|t| t.contains(tag))
                })
                .map(|(_, r)| r)
                .collect();
            if group.is_empty() {
                continue;
            }
            s.push_str(&format!(
                "<section class=\"region-group\" data-region=\"{}\">\
                 <h2 class=\"region-head\"><span class=\"car\">\u{25be}</span>{} <span class=\"rc\">({})</span></h2>\
                 <div class=\"grid\">",
                region.key(),
                region.label(),
                group.len(),
            ));
            for r in &group {
                let (status, cls) = match (&r.error, r.boot_ok && r.changed) {
                    (Some(_), _) => ("error", "err"),
                    (None, true) => ("ok", "ok"),
                    (None, false) => ("blank/static", "fail"),
                };
                let poster = format!("./screens/{}_{tag}.webp", r.rom_sha);
                let video_file = format!("{}_{tag}.mp4", r.rom_sha);
                let has_video = videos_dir.join(&video_file).exists();
                let sgb_border = tag == "sgb" && border_shas.contains(&r.rom_sha);
                let border_src = format!("./screens/{}_sgb_border.webp", r.rom_sha);
                let hero = if sgb_border && has_video {
                    format!(
                        "<div class=\"sgb-frame\">\
                         <img class=\"sgb-border\" loading=\"lazy\" src=\"{border_src}\" alt=\"\">\
                         <video class=\"hero sgb-screen\" muted loop playsinline preload=\"none\" \
                         data-poster=\"{poster}\" data-src=\"./videos/{}\"></video></div>",
                        html_escape(&video_file),
                    )
                } else if sgb_border {
                    format!("<img class=\"hero\" loading=\"lazy\" src=\"{border_src}\" alt=\"\">")
                } else if has_video {
                    // Lazy: the observer assigns poster/src from data-* near the
                    // viewport, so the page loads O(viewport) not O(page).
                    format!(
                        "<video class=\"hero\" muted loop playsinline preload=\"none\" \
                         data-poster=\"{poster}\" data-src=\"./videos/{}\"></video>",
                        html_escape(&video_file),
                    )
                } else {
                    format!("<img class=\"hero\" loading=\"lazy\" src=\"{poster}\" alt=\"\">")
                };
                let display_name = r.name.clone().unwrap_or_else(|| format!("sha:{}", r.rom_sha));
                let fps_txt = r.fps.map_or(String::new(), |f| format!("{f:.0} fps"));
                let fps_int = r.fps.unwrap_or(0.0) as i64;
                let size_attr = r.rom_bytes.unwrap_or(0);
                let id = format!("{tag}-{}", r.rom_sha);
                let detail = card_detail(r);
                s.push_str(&format!(
                    "<div class=\"card\" id=\"{id}\" data-name=\"{}\" data-region=\"{}\" \
                     data-status=\"{cls}\" data-mapper=\"{}\" data-size=\"{size_attr}\" data-fps=\"{fps_int}\">\
                     {hero}<div class=\"name\">{}</div>\
                     <div class=\"meta\"><span>{} {fps_txt}</span><span class=\"{cls}\">{status}</span></div>\
                     {detail}\
                     <div class=\"linkrow\"><button class=\"lk\" data-id=\"{id}\">\u{1f517} link</button></div></div>",
                    html_escape(&display_name.to_lowercase()),
                    region.key(),
                    html_escape(r.mapper.as_deref().unwrap_or("")),
                    html_escape(&display_name),
                    html_escape(&tag.to_ascii_uppercase()),
                ));
            }
            s.push_str("</div></section>");
        }
        s.push_str("</div>");
    }

    // Dependency-free behavior (lazy media + filter/sort/collapse/deep-link).
    // The IntersectionObserver still drives LOADING so only near-viewport media
    // is fetched; hidden/collapsed cards are display:none, so it releases them.
    s.push_str("<script>\n");
    s.push_str(include_str!("gallery.js"));
    s.push_str("\n</script>");
    s.push_str("</body></html>");
    std::fs::write(&out, &s).map_err(|e| format!("write {out}: {e}"))?;
    let cards: usize = tab_list
        .iter()
        .map(|tag| {
            rows.iter()
                .filter(|r| poster_tags.get(&r.rom_sha).is_some_and(|t| t.contains(tag)))
                .count()
        })
        .sum();
    println!("gallery: {} tabs, {cards} cards -> {out}", tab_list.len());
    Ok(true)
}

// ---------------------------------------------------------------------------
// arg helpers
// ---------------------------------------------------------------------------

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

/// All values following `--flag` up to the next `--` flag (so `--roms A B C`).
fn multi_arg(args: &[String], name: &str) -> Vec<String> {
    let Some(i) = args.iter().position(|a| a == name) else {
        return Vec::new();
    };
    args[i + 1..]
        .iter()
        .take_while(|a| !a.starts_with("--"))
        .cloned()
        .collect()
}

fn parse_num(args: &[String], name: &str, default: usize) -> Result<usize, String> {
    match arg(args, name) {
        Some(v) => v.parse().map_err(|_| format!("bad {name} {v:?}")),
        None => Ok(default),
    }
}

/// Parse `--shard K/N` and reduce `roms` to the K-th of N interleaved slices
/// (`index % n == k-1`, 1-based K). Interleaving (not chunking) keeps shard
/// runtimes balanced when per-ROM cost varies along the sorted list, so CI can
/// fan one sweep across N processes and concatenate the partial manifests back
/// into the full library. Mirrors `apply_shard` in the test-suite runner.
fn apply_shard(roms: &mut Vec<(String, PathBuf)>, shard: &str) -> Result<(), String> {
    let (k, n) = shard
        .split_once('/')
        .and_then(|(k, n)| Some((k.parse::<usize>().ok()?, n.parse::<usize>().ok()?)))
        .ok_or_else(|| format!("invalid --shard '{shard}' (expected K/N, e.g. 2/4)"))?;
    if k == 0 || n == 0 || k > n {
        return Err(format!("invalid --shard '{shard}' (need 1 <= K <= N)"));
    }
    let mut index = 0usize;
    roms.retain(|_| {
        let keep = index % n == k - 1;
        index += 1;
        keep
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_classifier_matches_python() {
        let cases: &[(&str, Region)] = &[
            ("Tetris (USA)", Region::Us),
            ("Kirby (U)", Region::Us),
            ("Zelda (Europe)", Region::Europe),
            ("Game (France)", Region::Europe),
            ("Mario (Japan)", Region::Japan),
            ("Foo (J)", Region::Japan),
            ("Bar (World)", Region::Global),
            ("Baz (USA, Europe)", Region::Global),
            ("Qux (Japan, USA)", Region::Global),
            ("Wobble (Taiwan)", Region::Global),
            ("Combo (UE)", Region::Global),
            ("Combo (JUE)", Region::Global),
            // Language lists and revision/enhancement tokens are not regions.
            ("Multi (En,Fr,De)", Region::Global),
            ("Game (USA) (Rev 1)", Region::Us),
            ("Game (Europe) (SGB Enhanced)", Region::Europe),
            ("NoTokenHere", Region::Global),
            // Specific region wins even when a language list precedes it.
            ("Game (En,Fr,De) (Europe)", Region::Europe),
        ];
        for &(name, want) in cases {
            assert_eq!(region_of(name), want, "{name:?}");
        }
    }

    /// Minimal single-file STORED (uncompressed) zip. Well-formed enough for
    /// `Cartridge::extract_rom_bytes` to unzip, without pulling in a zip writer.
    fn stored_zip(name: &str, data: &[u8]) -> Vec<u8> {
        let crc = crc32(data);
        let nlen = name.len() as u16;
        let size = data.len() as u32;
        let mut z = Vec::new();
        // Local file header.
        z.extend_from_slice(b"PK\x03\x04");
        z.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // ver, flags, store, time, date
        z.extend_from_slice(&crc.to_le_bytes());
        z.extend_from_slice(&size.to_le_bytes()); // compressed size
        z.extend_from_slice(&size.to_le_bytes()); // uncompressed size
        z.extend_from_slice(&nlen.to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes()); // extra len
        z.extend_from_slice(name.as_bytes());
        z.extend_from_slice(data);
        // Central directory.
        let cd_off = z.len() as u32;
        let mut cd = Vec::new();
        cd.extend_from_slice(b"PK\x01\x02");
        cd.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // made/needed, flags, store, time, date
        cd.extend_from_slice(&crc.to_le_bytes());
        cd.extend_from_slice(&size.to_le_bytes());
        cd.extend_from_slice(&size.to_le_bytes());
        cd.extend_from_slice(&nlen.to_le_bytes());
        cd.extend_from_slice(&[0u8; 2]); // extra
        cd.extend_from_slice(&[0u8; 2]); // comment
        cd.extend_from_slice(&[0u8; 2]); // disk start
        cd.extend_from_slice(&[0u8; 2]); // internal attrs
        cd.extend_from_slice(&[0u8; 4]); // external attrs
        cd.extend_from_slice(&0u32.to_le_bytes()); // local header offset
        cd.extend_from_slice(name.as_bytes());
        let cd_size = cd.len() as u32;
        z.extend_from_slice(&cd);
        // End of central directory.
        z.extend_from_slice(b"PK\x05\x06");
        z.extend_from_slice(&[0u8; 4]); // disk numbers
        z.extend_from_slice(&1u16.to_le_bytes()); // entries on this disk
        z.extend_from_slice(&1u16.to_le_bytes()); // total entries
        z.extend_from_slice(&cd_size.to_le_bytes());
        z.extend_from_slice(&cd_off.to_le_bytes());
        z.extend_from_slice(&0u16.to_le_bytes()); // comment len
        z
    }

    #[test]
    fn rom_sha_is_container_independent() {
        // A byte pattern standing in for a ROM image.
        let rom: Vec<u8> = (0..4096u32).map(|i| (i.wrapping_mul(31) ^ 0xa5) as u8).collect();

        let bare = rom_sha(&rom);
        let zipped = rom_sha(&stored_zip("game.gb", &rom));
        let renamed = rom_sha(&stored_zip("other-name.gbc", &rom));

        // Bare vs zipped vs a differently-named zip of the same ROM: one identity.
        assert_eq!(bare, zipped, "zipping a ROM must not change its rom_sha");
        assert_eq!(bare, renamed, "the archive's inner filename must not matter");
        assert_eq!(bare.len(), 16, "rom_sha is 8 bytes hex-encoded");

        // A different ROM must key differently.
        let other: Vec<u8> = rom.iter().map(|b| b ^ 0xff).collect();
        assert_ne!(bare, rom_sha(&other));
    }

    #[test]
    fn shards_partition_roms_exactly() {
        let all: Vec<(String, PathBuf)> =
            (0..10).map(|i| (format!("{i:02}"), PathBuf::from(format!("{i}.gb")))).collect();

        // Every ROM lands in exactly one of the 3 interleaved shards.
        let mut seen = Vec::new();
        for k in 1..=3 {
            let mut shard = all.clone();
            apply_shard(&mut shard, &format!("{k}/3")).unwrap();
            for (key, _) in &shard {
                assert!(!seen.contains(key), "{key} duplicated across shards");
                seen.push(key.clone());
            }
        }
        assert_eq!(seen.len(), 10, "shards must cover the whole list exactly once");

        // 1/1 is the whole library unchanged.
        let mut whole = all.clone();
        apply_shard(&mut whole, "1/1").unwrap();
        assert_eq!(whole, all);

        for bad in ["0/4", "5/4", "x/4", "2", "2/0"] {
            let mut v = all.clone();
            assert!(apply_shard(&mut v, bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn rom_sha_falls_back_for_non_archive() {
        // Not a PK zip: hashed as-is, and identical to a direct sha256 prefix.
        let raw = b"\x00\x01\x02\x03 not a zip";
        let want: String = sha256(raw)[..8].iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(rom_sha(raw), want);
    }
}
