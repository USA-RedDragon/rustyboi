//! Two-instance link-cable driver: the reference integration for
//! `GB::connect_link` (frontends follow the same shape: create both
//! instances, connect, pump them in lockstep — per frame is enough, the
//! link's hold/arm handshake absorbs sub-frame interleave).
//!
//! Runs two ROMs joined by a link cable with optional battery saves and
//! per-side scripted joypad input, dumping PNG screenshots for headless
//! verification (e.g. driving two Pokémon instances to the Cable Club).
//!
//!   cargo run --release -p rustyboi-core --example link_demo -- \
//!     a.gb b.gb --sav-a a.sav --sav-b b.sav \
//!     --script-a a.txt --script-b b.txt \
//!     --frames 3600 --out shots --dump-every 60
//!
//! Script lines: `<start_frame> <end_frame> <btn>[+<btn>...]` (half-open
//! frame range, buttons: a b start select up down left right), `#` comments.

use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Frame, GB, Hardware};
use rustyboi_core_lib::input::ButtonState;

use std::fs;
use std::path::{Path, PathBuf};

struct ScriptEntry {
    start: u32,
    end: u32,
    buttons: ButtonState,
}

fn parse_script(path: &Path) -> Vec<ScriptEntry> {
    let mut out = Vec::new();
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    for (ln, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(start), Some(end), Some(btns)) = (parts.next(), parts.next(), parts.next())
        else {
            panic!("{}:{}: expected `<start> <end> <buttons>`", path.display(), ln + 1);
        };
        let mut buttons = ButtonState::default();
        for b in btns.split('+') {
            match b {
                "a" => buttons.a = true,
                "b" => buttons.b = true,
                "start" => buttons.start = true,
                "select" => buttons.select = true,
                "up" => buttons.up = true,
                "down" => buttons.down = true,
                "left" => buttons.left = true,
                "right" => buttons.right = true,
                "none" => {}
                other => panic!("{}:{}: unknown button {other}", path.display(), ln + 1),
            }
        }
        out.push(ScriptEntry {
            start: start.parse().unwrap(),
            end: end.parse().unwrap(),
            buttons,
        });
    }
    out
}

fn buttons_at(script: &[ScriptEntry], frame: u32) -> ButtonState {
    let mut state = ButtonState::default();
    for e in script.iter().filter(|e| (e.start..e.end).contains(&frame)) {
        state.a |= e.buttons.a;
        state.b |= e.buttons.b;
        state.start |= e.buttons.start;
        state.select |= e.buttons.select;
        state.up |= e.buttons.up;
        state.down |= e.buttons.down;
        state.left |= e.buttons.left;
        state.right |= e.buttons.right;
    }
    state
}

fn load_gb(rom: &Path, sav: Option<&Path>) -> GB {
    let bytes = fs::read(rom).unwrap_or_else(|e| panic!("{}: {e}", rom.display()));
    let mut cart = Cartridge::from_bytes(&bytes).unwrap();
    if let Some(sav) = sav {
        let sram = fs::read(sav).unwrap_or_else(|e| panic!("{}: {e}", sav.display()));
        cart.load_sram_bytes(&sram).unwrap();
    }
    let mut gb = GB::new(Hardware::DMG);
    gb.insert(cart);
    gb.skip_bios();
    gb
}

fn frame_rgb(frame: Frame) -> Vec<u8> {
    match frame {
        Frame::Monochrome(data) => data
            .iter()
            .flat_map(|&shade| {
                let g = 0xFF - (shade & 3) * 0x55;
                [g, g, g]
            })
            .collect(),
        Frame::Color(data) => data.to_vec(),
    }
}

