//! Headless movie / regression / compat driver for rustyboi.
//!
//! One binary exposing the three file-owning payoffs of the deterministic
//! `rustyboi_core_lib::movie` core (record/replay live in the core; this bin
//! only owns files, PNGs and HTML):
//!
//!   movie record  --rom R [--movie M] [--mode M] [--frames N]
//!                 [--input "0:START;5:;20:A"] [--author A] [--golden G]
//!       Play a ROM under a scripted `frame:BUTTONS` timeline, write the movie
//!       to `--movie` (default <rom>.rbmv) and print (or write to `--golden`)
//!       the final-frame golden hash. Regenerates goldens intentionally.
//!
//!   movie replay  --manifest FILE [--record-goldens]
//!       Replay a corpus manifest of `rom | movie | golden_hash` rows and assert
//!       each final-frame hash matches its golden. Non-zero exit on any
//!       mismatch (CI gate). `--record-goldens` rewrites the manifest in place
//!       with freshly-observed hashes instead of asserting.
//!
//!   movie compat  --roms DIR [--out gallery.html] [--mode dmg|cgb|auto]
//!                 [--frames N] [--input SCRIPT]
//!       Boot every ROM under a short input timeline, screenshot the final
//!       frame, record boot-ok (non-blank + changing), and emit a self-
//!       contained HTML gallery with embedded data-URI thumbnails.
//!
//! Determinism (no wall clock / RTC / threads in core) makes every hash and
//! screenshot reproducible: the same ROM + movie always yields the same bytes.

