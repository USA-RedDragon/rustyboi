//! Dirty-line sweep: measure, across a large ROM corpus, how often games write
//! render-affecting PPU registers DURING mode 3 (the "dirty line" signature a
//! scanline/fast renderer would get wrong). Decides whether a per-line adaptive
//! fast/accurate renderer is worth building.
//!
//! Usage:
//!   dirtysweep [--frames N] [--warmup N] [--jobs N] [--csv out.csv]
//!              [--html out.html] [DIR ...]
//!   dirtysweep --sanity            # probe self-check vs mealybug mid-m3 ROMs
//!
//! With no DIR args it sweeps the owner's collection:
//!   ~/Downloads/gb/GBC/*.zip and ~/Downloads/gb/GB/*.zip
//!
//! CAVEAT (stated in the report): games only reach as far as the scripted input
//! drives them, so a game clean through title/menu may go dirty in gameplay we
//! never reached. The reported "clean %" is therefore an OPTIMISTIC UPPER BOUND
//! on fast-path coverage.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;
use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Hardware, GB};
use rustyboi_core_lib::input::ButtonState;
use rustyboi_core_lib::ppu::WatchedReg;

const DEFAULT_FRAMES: usize = 1800; // ~30s of gameplay
const DEFAULT_WARMUP: usize = 200; // frames to boot past logo before scripting

struct Config {
    frames: usize,
    warmup: usize,
    jobs: usize,
    csv: Option<PathBuf>,
    html: Option<PathBuf>,
    dirs: Vec<PathBuf>,
    sanity: bool,
}

fn parse_args() -> Config {
    let mut cfg = Config {
        frames: DEFAULT_FRAMES,
        warmup: DEFAULT_WARMUP,
        jobs: 0,
        csv: None,
        html: None,
        dirs: Vec::new(),
        sanity: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--frames" => cfg.frames = it.next().and_then(|s| s.parse().ok()).unwrap_or(cfg.frames),
            "--warmup" => cfg.warmup = it.next().and_then(|s| s.parse().ok()).unwrap_or(cfg.warmup),
            "--jobs" => cfg.jobs = it.next().and_then(|s| s.parse().ok()).unwrap_or(0),
            "--csv" => cfg.csv = it.next().map(PathBuf::from),
            "--html" => cfg.html = it.next().map(PathBuf::from),
            "--sanity" => cfg.sanity = true,
            other => cfg.dirs.push(PathBuf::from(other)),
        }
    }
    cfg
}

/// Per-game result of a sweep run.
struct GameResult {
    name: String,
    hardware: &'static str,
    ok: bool,
    err: Option<String>,
    total_visible: u64,
    dirty_lines: u64,
    dirty_pct: f64,
    ever_dirty: bool,
    per_reg_dirty: [u64; 9],
    palette_blocked: u64,
}

/// Light scripted input to nudge games past intros: cycle Start / A / (nothing)
/// so most titles reach gameplay where raster effects live. A change in the held
/// set every ~30 frames produces the high->low joypad edges games watch for.
fn scripted_input(frame: usize) -> ButtonState {
    let none = ButtonState::default();
    // 4 phases over ~60 frames: Start pressed, released, A pressed, released.
    match (frame / 15) % 4 {
        0 => ButtonState { start: true, ..none },
        1 => none,
        2 => ButtonState { a: true, ..none },
        _ => none,
    }
}

fn run_one(path: &Path, cfg: &Config) -> GameResult {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return err_result(name, format!("read: {e}")),
    };
    let cart = match Cartridge::from_bytes(&bytes) {
        Ok(c) => c,
        Err(e) => return err_result(name, format!("load: {e}")),
    };
    let (hardware, hw_name) = if cart.supports_cgb() {
        (Hardware::CGB, "CGB")
    } else {
        (Hardware::DMG, "DMG")
    };

    let mut gb = GB::new(hardware);
    gb.insert(cart);
    gb.skip_bios();

    // Warm-up (no probe): boot past the logo/menu without counting.
    for _ in 0..cfg.warmup {
        gb.run_until_frame(false);
    }

    gb.attach_dirty_probe();
    for f in 0..cfg.frames {
        gb.set_input_state(scripted_input(f));
        gb.run_until_frame(false);
        gb.dirty_probe_end_frame();
    }

    let probe = gb.take_dirty_probe().expect("probe attached");
    GameResult {
        name,
        hardware: hw_name,
        ok: true,
        err: None,
        total_visible: probe.total_visible_lines,
        dirty_lines: probe.total_dirty_lines,
        dirty_pct: probe.dirty_line_pct(),
        ever_dirty: probe.ever_dirty(),
        per_reg_dirty: probe.per_reg_dirty_lines,
        palette_blocked: probe.palette_blocked_events,
    }
}

