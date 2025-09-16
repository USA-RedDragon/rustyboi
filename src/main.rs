mod gb;
mod cpu;
mod memory;
mod ppu;

fn main() {
    gb::GB::new().run();
}
