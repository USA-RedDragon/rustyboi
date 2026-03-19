//! Diagnostic: run a ROM for N frames and dump cartridge save RAM to a file.
//! Usage: sramdump <rom> <out.bin> [frames] [dmg|cgb]
use rustyboi_core_lib::cartridge::Cartridge;
use rustyboi_core_lib::gb::{Hardware, GB};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: sramdump <rom> <out.bin> [frames] [dmg|cgb]");
        std::process::exit(1);
    }
    let frames: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(800);
    let bytes = std::fs::read(&args[1]).expect("read ROM file");
    let cart = Cartridge::from_bytes(&bytes).expect("load ROM");
    let hardware = match args.get(4).map(|s| s.as_str()) {
        Some("dmg") => Hardware::DMG,
        Some("cgb") => Hardware::CGB,
        _ => {
            if cart.supports_cgb() {
                Hardware::CGB
            } else {
                Hardware::DMG
            }
        }
    };
    let mut gb = GB::new(hardware);
    gb.insert(cart);
    gb.skip_bios();
    for _ in 0..frames {
        gb.run_until_frame(false);
    }
    let sram = gb.cartridge().expect("cartridge").save_ram().to_vec();
    std::fs::write(&args[2], &sram).expect("write dump");
    eprintln!("dumped {} bytes", sram.len());
}