fn err_result(name: String, err: String) -> GameResult {
    GameResult {
        name,
        hardware: "?",
        ok: false,
        err: Some(err),
        total_visible: 0,
        dirty_lines: 0,
        dirty_pct: 0.0,
        ever_dirty: false,
        per_reg_dirty: [0; 9],
        palette_blocked: 0,
    }
}

fn collect_roms(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut roms = Vec::new();
    for d in dirs {
        if d.is_file() {
            roms.push(d.clone());
            continue;
        }
        let Ok(rd) = std::fs::read_dir(d) else {
            eprintln!("warn: cannot read dir {}", d.display());
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let ext = p
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.to_ascii_lowercase());
            if matches!(ext.as_deref(), Some("zip" | "gb" | "gbc")) {
                roms.push(p);
            }
        }
    }
    roms.sort();
    roms
}

fn default_dirs() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    vec![
        PathBuf::from(format!("{home}/Downloads/gb/GBC")),
        PathBuf::from(format!("{home}/Downloads/gb/GB")),
    ]
}

fn main() {
    let cfg = parse_args();

    if cfg.jobs > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(cfg.jobs)
            .build_global()
            .ok();
    }

    if cfg.sanity {
        run_sanity();
        return;
    }

    let dirs = if cfg.dirs.is_empty() {
        default_dirs()
    } else {
        cfg.dirs.clone()
    };
    let roms = collect_roms(&dirs);
    if roms.is_empty() {
        eprintln!("no ROMs found in: {dirs:?}");
        std::process::exit(1);
    }
    eprintln!(
        "sweeping {} ROMs  (warmup {} + {} frames each)",
        roms.len(),
        cfg.warmup,
        cfg.frames
    );

    let done = AtomicUsize::new(0);
    let total = roms.len();
    let mut results: Vec<GameResult> = roms
        .par_iter()
        .map(|p| {
            let r = run_one(p, &cfg);
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_multiple_of(25) || n == total {
                eprintln!("  {n}/{total}");
            }
            r
        })
        .collect();
    results.sort_by(|a, b| {
        b.dirty_pct
            .partial_cmp(&a.dirty_pct)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });

    report(&results, &cfg);

    if let Some(csv) = &cfg.csv {
        write_csv(csv, &results);
        eprintln!("wrote CSV: {}", csv.display());
    }
    if let Some(html) = &cfg.html {
        write_html(html, &results, &cfg);
        eprintln!("wrote HTML: {}", html.display());
    }
}

