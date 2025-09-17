mod gb;
mod cartridge;
mod cpu;
mod display;
mod memory;
mod ppu;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to the ROM file
    #[arg(short, long)]
    rom: Option<String>,

    #[arg(short, long)]
    bios: Option<String>,
}

fn main() {
    let args = Args::parse();

    let mut gb = gb::GB::new(display::Terminal::new());

    if let Some(bios) = args.bios {
        gb.load_bios(&bios)
            .expect("Failed to load BIOS file");
    }

    if let Some(rom) = args.rom {
        let cartridge = cartridge::Cartridge::load(&rom)
            .expect("Failed to load ROM file");
        gb.insert(cartridge);
    }

    gb.run();
}
