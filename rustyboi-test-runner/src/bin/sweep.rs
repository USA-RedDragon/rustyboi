//! Library-wide regression + performance sweep.
//!
//! Runs every ROM in a game library under a deterministic input masher,
//! recording per-ROM performance, a cumulative every-frame framebuffer hash,
//! checkpoint hashes, a final-frame screenshot, and one screenshot per
//! checkpoint (`<key>_cpN.png`, rendered by `gallery` as a per-ROM strip so a
//! blank/fading final frame doesn't hide what the run actually did). Two
//! sweeps compare into a
//! regression report: any hash difference is a behavior change (the library is
//! ~1200 canaries wide), any fps drop beyond tolerance is a perf regression.
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
use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Hardware, GB};
use rustyboi_core_lib::movie::{frame_hash, frame_is_non_blank, sha256};
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
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
                 [--warmup N] [--jobs N] [--timeout SECS] [--no-screens] [--only SUBSTR]\n  \
                 sweep compare --base A.jsonl --cand B.jsonl [--min-ratio R] [--ignore-perf] \
                 [--out report.md]\n  \
                 sweep gallery --manifest M.jsonl --screens DIR [--out gallery.html]"
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
    /// CRC32 -> No-Intro game name (auto-fetched DATs). Empty = file stems.
    names: std::collections::HashMap<u32, String>,
}

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
    let strip_names = args.iter().any(|a| a == "--strip-names");
    let only = arg(args, "--only");
    if warmup >= frames {
        return Err("run: --warmup must be < --frames".into());
    }

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
    let cfg = RunCfg { frames, warmup, timeout_secs, screens_dir, names };

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
            if let Some(dir) = &cfg.screens_dir {
                if let Some(rgb) = &out.final_rgb {
                    let png = encode_rgb_png(160, 144, rgb);
                    let file = dir.join(format!("{}.png", sanitize(key)));
                    if let Err(e) = std::fs::write(&file, png) {
                        eprintln!("screenshot {}: {e}", file.display());
                    }
                }
                for (i, rgb) in out.checkpoint_rgbs.iter().enumerate() {
                    let png = encode_rgb_png(160, 144, rgb);
                    let file = dir.join(format!("{}_cp{i}.png", sanitize(key)));
                    if let Err(e) = std::fs::write(&file, png) {
                        eprintln!("screenshot {}: {e}", file.display());
                    }
                }
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
    row
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
    /// One RGB frame per entry in `checkpoints` (same order); empty when
    /// screenshots are disabled. Pure copies taken at instants where the
    /// checkpoint hash is already computed — no effect on emulation or hashes.
    checkpoint_rgbs: Vec<Vec<u8>>,
}

fn emulate(bytes: &[u8], seed: u64, cfg: &RunCfg) -> Result<EmuOut, String> {
    let cart = Cartridge::from_bytes(bytes).map_err(|e| format!("load: {e}"))?;
    let hardware = if cart.supports_cgb() { Hardware::CGB } else { Hardware::DMG };
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
    let mut checkpoint_rgbs = Vec::new();
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
            if cfg.screens_dir.is_some() {
                checkpoint_rgbs.push(frame_rgb(&frame));
            }
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
        checkpoint_rgbs,
    })
}

fn sanitize(key: &str) -> String {
    key.chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '(' | ')' | '[' | ']' | '!' | '+' | ',' | '\'' | ' ') { c } else { '_' })
        .collect()
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

    let mut s = String::new();
    s.push_str("<!doctype html><html><head><meta charset=\"utf-8\">");
    s.push_str("<title>rustyboi library sweep</title><style>");
    s.push_str(
        "body{font-family:system-ui,sans-serif;background:#111;color:#eee;margin:0;padding:24px}\
         h1{font-weight:600}\
         .grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(200px,1fr));gap:16px}\
         .card{background:#1c1c1c;border-radius:8px;padding:12px;border:1px solid #333}\
         .card img{width:100%;image-rendering:pixelated;border-radius:4px;background:#000}\
         .name{font-size:14px;margin-top:8px;word-break:break-all}\
         .meta{font-size:12px;color:#999;display:flex;justify-content:space-between;margin-top:4px}\
         .ok{color:#4ade80}.fail{color:#f87171}.err{color:#fbbf24}\
         .strip{display:flex;gap:2px;margin-top:6px}\
         .strip img{flex:1;min-width:0;image-rendering:pixelated;border-radius:2px;background:#000}",
    );
    s.push_str("</style></head><body>");
    s.push_str(&format!(
        "<h1>rustyboi library sweep &mdash; {ok}/{} in gameplay, median {:.0} fps</h1><div class=\"grid\">",
        rows.len(),
        med.unwrap_or(0.0)
    ));
    for r in &rows {
        let (status, cls) = match (&r.error, r.boot_ok && r.changed) {
            (Some(_), _) => ("error", "err"),
            (None, true) => ("ok", "ok"),
            (None, false) => ("blank/static", "fail"),
        };
        // Gallery is built from a fresh (named) sweep; fall back gracefully if
        // pointed at an anonymized manifest.
        let img = r
            .key
            .as_deref()
            .map(|k| format!("{}.png", sanitize(k)))
            .filter(|file| Path::new(&screens).join(file).exists())
            .map(|file| format!("<img src=\"./screens/{}\" alt=\"\">", html_escape(&file)))
            .unwrap_or_else(|| "<div style=\"aspect-ratio:10/9\"></div>".into());
        // Checkpoint strip: probe `<key>_cpN.png` on disk (older sweep dirs
        // have none -> no strip). Labels come from the row's checkpoint frames.
        let strip = r
            .key
            .as_deref()
            .map(|k| {
                (0..r.checkpoints.len())
                    .map(|i| (i, format!("{}_cp{i}.png", sanitize(k))))
                    .filter(|(_, file)| Path::new(&screens).join(file).exists())
                    .map(|(i, file)| {
                        format!(
                            "<img src=\"./screens/{}\" alt=\"\" title=\"checkpoint {i} @ frame {}\">",
                            html_escape(&file),
                            r.checkpoints[i].0
                        )
                    })
                    .collect::<String>()
            })
            .filter(|imgs| !imgs.is_empty())
            .map(|imgs| format!("<div class=\"strip\">{imgs}</div>"))
            .unwrap_or_default();
        let display_name = r.name.clone().unwrap_or_else(|| format!("sha:{}", r.rom_sha));
        let fps = r.fps.map_or(String::new(), |f| format!("{f:.0} fps"));
        s.push_str(&format!(
            "<div class=\"card\">{img}{strip}<div class=\"name\">{}</div>\
             <div class=\"meta\"><span>{} {fps}</span><span class=\"{cls}\">{status}</span></div></div>",
            html_escape(&display_name),
            html_escape(&r.hardware),
        ));
    }
    s.push_str("</div></body></html>");
    std::fs::write(&out, s).map_err(|e| format!("write {out}: {e}"))?;
    println!("gallery: {} cards -> {out}", rows.len());
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