fn report(results: &[GameResult], cfg: &Config) {
    let loaded: Vec<&GameResult> = results.iter().filter(|r| r.ok).collect();
    let failed: Vec<&GameResult> = results.iter().filter(|r| !r.ok).collect();
    // Games that actually rendered any visible line (LCD came on).
    let rendered: Vec<&GameResult> = loaded.iter().copied().filter(|r| r.total_visible > 0).collect();

    let n_loaded = loaded.len();
    let n_rendered = rendered.len();
    let n_clean = rendered.iter().filter(|r| !r.ever_dirty).count();
    let n_dirty = rendered.iter().filter(|r| r.ever_dirty).count();

    let total_visible: u64 = rendered.iter().map(|r| r.total_visible).sum();
    let total_dirty: u64 = rendered.iter().map(|r| r.dirty_lines).sum();
    let overall_clean_line_frac = if total_visible > 0 {
        1.0 - total_dirty as f64 / total_visible as f64
    } else {
        1.0
    };

    let mut per_reg: [u64; 9] = [0; 9];
    for r in &rendered {
        for (acc, dirty) in per_reg.iter_mut().zip(r.per_reg_dirty.iter()) {
            *acc += dirty;
        }
    }
    let palette_blocked: u64 = rendered.iter().map(|r| r.palette_blocked).sum();

    println!("\n=========================================================");
    println!(" DIRTY-LINE SWEEP  (mid-mode-3 render-register writes)");
    println!("=========================================================");
    println!(" ROMs seen:            {}", results.len());
    println!(" failed to load:       {}", failed.len());
    println!(" loaded:               {n_loaded}");
    println!(" rendered >=1 line:    {n_rendered}");
    println!(" frames/game:          {} (+{} warmup)", cfg.frames, cfg.warmup);
    println!("---------------------------------------------------------");
    println!(" HEADLINE (over {n_rendered} games that rendered):");
    if n_rendered > 0 {
        println!(
            "   CLEAN games (0 dirty lines):  {n_clean}  ({:.1}%)   <-- optimistic upper bound",
            100.0 * n_clean as f64 / n_rendered as f64
        );
        println!(
            "   DIRTY games (>=1 dirty line): {n_dirty}  ({:.1}%)",
            100.0 * n_dirty as f64 / n_rendered as f64
        );
    }
    println!(
        "   OVERALL CLEAN-LINE FRACTION:  {:.3}%  <-- fast-path coverage / speed-ceiling proxy",
        100.0 * overall_clean_line_frac
    );
    println!(
        "     ({} clean of {} rendered visible lines)",
        total_visible - total_dirty,
        total_visible
    );
    println!("---------------------------------------------------------");
    println!(" Per-game dirty-line % distribution (rendered games):");
    print_histogram(&rendered);
    println!("---------------------------------------------------------");
    println!(" Register ranking (cumulative dirty lines caused):");
    let mut ranked: Vec<(usize, u64)> = (0..9).map(|i| (i, per_reg[i])).collect();
    ranked.sort_by_key(|&(_, cnt)| std::cmp::Reverse(cnt));
    let reg_total: u64 = per_reg.iter().sum::<u64>().max(1);
    for (i, cnt) in ranked {
        println!(
            "   {:>5}  {:>12}  ({:>5.1}%)",
            WatchedReg::ALL[i].name(),
            cnt,
            100.0 * cnt as f64 / reg_total as f64
        );
    }
    println!(
        "   (note: a dirty line can be attributed to multiple registers,\n    so per-reg counts sum to >= dirty-line total)"
    );
    println!("---------------------------------------------------------");
    println!(
        " CGB palette (BCPD/OCPD) mid-mode-3 writes BLOCKED by PPU: {palette_blocked}"
    );
    println!(
        "   (dropped on hardware -> harmless to a scanline renderer; excluded above)"
    );
    if !failed.is_empty() {
        println!("---------------------------------------------------------");
        println!(" Failed to load ({}):", failed.len());
        for r in failed.iter().take(40) {
            println!("   {:<40} {}", r.name, r.err.as_deref().unwrap_or(""));
        }
        if failed.len() > 40 {
            println!("   ... and {} more (see CSV)", failed.len() - 40);
        }
    }
    println!("---------------------------------------------------------");
    println!(" Dirtiest 20 games:");
    for r in rendered.iter().take(20) {
        println!(
            "   {:>6.2}%  {:<40} [{}]  {} dirty / {} lines",
            r.dirty_pct, r.name, r.hardware, r.dirty_lines, r.total_visible
        );
    }
    println!("=========================================================");
    println!(
        " CAVEAT: scripted input only reaches so far into each game; a game\n \
         clean here may go dirty in unreached gameplay. CLEAN% is an\n \
         OPTIMISTIC UPPER BOUND on fast-path coverage."
    );
    println!("=========================================================\n");
}

const BUCKETS: [(&str, f64, f64); 8] = [
    ("0%      (clean)", 0.0, 0.0),
    ("0-0.1%", 0.0, 0.1),
    ("0.1-1%", 0.1, 1.0),
    ("1-5%", 1.0, 5.0),
    ("5-20%", 5.0, 20.0),
    ("20-50%", 20.0, 50.0),
    ("50-90%", 50.0, 90.0),
    ("90-100%", 90.0, 100.01),
];