use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Frame, Hardware, GB};
use rustyboi_core_lib::input::ButtonState;
use rustyboi_core_lib::movie::{
    frame_hash, frame_is_non_blank, replay, sha256, Movie, MovieMeta, MovieStart, Recorder,
};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let sub = args.get(1).map(String::as_str);
    let rest = &args[args.len().min(2)..];
    let result = match sub {
        Some("record") => cmd_record(rest),
        Some("replay") => cmd_replay(rest),
        Some("compat") => cmd_compat(rest),
        _ => {
            eprintln!(
                "usage:\n  movie record  --rom R [--movie M] [--mode dmg|cgb|auto] \
                 [--frames N] [--input SCRIPT] [--author A] [--golden FILE]\n  \
                 movie replay  --manifest FILE [--record-goldens]\n  \
                 movie compat  --roms DIR [--out gallery.html] [--mode dmg|cgb|auto] \
                 [--frames N] [--input SCRIPT]"
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// record
// ---------------------------------------------------------------------------

fn cmd_record(args: &[String]) -> Result<(), String> {
    let rom_path = arg(args, "--rom").ok_or("record: --rom <path> required")?;
    let mode = arg(args, "--mode").unwrap_or_else(|| "auto".into());
    let frames: usize = parse_num(args, "--frames", 900)?;
    let script = arg(args, "--input").unwrap_or_default();
    let author = arg(args, "--author").unwrap_or_default();
    let movie_path = arg(args, "--movie")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_movie_path(&rom_path));

    let rom_bytes = std::fs::read(&rom_path).map_err(|e| format!("read {rom_path}: {e}"))?;
    let cart = Cartridge::from_bytes(&rom_bytes).map_err(|e| format!("load ROM: {e}"))?;
    let hardware = resolve_hardware(&mode, &cart);
    let rom_hash = sha256(&rom_bytes);

    let mut gb = GB::new(hardware);
    let rom_name = Path::new(&rom_path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    gb.insert(cart);
    gb.skip_bios();

    let timeline = expand_timeline(&script, frames);
    let mut recorder = Recorder::new(&mut gb, rom_hash, hardware).with_meta(MovieMeta {
        author,
        rom_name: rom_name.clone(),
        frame_count: 0,
        note: format!("recorded via movie record; mode={mode}"),
    });
    let mut final_hash = 0u64;
    for input in &timeline {
        final_hash = recorder.set_input(*input);
    }
    let movie = recorder.finish();
    let bytes = movie.to_bytes();
    std::fs::write(&movie_path, &bytes).map_err(|e| format!("write {}: {e}", movie_path.display()))?;

    println!(
        "recorded {} frames of {rom_name} ({hardware:?}) -> {} ({} bytes)",
        movie.inputs.len(),
        movie_path.display(),
        bytes.len()
    );
    println!("golden_final_hash={final_hash:016x}");
    if let Some(golden_path) = arg(args, "--golden") {
        std::fs::write(&golden_path, format!("{final_hash:016x}\n"))
            .map_err(|e| format!("write golden {golden_path}: {e}"))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// replay (regression harness)
// ---------------------------------------------------------------------------

struct Row {
    rom: String,
    movie: String,
    golden: u64,
    /// Original manifest line, for `--record-goldens` rewriting.
    raw: String,
}

fn cmd_replay(args: &[String]) -> Result<(), String> {
    let manifest = arg(args, "--manifest").ok_or("replay: --manifest <file> required")?;
    let record_goldens = args.iter().any(|a| a == "--record-goldens");
    let text = std::fs::read_to_string(&manifest).map_err(|e| format!("read {manifest}: {e}"))?;
    let base = Path::new(&manifest).parent().unwrap_or_else(|| Path::new("."));

    let mut rows = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            rows.push(Row { rom: String::new(), movie: String::new(), golden: 0, raw: line.to_string() });
            continue;
        }
        let fields: Vec<&str> = trimmed.split('|').map(str::trim).collect();
        if fields.len() < 3 {
            return Err(format!("{manifest}:{}: want `rom | movie | golden_hash`", lineno + 1));
        }
        let golden = u64::from_str_radix(fields[2], 16)
            .map_err(|_| format!("{manifest}:{}: bad hex golden {:?}", lineno + 1, fields[2]))?;
        rows.push(Row {
            rom: fields[0].to_string(),
            movie: fields[1].to_string(),
            golden,
            raw: line.to_string(),
        });
    }

    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut rewritten = Vec::new();
    for row in &rows {
        if row.rom.is_empty() {
            rewritten.push(row.raw.clone());
            continue;
        }
        let observed = replay_one(base, &row.rom, &row.movie)?;
        if record_goldens {
            rewritten.push(format!("{} | {} | {:016x}", row.rom, row.movie, observed));
            println!("[golden ] {:016x}  {}", observed, row.rom);
            continue;
        }
        if observed == row.golden {
            passed += 1;
            println!("[  ok   ] {:016x}  {}", observed, row.rom);
        } else {
            failed += 1;
            println!(
                "[ FAIL  ] {}: expected {:016x}, got {:016x}",
                row.rom, row.golden, observed
            );
        }
    }

    if record_goldens {
        let out = rewritten.join("\n") + "\n";
        std::fs::write(&manifest, out).map_err(|e| format!("rewrite {manifest}: {e}"))?;
        println!("wrote fresh goldens into {manifest}");
        return Ok(());
    }

    println!("replay: {passed} passed, {failed} failed");
    if failed > 0 {
        Err(format!("{failed} golden mismatch(es)"))
    } else {
        Ok(())
    }
}

/// Load a movie, reconstruct its start state against the manifest's ROM, replay
/// it, and return the observed final-frame hash. Enforces the movie's embedded
/// ROM hash against the actual ROM bytes.
fn replay_one(base: &Path, rom_field: &str, movie_field: &str) -> Result<u64, String> {
    let rom_path = resolve_rel(base, rom_field);
    let movie_path = resolve_rel(base, movie_field);
    let rom_bytes = std::fs::read(&rom_path).map_err(|e| format!("read {}: {e}", rom_path.display()))?;
    let movie_bytes =
        std::fs::read(&movie_path).map_err(|e| format!("read {}: {e}", movie_path.display()))?;
    let movie = Movie::from_bytes(&movie_bytes)
        .map_err(|e| format!("{}: {e}", movie_path.display()))?;

    if sha256(&rom_bytes) != movie.rom_sha256 {
        return Err(format!(
            "{}: ROM hash mismatch — movie was recorded against a different ROM",
            movie_path.display()
        ));
    }

    let cart = Cartridge::from_bytes(&rom_bytes).map_err(|e| format!("load ROM: {e}"))?;
    let mut gb = match &movie.start {
        MovieStart::PowerOn => {
            let mut gb = GB::new(movie.hardware);
            gb.insert(cart);
            gb.skip_bios();
            gb
        }
        MovieStart::SaveState(blob) => serde_json::from_slice::<GB>(blob)
            .map_err(|e| format!("{}: deserialize savestate: {e}", movie_path.display()))?,
    };
    Ok(replay(&movie, &mut gb, false).final_frame_hash)
}

// ---------------------------------------------------------------------------
// compat (matrix + screenshots + HTML gallery)
// ---------------------------------------------------------------------------

struct CompatEntry {
    name: String,
    hardware: String,
    png: Vec<u8>,
    boot_ok: bool,
    changed: bool,
}

fn cmd_compat(args: &[String]) -> Result<(), String> {
    let roms_dir = arg(args, "--roms").ok_or("compat: --roms <dir> required")?;
    let out = arg(args, "--out").unwrap_or_else(|| "gallery.html".into());
    let mode = arg(args, "--mode").unwrap_or_else(|| "auto".into());
    let frames: usize = parse_num(args, "--frames", 600)?;
    // Default "boot movie": tap Start a few times, then idle.
    let script = arg(args, "--input")
        .unwrap_or_else(|| "0:;30:START;36:;90:START;96:;180:START;186:".into());

    let mut rom_paths: Vec<PathBuf> = std::fs::read_dir(&roms_dir)
        .map_err(|e| format!("read dir {roms_dir}: {e}"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| matches!(e.to_ascii_lowercase().as_str(), "gb" | "gbc" | "zip"))
                .unwrap_or(false)
        })
        .collect();
    rom_paths.sort();
    if rom_paths.is_empty() {
        return Err(format!("no .gb/.gbc/.zip ROMs in {roms_dir}"));
    }

    let timeline = expand_timeline(&script, frames);
    let mut entries = Vec::new();
    for rom_path in &rom_paths {
        match run_compat(rom_path, &mode, &timeline) {
            Ok(entry) => {
                println!(
                    "[{}] {} ({})",
                    if entry.boot_ok && entry.changed { "boot-ok" } else { " FAIL  " },
                    entry.name,
                    entry.hardware
                );
                entries.push(entry);
            }
            Err(e) => eprintln!("skip {}: {e}", rom_path.display()),
        }
    }

    let html = render_gallery(&entries);
    std::fs::write(&out, html).map_err(|e| format!("write {out}: {e}"))?;
    let ok = entries.iter().filter(|e| e.boot_ok && e.changed).count();
    println!("compat: {ok}/{} booted; gallery -> {out}", entries.len());
    Ok(())
}

fn run_compat(rom_path: &Path, mode: &str, timeline: &[ButtonState]) -> Result<CompatEntry, String> {
    let rom_bytes = std::fs::read(rom_path).map_err(|e| format!("{e}"))?;
    let cart = Cartridge::from_bytes(&rom_bytes).map_err(|e| format!("{e}"))?;
    let hardware = resolve_hardware(mode, &cart);
    let name = rom_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let mut gb = GB::new(hardware);
    gb.insert(cart);
    gb.skip_bios();

    let mut first_hash = None;
    let mut changed = false;
    let mut last_frame: Option<Frame> = None;
    for input in timeline {
        gb.set_input_state(*input);
        let (frame, _bp) = gb.run_until_frame(false);
        let h = frame_hash(&frame);
        match first_hash {
            None => first_hash = Some(h),
            Some(f) if f != h => changed = true,
            _ => {}
        }
        last_frame = Some(frame);
    }
    let frame = last_frame.ok_or("no frames rendered")?;
    let boot_ok = frame_is_non_blank(&frame);
    let png = encode_rgb_png(160, 144, &frame_rgb(&frame));

    Ok(CompatEntry {
        name,
        hardware: format!("{hardware:?}"),
        png,
        boot_ok,
        changed,
    })
}

fn render_gallery(entries: &[CompatEntry]) -> String {
    let ok = entries.iter().filter(|e| e.boot_ok && e.changed).count();
    let mut s = String::new();
    s.push_str("<!doctype html><html><head><meta charset=\"utf-8\">");
    s.push_str("<title>rustyboi compat matrix</title><style>");
    s.push_str(
        "body{font-family:system-ui,sans-serif;background:#111;color:#eee;margin:0;padding:24px}\
         h1{font-weight:600}\
         .grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(200px,1fr));gap:16px}\
         .card{background:#1c1c1c;border-radius:8px;padding:12px;border:1px solid #333}\
         .card img{width:100%;image-rendering:pixelated;border-radius:4px;background:#000}\
         .name{font-size:14px;margin-top:8px;word-break:break-all}\
         .meta{font-size:12px;color:#999;display:flex;justify-content:space-between;margin-top:4px}\
         .ok{color:#4ade80}.fail{color:#f87171}",
    );
    s.push_str("</style></head><body>");
    s.push_str(&format!(
        "<h1>rustyboi compat matrix &mdash; {ok}/{} booted</h1><div class=\"grid\">",
        entries.len()
    ));
    for e in entries {
        let status = if e.boot_ok && e.changed { "boot-ok" } else { "blank/static" };
        let cls = if e.boot_ok && e.changed { "ok" } else { "fail" };
        let b64 = base64(&e.png);
        s.push_str(&format!(
            "<div class=\"card\"><img src=\"data:image/png;base64,{b64}\" alt=\"{}\">\
             <div class=\"name\">{}</div>\
             <div class=\"meta\"><span>{}</span><span class=\"{cls}\">{status}</span></div></div>",
            html_escape(&e.name),
            html_escape(&e.name),
            html_escape(&e.hardware)
        ));
    }
    s.push_str("</div></body></html>");
    s
}

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

/// Value of `--flag value`, if present.
fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn parse_num(args: &[String], name: &str, default: usize) -> Result<usize, String> {
    match arg(args, name) {
        Some(v) => v.parse().map_err(|_| format!("bad {name} {v:?}")),
        None => Ok(default),
    }
}

fn default_movie_path(rom: &str) -> PathBuf {
    let mut p = PathBuf::from(rom);
    p.set_extension("rbmv");
    p
}

fn resolve_rel(base: &Path, field: &str) -> PathBuf {
    let p = PathBuf::from(field);
    if p.is_absolute() { p } else { base.join(p) }
}

fn resolve_hardware(mode: &str, cart: &Cartridge) -> Hardware {
    match mode {
        "dmg" => Hardware::DMG,
        "cgb" => Hardware::CGB,
        "auto" | "" => {
            if cart.supports_cgb() {
                Hardware::CGB
            } else {
                Hardware::DMG
            }
        }
        other => {
            eprintln!("warning: unknown mode {other:?}, using auto");
            if cart.supports_cgb() { Hardware::CGB } else { Hardware::DMG }
        }
    }
}

/// Parse a `frame:BUTTONS` script (same syntax as printer-drive) and expand it
/// into one `ButtonState` per frame for `frames` frames: the button state at
/// each frame is the most recent event at or before it.
fn expand_timeline(script: &str, frames: usize) -> Vec<ButtonState> {
    let mut events: Vec<(usize, ButtonState)> = script
        .split(';')
        .filter(|s| !s.trim().is_empty())
        .map(|entry| {
            let (f, b) = entry.split_once(':').unwrap_or_else(|| panic!("bad event {entry:?}"));
            (f.trim().parse().expect("bad frame number"), parse_buttons(b.trim()))
        })
        .collect();
    events.sort_by_key(|e| e.0);

    let mut timeline = Vec::with_capacity(frames);
    let mut cur = ButtonState::default();
    let mut next = 0usize;
    for f in 0..frames {
        while let Some((ef, eb)) = events.get(next) {
            if *ef <= f {
                cur = *eb;
                next += 1;
            } else {
                break;
            }
        }
        timeline.push(cur);
    }
    timeline
}

fn parse_buttons(spec: &str) -> ButtonState {
    let mut b = ButtonState::default();
    for name in spec.split('+').filter(|s| !s.is_empty()) {
        match name.to_ascii_uppercase().as_str() {
            "A" => b.a = true,
            "B" => b.b = true,
            "START" => b.start = true,
            "SELECT" => b.select = true,
            "UP" => b.up = true,
            "DOWN" => b.down = true,
            "LEFT" => b.left = true,
            "RIGHT" => b.right = true,
            other => panic!("unknown button {other:?}"),
        }
    }
    b
}

/// GB pixel buffer -> RGB888 (monochrome mapped to a gray ramp).
fn frame_rgb(frame: &Frame) -> Vec<u8> {
    match frame {
        Frame::Monochrome(data) => data
            .iter()
            .flat_map(|p| {
                let g = match p {
                    0 => 0xFFu8,
                    1 => 0xAA,
                    2 => 0x55,
                    _ => 0x00,
                };
                [g, g, g]
            })
            .collect(),
        Frame::Color(data) => data.to_vec(),
    }
}

/// RGB888 -> PNG (stored-deflate zlib, color type 2). No external deps.
fn encode_rgb_png(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        let mut crc = 0xFFFF_FFFFu32;
        for &b in kind.iter().chain(data) {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xEDB8_8320 & (0u32.wrapping_sub(crc & 1)));
            }
        }
        png.extend_from_slice(&(!crc).to_be_bytes());
    }
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB
    chunk(&mut png, b"IHDR", &ihdr);
    let mut raw = Vec::with_capacity((width as usize * 3 + 1) * height as usize);
    for row in rgb.chunks(width as usize * 3) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut idat = vec![0x78, 0x01];
    for (i, block) in raw.chunks(0xFFFF).enumerate() {
        idat.push(((i + 1) * 0xFFFF >= raw.len()) as u8);
        idat.extend_from_slice(&(block.len() as u16).to_le_bytes());
        idat.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        idat.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    idat.extend_from_slice(&(((b << 16) | a).to_be_bytes()));
    chunk(&mut png, b"IDAT", &idat);
    chunk(&mut png, b"IEND", &[]);
    png
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
