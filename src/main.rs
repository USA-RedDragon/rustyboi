#![forbid(unsafe_code)]

mod gb;
mod cartridge;
mod cpu;
mod display;
mod memory;
mod ppu;

use clap::Parser;
use pixels;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to the ROM file
    #[arg(short, long)]
    rom: Option<String>,

    #[arg(short, long)]
    bios: Option<String>,

    #[arg(short, long, default_value_t = false)]
    gui: bool,
}

fn main() -> Result<(), pixels::Error> {
    let args = Args::parse();

    if args.gui {
        return display::run_with_gui(args.bios, args.rom);
    }

    let mut gb = gb::GB::new();
    gb.set_display_callback(Box::new(display::Terminal::render_frame));

    if let Some(rom) = args.rom {
        let cartridge = cartridge::Cartridge::load(&rom)
            .expect("Failed to load ROM file");
        gb.insert(cartridge);
    }

    if let Some(bios) = args.bios {
        gb.load_bios(&bios)
            .expect("Failed to load BIOS file");
    }

    gb.run();
    Ok(())
}