fn bucket_of(pct: f64) -> usize {
    if pct == 0.0 {
        return 0;
    }
    for (i, (_, lo, hi)) in BUCKETS.iter().enumerate() {
        if i == 0 {
            continue;
        }
        if pct > *lo && pct <= *hi {
            return i;
        }
    }
    BUCKETS.len() - 1
}

fn print_histogram(rendered: &[&GameResult]) {
    let mut counts = [0usize; 8];
    for r in rendered {
        counts[bucket_of(r.dirty_pct)] += 1;
    }
    let max = counts.iter().copied().max().unwrap_or(1).max(1);
    for (i, (label, _, _)) in BUCKETS.iter().enumerate() {
        let c = counts[i];
        let bar = "#".repeat((c * 40 / max).max(if c > 0 { 1 } else { 0 }));
        println!("   {:<16} {:>4}  {}", label, c, bar);
    }
}

fn write_csv(path: &Path, results: &[GameResult]) {
    let mut s = String::new();
    s.push_str("name,hardware,ok,error,total_visible_lines,dirty_lines,dirty_pct,ever_dirty,");
    // Per-reg columns are EFFECTIVE dirty-line counts (a line counted once per
    // reg). For BCPD/OCPD these are the rare non-blocked cases; the separate
    // palette_blocked_events column holds the (dropped) mid-m3 palette writes.
    s.push_str("LCDC,SCY,SCX,BGP,OBP0,OBP1,WX,BCPD_eff,OCPD_eff,palette_blocked_events\n");
    for r in results {
        s.push_str(&format!(
            "{},{},{},{},{},{},{:.4},{},{},{},{},{},{},{},{},{},{},{}\n",
            csv_escape(&r.name),
            r.hardware,
            r.ok,
            csv_escape(r.err.as_deref().unwrap_or("")),
            r.total_visible,
            r.dirty_lines,
            r.dirty_pct,
            r.ever_dirty,
            r.per_reg_dirty[0],
            r.per_reg_dirty[1],
            r.per_reg_dirty[2],
            r.per_reg_dirty[3],
            r.per_reg_dirty[4],
            r.per_reg_dirty[5],
            r.per_reg_dirty[6],
            r.per_reg_dirty[7],
            r.per_reg_dirty[8],
            r.palette_blocked,
        ));
    }
    let _ = std::fs::write(path, s);
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn write_html(path: &Path, results: &[GameResult], cfg: &Config) {
    let rendered: Vec<&GameResult> = results.iter().filter(|r| r.ok && r.total_visible > 0).collect();
    let n_rendered = rendered.len();
    let n_clean = rendered.iter().filter(|r| !r.ever_dirty).count();
    let total_visible: u64 = rendered.iter().map(|r| r.total_visible).sum();
    let total_dirty: u64 = rendered.iter().map(|r| r.dirty_lines).sum();
    let clean_frac = if total_visible > 0 {
        100.0 * (1.0 - total_dirty as f64 / total_visible as f64)
    } else {
        100.0
    };
    let mut counts = [0usize; 8];
    for r in &rendered {
        counts[bucket_of(r.dirty_pct)] += 1;
    }

    let mut rows = String::new();
    for r in results.iter().filter(|r| r.ok && r.total_visible > 0) {
        rows.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td class=n>{:.2}</td><td class=n>{}</td><td class=n>{}</td></tr>",
            html_escape(&r.name),
            r.hardware,
            r.dirty_pct,
            r.dirty_lines,
            r.total_visible
        ));
    }
    let mut hist = String::new();
    let max = counts.iter().copied().max().unwrap_or(1).max(1);
    for (i, (label, _, _)) in BUCKETS.iter().enumerate() {
        let w = counts[i] * 300 / max;
        hist.push_str(&format!(
            "<div class=hrow><span class=hl>{}</span><span class=hbar style='width:{}px'></span><span class=hn>{}</span></div>",
            label, w, counts[i]
        ));
    }

    let html = format!(
        "<h1>Dirty-line sweep</h1>\
<p class=sub>mid-mode-3 render-register writes &mdash; scanline-renderer feasibility.\
 {n_rendered} games rendered, {frames} frames each.</p>\
<div class=cards>\
<div class=card><div class=big>{clean_pct:.1}%</div><div>games clean (0 dirty lines)<br><em>optimistic upper bound</em></div></div>\
<div class=card><div class=big>{clean_frac:.2}%</div><div>overall clean-line fraction<br>(fast-path coverage)</div></div>\
</div>\
<h2>Per-game dirty-line % distribution</h2><div class=hist>{hist}</div>\
<h2>All rendered games (sorted dirtiest first)</h2>\
<table><thead><tr><th>ROM</th><th>HW</th><th>dirty %</th><th>dirty lines</th><th>visible lines</th></tr></thead><tbody>{rows}</tbody></table>\
<p class=caveat>CAVEAT: scripted input only reaches so far into each game; a game clean here may go dirty in unreached gameplay. Clean% is an optimistic upper bound.</p>\
<style>\
body{{font:14px/1.5 system-ui,sans-serif;max-width:900px;margin:2rem auto;padding:0 1rem;color:#1a1a2e}}\
h1{{margin-bottom:.2rem}} .sub{{color:#666;margin-top:0}}\
.cards{{display:flex;gap:1rem;flex-wrap:wrap;margin:1rem 0}}\
.card{{flex:1;min-width:220px;border:1px solid #ddd;border-radius:10px;padding:1rem;background:#f7f7fb}}\
.big{{font-size:2.2rem;font-weight:700;color:#3a3a8c}}\
.hist{{margin:1rem 0}} .hrow{{display:flex;align-items:center;gap:.5rem;margin:2px 0}}\
.hl{{width:120px;text-align:right;font-variant-numeric:tabular-nums;color:#555}}\
.hbar{{height:14px;background:#5a5ad8;border-radius:3px;min-width:2px}}\
.hn{{color:#555}}\
table{{border-collapse:collapse;width:100%;font-size:13px}}\
th,td{{border-bottom:1px solid #eee;padding:3px 8px;text-align:left}}\
td.n{{text-align:right;font-variant-numeric:tabular-nums}}\
.caveat{{color:#a33;font-style:italic;margin-top:1.5rem}}\
</style>",
        clean_pct = 100.0 * n_clean as f64 / n_rendered.max(1) as f64,
        frames = cfg.frames,
    );
    let _ = std::fs::write(path, html);
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

