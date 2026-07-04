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
//! ROM's `rom_sha`, never the (trademarked) file name — `screens/<sha>_<hw>.png`
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
//!                  [--strip-names] [--only SUBSTR]
//!       Sweep the library into DIR/manifest.jsonl + DIR/screens/*.png.
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
use rustyboi_core_lib::cartridge::Cartridge;
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
mod imaging;
#[path = "shared/masher.rs"]
mod masher;
use masher::masher;
use imaging::{encode_rgb_png, frame_rgb, html_escape};

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
                 [--hardware dmg,cgb,sgb,agb] [--only SUBSTR]\n  \
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

/// CRC32 (reflected, poly 0xEDB88320) — the checksum No-Intro DATs key on.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
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
    let strip_names = args.iter().any(|a| a == "--strip-names");
    let only = arg(args, "--only");
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
        None => vec![Hardware::DMG, Hardware::CGB, Hardware::SGB, Hardware::AGB],
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
        for &hw in &cfg.hardware {
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
        }
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
        let h = frame_hash(&frame);
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
            if frame_is_non_blank(&frame) {
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

/// H.264 encoder reading rawvideo rgb24 over stdin -> a temp mp4. H.264 plays
/// in every browser (HEVC did not). `-threads 1` caps x264 since the sweep
/// already parallelizes across ROMs; `-g 300` (long GOP) + `veryslow`/`crf 28`
/// exploit the heavy temporal redundancy of tiny 160x144 menu-heavy clips.
fn spawn_encoder(out: &Path) -> std::io::Result<std::process::Child> {
    Command::new("ffmpeg")
        .args([
            "-loglevel", "error",
            "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", "160x144",
            "-framerate", "4194304/70224", "-i", "-",
            "-c:v", "libx264", "-preset", "veryslow", "-crf", "28",
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
fn mux_av(video: &Path, pcm: &Path, out: &Path) -> bool {
    let status = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-i"])
        .arg(video)
        .args(["-f", "s16le", "-ar", "44100", "-ac", "2", "-i"])
        .arg(pcm)
        .args([
            "-c:v", "copy", "-c:a", "aac", "-b:a", "96k",
            "-movflags", "+faststart",
        ])
        .arg(out)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    matches!(status, Ok(s) if s.success()) && out.exists()
}

/// Write the single poster PNG, keyed on `sha`_`tag` (never a filename). This is
/// the video `poster=` and the no-ffmpeg/old-dir `<img>` fallback. Errors are
/// logged, not fatal.
fn write_poster_png(dir: &Path, sha: &str, tag: &str, poster: Option<&Vec<u8>>) {
    if let Some(rgb) = poster {
        let file = dir.join(format!("{sha}_{tag}.png"));
        if let Err(e) = std::fs::write(&file, encode_rgb_png(160, 144, rgb)) {
            eprintln!("screenshot {}: {e}", file.display());
        }
    }
}

/// Write the SGB 256x224 border still, keyed `<sha>_sgb_border.png`. Display-only
/// and SGB-only; the gallery composites the 160x144 video into its center window.
/// Errors are logged, not fatal.
fn write_border_png(dir: &Path, sha: &str, rgb: &[u8]) {
    let file = dir.join(format!("{sha}_sgb_border.png"));
    let (w, h) = (ppu::SGB_FRAME_WIDTH as u32, ppu::SGB_FRAME_HEIGHT as u32);
    if let Err(e) = std::fs::write(&file, encode_rgb_png(w, h, rgb)) {
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

        let h = frame_hash(&frame);
        hash_all = (hash_all ^ h).wrapping_mul(FNV_PRIME);
        // Poster = last non-blank frame of the tail; falls back to the very
        // last frame if the run ended blank (mirrors `emulate`'s selection).
        if in_tail && (frame_is_non_blank(&frame) || (f + 1 == cfg.frames && poster.is_none())) {
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
    let ok = rows
        .iter()
        .filter(|r| r.error.is_none() && r.boot_ok && r.changed)
        .count();
    let med = median(rows.iter().filter_map(|r| r.fps).collect());

    // Media is probed off disk purely by rom_sha (never the filename): scan the
    // screens dir for `<sha>_<tag>.png` posters (any legacy `_cpN` files skipped)
    // to learn which hardware tabs actually have content. This keeps the compared
    // manifest schema untouched — no per-hardware rows, no media sidecar.
    let mut poster_tags: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    let mut tabs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // SGB border stills (`<sha>_sgb_border.png`, 256x224): not a hardware tab —
    // tracked separately so the SGB tab can frame the 160x144 video inside it.
    let mut border_shas: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Ok(rd) = std::fs::read_dir(&screens_dir) {
        for e in rd.flatten() {
            let fname = e.file_name().to_string_lossy().into_owned();
            let Some(stem) = fname.strip_suffix(".png") else { continue };
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
    s.push_str(
        "body{font-family:system-ui,sans-serif;background:#111;color:#eee;margin:0;padding:24px}\
         h1{font-weight:600;font-size:20px}\
         .tabs{display:flex;gap:8px;flex-wrap:wrap;margin:16px 0;position:sticky;top:0;background:#111;padding:8px 0;z-index:2}\
         .tab{background:#1c1c1c;color:#ccc;border:1px solid #333;border-radius:6px;padding:6px 14px;font-size:14px;cursor:pointer}\
         .tab.active{background:#2563eb;color:#fff;border-color:#2563eb}\
         .tab-panel{display:none}.tab-panel.active{display:block}\
         .grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(200px,1fr));gap:16px}\
         .card{background:#1c1c1c;border-radius:8px;padding:12px;border:1px solid #333}\
         .hero{width:100%;aspect-ratio:10/9;object-fit:contain;image-rendering:pixelated;border-radius:4px;background:#000;display:block;cursor:pointer}\
         .hero.audible{outline:3px solid #4ade80;outline-offset:-3px}\
         .sgb-frame{position:relative;width:100%;aspect-ratio:256/224;border-radius:4px;background:#000;overflow:hidden;display:block}\
         .sgb-frame .sgb-border{position:absolute;inset:0;width:100%;height:100%;object-fit:fill;image-rendering:pixelated;pointer-events:none;z-index:0}\
         .hero.sgb-screen{position:absolute;left:18.75%;top:17.857%;width:62.5%;height:64.286%;aspect-ratio:auto;object-fit:fill;border-radius:0;z-index:1}\
         .name{font-size:14px;margin-top:8px;word-break:break-all}\
         .meta{font-size:12px;color:#999;display:flex;justify-content:space-between;margin-top:4px}\
         .ok{color:#4ade80}.fail{color:#f87171}.err{color:#fbbf24}\
         .empty{color:#888;margin-top:24px}",
    );
    s.push_str("</style></head><body>");
    s.push_str(&format!(
        "<h1>rustyboi library sweep &mdash; {ok}/{} in gameplay, median {:.0} fps</h1>",
        rows.len(),
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

    // Tab bar.
    s.push_str("<div class=\"tabs\">");
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
    s.push_str("</div>");

    // One panel per hardware tab; cards derived entirely from rom_sha + tag.
    for (i, tag) in tab_list.iter().enumerate() {
        s.push_str(&format!(
            "<div class=\"tab-panel{}\" id=\"panel-{tag}\"><div class=\"grid\">",
            if i == 0 { " active" } else { "" }
        ));
        for r in &rows {
            let has_poster = poster_tags.get(&r.rom_sha).is_some_and(|t| t.contains(tag));
            if !has_poster {
                continue;
            }
            let (status, cls) = match (&r.error, r.boot_ok && r.changed) {
                (Some(_), _) => ("error", "err"),
                (None, true) => ("ok", "ok"),
                (None, false) => ("blank/static", "fail"),
            };
            let poster = format!("./screens/{}_{tag}.png", r.rom_sha);
            let video_file = format!("{}_{tag}.mp4", r.rom_sha);
            let has_video = videos_dir.join(&video_file).exists();
            // SGB cards whose game uploaded a border render inside a 256x224 frame
            // still, with the 160x144 video composited into the GB-screen window
            // at (48,40). The border img is lazy (loading="lazy") and pointer-none
            // so clicks reach the video; every other card is unchanged.
            let sgb_border = tag == "sgb" && border_shas.contains(&r.rom_sha);
            let border_src = format!("./screens/{}_sgb_border.png", r.rom_sha);
            let hero = if sgb_border && has_video {
                format!(
                    "<div class=\"sgb-frame\">\
                     <img class=\"sgb-border\" loading=\"lazy\" src=\"{border_src}\" alt=\"\">\
                     <video class=\"hero sgb-screen\" muted loop playsinline preload=\"none\" \
                     data-poster=\"{poster}\" data-src=\"./videos/{}\"></video></div>",
                    html_escape(&video_file),
                )
            } else if sgb_border {
                // Border still but no video: the framed still is the hero.
                format!("<img class=\"hero\" loading=\"lazy\" src=\"{border_src}\" alt=\"\">")
            } else if has_video {
                // No eager poster/src: the observer assigns them from data-* when the
                // card nears the viewport, so the page loads O(viewport) not O(page).
                format!(
                    "<video class=\"hero\" muted loop playsinline preload=\"none\" \
                     data-poster=\"{poster}\" data-src=\"./videos/{}\"></video>",
                    html_escape(&video_file),
                )
            } else {
                format!("<img class=\"hero\" loading=\"lazy\" src=\"{poster}\" alt=\"\">")
            };
            let display_name = r.name.clone().unwrap_or_else(|| format!("sha:{}", r.rom_sha));
            let fps = r.fps.map_or(String::new(), |f| format!("{f:.0} fps"));
            s.push_str(&format!(
                "<div class=\"card\">{hero}<div class=\"name\">{}</div>\
                 <div class=\"meta\"><span>{} {fps}</span><span class=\"{cls}\">{status}</span></div></div>",
                html_escape(&display_name),
                html_escape(&tag.to_ascii_uppercase()),
            ));
        }
        s.push_str("</div></div>");
    }

    // Dependency-free JS: the IntersectionObserver drives LOADING (not just
    // play/pause) so only near-viewport media is fetched. Entering + active
    // assigns poster/src from data-* then plays; leaving pauses, re-mutes, and
    // releases the download (keeps the poster). Exactly one audible video.
    s.push_str(
        "<script>\
        (function(){\
        var vids=function(){return Array.prototype.slice.call(document.querySelectorAll('video.hero'));};\
        function loadAndPlay(v){\
          if(!v.src){v.poster=v.dataset.poster;v.src=v.dataset.src;v.load();}\
          v.play().catch(function(){});\
        }\
        function release(v){\
          v.pause();if(!v.muted){v.muted=true;v.classList.remove('audible');}\
          if(v.src){v.removeAttribute('src');v.load();}\
        }\
        var io=new IntersectionObserver(function(es){\
          es.forEach(function(e){var v=e.target;\
            var panel=v.closest('.tab-panel');var active=panel&&panel.classList.contains('active');\
            if(e.isIntersecting&&active){loadAndPlay(v);}\
            else{release(v);}\
          });\
        },{rootMargin:'400px 0px'});\
        vids().forEach(function(v){io.observe(v);\
          v.addEventListener('click',function(ev){ev.preventDefault();\
            if(v.muted){vids().forEach(function(o){if(o!==v){o.muted=true;o.classList.remove('audible');}});\
              v.muted=false;v.classList.add('audible');loadAndPlay(v);}\
            else{v.muted=true;v.classList.remove('audible');}\
          });\
        });\
        function activate(tab){\
          document.querySelectorAll('.tab').forEach(function(b){b.classList.toggle('active',b.dataset.tab===tab);});\
          document.querySelectorAll('.tab-panel').forEach(function(p){\
            var on=p.id==='panel-'+tab;p.classList.toggle('active',on);\
            if(!on){p.querySelectorAll('video.hero').forEach(function(v){release(v);});}\
          });\
          var panel=document.getElementById('panel-'+tab);\
          if(panel){panel.querySelectorAll('video.hero').forEach(function(v){\
            var r=v.getBoundingClientRect();if(r.top<innerHeight&&r.bottom>0){loadAndPlay(v);}\
          });}\
        }\
        document.querySelectorAll('.tab').forEach(function(b){b.addEventListener('click',function(){activate(b.dataset.tab);});});\
        })();\
        </script>",
    );
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn rom_sha_falls_back_for_non_archive() {
        // Not a PK zip: hashed as-is, and identical to a direct sha256 prefix.
        let raw = b"\x00\x01\x02\x03 not a zip";
        let want: String = sha256(raw)[..8].iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(rom_sha(raw), want);
    }
}