fn dump_png(gb: &mut GB, dir: &Path, side: char, frame_no: u32) {
    let rgb = frame_rgb(gb.get_current_frame());
    let path = dir.join(format!("{side}-f{frame_no:06}.png"));
    fs::write(&path, encode_rgb_png(160, 144, &rgb)).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut roms: Vec<PathBuf> = Vec::new();
    let (mut sav_a, mut sav_b, mut script_a, mut script_b) = (None, None, None, None);
    let mut out_dir = PathBuf::from("link-shots");
    let mut frames = 600u32;
    let mut dump_every = 0u32;
    let mut dumps: Vec<u32> = Vec::new();
    let mut watches: Vec<u16> = vec![0xFF01, 0xFF02];
    let (mut save_out_a, mut save_out_b): (Option<PathBuf>, Option<PathBuf>) = (None, None);

    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let mut val = || it.next().expect("missing option value");
        match arg.as_str() {
            "--sav-a" => sav_a = Some(PathBuf::from(val())),
            "--sav-b" => sav_b = Some(PathBuf::from(val())),
            "--save-out-a" => save_out_a = Some(PathBuf::from(val())),
            "--save-out-b" => save_out_b = Some(PathBuf::from(val())),
            "--script-a" => script_a = Some(PathBuf::from(val())),
            "--script-b" => script_b = Some(PathBuf::from(val())),
            "--out" => out_dir = PathBuf::from(val()),
            "--frames" => frames = val().parse().unwrap(),
            "--dump-every" => dump_every = val().parse().unwrap(),
            "--dump" => dumps.extend(val().split(',').map(|f| f.parse::<u32>().unwrap())),
            "--watch" => {
                watches.push(u16::from_str_radix(val().trim_start_matches("0x"), 16).unwrap())
            }
            other => roms.push(PathBuf::from(other)),
        }
    }
    let [rom_a, rom_b] = roms.as_slice() else {
        panic!("usage: link_demo <rom_a> <rom_b> [options]");
    };

    let script_a = script_a.map(|p| parse_script(&p)).unwrap_or_default();
    let script_b = script_b.map(|p| parse_script(&p)).unwrap_or_default();
    fs::create_dir_all(&out_dir).unwrap();

    let mut a = load_gb(rom_a, sav_a.as_deref());
    let mut b = load_gb(rom_b, sav_b.as_deref());
    GB::connect_link(&mut a, &mut b);

    let mut watch_prev: Vec<(u8, u8)> = Vec::new();
    for frame in 0..frames {
        a.set_input_state(buttons_at(&script_a, frame));
        b.set_input_state(buttons_at(&script_b, frame));
        a.run_until_frame(false);
        b.run_until_frame(false);

        let watch_now: Vec<(u8, u8)> = watches
            .iter()
            .map(|&addr| (a.read_memory(addr), b.read_memory(addr)))
            .collect();
        if watch_now != watch_prev {
            let mut line = format!("f={frame:05}");
            for (i, &addr) in watches.iter().enumerate() {
                line.push_str(&format!(
                    " [{addr:04X}] A={:02X} B={:02X}",
                    watch_now[i].0, watch_now[i].1
                ));
            }
            println!("{line}");
            watch_prev = watch_now;
        }

        if (dump_every != 0 && frame % dump_every == 0) || dumps.contains(&frame) {
            dump_png(&mut a, &out_dir, 'a', frame);
            dump_png(&mut b, &out_dir, 'b', frame);
        }
    }
    dump_png(&mut a, &out_dir, 'a', frames);
    dump_png(&mut b, &out_dir, 'b', frames);
    if let Some(path) = save_out_a {
        fs::write(&path, a.cartridge_mut().unwrap().save_ram()).unwrap();
    }
    if let Some(path) = save_out_b {
        fs::write(&path, b.cartridge_mut().unwrap().save_ram()).unwrap();
    }
    println!("done: {frames} frames, shots in {}", out_dir.display());
}

// ---- minimal RGB PNG (stored deflate), no dependencies --------------------

fn encode_rgb_png(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    assert_eq!(rgb.len(), (width * height * 3) as usize);
    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit RGB
    write_chunk(&mut png, b"IHDR", &ihdr);

    let stride = width as usize * 3;
    let mut raw = Vec::with_capacity((stride + 1) * height as usize);
    for row in rgb.chunks(stride) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut idat = vec![0x78, 0x01];
    let n_blocks = raw.len().div_ceil(0xFFFF);
    for (i, block) in raw.chunks(0xFFFF).enumerate() {
        idat.push((i + 1 == n_blocks) as u8);
        idat.extend_from_slice(&(block.len() as u16).to_le_bytes());
        idat.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        idat.extend_from_slice(block);
    }
    idat.extend_from_slice(&adler32(&raw).to_be_bytes());
    write_chunk(&mut png, b"IDAT", &idat);
    write_chunk(&mut png, b"IEND", &[]);
    png
}

fn write_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = png.len();
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let crc = crc32(&png[start..]);
    png.extend_from_slice(&crc.to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += byte as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}