// --- probe self-check against the mealybug mid-mode-3 suite ROMs ---

fn run_sanity() {
    // These ROMs deliberately write render regs during mode 3 every frame; the
    // probe MUST report them heavily dirty. If it doesn't, the probe is wrong.
    let candidates = [
        "/home/reddragon/projects/rustyboi/gb-test-roms/mealybug-tearoom-tests/ppu/m3_bgp_change.gb",
        "/home/reddragon/projects/rustyboi/gb-test-roms/mealybug-tearoom-tests/ppu/m3_scx_high_5_bits.gb",
        "/home/reddragon/projects/rustyboi/gb-test-roms/mealybug-tearoom-tests/ppu/m3_lcdc_bg_en_change.gb",
        "/home/reddragon/projects/rustyboi/gb-test-roms/mealybug-tearoom-tests/ppu/m3_wx_5_change.gb",
    ];
    let cfg = Config {
        frames: 40,
        warmup: 40,
        jobs: 1,
        csv: None,
        html: None,
        dirs: vec![],
        sanity: true,
    };
    println!("SANITY: probe vs mealybug mid-mode-3 ROMs (expect heavy dirtiness)");
    let mut any = false;
    for c in candidates {
        let p = Path::new(c);
        if !p.exists() {
            eprintln!("  (skip, not found) {c}");
            continue;
        }
        any = true;
        let r = run_one(p, &cfg);
        let breakdown: Vec<String> = (0..9)
            .filter(|&i| r.per_reg_dirty[i] > 0)
            .map(|i| format!("{}={}", WatchedReg::ALL[i].name(), r.per_reg_dirty[i]))
            .collect();
        let verdict = if r.dirty_pct > 1.0 { "OK dirty" } else { "!! NOT DIRTY (probe bug?)" };
        println!(
            "  {:<32} {:>6.2}% dirty  ({} / {} lines)  [{}]  {}",
            p.file_stem().unwrap().to_string_lossy(),
            r.dirty_pct,
            r.dirty_lines,
            r.total_visible,
            breakdown.join(" "),
            verdict,
        );
    }
    if !any {
        eprintln!("no mealybug ROMs found; adjust paths in run_sanity()");
    }
}
