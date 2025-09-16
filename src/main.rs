mod gb;
mod cpu;
mod memory;
mod ppu;

fn main() {
    let mut gb = gb::GB::new();
    loop {
        gb.step();
    }
}
